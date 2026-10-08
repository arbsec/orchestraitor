//! Unit tests: JSON parse of clippy fixtures, tool-absent behavior,
//! bounds validation, dedup.

// Test-only allowances mirror `tests/cli.rs` in the CLI crate: the scripted
// harness fails the test loudly on unexpected results.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::time::Duration;

use super::*;
use crate::clippy::{CLIPPY_FIX_ARGS, CLIPPY_FIX_PEDANTIC_ARGS, CLIPPY_PEDANTIC_ARGS};
use crate::executor::{ExecOutput, SimplifyError, SimplifyExecutor, ToolOutcome, ToolSpec};
use crate::report::{SimplifyReport, Suggestion, SuggestionClass, ToolStatus};

/// Scripted executor: records whether a program was probed/ran and returns
/// canned outcomes. Outcomes are keyed by `program + args` (not just the
/// label) so a test can give the check invocation and the apply invocation
/// of the same tool different results.
struct ScriptedExecutor {
    available_programs: Vec<&'static str>,
    outcomes: std::sync::Mutex<std::collections::BTreeMap<String, ToolOutcome>>,
}

impl ScriptedExecutor {
    fn with_available(programs: &[&'static str]) -> Self {
        Self {
            available_programs: programs.to_vec(),
            outcomes: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        }
    }

    fn script(&self, label: &str, outcome: ToolOutcome) {
        self.outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(label.to_string(), outcome);
    }

    /// Scripts an outcome for a specific argv (program + argument list).
    fn script_argv(&self, program: &str, args: &[&str], outcome: ToolOutcome) {
        self.outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(argv_key(program, args), outcome);
    }
}

/// Outcome key for a program + argument list (used by `script_argv`).
fn argv_key(program: &str, args: &[&str]) -> String {
    format!("{program} {args:?}")
}

impl SimplifyExecutor for ScriptedExecutor {
    fn run(&self, spec: &ToolSpec, _dir: &Path, _timeout: Duration) -> ToolOutcome {
        let key = argv_key(spec.program, spec.args);
        let outcomes = self
            .outcomes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        outcomes
            .get(&key)
            .or_else(|| outcomes.get(spec.label))
            .cloned()
            .unwrap_or(ToolOutcome::Unavailable(SimplifyError::ToolUnavailable {
                tool: spec.label,
                reason: "spawn",
            }))
    }

    fn available(&self, program: &str) -> bool {
        self.available_programs.contains(&program)
    }
}

fn ran_output(stdout: &str) -> ToolOutcome {
    ToolOutcome::Ran(ExecOutput {
        code: Some(0),
        stdout: stdout.to_string(),
        stderr: String::new(),
    })
}

/// Machine-applicable clippy diagnostic with a `.rs` primary span (mirrors
/// real cargo clippy JSON, verified against clippy 1.96 output).
const MACHINE_APPLICABLE: &str = r#"{"reason":"compiler-message","message":{"level":"warning","message":"using `clone` on type `u32` which implements the `Copy` trait","code":{"code":"clippy::clone_on_copy","explanation":null},"spans":[{"file_name":"src/lib.rs","is_primary":true,"line_start":2,"line_end":2,"column_start":13,"column_end":22}],"children":[{"level":"help","message":"try removing the `clone` call","spans":[{"file_name":"src/lib.rs","is_primary":true,"line_start":2,"line_end":2,"column_start":13,"column_end":22,"suggestion_applicability":"MachineApplicable","suggested_replacement":"x"}]}]}}"#;

#[test]
fn clippy_parse_extracts_machine_applicable_suggestion() {
    let suggestions = crate::clippy::parse(MACHINE_APPLICABLE);
    assert_eq!(suggestions.len(), 1);
    let suggestion = &suggestions[0];
    assert_eq!(suggestion.class, SuggestionClass::SafeFix);
    assert_eq!(suggestion.path.as_deref(), Some("src/lib.rs"));
    assert_eq!(suggestion.line, Some(2));
    assert_eq!(suggestion.rule, "clippy::clone_on_copy");
    assert!(!suggestion.applied);
}

#[test]
fn clippy_fix_pass_marks_rs_safe_fixes_applied_only_on_success() {
    // The check runs first; the explicit `cargo clippy --fix` pass earns
    // `applied = true` for machine-applicable suggestions on `.rs` files
    // ONLY when it succeeds.
    let config = SimplifyConfig::default();
    let executor = ScriptedExecutor::with_available(&["cargo"]);
    executor.script_argv(
        "cargo",
        &["clippy", "--message-format", "json", "--quiet", "--"],
        ran_output(MACHINE_APPLICABLE),
    );
    executor.script_argv(
        "cargo",
        &["clippy", "--fix", "--allow-dirty", "--quiet"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    let (_statuses, suggestions) = crate::clippy::run(&executor, Path::new("/tmp"), &config, true);
    assert_eq!(suggestions.len(), 1);
    assert!(suggestions[0].applied, "successful fix pass earns applied");
}

#[test]
fn clippy_failed_fix_pass_keeps_suggestions_suggest_only() {
    let config = SimplifyConfig::default();
    let executor = ScriptedExecutor::with_available(&["cargo"]);
    executor.script_argv(
        "cargo",
        &["clippy", "--message-format", "json", "--quiet", "--"],
        ran_output(MACHINE_APPLICABLE),
    );
    executor.script_argv(
        "cargo",
        &["clippy", "--fix", "--allow-dirty", "--quiet"],
        ToolOutcome::Unavailable(SimplifyError::ToolUnavailable {
            tool: "clippy",
            reason: "timeout",
        }),
    );
    let (statuses, suggestions) = crate::clippy::run(&executor, Path::new("/tmp"), &config, true);
    assert_eq!(suggestions.len(), 1);
    assert!(
        !suggestions[0].applied,
        "failed fix pass must not claim applied"
    );
    // Both invocations are recorded: check + failed fix.
    assert_eq!(statuses.len(), 2);
}

#[test]
fn clippy_without_safe_policy_never_runs_the_fix_pass() {
    let config = SimplifyConfig::default();
    let executor = ScriptedExecutor::with_available(&["cargo"]);
    executor.script_argv(
        "cargo",
        &["clippy", "--message-format", "json", "--quiet", "--"],
        ran_output(MACHINE_APPLICABLE),
    );
    let (statuses, suggestions) = crate::clippy::run(&executor, Path::new("/tmp"), &config, false);
    assert_eq!(suggestions.len(), 1);
    assert!(
        !suggestions[0].applied,
        "no policy, no fix pass, no applied"
    );
    // Check only: the fix invocation never runs without the policy.
    assert_eq!(statuses.len(), 1);
}

#[test]
fn clippy_fix_pass_scope_matches_check_scope() {
    // Regression (CodeRabbit PR-536): the fix invocation must cover exactly
    // the check's lint scope — the same lint level (clippy::pedantic
    // included when the check enables it) — or a successful fix run could
    // mark a pedantic SafeFix applied that the fix scope never saw.
    //
    // The ScriptedExecutor keys outcomes by exact argv, so scripting the fix
    // outcome ONLY under the pedantic fix argv makes the pass's fix spawn
    // observable: the pedantic run below only earns `applied` if the fix
    // invocation actually carried `-W clippy::pedantic`.
    let config = SimplifyConfig {
        pedantic: true,
        ..SimplifyConfig::default()
    };
    let executor = ScriptedExecutor::with_available(&["cargo"]);
    executor.script_argv(
        "cargo",
        CLIPPY_PEDANTIC_ARGS,
        ran_output(MACHINE_APPLICABLE),
    );
    executor.script_argv(
        "cargo",
        CLIPPY_FIX_PEDANTIC_ARGS,
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    let (_statuses, suggestions) = crate::clippy::run(&executor, Path::new("/tmp"), &config, true);
    assert_eq!(suggestions.len(), 1);
    assert!(
        suggestions[0].applied,
        "pedantic fix pass earns applied only because its argv matches the check scope"
    );

    // Symmetric negative: with the same pedantic config, a fix outcome
    // scripted ONLY under the non-pedantic fix argv is never selected, so
    // `applied` stays false — proving the pedantic run never falls back to
    // the base fix scope.
    let mismatched = ScriptedExecutor::with_available(&["cargo"]);
    mismatched.script_argv(
        "cargo",
        CLIPPY_PEDANTIC_ARGS,
        ran_output(MACHINE_APPLICABLE),
    );
    mismatched.script_argv(
        "cargo",
        CLIPPY_FIX_ARGS,
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    let (_statuses, suggestions) =
        crate::clippy::run(&mismatched, Path::new("/tmp"), &config, true);
    assert_eq!(suggestions.len(), 1);
    assert!(
        !suggestions[0].applied,
        "a fix run with the WRONG (base) scope must not mark pedantic suggestions applied"
    );
}

#[test]
fn clippy_parse_ignores_non_diagnostic_lines() {
    let stream = concat!(
        "{\"reason\":\"compiler-artifact\",\"package_id\":\"x\"}\n",
        "not json at all\n",
        "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"message\":\"m\",\"code\":{\"code\":\"clippy::redundant_clone\"},\"spans\":[{\"file_name\":\"src/a.rs\",\"is_primary\":true,\"line_start\":7,\"line_end\":7,\"column_start\":1,\"column_end\":2}],\"children\":[]}}\n",
    );
    let suggestions = crate::clippy::parse(stream);
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].class, SuggestionClass::Semantic);
    assert_eq!(suggestions[0].line, Some(7));
}

#[test]
fn clippy_parse_skips_errors_without_lint_code() {
    let stream = concat!(
        "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"message\":\"could not compile\",\"code\":null,\"spans\":[],\"children\":[]}}\n",
        "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"message\":\"unused variable\",\"code\":{\"code\":\"unused_variables\"},\"spans\":[{\"file_name\":\"src/b.rs\",\"is_primary\":true,\"line_start\":3,\"line_end\":3,\"column_start\":5,\"column_end\":6}],\"children\":[]}}\n",
    );
    let suggestions = crate::clippy::parse(stream);
    assert_eq!(
        suggestions.len(),
        1,
        "error diagnostics are not suggestions"
    );
    assert_eq!(suggestions[0].rule, "unused_variables");
}

#[test]
fn clippy_absent_tool_is_captured_not_fatal() {
    let executor = ScriptedExecutor::with_available(&[]);
    let config = SimplifyConfig::default();
    let (statuses, suggestions) = crate::clippy::run(&executor, Path::new("/tmp"), &config, false);
    assert_eq!(
        statuses,
        vec![ToolStatus::Unavailable {
            tool: "clippy".to_string(),
            reason: "spawn".to_string()
        }]
    );
    assert!(suggestions.is_empty());
}

#[test]
fn format_absent_tools_yield_unavailable_statuses() {
    let executor = ScriptedExecutor::with_available(&[]);
    let (statuses, suggestions) = crate::format::run(&executor, Path::new("/tmp"), false);
    assert_eq!(statuses.len(), 2);
    assert!(
        statuses
            .iter()
            .all(|status| matches!(status, ToolStatus::Unavailable { .. }))
    );
    assert!(suggestions.is_empty());
}

#[test]
fn format_check_parses_unformatted_files() {
    let executor = ScriptedExecutor::with_available(&["cargo", "rumdl"]);
    executor.script_argv(
        "cargo",
        &["fmt", "--check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "Diff in src/lib.rs at line 1:\nDiff in src/other.rs at line 9:\n".to_string(),
        }),
    );
    executor.script_argv(
        "rumdl",
        &["check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(1),
            stdout: "docs/cli/x.md:12:1: MK001 rule\n".to_string(),
            stderr: String::new(),
        }),
    );
    let (statuses, suggestions) = crate::format::run(&executor, Path::new("/tmp"), false);
    assert_eq!(statuses.len(), 2);
    assert_eq!(suggestions.len(), 3);
    assert!(
        suggestions
            .iter()
            .all(|s| s.class == SuggestionClass::Format)
    );
    assert!(suggestions.iter().all(|s| !s.applied));
    assert_eq!(suggestions[0].path.as_deref(), Some("src/lib.rs"));
    assert_eq!(suggestions[2].path.as_deref(), Some("docs/cli/x.md"));
}

#[test]
fn format_apply_runs_plain_fmt_after_check_and_marks_applied() {
    // The apply path is check FIRST (the finding source), then a plain
    // `cargo fmt` WITHOUT `--check` performing the rewrite. Suggestions are
    // marked applied only because the fixing command succeeded.
    let executor = ScriptedExecutor::with_available(&["cargo", "rumdl"]);
    executor.script_argv(
        "cargo",
        &["fmt", "--check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "Diff in src/lib.rs at line 1:\n".to_string(),
        }),
    );
    executor.script_argv(
        "cargo",
        &["fmt"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    executor.script_argv(
        "rumdl",
        &["check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    let (statuses, suggestions) = crate::format::run(&executor, Path::new("/tmp"), true);
    // Statuses: check + apply for cargo-fmt, check + apply for rumdl.
    assert_eq!(statuses.len(), 4);
    assert_eq!(suggestions.len(), 1);
    assert!(suggestions[0].applied);
    assert_eq!(suggestions[0].path.as_deref(), Some("src/lib.rs"));
}

#[test]
fn format_failed_apply_keeps_suggestions_unapplied() {
    // When the fixing command fails, findings stay suggest-only.
    let executor = ScriptedExecutor::with_available(&["cargo", "rumdl"]);
    executor.script_argv(
        "cargo",
        &["fmt", "--check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "Diff in src/lib.rs at line 1:\n".to_string(),
        }),
    );
    executor.script_argv(
        "cargo",
        &["fmt"],
        ToolOutcome::Unavailable(SimplifyError::ToolUnavailable {
            tool: "cargo-fmt",
            reason: "timeout",
        }),
    );
    executor.script_argv(
        "rumdl",
        &["check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    let (_statuses, suggestions) = crate::format::run(&executor, Path::new("/tmp"), true);
    assert_eq!(suggestions.len(), 1);
    assert!(!suggestions[0].applied, "failed fix must not claim applied");
}

#[test]
fn format_apply_marks_suggestions_applied() {
    let executor = ScriptedExecutor::with_available(&["cargo", "rumdl"]);
    executor.script_argv(
        "cargo",
        &["fmt", "--check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    executor.script_argv(
        "rumdl",
        &["check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    let (statuses, suggestions) = crate::format::run(&executor, Path::new("/tmp"), true);
    assert_eq!(statuses.len(), 4);
    assert!(suggestions.is_empty(), "applied fixes leave no findings");
}

#[test]
fn deadcode_machete_absent_is_suggest_only_noop() {
    let executor = ScriptedExecutor::with_available(&[]);
    let config = SimplifyConfig::default();
    let (status, suggestions) = crate::deadcode::run(&executor, Path::new("/tmp"), &config);
    assert!(matches!(status, ToolStatus::Unavailable { .. }));
    assert!(suggestions.is_empty());
}

#[test]
fn deadcode_machete_findings_are_never_applied() {
    let executor = ScriptedExecutor::with_available(&["cargo-machete"]);
    executor.script_argv(
        "cargo-machete",
        &[],
        ran_output("serde serde /tmp/probe/Cargo.toml\n"),
    );
    let config = SimplifyConfig::default();
    let (status, suggestions) = crate::deadcode::run(&executor, Path::new("/tmp"), &config);
    assert!(matches!(status, ToolStatus::Ran { .. }));
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].class, SuggestionClass::Semantic);
    assert!(!suggestions[0].applied, "dead-code is suggest-only");
}

#[test]
fn report_dedup_collapses_identical_findings() {
    let mut report = SimplifyReport::new(true);
    let suggestion = Suggestion {
        class: SuggestionClass::SafeFix,
        path: Some("src/a.rs".to_string()),
        line: Some(3),
        rule: "clippy::redundant_clone".to_string(),
        message: "first".to_string(),
        applied: false,
    };
    report.push(suggestion.clone());
    report.push(Suggestion {
        message: "second".to_string(),
        ..suggestion
    });
    report.dedup();
    assert_eq!(report.suggestions.len(), 1);
}

#[test]
fn report_counts_applied_and_unaddressed() {
    let mut report = SimplifyReport::new(true);
    report.push(Suggestion {
        class: SuggestionClass::Format,
        path: None,
        line: None,
        rule: "rustfmt".to_string(),
        message: String::new(),
        applied: true,
    });
    report.push(Suggestion {
        class: SuggestionClass::Semantic,
        path: None,
        line: None,
        rule: "dead_code".to_string(),
        message: String::new(),
        applied: false,
    });
    assert_eq!(report.auto_applied_count, 1);
    assert_eq!(report.unaddressed(None), 1);
    assert_eq!(report.unaddressed(Some(SuggestionClass::Semantic)), 1);
    assert_eq!(report.unaddressed(Some(SuggestionClass::Format)), 0);
}

#[test]
fn config_bounds_are_validated_fail_closed() {
    assert!(SimplifyConfig::default().validate().is_ok());
    let zero_passes = SimplifyConfig {
        max_passes: 0,
        ..SimplifyConfig::default()
    };
    assert!(matches!(
        zero_passes.validate(),
        Err(SimplifyError::InvalidConfig { .. })
    ));
    let zero_files = SimplifyConfig {
        max_files: 0,
        ..SimplifyConfig::default()
    };
    assert!(matches!(
        zero_files.validate(),
        Err(SimplifyError::InvalidConfig { .. })
    ));
}

#[test]
fn pass_with_no_tools_reports_ran_false() {
    let executor = ScriptedExecutor::with_available(&[]);
    let config = SimplifyConfig::default();
    let pass =
        SimplifyPass::new(&config, &executor, FixPolicy::None, false, false).expect("valid config");
    let report = pass.run(Path::new("/tmp"));
    assert!(!report.ran, "no tool ran");
    assert!(report.ran_rules_only);
    assert!(report.suggestions.is_empty());
    assert_eq!(report.tools.len(), 4, "fmt+rumdl+clippy+machete statuses");
}

#[test]
fn pass_report_is_deterministic_and_sorted() {
    let executor = ScriptedExecutor::with_available(&["cargo", "rumdl", "cargo-machete"]);
    executor.script_argv("cargo", &["fmt", "--check"], ran_output(""));
    executor.script_argv("rumdl", &["check"], ran_output(""));
    executor.script_argv(
        "cargo",
        &["clippy", "--message-format", "json", "--quiet", "--"],
        ran_output(concat!(
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"message\":\"a\",\"code\":{\"code\":\"clippy::aa\"},\"spans\":[{\"file_name\":\"src/z.rs\",\"is_primary\":true,\"line_start\":1,\"line_end\":1,\"column_start\":1,\"column_end\":1}],\"children\":[]}}\n",
            "{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"message\":\"b\",\"code\":{\"code\":\"clippy::bb\"},\"spans\":[{\"file_name\":\"src/a.rs\",\"is_primary\":true,\"line_start\":2,\"line_end\":2,\"column_start\":1,\"column_end\":1}],\"children\":[]}}\n",
        )),
    );
    executor.script("cargo-machete", ran_output(""));
    let config = SimplifyConfig::default();
    let pass =
        SimplifyPass::new(&config, &executor, FixPolicy::None, false, false).expect("valid config");
    let report = pass.run(Path::new("/tmp"));
    assert!(report.ran);
    assert_eq!(
        report.suggestions.len(),
        2,
        "dedup keeps both distinct keys"
    );
}

#[test]
fn fix_policy_respects_config_flags() {
    // Format fix requires BOTH the CLI policy and the config flag.
    let executor = ScriptedExecutor::with_available(&["cargo", "rumdl"]);
    executor.script_argv(
        "cargo",
        &["fmt", "--check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "Diff in src/lib.rs at line 1:\n".to_string(),
        }),
    );
    executor.script_argv(
        "rumdl",
        &["check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    let config = SimplifyConfig::default();
    // auto_apply_format = false in this call: findings stay unapplied
    // even under FixPolicy::Format.
    let pass = SimplifyPass::new(&config, &executor, FixPolicy::Format, false, false)
        .expect("valid config");
    let report = pass.run(Path::new("/tmp"));
    assert_eq!(report.auto_applied_count, 0);
    assert_eq!(report.unaddressed(Some(SuggestionClass::Format)), 1);

    // With the flag on, findings are marked applied (the executor ran the
    // fixing invocation); a clean scripted run leaves nothing to apply.
    let clean = ScriptedExecutor::with_available(&["cargo", "rumdl"]);
    clean.script_argv(
        "cargo",
        &["fmt", "--check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    clean.script_argv(
        "rumdl",
        &["check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    let pass =
        SimplifyPass::new(&config, &clean, FixPolicy::Format, true, false).expect("valid config");
    let report = pass.run(Path::new("/tmp"));
    assert_eq!(
        report.auto_applied_count, 0,
        "clean run leaves nothing to apply"
    );

    // A run whose script reports findings and auto-applies them: the
    // suggestion is marked applied under the flag.
    let fixing = ScriptedExecutor::with_available(&["cargo", "rumdl"]);
    fixing.script_argv(
        "cargo",
        &["fmt", "--check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(1),
            stdout: String::new(),
            stderr: "Diff in src/lib.rs at line 1:\n".to_string(),
        }),
    );
    fixing.script_argv(
        "cargo",
        &["fmt"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    fixing.script_argv(
        "rumdl",
        &["check"],
        ToolOutcome::Ran(ExecOutput {
            code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }),
    );
    let pass =
        SimplifyPass::new(&config, &fixing, FixPolicy::Format, true, false).expect("valid config");
    let report = pass.run(Path::new("/tmp"));
    assert_eq!(report.auto_applied_count, 1);
    assert_eq!(report.unaddressed(Some(SuggestionClass::Format)), 0);
}

#[test]
fn report_serializes_with_stable_field_names() {
    let mut report = SimplifyReport::new(false);
    report.record_tool(ToolStatus::Unavailable {
        tool: "clippy".to_string(),
        reason: "spawn".to_string(),
    });
    let json = serde_json::to_string(&report).expect("serialize");
    assert!(json.contains("\"ran\":false"));
    assert!(json.contains("\"ran_rules_only\":true"));
    assert!(json.contains("\"auto_applied_count\":0"));
    assert!(json.contains("\"duration\":0"));
    assert!(json.contains("\"unavailable\""));
}
