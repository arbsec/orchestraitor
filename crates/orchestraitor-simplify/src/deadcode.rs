//! Dead-code runner: cargo-machete (when installed) plus the clippy
//! dead-code/unused family already surfaced by the clippy run. Suggest-only
//! (§9.5 semantic class) — never auto-rewritten.

use std::path::Path;

use crate::clippy;
use crate::executor::{SimplifyExecutor, ToolOutcome, ToolSpec};
use crate::report::{Suggestion, SuggestionClass, ToolStatus};
use crate::{SimplifyConfig, TOOL_TIMEOUT};

/// `cargo-machete`: finds unused dependencies. Optional tool — absent
/// installations are recorded, never fatal. `--json` selects the stable
/// machine-readable output contract (the default text format is
/// human-oriented headings and is not parsed).
const MACHETE_SPEC: ToolSpec = ToolSpec {
    program: "cargo-machete",
    args: &["--json"],
    label: "cargo-machete",
};

/// Runs the dead-code tier under `root`.
///
/// cargo-machete runs only when installed (`available` probe first, so the
/// common absent-tool case is a captured status, not a spawn failure). The
/// clippy `dead_code/unused` lint family is contributed by the clippy tier;
/// this module re-renders unused-dependency findings as semantic
/// suggestions. Everything here is suggest-only by construction: no `apply`
/// path exists in this module.
pub fn run(
    executor: &dyn SimplifyExecutor,
    root: &Path,
    _config: &SimplifyConfig,
) -> (ToolStatus, Vec<Suggestion>) {
    if !executor.available(MACHETE_SPEC.program) {
        return (
            ToolStatus::Unavailable {
                tool: MACHETE_SPEC.label.to_string(),
                reason: "spawn".to_string(),
            },
            Vec::new(),
        );
    }
    let outcome = executor.run(&MACHETE_SPEC, root, TOOL_TIMEOUT);
    let status = ToolStatus::from_outcome(MACHETE_SPEC.label, &outcome);
    match outcome {
        ToolOutcome::Ran(output) => {
            let suggestions = parse(&output.stdout);
            (status, suggestions)
        }
        ToolOutcome::Unavailable(_) => (status, Vec::new()),
    }
}

/// Parses `cargo-machete --json` output (the documented machine-readable
/// contract): `{"crates":[{"package_name":…,"manifest_path":…,"unused":[…],
/// "ignored_used":[…]}]}`. Each unused dependency of a crate becomes one
/// semantic suggestion carrying the manifest path. Unparseable output yields
/// no suggestions (fail-open).
#[must_use]
pub fn parse(stdout: &str) -> Vec<Suggestion> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Vec::new();
    };
    let Some(crates) = value.get("crates").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut suggestions = Vec::new();
    for analyzed in crates {
        let path = analyzed
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        for dependency in analyzed
            .get("unused")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(dependency) = dependency.as_str() else {
                continue;
            };
            suggestions.push(Suggestion {
                class: SuggestionClass::Semantic,
                path: (!path.is_empty()).then(|| path.to_string()),
                line: None,
                rule: clippy::UNUSED_DEPENDENCY_RULE.to_string(),
                message: format!("unused dependency `{dependency}` (cargo-machete)"),
                applied: false,
            });
        }
    }
    suggestions
}
