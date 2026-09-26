//! `docs-check` — documentation-invariant validation for the spec set.
//!
//! Three checks run against a repository root:
//!
//! 1. Index integrity — the markdown table under `## Compatibility index` in
//!    `docs/spec/spec.md` must have the expected header, well-formed rows,
//!    unique identifiers, and location cells (`<file>#<anchor>`) whose file
//!    exists and whose anchor matches a heading slug in that file.
//! 2. Legacy reference resolution — markdown, YAML, TOML, and Rust comment
//!    lines are scanned for section references of the document-qualified and
//!    bare forms. References qualified to the spec resolve through the
//!    compatibility index; references qualified to the technology stack
//!    resolve against that document's heading numbers directly and need no
//!    index row. A small documented exception set
//!    ([`TECH_STACK_SCOPED`]) covers identifiers that are stack-scoped even
//!    when cited spec-side: they resolve only when the index file carries an
//!    explanatory footnote naming the identifier outside the table.
//! 3. Markdown links — every relative link to a local markdown file (with or
//!    without a fragment) inside `docs/spec/*.md` must point at an existing
//!    file, and an existing heading anchor when a fragment is present.
//!
//! Anchors follow the GitHub heading-slug algorithm; see [`github_slug`].

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Path of the compatibility index, relative to the repository root.
const INDEX_PATH: &str = "docs/spec/spec.md";
/// Path of the technology-stack document, relative to the repository root.
const TECH_STACK_PATH: &str = "docs/spec/tech-stack.md";
/// Directory whose markdown files are subject to the link check.
const SPEC_DIR: &str = "docs/spec";
/// Heading that introduces the index table.
const INDEX_HEADING: &str = "Compatibility index";
/// Expected index-table header cells.
const INDEX_HEADER: [&str; 2] = ["Legacy \u{a7}", "New location"];

/// Directory names skipped while scanning for legacy references. `.git` and
/// `target` are repository/build metadata, `.omo` is plan state, and
/// `.arbitraitor` is hook cache litter.
const SKIP_DIRS: [&str; 4] = [".git", ".omo", ".arbitraitor", "target"];
/// File names skipped while scanning for legacy references.
const SKIP_FILES: [&str; 1] = ["Cargo.lock"];
/// Extensions scanned for legacy references (phase 2).
const SCAN_EXTENSIONS: [&str; 5] = ["md", "yml", "yaml", "toml", "rs"];

/// Identifiers that live in the technology-stack document even when cited
/// spec-side. Such an identifier resolves only when the index file carries an
/// explanatory footnote: a line outside the table that names the identifier.
/// `3.4` is the stack's provider-inference rule, never a spec section.
const TECH_STACK_SCOPED: [&str; 1] = ["3.4"];

/// A single validation failure, located by repo-relative path and line.
#[derive(Debug)]
pub(crate) struct Failure {
    /// Repo-relative path of the offending file.
    pub(crate) path: PathBuf,
    /// 1-based line number when the failure is tied to a line.
    pub(crate) line: Option<usize>,
    /// Human-readable description of what failed.
    pub(crate) message: String,
}

/// Aggregated result of all documentation checks.
#[derive(Debug, Default)]
pub(crate) struct Report {
    /// Number of data rows parsed from the compatibility index.
    pub(crate) index_rows: usize,
    /// Number of legacy references found and checked.
    pub(crate) references_checked: usize,
    /// Number of local markdown links checked.
    pub(crate) links_checked: usize,
    /// Every failure found; empty means all checks passed.
    pub(crate) failures: Vec<Failure>,
}

/// One parsed compatibility-index row.
#[derive(Debug)]
struct IndexEntry {
    /// Bare identifier (no section sign), e.g. `9.33.2` or `MVP-1`.
    id: String,
    /// Target document relative to `docs/spec/`.
    file: String,
    /// Expected GitHub anchor slug in the target document.
    anchor: String,
}

/// Parse result of the compatibility index.
#[derive(Default)]
struct Index {
    /// All well-formed rows (first occurrence wins for duplicates).
    entries: Vec<IndexEntry>,
    /// Every identifier that resolves through the index.
    ids: HashSet<String>,
    /// Exception identifiers documented by a footnote outside the table.
    footnote_ids: HashSet<String>,
}

/// Run every documentation check against the repository at `root`.
pub(crate) fn run(root: &Path) -> Report {
    let mut report = Report::default();
    let mut slugs = SlugCache::default();

    let index = parse_index(root, &mut report);
    check_index_targets(root, &index, &mut slugs, &mut report);
    report.index_rows = index.entries.len();
    let tech_stack_sections = load_tech_stack_sections(root, &mut report);
    check_references(root, &index, &tech_stack_sections, &mut report);
    check_markdown_links(root, &mut slugs, &mut report);

    report
}

/// GitHub heading-anchor slug: lowercase; keep alphanumerics, `_`, and `-`;
/// turn each space into `-`; drop every other character.
fn github_slug(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    for ch in title.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            for lower in ch.to_lowercase() {
                slug.push(lower);
            }
        } else if ch == ' ' {
            slug.push('-');
        }
    }
    slug
}

/// Lazily computed heading-slug sets keyed by absolute file path; `None`
/// means the file could not be read.
#[derive(Default)]
struct SlugCache {
    files: HashMap<PathBuf, Option<HashSet<String>>>,
}

impl SlugCache {
    /// Slugs of `path`, or `None` when the file is unreadable.
    fn get(&mut self, path: &Path) -> Option<&HashSet<String>> {
        self.files
            .entry(path.to_path_buf())
            .or_insert_with(|| fs::read_to_string(path).ok().map(|c| document_slugs(&c)))
            .as_ref()
    }
}

/// Every GitHub anchor slug produced by headings in `content`, applying
/// GitHub's `-1`, `-2`, ... suffixes to duplicate headings in document order.
fn document_slugs(content: &str) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut slugs = HashSet::new();
    for (_, title) in headings(content) {
        let base = github_slug(title);
        let occurrence = seen.entry(base.clone()).or_insert(0);
        let slug = if *occurrence == 0 {
            base
        } else {
            format!("{base}-{occurrence}")
        };
        *occurrence += 1;
        slugs.insert(slug);
    }
    slugs
}

/// Numbered section tokens from headings: for `### 3.2 Endpoints` the section
/// number is `3.2`; trailing dots on heading numbers are dropped.
fn section_numbers(content: &str) -> HashSet<String> {
    headings(content)
        .into_iter()
        .filter_map(|(_, title)| title.split_whitespace().next())
        .map(|token| token.trim_end_matches('.'))
        .filter(|token| {
            token.chars().next().is_some_and(|c| c.is_ascii_digit())
                && token.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
        .map(str::to_string)
        .collect()
}

#[derive(Clone, Copy)]
enum Fence {
    Backtick,
    Tilde,
}

/// `(line_number, title)` for every ATX heading in `content`, skipping
/// `#`-runs inside fenced code blocks.
fn headings(content: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut fence: Option<Fence> = None;
    for (offset, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fence = match fence {
                Some(Fence::Backtick) => None,
                None => Some(Fence::Backtick),
                Some(Fence::Tilde) => Some(Fence::Tilde),
            };
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = match fence {
                Some(Fence::Tilde) => None,
                None => Some(Fence::Tilde),
                Some(Fence::Backtick) => Some(Fence::Backtick),
            };
            continue;
        }
        if fence.is_some() {
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if hashes == 0 || hashes > 6 {
            continue;
        }
        let Some(title) = trimmed[hashes..].strip_prefix(' ') else {
            continue;
        };
        out.push((offset + 1, title.trim_end()));
    }
    out
}

/// Cells of a markdown table row, trims the outer pipes and each cell.
fn table_cells(line: &str) -> Vec<&str> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner.split('|').map(str::trim).collect()
}

/// Whether a table row is a `---`-style separator row.
fn is_separator(cells: &[&str]) -> bool {
    !cells.is_empty()
        && cells
            .iter()
            .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-'))
}

/// Parse one data row into `(identifier, file, anchor)` or return the reason
/// the row is malformed.
fn parse_row(cells: &[&str]) -> Result<(String, String, String), String> {
    if cells.len() != 2 {
        return Err(format!("expected 2 cells, found {}", cells.len()));
    }
    let id_cell = cells[0].trim_matches('`').trim();
    let id = if let Some(number) = id_cell.strip_prefix('\u{a7}') {
        number.to_string()
    } else if let Some(number) = id_cell.strip_prefix("MVP-") {
        format!("MVP-{number}")
    } else {
        return Err(format!(
            "identifier `{id_cell}` is not a section number or milestone id"
        ));
    };
    let valid = id
        .strip_prefix("MVP-")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        || (!id.is_empty()
            && id.chars().next().is_some_and(|c| c.is_ascii_digit())
            && id.chars().all(|c| c.is_ascii_digit() || c == '.')
            && !id.ends_with('.'));
    if !valid {
        return Err(format!("malformed identifier `{id_cell}`"));
    }

    let location = cells[1];
    let location = location
        .strip_prefix('`')
        .and_then(|l| l.strip_suffix('`'))
        .ok_or_else(|| {
            format!("location `{location}` is not backtick-quoted as `` <file>#<anchor> ``")
        })?;
    let (file, anchor) = location
        .split_once('#')
        .ok_or_else(|| "location has no `#<anchor>` part".to_string())?;
    if file.is_empty() || anchor.is_empty() {
        return Err(format!(
            "location `{location}` needs a non-empty file and anchor"
        ));
    }
    Ok((id, file.to_string(), anchor.to_string()))
}

/// Parse the compatibility index under [`INDEX_PATH`], pushing one failure
/// per malformed row (and per missing structural element) into `report`.
fn parse_index(root: &Path, report: &mut Report) -> Index {
    let rel = Path::new(INDEX_PATH);
    let fail = |line: Option<usize>, message: String, report: &mut Report| {
        report.failures.push(Failure {
            path: rel.to_path_buf(),
            line,
            message,
        });
    };

    let content = match fs::read_to_string(root.join(rel)) {
        Ok(content) => content,
        Err(err) => {
            fail(
                None,
                format!("cannot read compatibility index: {err}"),
                report,
            );
            return Index::default();
        }
    };
    let lines: Vec<&str> = content.lines().collect();
    let footnote_ids = footnote_identifiers(&lines);

    let Some((heading_no, _)) = headings(&content)
        .into_iter()
        .find(|(_, title)| *title == INDEX_HEADING)
    else {
        fail(
            None,
            format!("no `## {INDEX_HEADING}` heading found"),
            report,
        );
        return Index {
            footnote_ids,
            ..Index::default()
        };
    };

    // The table is the maximal run of pipe-prefixed lines after the heading.
    let mut table: Vec<(usize, &str)> = Vec::new();
    for (offset, line) in lines.iter().enumerate().skip(heading_no) {
        if line.trim_start().starts_with('|') {
            table.push((offset + 1, line));
        } else if !table.is_empty() {
            break;
        }
    }
    if table.is_empty() {
        fail(
            Some(heading_no),
            format!("no markdown table under the `## {INDEX_HEADING}` heading"),
            report,
        );
        return Index {
            footnote_ids,
            ..Index::default()
        };
    }

    let (header_no, header) = table[0];
    if table_cells(header) != INDEX_HEADER {
        fail(
            Some(header_no),
            format!(
                "unexpected index-table header; expected `| {} | {} |`",
                INDEX_HEADER[0], INDEX_HEADER[1]
            ),
            report,
        );
    }

    let mut index = Index {
        footnote_ids,
        ..Index::default()
    };
    let separator_seen = parse_table_rows(&table[1..], &mut index, rel, report);
    if !separator_seen {
        fail(
            Some(header_no),
            "index table has no `---` separator row".to_string(),
            report,
        );
    }
    index
}

/// Parse the data rows after the header, flagging malformed rows and
/// duplicate identifiers. Returns true when a separator row was present.
fn parse_table_rows(
    table: &[(usize, &str)],
    index: &mut Index,
    rel: &Path,
    report: &mut Report,
) -> bool {
    let mut first_seen: HashMap<String, usize> = HashMap::new();
    let mut separator_seen = false;
    for &(line_no, row) in table {
        let cells = table_cells(row);
        if is_separator(&cells) {
            separator_seen = true;
            continue;
        }
        match parse_row(&cells) {
            Err(reason) => report.failures.push(Failure {
                path: rel.to_path_buf(),
                line: Some(line_no),
                message: format!("malformed index row: {reason}"),
            }),
            Ok((id, file, anchor)) => {
                if let Some(first) = first_seen.get(&id) {
                    report.failures.push(Failure {
                        path: rel.to_path_buf(),
                        line: Some(line_no),
                        message: format!(
                            "duplicate identifier `\u{a7}{id}` (first row at line {first})"
                        ),
                    });
                } else {
                    first_seen.insert(id.clone(), line_no);
                    index.ids.insert(id.clone());
                    index.entries.push(IndexEntry { id, file, anchor });
                }
            }
        }
    }
    separator_seen
}

/// Parse every index target (`docs/spec/<file>`) and verify the anchor slug.
fn check_index_targets(root: &Path, index: &Index, slugs: &mut SlugCache, report: &mut Report) {
    let index_rel = Path::new(INDEX_PATH);
    let spec_dir = root.join(SPEC_DIR);
    for entry in &index.entries {
        let target = spec_dir.join(&entry.file);
        match slugs.get(&target) {
            None => report.failures.push(Failure {
                path: index_rel.to_path_buf(),
                line: None,
                message: format!(
                    "index row `\u{a7}{}`: target document `{}` cannot be read",
                    entry.id, entry.file
                ),
            }),
            Some(target_slugs) => {
                if !target_slugs.contains(&entry.anchor) {
                    report.failures.push(Failure {
                        path: index_rel.to_path_buf(),
                        line: None,
                        message: format!(
                            "index row `\u{a7}{}`: anchor `#{}` not found in `{}`",
                            entry.id, entry.anchor, entry.file
                        ),
                    });
                }
            }
        }
    }
}

/// Section numbers declared by the technology-stack document's headings.
fn load_tech_stack_sections(root: &Path, report: &mut Report) -> HashSet<String> {
    let rel = Path::new(TECH_STACK_PATH);
    match fs::read_to_string(root.join(rel)) {
        Ok(content) => section_numbers(&content),
        Err(err) => {
            report.failures.push(Failure {
                path: rel.to_path_buf(),
                line: None,
                message: format!("cannot read technology-stack document: {err}"),
            });
            HashSet::new()
        }
    }
}

/// Identifiers from [`TECH_STACK_SCOPED`] documented by a footnote: a line
/// outside the index table that names the identifier.
fn footnote_identifiers(lines: &[&str]) -> HashSet<String> {
    TECH_STACK_SCOPED
        .into_iter()
        .filter(|id| {
            let needle = format!("\u{a7}{id}");
            lines
                .iter()
                .any(|line| !line.trim_start().starts_with('|') && line.contains(&needle))
        })
        .map(str::to_string)
        .collect()
}

/// Section-reference identifiers following `prefix` (with optional `.md`,
/// whitespace, then the section sign) in one line.
fn references_in_line(line: &str, prefix: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (idx, _) in line.match_indices(prefix) {
        let word_char_before = idx > 0
            && line[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if word_char_before {
            continue;
        }
        let mut rest = &line[idx + prefix.len()..];
        if let Some(stripped) = rest.strip_prefix(".md") {
            rest = stripped;
        }
        let after_space = rest.trim_start();
        if after_space.len() == rest.len() {
            continue;
        }
        let Some(after) = after_space.strip_prefix('\u{a7}') else {
            continue;
        };
        let id: String = after
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let id = id.trim_end_matches('.');
        if id.is_empty() || !id.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        found.push(id.to_string());
    }
    found
}

/// Scan phase-2 files and check every legacy reference against the index (or
/// the stack document for stack-qualified references).
fn check_references(
    root: &Path,
    index: &Index,
    tech_stack_sections: &HashSet<String>,
    report: &mut Report,
) {
    let files = walk_scanned_files(root, report);
    for rel in files {
        let content = match fs::read_to_string(root.join(&rel)) {
            Ok(content) => content,
            Err(err) => {
                report.failures.push(Failure {
                    path: rel,
                    line: None,
                    message: format!("cannot read file while scanning references: {err}"),
                });
                continue;
            }
        };
        // Rust sources contribute comment lines only, matching the doc-comment
        // convention without tripping on string literals.
        let comments_only = rel.extension().and_then(|e| e.to_str()) == Some("rs");
        for (offset, line) in content.lines().enumerate() {
            let line_no = offset + 1;
            let scanned = if comments_only {
                let trimmed = line.trim_start();
                if trimmed.starts_with("//") {
                    trimmed
                } else {
                    continue;
                }
            } else {
                line
            };
            for prefix in ["spec", "tech-stack"] {
                for id in references_in_line(scanned, prefix) {
                    report.references_checked += 1;
                    let resolved = if prefix == "spec" {
                        index.ids.contains(&id)
                            || (TECH_STACK_SCOPED.contains(&id.as_str())
                                && index.footnote_ids.contains(&id))
                    } else {
                        tech_stack_sections.contains(&id)
                    };
                    if !resolved {
                        report.failures.push(Failure {
                            path: rel.clone(),
                            line: Some(line_no),
                            message: format!("unresolved legacy reference `{prefix} \u{a7}{id}`"),
                        });
                    }
                }
            }
        }
    }
}

/// Repo-relative paths of every file subject to the reference scan, sorted.
fn walk_scanned_files(root: &Path, report: &mut Report) -> Vec<PathBuf> {
    fn visit(dir: &Path, root: &Path, out: &mut Vec<PathBuf>, report: &mut Report) {
        let read_dir = match fs::read_dir(dir) {
            Ok(read_dir) => read_dir,
            Err(err) => {
                let rel = dir.strip_prefix(root).unwrap_or(dir).to_path_buf();
                report.failures.push(Failure {
                    path: rel,
                    line: None,
                    message: format!("cannot list directory: {err}"),
                });
                return;
            }
        };
        let mut entries: Vec<fs::DirEntry> = read_dir.filter_map(Result::ok).collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(err) => {
                    let rel = entry
                        .path()
                        .strip_prefix(root)
                        .unwrap_or(&entry.path())
                        .to_path_buf();
                    report.failures.push(Failure {
                        path: rel,
                        line: None,
                        message: format!("cannot inspect directory entry: {err}"),
                    });
                    continue;
                }
            };
            if file_type.is_dir() {
                if !SKIP_DIRS.contains(&name.as_ref()) {
                    visit(&entry.path(), root, out, report);
                }
            } else if file_type.is_file() {
                let path = entry.path();
                if SKIP_FILES.contains(&name.as_ref()) {
                    continue;
                }
                let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
                if rel == Path::new(INDEX_PATH) {
                    continue;
                }
                let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                    continue;
                };
                if SCAN_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()) {
                    out.push(rel);
                }
            }
        }
    }

    let mut out = Vec::new();
    visit(root, root, &mut out, report);
    out.sort();
    out
}

/// Markdown link targets found in one line (raw, unvalidated).
fn links_in_line(line: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut haystack = line;
    while let Some(open) = haystack.find("](") {
        let after = &haystack[open + 2..];
        let candidate = after.trim_start();
        let (target, consumed) = if let Some(angle) = candidate.strip_prefix('<') {
            let Some(close) = angle.find('>') else {
                haystack = after;
                continue;
            };
            // +1 for the leading `<`, +1 for the trailing `>`.
            (angle[..close].to_string(), close + 2)
        } else {
            let Some(close) = candidate.find(')') else {
                haystack = after;
                continue;
            };
            (
                candidate[..close]
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string(),
                close,
            )
        };
        haystack = &candidate[consumed..];
        if !target.is_empty() {
            links.push(target);
        }
    }
    links
}

/// Join `rel` onto `base`, resolving `.` and `..` textually.
fn normalize_join(base: &Path, rel: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for component in Path::new(rel).components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// Check every local markdown link inside `docs/spec/*.md`.
fn check_markdown_links(root: &Path, slugs: &mut SlugCache, report: &mut Report) {
    let spec_dir = root.join(SPEC_DIR);
    let read_dir = match fs::read_dir(&spec_dir) {
        Ok(read_dir) => read_dir,
        Err(err) => {
            report.failures.push(Failure {
                path: PathBuf::from(SPEC_DIR),
                line: None,
                message: format!("cannot list spec directory: {err}"),
            });
            return;
        }
    };
    let mut files: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    files.sort();

    for file in files {
        let rel = file.strip_prefix(root).unwrap_or(&file).to_path_buf();
        let content = match fs::read_to_string(&file) {
            Ok(content) => content,
            Err(err) => {
                report.failures.push(Failure {
                    path: rel,
                    line: None,
                    message: format!("cannot read spec document: {err}"),
                });
                continue;
            }
        };
        for (offset, line) in content.lines().enumerate() {
            let line_no = offset + 1;
            for target in links_in_line(line) {
                if target.contains("://") || target.starts_with("mailto:") {
                    continue;
                }
                let (file_part, anchor) = match target.split_once('#') {
                    Some((f, a)) => (f, Some(a)),
                    None => (target.as_str(), None),
                };
                let target_file = if file_part.is_empty() {
                    file.clone()
                } else {
                    if !file_part.to_ascii_lowercase().ends_with(".md") {
                        continue;
                    }
                    normalize_join(&spec_dir, file_part)
                };
                report.links_checked += 1;
                match slugs.get(&target_file) {
                    None => report.failures.push(Failure {
                        path: rel.clone(),
                        line: Some(line_no),
                        message: format!("markdown link target `{target}` not found"),
                    }),
                    Some(target_slugs) => {
                        if let Some(anchor) = anchor
                            && !anchor.is_empty()
                            && !target_slugs.contains(anchor)
                        {
                            report.failures.push(Failure {
                                path: rel.clone(),
                                line: Some(line_no),
                                message: format!(
                                    "markdown link anchor `#{anchor}` not found in `{file_part}`"
                                ),
                            });
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use std::fmt::Write as _;

    use super::*;

    /// Write `files` (`(relative path, content)` pairs) into a fresh tempdir.
    fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().expect("tempdir must be creatable");
        for (rel, content) in files {
            let path = dir.path().join(rel);
            fs::create_dir_all(path.parent().expect("fixture paths have a parent"))
                .expect("fixture parent dir must be creatable");
            fs::write(&path, content).expect("fixture file must be writable");
        }
        dir
    }

    /// A structurally valid compatibility index with one row per
    /// `(identifier, location)` pair plus an anchor-only self link.
    fn index_doc(rows: &[(&str, &str)], footnote: Option<&str>) -> String {
        let mut doc = String::from(
            "# Orchestraitor specification \u{2014} compatibility index\n\
             \n\
             See the [Document map](#document-map).\n\
             \n\
             ## Compatibility index\n\
             \n\
             | Legacy \u{a7} | New location |\n\
             | --- | --- |\n",
        );
        for (id, location) in rows {
            writeln!(doc, "| \u{a7}{id} | `{location}` |")
                .expect("writing to a String cannot fail");
        }
        if let Some(note) = footnote {
            writeln!(doc, "\n{note}").expect("writing to a String cannot fail");
        }
        doc.push_str("\n## Document map\n");
        doc
    }

    /// Technology-stack document with numbered headings 1, 3.2, and 3.4.
    const TECH_STACK_DOC: &str = "# Stack\n\
                                  \n\
                                  ## 1. Baseline\n\
                                  \n\
                                  ### 3.2 Endpoint environment variables\n\
                                  \n\
                                  ### 3.4 Provider inference rule\n";

    /// Overview document whose heading matches `22-security-ownership-invariant`.
    const OVERVIEW_DOC: &str = "# Overview\n\
                                \n\
                                ### 2.2 Security ownership invariant\n\
                                \n\
                                See the [stack](tech-stack.md#32-endpoint-environment-variables).\n";

    /// Orchestrator document whose heading matches `933-delivery`.
    const ORCHESTRATOR_DOC: &str = "# Orchestrator\n\n#### 9.33 Delivery\n";

    #[test]
    fn happy_path_passes_with_every_check_kind_exercised() {
        // Given: a valid index (with the stack-scoped footnote), resolvable
        // legacy references (bare, `.md`-suffixed, stack-qualified, the
        // footnoted exception, and a Rust doc comment), valid intra-spec
        // links, and stale references planted only in skipped locations.
        let dir = fixture(&[
            (
                INDEX_PATH,
                &index_doc(
                    &[
                        ("2.2", "00-overview.md#22-security-ownership-invariant"),
                        ("9.33", "10-orchestrator.md#933-delivery"),
                    ],
                    Some("* `\u{a7}3.4` is stack-scoped; see the technology-stack document."),
                ),
            ),
            ("docs/spec/00-overview.md", OVERVIEW_DOC),
            ("docs/spec/10-orchestrator.md", ORCHESTRATOR_DOC),
            (TECH_STACK_PATH, TECH_STACK_DOC),
            (
                "README.md",
                "See spec \u{a7}2.2, spec.md \u{a7}9.33.\n\
                 The stack covers spec \u{a7}3.4 and tech-stack \u{a7}3.2.\n",
            ),
            (
                "src/main.rs",
                "//! Daemon wiring; see spec \u{a7}2.2.\nfn main() {}\n",
            ),
            (
                "notes.txt",
                "stale ref spec \u{a7}9.99 in an unscanned extension\n",
            ),
            (
                ".git/hooks/junk.md",
                "stale ref spec \u{a7}9.99 inside .git\n",
            ),
            (
                "target/debug/junk.md",
                "stale ref spec \u{a7}9.99 inside target\n",
            ),
        ]);
        // When: docs-check runs against the fixture root.
        let report = run(dir.path());
        // Then: no failures, and each check observed real work.
        assert!(
            report.failures.is_empty(),
            "unexpected failures: {:?}",
            report.failures
        );
        assert_eq!(report.index_rows, 2);
        assert_eq!(report.references_checked, 5);
        assert!(report.links_checked >= 2);
    }

    #[test]
    fn broken_index_anchor_fails_with_the_offending_row() {
        // Given: an index row whose anchor does not resolve in the target.
        let dir = fixture(&[
            (
                INDEX_PATH,
                &index_doc(&[("2.2", "00-overview.md#does-not-exist")], None),
            ),
            ("docs/spec/00-overview.md", OVERVIEW_DOC),
            (TECH_STACK_PATH, TECH_STACK_DOC),
        ]);
        let report = run(dir.path());
        // Then: exactly one failure, naming the identifier and anchor.
        assert_eq!(report.failures.len(), 1, "failures: {:?}", report.failures);
        let failure = &report.failures[0];
        assert_eq!(failure.path, Path::new(INDEX_PATH));
        assert!(failure.message.contains("\u{a7}2.2"));
        assert!(failure.message.contains("#does-not-exist"));
    }

    #[test]
    fn duplicate_identifier_fails() {
        // Given: two index rows carrying the same identifier.
        let dir = fixture(&[
            (
                INDEX_PATH,
                &index_doc(
                    &[
                        ("2.2", "00-overview.md#22-security-ownership-invariant"),
                        ("2.2", "00-overview.md#22-security-ownership-invariant"),
                    ],
                    None,
                ),
            ),
            ("docs/spec/00-overview.md", OVERVIEW_DOC),
            (TECH_STACK_PATH, TECH_STACK_DOC),
        ]);
        let report = run(dir.path());
        // Then: one duplicate failure pointing at the second row's line.
        let duplicates: Vec<&Failure> = report
            .failures
            .iter()
            .filter(|f| f.message.contains("duplicate identifier"))
            .collect();
        assert_eq!(duplicates.len(), 1, "failures: {:?}", report.failures);
        assert_eq!(duplicates[0].line, Some(10));
    }

    #[test]
    fn stale_reference_fails_naming_file_and_line() {
        // Given: a scanned file referencing an identifier no index row covers.
        let dir = fixture(&[
            (
                INDEX_PATH,
                &index_doc(
                    &[("2.2", "00-overview.md#22-security-ownership-invariant")],
                    None,
                ),
            ),
            ("docs/spec/00-overview.md", OVERVIEW_DOC),
            (TECH_STACK_PATH, TECH_STACK_DOC),
            ("README.md", "intro\nsee spec.md \u{a7}9.99 for details\n"),
        ]);
        let report = run(dir.path());
        // Then: one failure at README.md line 2 naming the identifier.
        assert_eq!(report.failures.len(), 1, "failures: {:?}", report.failures);
        let failure = &report.failures[0];
        assert_eq!(failure.path, Path::new("README.md"));
        assert_eq!(failure.line, Some(2));
        assert!(failure.message.contains("9.99"));
    }

    #[test]
    fn tech_stack_qualified_reference_needs_no_index_row() {
        // Given: a stack-qualified reference whose number only exists as a
        // technology-stack heading, plus one stack number that does not exist.
        let dir = fixture(&[
            (
                INDEX_PATH,
                &index_doc(
                    &[("2.2", "00-overview.md#22-security-ownership-invariant")],
                    None,
                ),
            ),
            ("docs/spec/00-overview.md", OVERVIEW_DOC),
            (TECH_STACK_PATH, TECH_STACK_DOC),
            (
                "README.md",
                "see tech-stack \u{a7}3.2 and tech-stack.md \u{a7}1\n",
            ),
        ]);
        let report = run(dir.path());
        // Then: both references resolve without any index row.
        assert!(
            report.failures.is_empty(),
            "failures: {:?}",
            report.failures
        );
    }

    #[test]
    fn markdown_link_to_missing_file_fails() {
        // Given: an intra-spec link to a document that does not exist.
        let dir = fixture(&[
            (
                INDEX_PATH,
                &index_doc(
                    &[("2.2", "00-overview.md#22-security-ownership-invariant")],
                    None,
                ),
            ),
            (
                "docs/spec/00-overview.md",
                "# Overview\n\n### 2.2 Security ownership invariant\n\nSee [gone](99-missing.md).\n",
            ),
            (TECH_STACK_PATH, TECH_STACK_DOC),
        ]);
        let report = run(dir.path());
        // Then: one failure naming the missing target and its line.
        assert_eq!(report.failures.len(), 1, "failures: {:?}", report.failures);
        let failure = &report.failures[0];
        assert_eq!(failure.path, Path::new("docs/spec/00-overview.md"));
        assert_eq!(failure.line, Some(5));
        assert!(failure.message.contains("99-missing.md"));
    }

    #[test]
    fn markdown_link_with_unknown_anchor_fails() {
        // Given: an intra-spec link whose file exists but anchor does not.
        let dir = fixture(&[
            (
                INDEX_PATH,
                &index_doc(
                    &[("2.2", "00-overview.md#22-security-ownership-invariant")],
                    None,
                ),
            ),
            (
                "docs/spec/00-overview.md",
                "# Overview\n\n### 2.2 Security ownership invariant\n\nSee [stack](tech-stack.md#99-nope).\n",
            ),
            (TECH_STACK_PATH, TECH_STACK_DOC),
        ]);
        let report = run(dir.path());
        // Then: one failure naming the dangling anchor.
        assert_eq!(report.failures.len(), 1, "failures: {:?}", report.failures);
        assert!(report.failures[0].message.contains("#99-nope"));
    }

    #[test]
    fn missing_index_table_fails_loudly() {
        // Given: an index file without the compatibility-index section.
        let dir = fixture(&[
            (INDEX_PATH, "# Index\n\nNothing here.\n"),
            (TECH_STACK_PATH, TECH_STACK_DOC),
        ]);
        let report = run(dir.path());
        // Then: failure, never a silent pass.
        assert!(!report.failures.is_empty());
        assert!(
            report
                .failures
                .iter()
                .any(|f| f.message.contains("Compatibility index"))
        );
    }

    #[test]
    fn malformed_index_row_fails_loudly() {
        // Given: a table row whose location cell is not a quoted target.
        let dir = fixture(&[
            (
                INDEX_PATH,
                "# Index\n\n## Compatibility index\n\n| Legacy \u{a7} | New location |\n| --- | --- |\n| \u{a7}2.2 | broken |\n",
            ),
            (TECH_STACK_PATH, TECH_STACK_DOC),
        ]);
        let report = run(dir.path());
        // Then: a malformed-row failure on that row's line.
        assert!(!report.failures.is_empty());
        let malformed: Vec<&Failure> = report
            .failures
            .iter()
            .filter(|f| f.message.contains("malformed index row"))
            .collect();
        assert_eq!(malformed.len(), 1, "failures: {:?}", report.failures);
        assert_eq!(malformed[0].line, Some(7));
    }

    #[test]
    fn tech_stack_scoped_exception_without_footnote_fails() {
        // Given: a bare spec-side reference to the stack-scoped identifier
        // but no explanatory footnote anywhere outside the table.
        let dir = fixture(&[
            (
                INDEX_PATH,
                &index_doc(
                    &[("2.2", "00-overview.md#22-security-ownership-invariant")],
                    None,
                ),
            ),
            ("docs/spec/00-overview.md", OVERVIEW_DOC),
            (TECH_STACK_PATH, TECH_STACK_DOC),
            ("README.md", "the rule lives at spec \u{a7}3.4\n"),
        ]);
        let report = run(dir.path());
        // Then: failure, because only the footnote makes the exception valid.
        assert_eq!(report.failures.len(), 1, "failures: {:?}", report.failures);
        assert!(report.failures[0].message.contains("3.4"));
    }

    #[test]
    fn slug_cases_from_the_spec_split() {
        // Given: real and synthetic heading titles.
        // Then: GitHub's slug rules hold (dots stripped, spaces hyphenated,
        // symbols dropped, lowercase).
        assert_eq!(
            github_slug("2.2 Security ownership invariant"),
            "22-security-ownership-invariant"
        );
        assert_eq!(
            github_slug("9.22.11 Design principle"),
            "92211-design-principle"
        );
        assert_eq!(
            github_slug("9.27.3 Orchestration vs. security enforcement \u{2014} boundary"),
            "9273-orchestration-vs-security-enforcement--boundary"
        );
        assert_eq!(github_slug("Foo: bar (baz)!"), "foo-bar-baz");
    }

    #[test]
    fn duplicate_headings_get_numeric_suffixes() {
        // Given: a document repeating one heading three times.
        let slugs = document_slugs("## A B\n## A B\n## A B\n");
        // Then: GitHub's suffix scheme applies in document order.
        assert!(slugs.contains("a-b"));
        assert!(slugs.contains("a-b-1"));
        assert!(slugs.contains("a-b-2"));
        assert_eq!(slugs.len(), 3);
    }

    #[test]
    fn fenced_code_blocks_do_not_produce_heading_slugs() {
        // Given: `#`-lines inside a fenced block plus one real heading.
        let slugs = document_slugs("```md\n# not a heading\n```\n# Real\n");
        // Then: only the real heading is anchored.
        assert!(slugs.contains("real"));
        assert!(!slugs.contains("not-a-heading"));
    }

    #[test]
    fn reference_parser_honours_word_boundaries_and_trailing_dots() {
        // Given: one line mixing matchable and unmatchable shapes.
        let line = "spec \u{a7}9.99. and spec.md \u{a7}2.2, but aspect \u{a7}5 and spec \u{a7}N are not refs";
        // Then: only real numeric identifiers parse, dots trimmed.
        assert_eq!(references_in_line(line, "spec"), vec!["9.99", "2.2"]);
        assert!(references_in_line("prefix tech-stack \u{a7}3.1.", "tech-stack") == vec!["3.1"]);
    }
}
