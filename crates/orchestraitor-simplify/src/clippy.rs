//! clippy runner: parses `cargo clippy --message-format json` diagnostics
//! into typed suggestions.

use crate::executor::{SimplifyExecutor, ToolOutcome, ToolSpec};
use crate::report::{Suggestion, SuggestionClass, ToolStatus};
use crate::{SimplifyConfig, TOOL_TIMEOUT};

/// clippy invocation (base form; the pedantic variant appends one arg).
const CLIPPY_ARGS: &[&str] = &["clippy", "--message-format", "json", "--quiet", "--"];
const CLIPPY_PEDANTIC_ARGS: &[&str] = &[
    "clippy",
    "--message-format",
    "json",
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

/// Rule label for unused-dependency findings (shared with `deadcode`).
pub(crate) const UNUSED_DEPENDENCY_RULE: &str = "unused-dependency";

/// Runs clippy under `root` and renders its diagnostics as suggestions.
///
/// Non-zero clippy exits are normal (findings exist) and still parse: only a
/// spawn failure or timeout yields `Unavailable`. Applied suggestions carry
/// `applied = true` only when `apply_safe` is set AND the replacement targets
/// a `.rs` file — in this slice safe-fix auto-apply is conservatively narrow
/// pending the Arbitraitor classification gate (PR-2).
pub fn run(
    executor: &dyn SimplifyExecutor,
    root: &std::path::Path,
    config: &SimplifyConfig,
    apply_safe: bool,
) -> (ToolStatus, Vec<Suggestion>) {
    let spec = if config.pedantic {
        &CLIPPY_PEDANTIC_SPEC
    } else {
        &CLIPPY_SPEC
    };
    if !executor.available(spec.program) {
        return (
            ToolStatus::Unavailable {
                tool: spec.label.to_string(),
                reason: "spawn".to_string(),
            },
            Vec::new(),
        );
    }
    let outcome = executor.run(spec, root, TOOL_TIMEOUT);
    let status = ToolStatus::from_outcome(spec.label, &outcome);
    match outcome {
        ToolOutcome::Ran(output) => {
            let suggestions = parse(&output.stdout, apply_safe);
            (status, suggestions)
        }
        ToolOutcome::Unavailable(_) => (status, Vec::new()),
    }
}

/// Parses clippy JSON diagnostics from a cargo JSON stream (lines that are
/// not `compiler-message` diagnostics are skipped).
///
/// A diagnostic becomes a suggestion when it carries a lint code
/// (`clippy::*` or rustc lint codes) and a primary span. Diagnostics with a
/// machine-applicable replacement render as `SafeFix` suggestions (applied
/// only under the explicit flag on `.rs` files); everything else is
/// Semantic and suggest-only.
#[must_use]
pub fn parse(stream: &str, apply_safe: bool) -> Vec<Suggestion> {
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
        let file_is_rs = path.as_deref().is_some_and(|file| {
            std::path::Path::new(file)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"))
        });
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
            applied: apply_safe && machine_applicable && file_is_rs,
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
