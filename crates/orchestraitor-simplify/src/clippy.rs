//! clippy runner: parses `cargo clippy --message-format json` diagnostics
//! into typed suggestions.

use crate::executor::{SimplifyExecutor, ToolOutcome, ToolSpec};
use crate::report::{Suggestion, SuggestionClass, ToolStatus};
use crate::{SimplifyConfig, TOOL_TIMEOUT};

/// clippy invocation (base form; the pedantic variant appends one arg).
pub(crate) const CLIPPY_ARGS: &[&str] = &["clippy", "--message-format", "json", "--quiet", "--"];
pub(crate) const CLIPPY_PEDANTIC_ARGS: &[&str] = &[
    "clippy",
    "--message-format",
    "json",
    "--quiet",
    "--",
    "-W",
    "clippy::pedantic",
];

/// The clippy fix invocation (`cargo clippy --fix --allow-dirty`): applies
/// machine-applicable suggestions. Runs ONLY under the safe-fix policy,
/// AFTER the check pass; `applied` is true only when this succeeded.
/// The trailing lint args mirror the check invocation exactly (same lint
/// level), so the fix scope covers every suggestion the check reported —
/// a pedantic `SafeFix` can never be marked applied by a fix run that never
/// saw the pedantic lints.
pub(crate) const CLIPPY_FIX_ARGS: &[&str] = &["clippy", "--fix", "--allow-dirty", "--quiet"];
pub(crate) const CLIPPY_FIX_PEDANTIC_ARGS: &[&str] = &[
    "clippy",
    "--fix",
    "--allow-dirty",
    "--quiet",
    "--",
    "-W",
    "clippy::pedantic",
];

/// Base clippy spec.
const CLIPPY_SPEC: ToolSpec = ToolSpec {
    program: "cargo",
    args: CLIPPY_ARGS,
    label: "clippy",
};

/// Pedantic clippy spec.
const CLIPPY_PEDANTIC_SPEC: ToolSpec = ToolSpec {
    program: "cargo",
    args: CLIPPY_PEDANTIC_ARGS,
    label: "clippy",
};

/// Clippy fix spec (lint scope mirrors the check spec).
const CLIPPY_FIX_SPEC: ToolSpec = ToolSpec {
    program: "cargo",
    args: CLIPPY_FIX_ARGS,
    label: "clippy",
};

/// Pedantic clippy fix spec.
const CLIPPY_FIX_PEDANTIC_SPEC: ToolSpec = ToolSpec {
    program: "cargo",
    args: CLIPPY_FIX_PEDANTIC_ARGS,
    label: "clippy",
};

/// Rule label for unused-dependency findings (shared with `deadcode`).
pub(crate) const UNUSED_DEPENDENCY_RULE: &str = "unused-dependency";

/// Runs clippy under `root` and renders its diagnostics as suggestions.
///
/// Non-zero clippy exits are normal (findings exist) and still parse: only a
/// spawn failure or timeout yields `Unavailable`. When `apply_safe` is set,
/// a `cargo clippy --fix --allow-dirty` invocation runs AFTER the check and
/// a verify check runs after a successful fix; a suggestion is marked
/// `applied` only when it is gone from the verify output (a zero fix exit
/// alone proves nothing — rustfix can fail and roll back individual
/// suggestions). The safe-fix surface stays conservatively narrow pending
/// the Arbitraitor classification gate (PR-2).
pub fn run(
    executor: &dyn SimplifyExecutor,
    root: &std::path::Path,
    config: &SimplifyConfig,
    apply_safe: bool,
) -> (Vec<ToolStatus>, Vec<Suggestion>) {
    let (spec, fix_spec) = if config.pedantic {
        (&CLIPPY_PEDANTIC_SPEC, &CLIPPY_FIX_PEDANTIC_SPEC)
    } else {
        (&CLIPPY_SPEC, &CLIPPY_FIX_SPEC)
    };
    if !executor.available(spec.program) {
        return (
            vec![ToolStatus::Unavailable {
                tool: spec.label.to_string(),
                reason: "spawn".to_string(),
            }],
            Vec::new(),
        );
    }
    let check = executor.run(spec, root, TOOL_TIMEOUT);
    let mut statuses = vec![ToolStatus::from_outcome(spec.label, &check)];
    match check {
        ToolOutcome::Ran(output) => {
            let mut suggestions = parse(&output.stdout);
            // The fix pass runs only under the policy AND only after a
            // successful check parse; `applied` is earned by disappearance
            // from a verify check, not by the fix run's exit code.
            if apply_safe {
                let fix = executor.run(fix_spec, root, TOOL_TIMEOUT);
                let fix_succeeded = matches!(&fix, ToolOutcome::Ran(outcome) if outcome.success());
                statuses.push(ToolStatus::from_outcome(CLIPPY_FIX_SPEC.label, &fix));
                if fix_succeeded {
                    let verify = executor.run(spec, root, TOOL_TIMEOUT);
                    if let ToolOutcome::Ran(verify_output) = &verify {
                        let verified = parse(&verify_output.stdout);
                        // Identity ignores `line`: an applied fix can shift
                        // the line numbers of surviving suggestions below
                        // it, so a line-exact match would misreport those
                        // survivors as applied. Class + file + rule is the
                        // stable identity across a fix pass.
                        let resolved: std::collections::HashSet<_> = verified
                            .iter()
                            .filter(|suggestion| suggestion.class == SuggestionClass::SafeFix)
                            .map(|suggestion| (suggestion.path.clone(), suggestion.rule.clone()))
                            .collect();
                        for suggestion in &mut suggestions {
                            if suggestion.class == SuggestionClass::SafeFix
                                && !resolved
                                    .contains(&(suggestion.path.clone(), suggestion.rule.clone()))
                            {
                                suggestion.applied = true;
                            }
                        }
                    }
                    statuses.push(ToolStatus::from_outcome(spec.label, &verify));
                }
            }
            (statuses, suggestions)
        }
        ToolOutcome::Unavailable(_) => (statuses, Vec::new()),
    }
}

/// Parses clippy JSON diagnostics from a cargo JSON stream (lines that are
/// not `compiler-message` diagnostics are skipped).
///
/// A diagnostic becomes a suggestion when it carries a lint code
/// (`clippy::*` or rustc lint codes) and a primary span. Diagnostics with a
/// machine-applicable replacement render as `SafeFix` suggestions;
/// everything else is Semantic and suggest-only. `applied` is set by the
/// runner after a successful fix pass — never here.
#[must_use]
pub fn parse(stream: &str) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();
    for line in stream.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-message") {
            continue;
        }
        let Some(message) = value.get("message") else {
            continue;
        };
        // Errors are compile failures, not simplification material; the tool
        // exit status records them.
        if message.get("level").and_then(serde_json::Value::as_str) != Some("warning") {
            continue;
        }
        let Some(code) = message
            .get("code")
            .and_then(|code| code.get("code"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let (path, line_no) = primary_location(message);
        let machine_applicable = has_machine_applicable_replacement(message);
        suggestions.push(Suggestion {
            class: if machine_applicable {
                SuggestionClass::SafeFix
            } else {
                SuggestionClass::Semantic
            },
            path,
            line: line_no,
            rule: code.to_string(),
            message: message
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            // Never set here: the runner marks `applied` only after a real
            // fix pass succeeds.
            applied: false,
        });
    }
    suggestions
}

/// Extracts the primary span's file and 1-based start line.
fn primary_location(message: &serde_json::Value) -> (Option<String>, Option<u32>) {
    let primary = message
        .get("spans")
        .and_then(serde_json::Value::as_array)
        .and_then(|spans| {
            spans.iter().find(|span| {
                span.get("is_primary").and_then(serde_json::Value::as_bool) == Some(true)
            })
        });
    match primary {
        Some(span) => {
            let file = span
                .get("file_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            (
                (!file.is_empty()).then(|| file.to_string()),
                span.get("line_start")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|line| u32::try_from(line).ok()),
            )
        }
        None => (None, None),
    }
}

/// Whether any help child carries a `MachineApplicable` suggested replacement.
fn has_machine_applicable_replacement(message: &serde_json::Value) -> bool {
    message
        .get("children")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|children| {
            children.iter().any(|child| {
                child
                    .get("spans")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|spans| {
                        spans.iter().any(|span| {
                            span.get("suggestion_applicability")
                                .and_then(serde_json::Value::as_str)
                                == Some("MachineApplicable")
                                && span
                                    .get("suggested_replacement")
                                    .and_then(serde_json::Value::as_str)
                                    .is_some()
                        })
                    })
            })
        })
}
