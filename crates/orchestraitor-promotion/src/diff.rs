//! Semantic and textual diff generation (spec §9.14).
//!
//! The promotion pipeline generates both a textual (unified) diff for the
//! review surface and a semantic summary that classifies the nature of the
//! change. This is presentation logic — it contains no security decisions.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Maximum lines per side before the diff falls back to a summary.
///
/// The LCS table is O(n·m) in memory; this budget keeps it bounded for the
/// review surface. The production transaction engine stores full patches in
/// the filesystem CAS (spec §9.5) and only renders compact deltas here.
const MAX_DIFF_LINES: usize = 4_000;

/// A single line in a textual diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffLine {
    /// An unchanged context line.
    Context(String),
    /// A line present only in the new content.
    Added(String),
    /// A line present only in the old content.
    Removed(String),
}

/// A contiguous hunk of changes in a textual diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffHunk {
    /// Starting line number in the old content (1-based).
    pub old_start: usize,
    /// Number of old-content lines covered by this hunk.
    pub old_count: usize,
    /// Starting line number in the new content (1-based).
    pub new_start: usize,
    /// Number of new-content lines covered by this hunk.
    pub new_count: usize,
    /// The lines in this hunk, in order.
    pub lines: Vec<DiffLine>,
}

/// A textual (unified-style) diff for a single path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextualDiff {
    /// Repository-relative path of the changed file.
    pub path: PathBuf,
    /// Whether the diff was truncated to a summary due to size.
    pub truncated: bool,
    /// The hunks composing the diff.
    pub hunks: Vec<DiffHunk>,
}

/// The semantic nature of a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// The file was newly created.
    Added,
    /// The file was deleted.
    Removed,
    /// The file was modified in place.
    Modified,
}

/// A semantic diff summary for a single path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticDiff {
    /// Repository-relative path of the changed file.
    pub path: PathBuf,
    /// The kind of change.
    pub kind: ChangeKind,
    /// Number of lines added.
    pub lines_added: usize,
    /// Number of lines removed.
    pub lines_removed: usize,
    /// Human-readable summary.
    pub summary: String,
}

/// Computes a textual diff between old and new content for a path.
///
/// When `old` is empty the change is an addition; when `new` is empty it is a
/// removal. Files exceeding [`MAX_DIFF_LINES`] lines per side produce a
/// truncated diff with no hunks.
#[must_use]
pub fn compute_textual_diff(path: PathBuf, old: &[u8], new: &[u8]) -> TextualDiff {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);

    if old_lines.len() > MAX_DIFF_LINES || new_lines.len() > MAX_DIFF_LINES {
        return TextualDiff {
            path,
            truncated: true,
            hunks: Vec::new(),
        };
    }

    let raw = lcs_diff(&old_lines, &new_lines);
    let hunks = group_hunks(&raw);

    TextualDiff {
        path,
        truncated: false,
        hunks,
    }
}

/// Computes a semantic diff summary from old and new content.
#[must_use]
pub fn compute_semantic_diff(path: PathBuf, old: &[u8], new: &[u8]) -> SemanticDiff {
    let kind = if old.is_empty() && !new.is_empty() {
        ChangeKind::Added
    } else if !old.is_empty() && new.is_empty() {
        ChangeKind::Removed
    } else {
        ChangeKind::Modified
    };

    let diff = compute_textual_diff(path.clone(), old, new);
    let (lines_added, lines_removed) = diff.hunks.iter().flat_map(|h| &h.lines).fold(
        (0, 0),
        |(added, removed), line| match line {
            DiffLine::Added(_) => (added + 1, removed),
            DiffLine::Removed(_) => (added, removed + 1),
            DiffLine::Context(_) => (added, removed),
        },
    );

    let summary = format!(
        "{kind:?}: +{lines_added} -{lines_removed} lines{}",
        if diff.truncated {
            " (diff truncated)"
        } else {
            ""
        }
    );

    SemanticDiff {
        path,
        kind,
        lines_added,
        lines_removed,
        summary,
    }
}

fn split_lines(bytes: &[u8]) -> Vec<&str> {
    std::str::from_utf8(bytes)
        .map(|text| text.lines().collect::<Vec<_>>())
        .unwrap_or_default()
}

/// LCS-based diff producing an ordered list of diff lines.
fn lcs_diff<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<DiffLine> {
    let m = old.len();
    let n = new.len();
    let mut dp = vec![vec![0_u32; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1].saturating_add(1)
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    let mut result = Vec::with_capacity(m + n);
    let (mut i, mut j) = (0, 0);
    while i < m && j < n {
        if old[i] == new[j] {
            result.push(DiffLine::Context(old[i].to_string()));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            result.push(DiffLine::Removed(old[i].to_string()));
            i += 1;
        } else {
            result.push(DiffLine::Added(new[j].to_string()));
            j += 1;
        }
    }
    while i < m {
        result.push(DiffLine::Removed(old[i].to_string()));
        i += 1;
    }
    while j < n {
        result.push(DiffLine::Added(new[j].to_string()));
        j += 1;
    }
    result
}

/// Groups raw diff lines into hunks, collapsing runs of context.
///
/// A hunk opens at the first changed line, absorbs trailing context up to a
/// small window, and closes when a long context run separates changes.
fn group_hunks(lines: &[DiffLine]) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut current: Option<DiffHunk> = None;
    let (mut old_line, mut new_line) = (1_usize, 1_usize);

    for line in lines {
        let is_change = !matches!(line, DiffLine::Context(_));
        let context_full = current.as_ref().is_some_and(|h| h.lines.len() >= 6);

        if is_change {
            match &mut current {
                None => {
                    current = Some(DiffHunk {
                        old_start: old_line,
                        old_count: 0,
                        new_start: new_line,
                        new_count: 0,
                        lines: vec![line.clone()],
                    });
                }
                Some(hunk) => {
                    hunk.lines.push(line.clone());
                }
            }
        } else if context_full {
            if let Some(mut hunk) = current.take() {
                hunk.old_count = count_old(&hunk.lines);
                hunk.new_count = count_new(&hunk.lines);
                hunks.push(hunk);
            }
        } else if let Some(hunk) = &mut current {
            hunk.lines.push(line.clone());
        }

        match line {
            DiffLine::Context(_) => {
                old_line += 1;
                new_line += 1;
            }
            DiffLine::Added(_) => {
                new_line += 1;
            }
            DiffLine::Removed(_) => {
                old_line += 1;
            }
        }
    }

    if let Some(mut hunk) = current {
        hunk.old_count = count_old(&hunk.lines);
        hunk.new_count = count_new(&hunk.lines);
        hunks.push(hunk);
    }
    hunks
}

fn count_old(lines: &[DiffLine]) -> usize {
    lines
        .iter()
        .filter(|l| matches!(l, DiffLine::Context(_) | DiffLine::Removed(_)))
        .count()
}

fn count_new(lines: &[DiffLine]) -> usize {
    lines
        .iter()
        .filter(|l| matches!(l, DiffLine::Context(_) | DiffLine::Added(_)))
        .count()
}
