//! Dead-code runner: cargo-machete (when installed) plus the clippy
//! dead-code/unused family already surfaced by the clippy run. Suggest-only
//! (§9.5 semantic class) — never auto-rewritten.

use std::path::Path;

use crate::clippy;
use crate::executor::{SimplifyExecutor, ToolOutcome, ToolSpec};
use crate::report::{Suggestion, SuggestionClass, ToolStatus};
use crate::{SimplifyConfig, TOOL_TIMEOUT};

/// `cargo-machete`: finds unused dependencies. Optional tool — absent
/// installations are recorded, never fatal.
const MACHETE_SPEC: ToolSpec = ToolSpec {
    program: "cargo-machete",
    args: &[],
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

/// Parses cargo-machete output lines of the form
/// `crate_name package_name path` (its summary format) into unused-dependency
/// suggestions. Unparseable output yields no suggestions (fail-open).
#[must_use]
pub fn parse(stdout: &str) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let (Some(crate_name), Some(_package), Some(path)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        suggestions.push(Suggestion {
            class: SuggestionClass::Semantic,
            path: Some(path.to_string()),
            line: None,
            rule: clippy::UNUSED_DEPENDENCY_RULE.to_string(),
            message: format!("unused dependency `{crate_name}` (cargo-machete)"),
            applied: false,
        });
    }
    suggestions
}
