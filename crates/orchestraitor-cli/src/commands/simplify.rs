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

/// Runs an `orc simplify` subcommand.
///
/// # Errors
/// Returns a diagnostic only when configuration resolution fails; the pass
/// itself is fail-open (tool unavailability is reported, never fatal).
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
    let config = resolved_config(paths)?;

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
    let project_dir = project_root(paths);
    let staged_paths = if args.staged {
        Some(staged_files(&project_dir))
    } else {
        None
    };

    let fix = match args.fix {
        SimplifyFixMode::None => FixPolicy::None,
        SimplifyFixMode::Format => FixPolicy::Format,
        SimplifyFixMode::Safe => FixPolicy::Safe,
    };
    let (pass_config, auto_apply_format, auto_apply_safe_fixes) = pass_settings(&config);
    let executor = ProcessExecutor;
    let pass = SimplifyPass::new(
        &pass_config,
        &executor,
        fix,
        auto_apply_format,
        auto_apply_safe_fixes,
    )
    .map_err(|error| miette::miette!("{SIMPLIFY_UNAVAILABLE_CODE}: {error}"))?;
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
                "warning [{SIMPLIFY_UNAVAILABLE_CODE}]: {pedantic} unaddressed suggestion(s) \
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
/// worktree-relative; a suggestion with no path survives (workspace-level
/// findings are always reported). `staged` is `Some(None)` when git failed
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
    let normalize = |path: &str| -> String {
        let path = path.trim_start_matches("./");
        let absolute = Path::new(path).is_absolute();
        if absolute {
            return Path::new(path).strip_prefix(project_dir).map_or_else(
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

/// Staged (index) files, worktree-relative, via `git diff --cached --name-only`.
/// `None` = git could not run or failed (fail-open: the caller drops the
/// staged filter and reports everything, rather than silently narrowing to
/// nothing); `Some(list)` = the index was read (possibly empty — a genuinely
/// empty index is a real empty scope, not an error).
fn staged_files(project_dir: &Path) -> Option<Vec<String>> {
    let output = std::process::Command::new("git")
        .current_dir(project_dir)
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
