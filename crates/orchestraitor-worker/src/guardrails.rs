//! Anti-stuck guardrails for the worker attempt loop (spec
//! `10-orchestrator.md` §9.27.1 resource governance, §9.36 stall and churn
//! detection): pure, unit-tested detection primitives consumed by `run.rs`.
//!
//! Guards: tool-loop churn ([`ChurnWindow`]), no-progress fingerprint
//! ([`NoProgressGuard`] + [`progress_fingerprint`]), CI-poll budget
//! ([`PollBudget`] + [`poll_shaped`]). Each is off when its threshold is
//! zero; defaults come from [`GuardrailsConfig::bootstrap_defaults`].

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

/// Guardrail thresholds for one worker run. Defaults are active; an explicit
/// `0` disables the corresponding guard deliberately (the loop layer
/// validates that a zero is deliberate — see the `[loop.guardrails]` docs).
#[derive(Clone, Debug)]
pub struct GuardrailsConfig {
    /// Consecutive identical no-progress fingerprints that fail an attempt.
    pub no_progress_turns: u32,
    /// Repetitions of one normalized tool-call shape within
    /// [`GuardrailsConfig::tool_repeat_window`] turns that kill an attempt.
    pub tool_repeat_count: u32,
    /// Sliding-window length (in tool turns) for the churn guard.
    pub tool_repeat_window: u32,
    /// Cumulative poll-shaped bash wall-clock per attempt.
    pub ci_poll_budget: Duration,
}

impl GuardrailsConfig {
    /// Bootstrap defaults: no-progress 5 turns (inside the 25-turn attempt
    /// bound), churn 4 repeats in an 8-turn window, 30m cumulative CI-poll
    /// wait per attempt.
    #[must_use]
    pub fn bootstrap_defaults() -> Self {
        Self {
            no_progress_turns: 5,
            tool_repeat_count: 4,
            tool_repeat_window: 8,
            ci_poll_budget: Duration::from_mins(30),
        }
    }

    /// Returns true when the no-progress guard is enabled.
    #[must_use]
    pub fn no_progress_enabled(&self) -> bool {
        self.no_progress_turns > 0
    }

    /// Returns true when the churn guard is enabled (needs at least 2
    /// repeats in a non-empty window to be meaningful).
    #[must_use]
    pub fn churn_enabled(&self) -> bool {
        self.tool_repeat_count > 1 && self.tool_repeat_window > 0
    }

    /// Returns true when the CI-poll budget guard is enabled.
    #[must_use]
    pub fn poll_budget_enabled(&self) -> bool {
        !self.ci_poll_budget.is_zero()
    }
}

/// Normalized shape of one dispatched tool call: tool id plus a coarse
/// argument fingerprint, paired with the call's exit-code class. Two calls
/// with the same shape did "the same kind of thing" — the churn guard fires
/// on repetition of the KIND of call, never on identical payloads alone
/// (a model rewriting a big file each turn must NOT churn-fire, so
/// `write_file` shapes carry the path; a `bash` shape carries the first
/// shell word).
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ToolCallShape {
    /// Tool name as receipted (`bash`, `read_file`, `write_file`, `search`,
    /// or the sanitized refused name).
    pub tool: String,
    /// Coarse argument fingerprint (first shell word for `bash`, the path
    /// for file tools, a bucketed pattern size for `search`).
    pub arg_shape: String,
    /// Exit-code class of the call.
    pub exit_class: ExitClass,
}

/// Exit-code class of one tool call.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExitClass {
    /// Exit code 0 (or a non-bash tool that completed).
    Ok,
    /// A recorded non-zero exit code.
    NonZero,
    /// No exit code exists (refusal, mediation failure, non-bash tool).
    None,
}

/// Buckets a `search` pattern length (1..2, 2..4, ... 64+ chars) so two
/// long-but-different patterns of the same size class count as one shape.
fn pattern_bucket(chars: usize) -> u64 {
    let mut size = 1_u64;
    let chars = u64::try_from(chars).unwrap_or(u64::MAX);
    while size < 64 && chars > size {
        size *= 2;
    }
    size
}

/// Extracts the first shell word of a bash script, skipping leading
/// whitespace and environment-assignment prefixes (`VAR=x cmd`).
fn bash_first_word(script: &str) -> String {
    let mut rest = script.trim_start();
    while let Some(eq) = rest.find('=') {
        let name = &rest[..eq];
        if name.is_empty()
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            || name
                .chars()
                .next()
                .is_some_and(|c: char| c.is_ascii_digit())
        {
            break;
        }
        let Some(space) = rest.find(char::is_whitespace) else {
            break;
        };
        rest = rest[space..].trim_start();
    }
    rest.split(char::is_whitespace)
        .next()
        .unwrap_or_default()
        .trim_matches(|c| c == '\'' || c == '"')
        .chars()
        .take(64)
        .collect()
}

/// Computes the normalized shape of one dispatched action + its receipt.
#[must_use]
pub(crate) fn tool_call_shape(
    action: &crate::action::WorkerAction,
    receipt: &crate::tools::ToolReceipt,
) -> ToolCallShape {
    let exit_class = match receipt.exit_code {
        Some(0) => ExitClass::Ok,
        Some(_) => ExitClass::NonZero,
        None if receipt.outcome == "completed" => ExitClass::Ok,
        None => ExitClass::None,
    };
    let arg_shape = match action {
        crate::action::WorkerAction::Bash { script } => {
            format!("bash:{}", bash_first_word(script))
        }
        crate::action::WorkerAction::ReadFile { path }
        | crate::action::WorkerAction::WriteFile { path, .. } => format!("path:{path}"),
        crate::action::WorkerAction::Search { pattern, .. } => {
            format!("pattern:{}", pattern_bucket(pattern.chars().count()))
        }
        crate::action::WorkerAction::Finish { .. } => "finish".to_string(),
    };
    ToolCallShape {
        tool: receipt.tool.clone(),
        arg_shape,
        exit_class,
    }
}

/// Sliding-window churn detector: the same [`ToolCallShape`] appearing
/// `tool_repeat_count` times within the last `tool_repeat_window`
/// observations fires.
#[derive(Debug)]
pub struct ChurnWindow {
    window: usize,
    repeat_count: usize,
    shapes: VecDeque<ToolCallShape>,
}

impl ChurnWindow {
    /// Creates a window; disabled configs get a zero window that never fires.
    #[must_use]
    pub fn new(config: &GuardrailsConfig) -> Self {
        Self {
            window: if config.churn_enabled() {
                config.tool_repeat_window as usize
            } else {
                0
            },
            repeat_count: config.tool_repeat_count.max(2) as usize,
            shapes: VecDeque::new(),
        }
    }

    /// Records one turn's shape; true when the churn bound fired.
    #[must_use]
    pub fn observe(&mut self, shape: ToolCallShape) -> bool {
        if self.window == 0 {
            return false;
        }
        self.shapes.push_back(shape);
        while self.shapes.len() > self.window {
            self.shapes.pop_front();
        }
        let Some(newest) = self.shapes.back() else {
            return false;
        };
        let count = self
            .shapes
            .iter()
            .filter(|candidate| **candidate == *newest)
            .count();
        count >= self.repeat_count
    }

    /// The evidence shapes currently in the window (for row detail).
    #[must_use]
    pub fn evidence(&self) -> Vec<ToolCallShape> {
        self.shapes.iter().cloned().collect()
    }
}

/// Cheap per-turn progress fingerprint: an FNV-1a digest of the worktree's
/// file surface (relative path + content length + mtime seconds per file,
/// skipping `.git`) plus the untrusted-write set. No subprocess, no index
/// lock — deterministic and cheap. Walk/stat failures contribute a stable
/// marker rather than failing the run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProgressFingerprint(u64);

fn fold(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0100_0193);
    }
}

/// Computes the fingerprint of the worktree at `root`.
#[must_use]
pub fn progress_fingerprint(root: &Path, untrusted_writes: &[String]) -> ProgressFingerprint {
    let mut hash = 0x811c_9dc5;
    fold(&mut hash, b"v1");
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            fold(&mut hash, b"|unreadable");
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                fold(&mut hash, b"|stat-failed");
                continue;
            };
            if file_type.is_dir() {
                // `.git` is skipped: mediated commits are the delivery
                // seam's progress story, not the churn detector's. Build
                // output (`target`, `node_modules`) is skipped too: a bash
                // `cargo build` churns thousands of files there without
                // representing task progress, which would mask genuine
                // no-progress in the source tree.
                let name = path.file_name().and_then(std::ffi::OsStr::to_str);
                if matches!(name, Some(".git" | "target" | "node_modules")) {
                    continue;
                }
                stack.push(path);
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                fold(&mut hash, b"|stat-failed");
                continue;
            };
            fold(
                &mut hash,
                path.strip_prefix(root)
                    .map_or_else(
                        |_| path.as_os_str().to_string_lossy().into_owned(),
                        |relative| relative.to_string_lossy().into_owned(),
                    )
                    .as_bytes(),
            );
            fold(&mut hash, &metadata.len().to_le_bytes());
            // mtime seconds only: unchanged files never produce sub-second
            // rewrites, and second granularity stays cheap and stable.
            fold(
                &mut hash,
                &metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |duration| duration.as_secs())
                    .to_le_bytes(),
            );
        }
    }
    for write in untrusted_writes {
        fold(&mut hash, b"|w:");
        fold(&mut hash, write.as_bytes());
    }
    ProgressFingerprint(hash)
}

/// Consecutive-identical-fingerprint tracker.
#[derive(Debug)]
pub struct NoProgressGuard {
    threshold: u32,
    streak: u32,
    last: Option<ProgressFingerprint>,
}

impl NoProgressGuard {
    /// Creates a guard; disabled configs never fire.
    #[must_use]
    pub fn new(config: &GuardrailsConfig) -> Self {
        Self {
            threshold: config.no_progress_turns,
            streak: 0,
            last: None,
        }
    }

    /// Records one turn's fingerprint; true when `threshold` consecutive
    /// identical fingerprints have been observed.
    #[must_use]
    pub fn observe(&mut self, fingerprint: ProgressFingerprint) -> bool {
        if self.threshold == 0 {
            return false;
        }
        if self.last == Some(fingerprint) {
            self.streak += 1;
        } else {
            self.streak = 1;
            self.last = Some(fingerprint);
        }
        self.streak >= self.threshold
    }
}

/// Poll-shaped bash execution detector (bounded heuristic). A script is
/// poll-shaped when it WAITS on an external CI surface:
///
/// - a `sleep` token anywhere in a compound script (`sleep 30 && gh pr
///   checks`, `while true; do sleep 60; gh run watch; done`), or
/// - a blocking watch command (`gh run watch`, or any command carrying a
///   `--watch` flag) even without an explicit sleep.
///
/// The CI surfaces are `gh pr checks`, `gh run watch`, `gh run list`, and
/// `gh pr view`. Only such scripts charge the budget.
#[must_use]
pub fn poll_shaped(script: &str) -> bool {
    let rest = script.to_ascii_lowercase();
    let ci_wait = [
        "gh pr checks",
        "gh run watch",
        "gh run list",
        "gh pr view",
        "--watch",
    ]
    .iter()
    .any(|needle| rest.contains(needle));
    if !ci_wait {
        return false;
    }
    // Either an explicit sleep somewhere in the script, or a blocking
    // watch surface on its own.
    let tokens: Vec<&str> = rest
        .split(|c: char| c.is_whitespace() || c == ';' || c == '&')
        .collect();
    tokens.contains(&"sleep") || rest.contains("gh run watch") || rest.contains("--watch")
}

/// Cumulative external-poll wall-clock budget for one attempt.
#[derive(Debug)]
pub struct PollBudget {
    budget: Duration,
    spent: Duration,
}

impl PollBudget {
    /// Creates a budget; disabled configs (`ci_poll_budget == 0`) never
    /// fire. Sub-second poll-shaped sleeps still charge (40 x 1s sleeps are
    /// exactly the churn the guard exists for).
    #[must_use]
    pub fn new(config: &GuardrailsConfig) -> Self {
        Self {
            budget: config.ci_poll_budget,
            spent: Duration::ZERO,
        }
    }

    /// Charges one poll-shaped execution's wall-clock; true when the
    /// cumulative budget is exceeded.
    #[must_use]
    pub fn charge(&mut self, elapsed: Duration) -> bool {
        if self.budget.is_zero() {
            return false;
        }
        self.spent = self.spent.saturating_add(elapsed);
        self.spent > self.budget
    }

    /// Cumulative poll wall-clock charged so far.
    #[must_use]
    pub fn spent(&self) -> Duration {
        self.spent
    }

    /// Wall-clock left before the budget fires; `None` when the guard is
    /// disabled (a poll-shaped dispatch then runs unbounded, as before).
    /// The caller passes this to the dispatch timeout so a poll-shaped
    /// Bash call is CANCELLED at the budget boundary instead of only being
    /// charged after it returns (spec `50-contracts-data.md` §21.10: the
    /// orchestrator never runs an unbounded poll loop).
    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        if self.budget.is_zero() {
            return None;
        }
        Some(self.budget.saturating_sub(self.spent))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn bash_shape(script: &str, exit_code: i32) -> ToolCallShape {
        tool_call_shape(
            &crate::action::WorkerAction::Bash {
                script: script.to_string(),
            },
            &crate::tools::ToolReceipt {
                sequence: 1,
                tool: "bash".to_string(),
                admitted: true,
                outcome: "completed",
                reason: None,
                exit_code: Some(exit_code),
            },
        )
    }

    #[test]
    fn bash_first_word_normalizes_env_prefixes_and_quotes() {
        assert_eq!(bash_first_word("mktemp -d"), "mktemp");
        assert_eq!(bash_first_word("  FOO=1 mktemp -d"), "mktemp");
        assert_eq!(bash_first_word("'gh' pr checks"), "gh");
        assert_eq!(bash_first_word(""), "");
    }

    #[test]
    fn tool_shape_groups_identical_bash_scripts_and_splits_by_exit_class() {
        let ok = bash_shape("mktemp -d", 0);
        let ok_again = bash_shape("mktemp -d", 0);
        let failed = bash_shape("mktemp -d", 1);
        assert_eq!(ok, ok_again);
        assert_ne!(ok, failed);
        assert_eq!(ok.tool, "bash");
        assert_eq!(ok.exit_class, ExitClass::Ok);
    }

    #[test]
    fn write_file_shapes_carry_paths_so_rewrites_do_not_churn() {
        let write = tool_call_shape(
            &crate::action::WorkerAction::WriteFile {
                path: "src/a.rs".to_string(),
                content: "one".to_string(),
            },
            &crate::tools::ToolReceipt {
                sequence: 1,
                tool: "write_file".to_string(),
                admitted: true,
                outcome: "completed",
                reason: None,
                exit_code: None,
            },
        );
        let rewrite = tool_call_shape(
            &crate::action::WorkerAction::WriteFile {
                path: "src/a.rs".to_string(),
                content: "completely different and much longer content".to_string(),
            },
            &crate::tools::ToolReceipt {
                sequence: 2,
                tool: "write_file".to_string(),
                admitted: true,
                outcome: "completed",
                reason: None,
                exit_code: None,
            },
        );
        assert_eq!(write, rewrite, "content size must not affect the shape");
        assert_eq!(write.arg_shape, "path:src/a.rs");
    }

    #[test]
    fn mktemp_loop_fires_the_churn_window() {
        // The incident shape: mktemp+printf+chmod x40. K=4 in W=8.
        let config = GuardrailsConfig::bootstrap_defaults();
        let mut window = ChurnWindow::new(&config);
        let mut fired = false;
        for i in 0..10 {
            let script = if i % 2 == 0 {
                "mktemp -d".to_string()
            } else {
                "printf x > /dev/null".to_string()
            };
            fired = window.observe(bash_shape(&script, 0));
            if fired {
                break;
            }
        }
        assert!(fired, "mktemp loop must fire by the 4th repeat");
        // Fires on the 7th observation: the window then holds
        // mktemp,printf,mktemp,printf,mktemp,printf,mktemp — the 4th
        // mktemp within the 8-turn window.
        assert_eq!(window.evidence().len(), 7);
    }

    #[test]
    fn alternating_distinct_work_never_fires_the_window() {
        let config = GuardrailsConfig::bootstrap_defaults();
        let mut window = ChurnWindow::new(&config);
        for i in 0..40_u32 {
            // Genuinely distinct command shapes (different first words);
            // `echo a` vs `echo b` intentionally share one shape.
            let script = format!("cmd{i} --flag");
            assert!(!window.observe(bash_shape(&script, 0)), "turn {i}");
        }
    }

    #[test]
    fn disabled_churn_never_fires() {
        let config = GuardrailsConfig {
            tool_repeat_count: 0,
            ..GuardrailsConfig::bootstrap_defaults()
        };
        assert!(!config.churn_enabled());
        let mut window = ChurnWindow::new(&config);
        for _ in 0..20 {
            assert!(!window.observe(bash_shape("mktemp -d", 0)));
        }
    }

    fn temp_tree(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(full, content).unwrap();
        }
        dir
    }

    #[test]
    fn identical_worktree_state_gives_identical_fingerprints() {
        let dir = temp_tree(&[("src/a.rs", "one"), ("src/b.rs", "two")]);
        let f1 = progress_fingerprint(dir.path(), &[]);
        let f2 = progress_fingerprint(dir.path(), &[]);
        assert_eq!(f1, f2);
    }

    #[test]
    fn a_content_change_changes_the_fingerprint() {
        let dir = temp_tree(&[("src/a.rs", "one")]);
        let before = progress_fingerprint(dir.path(), &[]);
        std::fs::write(dir.path().join("src/a.rs"), "changed").unwrap();
        let after = progress_fingerprint(dir.path(), &[]);
        assert_ne!(before, after);
    }

    #[test]
    fn git_dir_is_ignored_but_regular_files_are_not() {
        let dir = temp_tree(&[("src/a.rs", "one")]);
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main").unwrap();
        let with_git = progress_fingerprint(dir.path(), &[]);
        let without_git = temp_tree(&[("src/a.rs", "one")]);
        assert_eq!(with_git, progress_fingerprint(without_git.path(), &[]));
    }

    #[test]
    fn no_progress_guard_fires_on_threshold_consecutive_identical() {
        let config = GuardrailsConfig::bootstrap_defaults();
        let mut guard = NoProgressGuard::new(&config);
        let fp = ProgressFingerprint(42);
        assert!(!guard.observe(fp));
        assert!(!guard.observe(fp));
        assert!(!guard.observe(fp));
        assert!(!guard.observe(fp));
        assert!(guard.observe(fp), "5th identical fingerprint fires");
        // Progress resets the streak.
        assert!(!guard.observe(ProgressFingerprint(43)));
        assert!(!guard.observe(ProgressFingerprint(43)));
    }

    #[test]
    fn disabled_no_progress_never_fires() {
        let config = GuardrailsConfig {
            no_progress_turns: 0,
            ..GuardrailsConfig::bootstrap_defaults()
        };
        assert!(!config.no_progress_enabled());
        let mut guard = NoProgressGuard::new(&config);
        for _ in 0..20 {
            assert!(!guard.observe(ProgressFingerprint(7)));
        }
    }

    #[test]
    fn poll_shape_requires_sleep_plus_ci_reference() {
        assert!(poll_shaped("sleep 30 && gh pr checks 42"));
        assert!(poll_shaped("sleep 60; gh run watch --exit-status"));
        assert!(poll_shaped(
            "sleep 5 && gh pr view 1 --json statusCheckRollup"
        ));
        assert!(!poll_shaped("gh pr checks 42"));
        assert!(!poll_shaped("sleep 30"));
        assert!(!poll_shaped("echo done"));
    }

    #[test]
    fn poll_budget_accumulates_and_fires() {
        let config = GuardrailsConfig::bootstrap_defaults();
        let mut budget = PollBudget::new(&config);
        assert!(!budget.charge(Duration::from_mins(20)));
        assert!(!budget.charge(Duration::from_mins(10)));
        assert!(budget.charge(Duration::from_secs(1)), "exceeding fires");
        assert_eq!(
            budget.spent(),
            Duration::from_mins(30) + Duration::from_secs(1)
        );
    }

    #[test]
    fn disabled_poll_budget_never_fires() {
        let config = GuardrailsConfig {
            ci_poll_budget: Duration::ZERO,
            ..GuardrailsConfig::bootstrap_defaults()
        };
        assert!(!config.poll_budget_enabled());
        let mut budget = PollBudget::new(&config);
        assert!(!budget.charge(Duration::from_hours(100)));
    }

    #[test]
    fn remaining_reports_wall_clock_left_until_the_budget_fires() {
        let mut config = GuardrailsConfig::bootstrap_defaults();
        config.ci_poll_budget = Duration::from_secs(30);
        let mut budget = PollBudget::new(&config);
        assert_eq!(budget.remaining(), Some(Duration::from_secs(30)));
        assert!(!budget.charge(Duration::from_secs(10)));
        assert_eq!(budget.remaining(), Some(Duration::from_secs(20)));
        assert!(budget.charge(Duration::from_secs(21)), "exceeding fires");
        assert_eq!(budget.remaining(), Some(Duration::ZERO));
    }

    #[test]
    fn remaining_is_none_for_a_disabled_budget() {
        let config = GuardrailsConfig {
            ci_poll_budget: Duration::ZERO,
            ..GuardrailsConfig::bootstrap_defaults()
        };
        assert_eq!(PollBudget::new(&config).remaining(), None);
    }
}
