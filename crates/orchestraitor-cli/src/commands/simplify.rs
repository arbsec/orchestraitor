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

/// Runs an `orc simplify` subcommand.
///
/// # Errors
/// Returns a diagnostic only when writing the report fails; the pass itself
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
    // `staged = Some(None)` = git failed (fail-open: no staged filter);
    // `staged = Some(Some(list))` = the index was readable (an empty list
    // means genuinely nothing is staged, so file-scoped findings are out of
    // scope — pathless workspace-level findings always remain).
    //
    // STAGED SAFETY RULE: with `--staged`, format fixes never auto-apply.
    // rustfmt/rumdl rewrite whole files; applying them over a staged scope
    // would also rewrite UNSTAGED (and partially staged) hunks the user
    // never asked to touch. Under `--staged` the pass is check-only above
    // reporting; the hook compensates by running the same command without
    // `--staged` when the developer wants fixes.
    let project_dir = project_root(paths);
    let staged_paths = if args.staged {
        Some(staged_files(&project_dir).map(|files| strip_toplevel(&project_dir, &files)))
    } else {
        None
    };

    let fix = match args.fix {
        // Staged runs never auto-apply: see the STAGED SAFETY RULE above.
        _ if args.staged => FixPolicy::None,
        SimplifyFixMode::None => FixPolicy::None,
        SimplifyFixMode::Format => FixPolicy::Format,
        SimplifyFixMode::Safe => FixPolicy::Safe,
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
    // fast-feedback signal. Default behavior never blocks.
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
            std::process::exit(1);
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
/// repo-root-relative (matching normalized suggestion paths); a suggestion
/// with no path survives (workspace-level findings are always reported).
/// `staged` is `Some(None)` when git failed
/// (no staged filter — fail-open) and `Some(Some(list))` when the index was
/// read (an empty list is a real empty scope).
fn filter_report(
    report: &mut orchestraitor_simplify::SimplifyReport,
    staged: Option<&Option<Vec<String>>>,
    paths: &[String],
    project_dir: &Path,
) {
    let staged = staged.and_then(|inner| inner.as_ref());
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
        if let Some(staged) = staged
            && staged.contains(&normalized)
        {
            return true;
        }
        paths
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

/// Staged (index) files, relative to the git toplevel, via
/// `git diff --cached --name-only -z` run at the toplevel (`--relative` is
/// deliberately NOT used: it resolves against the current directory, which
/// would re-base the output). Callers re-base these entries onto the cargo
/// workspace root via [`strip_toplevel`], because tool-emitted suggestion
/// paths (clippy JSON, rustfmt diffs) are relative to that root, which can
/// be a subdirectory of the git worktree. `None` = git could not run or
/// failed (fail-open: the caller drops the staged filter and reports
/// everything, rather than silently narrowing to nothing); `Some(list)` =
/// the index was read (possibly empty — a genuinely empty index is a real
/// empty scope, not an error).
fn staged_files(project_dir: &Path) -> Option<Vec<String>> {
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
    Some(
        String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Re-bases staged paths (git-toplevel-relative) onto the cargo workspace
/// root (`project_dir`), matching the base of tool-emitted suggestion paths
/// (clippy JSON `file_name`, rustfmt diff paths). Entries outside
/// `project_dir` (e.g. sibling crates in the same worktree) are kept as-is —
/// they can never match a workspace-relative suggestion, and dropping them
/// would risk over-matching; absolute path forms are handled by the
/// canonical comparison in [`filter_report`]'s normalize step.
fn strip_toplevel(project_dir: &Path, files: &[String]) -> Vec<String> {
    files
        .iter()
        .map(|file| {
            // A staged entry is toplevel-relative (e.g. `crates/foo/src/lib.rs`);
            // when it lives under the project dir, re-express it relative to
            // the project dir (the base of tool-emitted suggestion paths).
            // Entries outside `project_dir` (sibling crates in the same
            // worktree) are kept as-is — they can never match a
            // workspace-relative suggestion, and dropping them would risk
            // over-matching.
            Path::new(file)
                .strip_prefix(project_dir)
                .map_or_else(|_| file.clone(), |rest| rest.to_string_lossy().into_owned())
        })
        .collect()
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
