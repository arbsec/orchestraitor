//! `orc simplify` implementation: the rule-driven pre-landing pass
//! (fail-open, loudly — never a blocker).

use std::io::Write;
use std::path::{Path, PathBuf};

use miette::{IntoDiagnostic, Result};
use orchestraitor_core::config::OrchestraitorConfig;
use orchestraitor_simplify::{
    FixPolicy, ProcessExecutor, SimplifyConfig as PassConfig, SimplifyPass, SuggestionClass,
    ToolStatus,
};
use serde::Serialize;

use crate::cli::{ConfigPaths, SimplifyCommand, SimplifyFixMode, SimplifyRunArgs};
use crate::commands::config::layers::load_layers;

/// Error code for simplify fail-open warnings (spec §9.34 registry,
/// `orchestraitor-model` `ErrorComponent::Simplify`).
pub(crate) const SIMPLIFY_UNAVAILABLE_CODE: &str = "ORC-SIMPLIFY-001";

/// Distinct code for the `--pedantic-check` refusal (spec §9.34: different
/// conditions carry different codes; this one signals unaddressed
/// suggestions above Format, not tool unavailability).
pub(crate) const SIMPLIFY_PEDANTIC_CODE: &str = "ORC-SIMPLIFY-002";

/// `--pedantic-check` found unaddressed suggestions above Format.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("{count} unaddressed suggestion(s) above Format class (pedantic-check)")]
#[diagnostic(code("ORC-SIMPLIFY-002"))]
pub struct PedanticCheckFailed {
    /// Number of unaddressed safe-fix and semantic suggestions.
    pub count: usize,
}

/// Runs an `orc simplify` subcommand.
///
/// # Errors
/// Returns a diagnostic when writing fails or [`PedanticCheckFailed`] when
/// the requested pedantic check finds unaddressed suggestions. The pass itself
/// is fail-open (tool unavailability AND configuration-resolution failure
/// are typed `ORC-SIMPLIFY-001` warnings followed by a skip, never fatal).
pub fn run<W: Write>(paths: &ConfigPaths, command: SimplifyCommand, writer: &mut W) -> Result<()> {
    match command {
        SimplifyCommand::Run(args) => run_pass(paths, &args, writer),
    }
}

#[derive(Debug, Serialize)]
struct ReportOutput<'a> {
    ran: bool,
    ran_rules_only: bool,
    #[serde(rename = "unaddressed")]
    unaddressed: usize,
    auto_applied_count: usize,
    suggestions: &'a [orchestraitor_simplify::Suggestion],
    tools: &'a [ToolStatus],
}

fn run_pass<W: Write>(paths: &ConfigPaths, args: &SimplifyRunArgs, writer: &mut W) -> Result<()> {
    // Fail-open, loudly: an unresolvable config (parse/validation failure)
    // is a typed ORC-SIMPLIFY-001 warning and a skip (exit 0) — matching the
    // crate's never-blocks contract. --pedantic-check keeps its fast-feedback
    // refusal: the skip is still the only non-zero exit path it owns.
    let config = match resolved_config(paths) {
        Ok(config) => config,
        Err(error) => {
            writeln!(
                std::io::stderr(),
                "warning [{SIMPLIFY_UNAVAILABLE_CODE}]: simplify configuration failed ({error:#}); \
                 skipping the pass (fail-open)"
            )
            .into_diagnostic()?;
            return Ok(());
        }
    };

    // Master switch: a disabled pass is a no-op (exit 0), per the built-in
    // default's contract with the hooks.
    let enabled = config
        .simplify
        .as_ref()
        .and_then(|simplify| simplify.enabled)
        .unwrap_or(true);
    if !enabled {
        writeln!(writer, "simplify disabled by config: no-op").into_diagnostic()?;
        return Ok(());
    }

    // Scope: the pass always runs over the project directory; `--staged` and
    // `--paths` filter the REPORTED suggestions (presentation scoping).
    //
    // STAGED SAFETY RULE: with `--staged`, format fixes never auto-apply.
    // rustfmt/rumdl rewrite whole files; applying them over a staged scope
    // would also rewrite UNSTAGED (and partially staged) hunks the user
    // never asked to touch. Under `--staged` the pass is check-only above
    // reporting; the hook compensates by running the same command without
    // `--staged` when the developer wants fixes.
    let project_dir = project_root(paths);
    let staged_paths = staged_scope(&project_dir, args.staged);
    let fix = if args.staged {
        // Staged runs never auto-apply: see the STAGED SAFETY RULE above.
        FixPolicy::None
    } else {
        match args.fix {
            SimplifyFixMode::None => FixPolicy::None,
            SimplifyFixMode::Format => FixPolicy::Format,
            SimplifyFixMode::Safe => FixPolicy::Safe,
        }
    };
    let (pass_config, auto_apply_format, auto_apply_safe_fixes) = pass_settings(&config);
    let executor = ProcessExecutor;
    // Fail-open, loudly: an invalid pass config (e.g. a zero bound) is the
    // same typed warning + skip as an unresolvable layered config — the
    // pass never blocks on its own configuration.
    let pass = match SimplifyPass::new(
        &pass_config,
        &executor,
        fix,
        auto_apply_format,
        auto_apply_safe_fixes,
    ) {
        Ok(pass) => pass,
        Err(error) => {
            writeln!(
                std::io::stderr(),
                "warning [{SIMPLIFY_UNAVAILABLE_CODE}]: simplify configuration failed ({error}); \
                 skipping the pass (fail-open)"
            )
            .into_diagnostic()?;
            return Ok(());
        }
    };
    let mut report = pass.run(&project_dir);

    // Scope filtering + path normalization for the report.
    filter_report(
        &mut report,
        staged_paths.as_ref(),
        &args.paths,
        &project_dir,
    );

    // Fail-open, loudly: unavailable tools surface as typed warnings.
    for status in &report.tools {
        if let ToolStatus::Unavailable { tool, reason } = status {
            writeln!(
                std::io::stderr(),
                "warning [{SIMPLIFY_UNAVAILABLE_CODE}]: simplify tool `{tool}` unavailable ({reason}); \
                 continuing without it (fail-open)"
            )
            .into_diagnostic()?;
        }
    }

    if args.json {
        let output = ReportOutput {
            ran: report.ran,
            ran_rules_only: report.ran_rules_only,
            unaddressed: report.unaddressed(None),
            auto_applied_count: report.auto_applied_count,
            suggestions: &report.suggestions,
            tools: &report.tools,
        };
        serde_json::to_writer_pretty(&mut *writer, &output).into_diagnostic()?;
        writeln!(writer).into_diagnostic()?;
    } else {
        write_human_report(&report, writer)?;
    }

    // Pedantic-check mode is the ONLY non-zero exit: the pre-push hook's
    // fast-feedback signal. Default behavior never blocks. The report is
    // returned as a typed error (mapped to exit code 1 by the binary entry
    // point) AFTER the writer is flushed — `process::exit` would skip
    // destructors and could drop a buffered report, and a typed error lets
    // library callers observe the refusal.
    if args.pedantic_check {
        let pedantic = report.unaddressed(Some(SuggestionClass::SafeFix))
            + report.unaddressed(Some(SuggestionClass::Semantic));
        if pedantic > 0 {
            writeln!(
                std::io::stderr(),
                "warning [{SIMPLIFY_PEDANTIC_CODE}]: {pedantic} unaddressed suggestion(s) \
                 above Format class (pedantic-check)"
            )
            .into_diagnostic()?;
            writer.flush().into_diagnostic()?;
            return Err(PedanticCheckFailed { count: pedantic }.into());
        }
    }
    Ok(())
}

fn write_human_report<W: Write>(
    report: &orchestraitor_simplify::SimplifyReport,
    writer: &mut W,
) -> Result<()> {
    writeln!(writer, "simplify: ran = {}", report.ran).into_diagnostic()?;
    for status in &report.tools {
        match status {
            ToolStatus::Ran { tool, exit_code } => {
                writeln!(
                    writer,
                    "  {tool}: ran (exit {})",
                    exit_code.map_or_else(|| "signal".to_string(), |code| code.to_string())
                )
                .into_diagnostic()?;
            }
            ToolStatus::Unavailable { tool, reason } => {
                writeln!(writer, "  {tool}: unavailable ({reason})").into_diagnostic()?;
            }
        }
    }
    writeln!(
        writer,
        "suggestions: {} (auto-applied {})",
        report.suggestions.len(),
        report.auto_applied_count
    )
    .into_diagnostic()?;
    for suggestion in &report.suggestions {
        let location = suggestion.path.as_deref().unwrap_or("-");
        let line = suggestion
            .line
            .map_or_else(String::new, |line| format!(":{line}"));
        let applied = if suggestion.applied { " [applied]" } else { "" };
        writeln!(
            writer,
            "  [{}] {location}{line} {}: {}{applied}",
            suggestion.class.label(),
            suggestion.rule,
            suggestion.message
        )
        .into_diagnostic()?;
    }
    Ok(())
}

/// Builds the pass config and auto-apply flags from the layered config
/// (built-in defaults when the `[simplify]` table is absent).
fn pass_settings(config: &OrchestraitorConfig) -> (PassConfig, bool, bool) {
    let table = config.simplify.as_ref();
    let pass_config = PassConfig {
        pedantic: table.and_then(|s| s.pedantic).unwrap_or(false),
        max_passes: table.and_then(|s| s.max_passes).unwrap_or(2),
        max_files: table.and_then(|s| s.max_files).unwrap_or(200),
        model_pass: table.and_then(|s| s.model_pass).unwrap_or(false),
    };
    let auto_apply_format = table.and_then(|s| s.auto_apply_format).unwrap_or(true);
    let auto_apply_safe_fixes = table.and_then(|s| s.auto_apply_safe_fixes).unwrap_or(false);
    (pass_config, auto_apply_format, auto_apply_safe_fixes)
}

/// Filters reported suggestions by staged/`--paths` scope. Staged paths are
/// re-based onto the cargo workspace root (matching normalized suggestion
/// paths); a suggestion with no path survives (workspace-level findings are
/// always reported). `staged` is `None` when the filter is dropped (no
/// `--staged`, git failure, or empty index — fail-open: the report is
/// unscoped rather than silently narrowed to nothing) and `Some(list)` when
/// the index produced a real scope.
fn filter_report(
    report: &mut orchestraitor_simplify::SimplifyReport,
    staged: Option<&Vec<String>>,
    paths: &[String],
    project_dir: &Path,
) {
    let staged = staged.filter(|list| !list.is_empty());
    if staged.is_none() && paths.is_empty() {
        return;
    }
    // The pass runs under `project_dir`, but that may be a non-canonical
    // form (relative like ".", symlinked, or containing `..`) while tool
    // output (e.g. `cargo fmt --check`'s `Diff in <abs path>`) is absolute
    // and real. Compare both sides canonically: canonicalize the project
    // dir once, falling back to it as-is when canonicalization fails.
    let project_dir_canonical =
        std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let normalize = |path: &str| -> String {
        let path = path.trim_start_matches("./");
        let absolute = Path::new(path).is_absolute();
        if absolute {
            return Path::new(path)
                .strip_prefix(&project_dir_canonical)
                .map_or_else(
                    |_| path.to_string(),
                    |rest| rest.to_string_lossy().into_owned(),
                );
        }
        path.to_string()
    };
    let in_scope = |path: &Option<String>| -> bool {
        let Some(path) = path else {
            return true;
        };
        let normalized = normalize(path);
        // Every ACTIVE filter must pass (intersection): `--staged --path X`
        // reports only staged findings under X — each flag narrows the
        // scope, per the `--path` doc ("restrict the report").
        if let Some(staged) = staged
            && !staged.contains(&normalized)
        {
            return false;
        }
        paths.is_empty()
            || paths
                .iter()
                .any(|candidate| normalize(candidate) == normalized)
    };
    report
        .suggestions
        .retain(|suggestion| in_scope(&suggestion.path));
    report.auto_applied_count = report
        .suggestions
        .iter()
        .filter(|suggestion| suggestion.applied)
        .count();
}

/// Staged (index) files, relative to the git toplevel, plus the toplevel
/// itself, via `git diff --cached --name-only -z` run at the toplevel
/// (`--relative` is deliberately NOT used: it resolves against the current
/// directory, which would re-base the output). Callers re-base these
/// entries onto the cargo workspace root via [`strip_toplevel`], because
/// tool-emitted suggestion paths (clippy JSON, rustfmt diffs) are relative
/// to that root, which can be a subdirectory of the git worktree.
/// `None` = git could not run or failed (fail-open: the caller drops the
/// staged filter and reports everything, rather than silently narrowing to
/// nothing); `Some((toplevel, list))` = the index was read (an empty list
/// also drops the staged filter downstream — a committed index must not
/// blind the check).
fn staged_files(project_dir: &Path) -> Option<(String, Vec<String>)> {
    let output = std::process::Command::new("git")
        .current_dir(project_dir)
        .args(["rev-parse", "--show-toplevel"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if toplevel.is_empty() {
        return None;
    }
    let output = std::process::Command::new("git")
        .current_dir(&toplevel)
        .args(["diff", "--cached", "--name-only", "-z"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let files = String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect();
    Some((toplevel, files))
}

/// Re-bases staged paths (git-toplevel-relative) onto the cargo workspace
/// root (`project_dir`), matching the base of tool-emitted suggestion paths
/// (clippy JSON `file_name`, rustfmt diff paths — the executor runs tools
/// with `current_dir(project_dir)`). The toplevel-relative entry is joined
/// onto the canonical toplevel and re-expressed relative to the canonical
/// project dir, which may be the toplevel itself, a subdirectory of it, a
/// relative ("." from the toplevel) or an absolute path. Entries outside
/// `project_dir` (e.g. sibling crates in the same worktree) are kept as-is —
/// they can never match a workspace-relative suggestion, and dropping them
/// would risk over-matching; absolute path forms are handled by the
/// canonical comparison in [`filter_report`]'s normalize step.
fn strip_toplevel(toplevel: &Path, project_dir: &Path, files: &[String]) -> Vec<String> {
    // Canonical forms: `canonicalize` resolves relative values against the
    // process cwd — exactly how `project_dir` is interpreted everywhere
    // else in this command. Falls back to the raw value when the path does
    // not exist (a plain string prefix strip of relative forms still
    // applies).
    let canonical_toplevel =
        std::fs::canonicalize(toplevel).unwrap_or_else(|_| toplevel.to_path_buf());
    let canonical_project =
        std::fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    files
        .iter()
        .map(|file| {
            canonical_toplevel
                .join(file)
                .strip_prefix(&canonical_project)
                .map_or_else(|_| file.clone(), |rest| rest.to_string_lossy().into_owned())
        })
        .collect()
}

/// Resolves the staged scope: `None` when `--staged` was not passed, when
/// git failed, or when the index was empty — in every one of those cases
/// the staged filter is dropped (fail-open) and the report is unscoped.
/// `Some(list)` scopes the report to those (re-based) paths.
fn staged_scope(project_dir: &Path, staged: bool) -> Option<Vec<String>> {
    if !staged {
        return None;
    }
    staged_files(project_dir)
        .map(|(toplevel, files)| strip_toplevel(Path::new(&toplevel), project_dir, &files))
        .filter(|files| !files.is_empty())
}

/// The directory the pass runs over: the project dir (never a colonized cwd —
/// fixture-chdir bug #529 lesson; hooks invoke with the repo root resolved).
fn project_root(paths: &ConfigPaths) -> PathBuf {
    PathBuf::from(&paths.project_dir)
}

fn resolved_config(paths: &ConfigPaths) -> Result<OrchestraitorConfig> {
    let layers = load_layers(paths)?;
    layers.resolver.resolve_config().map_err(|error| {
        miette::miette!(
            "configuration validation failed: {}",
            error.structured().cause
        )
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use orchestraitor_simplify::{SimplifyReport, Suggestion};

    /// RAII guard restoring the process cwd on drop — the chdir window in
    /// [`strip_toplevel_relative_project_dir_matches_cwd_interpretation`]
    /// must not leak into sibling tests even when an assertion panics.
    struct RestoreCwd(std::path::PathBuf);
    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            let _restored = std::env::set_current_dir(&self.0);
        }
    }

    #[test]
    fn strip_toplevel_rebases_subdir_project_dir() {
        // A real temp git repo: toplevel at temp/, cargo workspace root in
        // the nested `crate` dir. Staged entries are toplevel-relative;
        // suggestion paths are workspace-relative.
        let temp = tempfile::TempDir::new().expect("temp dir");
        let toplevel = temp.path().to_path_buf();
        let project_dir = toplevel.join("crate");
        std::fs::create_dir_all(&project_dir).expect("mkdir");
        let files = vec![
            "crate/src/lib.rs".to_string(), // inside the workspace root
            "sibling/other.rs".to_string(), // outside it
        ];
        let rebased = strip_toplevel(&toplevel, &project_dir, &files);
        assert_eq!(rebased[0], "src/lib.rs", "entry under project dir rebases");
        assert_eq!(
            rebased[1], "sibling/other.rs",
            "entry outside the project dir is kept as-is"
        );
    }

    #[test]
    fn strip_toplevel_identity_when_project_is_toplevel() {
        // Workspace root == toplevel (the common single-crate layout, and
        // the hook default `--project-dir .` run from the root): entries
        // pass through unchanged.
        let temp = tempfile::TempDir::new().expect("temp dir");
        let toplevel = temp.path().to_path_buf();
        let files = vec!["src/lib.rs".to_string()];
        let rebased = strip_toplevel(&toplevel, &toplevel, &files);
        assert_eq!(rebased, files);
    }

    #[test]
    fn strip_toplevel_relative_project_dir_matches_cwd_interpretation() {
        // A relative `--project-dir` is interpreted against the process
        // cwd everywhere in this command; canonicalize must agree. The
        // process-global cwd is shared with every other unit test in this
        // harness (github.rs has its own chdir tests), so this test
        // serializes on the same pattern: a static mutex around the chdir
        // window, with a guard that RESTORES the cwd even when an
        // assertion panics.
        static CWD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _lock = CWD_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _restore = RestoreCwd(std::env::current_dir().expect("cwd"));
        let temp = tempfile::TempDir::new().expect("temp dir");
        let toplevel = temp.path().to_path_buf();
        let project_dir = toplevel.join("crate");
        std::fs::create_dir_all(&project_dir).expect("mkdir");
        std::env::set_current_dir(&toplevel).expect("chdir to toplevel");
        let rebased = strip_toplevel(
            &toplevel,
            Path::new("crate"),
            &["crate/src/lib.rs".to_string()],
        );
        assert_eq!(rebased, vec!["src/lib.rs"]);
    }

    #[test]
    fn staged_scope_falls_back_only_when_empty_or_unavailable() {
        let mut report = SimplifyReport::new(true);
        for path in [Some("src/a.rs"), Some("src/b.rs"), None] {
            report.push(Suggestion {
                class: SuggestionClass::SafeFix,
                path: path.map(str::to_string),
                line: None,
                rule: "test".into(),
                message: "test".into(),
                applied: path.is_some(),
            });
        }
        // `None` (no --staged / git failure) and `Some(empty)` (empty index)
        // both drop the staged filter: the report stays unscoped.
        for staged in [None, Some(Vec::new())] {
            let mut filtered = report.clone();
            filter_report(&mut filtered, staged.as_ref(), &[], Path::new("/project"));
            assert_eq!(filtered.suggestions.len(), 3);
            assert_eq!(filtered.auto_applied_count, 2);
        }
        // A real staged scope narrows the report to staged files.
        let staged = Some(vec!["src/a.rs".to_string()]);
        let mut filtered = report.clone();
        filter_report(&mut filtered, staged.as_ref(), &[], Path::new("/project"));
        assert_eq!(filtered.suggestions.len(), 2);
        assert_eq!(filtered.suggestions[0].path.as_deref(), Some("src/a.rs"));
        assert!(filtered.suggestions[1].path.is_none());
        assert_eq!(filtered.auto_applied_count, 1);

        // An empty staged list with explicit `--paths` filters by the paths.
        let empty = Some(Vec::new());
        let mut filtered = report.clone();
        filter_report(
            &mut filtered,
            empty.as_ref(),
            &["src/a.rs".to_string()],
            Path::new("/project"),
        );
        assert_eq!(filtered.suggestions.len(), 2);
        assert_eq!(filtered.suggestions[0].path.as_deref(), Some("src/a.rs"));
        assert!(filtered.suggestions[1].path.is_none());
        assert_eq!(filtered.auto_applied_count, 1);

        // Both filters active: INTERSECTION — `--staged --path src/a.rs`
        // narrows to staged findings under src/a.rs only; a staged
        // `src/b.rs` is excluded because --path narrows further.
        let staged = Some(vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);
        let mut filtered = report.clone();
        filter_report(
            &mut filtered,
            staged.as_ref(),
            &["src/a.rs".to_string()],
            Path::new("/project"),
        );
        assert_eq!(filtered.suggestions.len(), 2);
        assert_eq!(filtered.suggestions[0].path.as_deref(), Some("src/a.rs"));
        assert!(filtered.suggestions[1].path.is_none());
        assert_eq!(filtered.auto_applied_count, 1);
    }
}
