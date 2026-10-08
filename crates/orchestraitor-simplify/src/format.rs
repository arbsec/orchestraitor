//! Format-class runner: cargo fmt and rumdl (spec §9.5 Format class —
//! semantics-preserving, auto-applied when configured).

use std::path::Path;

use crate::TOOL_TIMEOUT;
use crate::executor::{SimplifyExecutor, ToolOutcome, ToolSpec};
use crate::report::{Suggestion, SuggestionClass, ToolStatus};

/// `cargo fmt --check`: reports unformatted files on stderr with exit 1.
const FMT_SPEC: ToolSpec = ToolSpec {
    program: "cargo",
    args: &["fmt", "--check"],
    label: "cargo-fmt",
};

/// `cargo fmt` (no `--check`): performs the rewrite.
const FMT_APPLY_SPEC: ToolSpec = ToolSpec {
    program: "cargo",
    args: &["fmt"],
    label: "cargo-fmt",
};

/// `rumdl check`: reports markdown lint findings with exit 1.
const RUMDL_SPEC: ToolSpec = ToolSpec {
    program: "rumdl",
    args: &["check"],
    label: "rumdl",
};

/// `rumdl check --fix`: performs the rewrite.
const RUMDL_APPLY_SPEC: ToolSpec = ToolSpec {
    program: "rumdl",
    args: &["check", "--fix"],
    label: "rumdl",
};

/// Runs the Format-class tools under `root`.
///
/// When `apply` is set, fixes are auto-applied (`cargo fmt` writes, `rumdl
/// check --fix` fixes) and the resulting suggestions are marked applied;
/// otherwise both run in check mode and every finding is suggest-only. A
/// non-zero CHECK exit is the normal "findings exist" signal and parses;
/// only spawn failure/timeout yields `Unavailable`.
pub fn run(
    executor: &dyn SimplifyExecutor,
    root: &Path,
    apply: bool,
) -> (Vec<ToolStatus>, Vec<Suggestion>) {
    let mut statuses = Vec::new();
    let mut suggestions = Vec::new();

    // rustfmt: check FIRST (the finding source), then — when applying — a
    // second plain `cargo fmt` run that performs the rewrite. Suggestions
    // are marked applied only when that fixing command succeeded.
    if executor.available(FMT_SPEC.program) {
        let check = executor.run(&FMT_SPEC, root, TOOL_TIMEOUT);
        let fixed = apply.then(|| executor.run(&FMT_APPLY_SPEC, root, TOOL_TIMEOUT));
        // Record BOTH invocations when applying (check + apply); check-only
        // otherwise. The check status carries the finding signal; the apply
        // status carries the rewrite result.
        statuses.push(ToolStatus::from_outcome(FMT_SPEC.label, &check));
        if let Some(apply_outcome) = &fixed {
            statuses.push(ToolStatus::from_outcome(
                FMT_APPLY_SPEC.label,
                apply_outcome,
            ));
        }
        let apply_succeeded = matches!(&fixed, Some(ToolOutcome::Ran(output)) if output.success());
        if let ToolOutcome::Ran(output) = &check {
            for path in fmt_unformatted_files(&output.stderr) {
                suggestions.push(Suggestion {
                    class: SuggestionClass::Format,
                    path: Some(path),
                    line: None,
                    rule: "rustfmt".to_string(),
                    message: "file is not rustfmt-formatted".to_string(),
                    applied: apply_succeeded,
                });
            }
        }
    } else {
        statuses.push(ToolStatus::Unavailable {
            tool: FMT_SPEC.label.to_string(),
            reason: "spawn".to_string(),
        });
    }

    // rumdl (markdown): same check-then-apply shape.
    if executor.available(RUMDL_SPEC.program) {
        let check = executor.run(&RUMDL_SPEC, root, TOOL_TIMEOUT);
        let fixed = apply.then(|| executor.run(&RUMDL_APPLY_SPEC, root, TOOL_TIMEOUT));
        statuses.push(ToolStatus::from_outcome(RUMDL_SPEC.label, &check));
        if let Some(apply_outcome) = &fixed {
            statuses.push(ToolStatus::from_outcome(
                RUMDL_APPLY_SPEC.label,
                apply_outcome,
            ));
        }
        let apply_succeeded = matches!(&fixed, Some(ToolOutcome::Ran(output)) if output.success());
        if let ToolOutcome::Ran(output) = &check {
            for path in rumdl_flagged_files(&output.stdout) {
                suggestions.push(Suggestion {
                    class: SuggestionClass::Format,
                    path: Some(path),
                    line: None,
                    rule: "rumdl".to_string(),
                    message: "markdown lint finding (rumdl)".to_string(),
                    applied: apply_succeeded,
                });
            }
        }
    } else {
        statuses.push(ToolStatus::Unavailable {
            tool: RUMDL_SPEC.label.to_string(),
            reason: "spawn".to_string(),
        });
    }

    (statuses, suggestions)
}

/// Parses `cargo fmt --check` stderr ("Diff in <path> at line N:") into the
/// list of unformatted files, deduplicated in report order.
#[must_use]
pub fn fmt_unformatted_files(stderr: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("Diff in ")
            && let Some(path) = rest.split(" at line ").next()
        {
            let path = path.trim();
            if !path.is_empty() && !files.iter().any(|existing: &String| existing == path) {
                files.push(path.to_string());
            }
        }
    }
    files
}

/// Parses `rumdl check` output lines of the form `<path>:<line>:<col>: MK<nnn>`
/// into flagged file paths, deduplicated in report order.
#[must_use]
pub fn rumdl_flagged_files(stdout: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in stdout.lines() {
        let Some(colon) = line.find(':') else {
            continue;
        };
        let path = &line[..colon];
        // Heuristic guard: a path-like prefix that exists as text and is
        // followed by line/col numbers and a rule id.
        let rest = &line[colon + 1..];
        let numeric = rest
            .split(':')
            .next()
            .is_some_and(|first| first.parse::<u32>().is_ok());
        if !path.is_empty()
            && !path.contains(char::is_whitespace)
            && numeric
            && !files.iter().any(|existing: &String| existing == path)
        {
            files.push(path.to_string());
        }
    }
    files
}
