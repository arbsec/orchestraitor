//! Sub-session runtime tests (issue #535, T3): budget carving, depth
//! enforcement, child-await beats, typed failure mapping — over the
//! deterministic simulator (spec §21.3).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use super::{
    SubsessionError, SubsessionParent, default_internal_tools, parent_failure_class, run_subsession,
};
use crate::result::FailureClass;
use crate::tooldef::{ToolBudget, ToolDefinition, ToolMechanism};
use orchestraitor_testkit::{OpenAiMockServer, PlannedResponse};

fn parent(depth: u8) -> SubsessionParent {
    SubsessionParent {
        worktree_root: std::env::temp_dir(),
        remaining: Duration::from_mins(10),
        depth,
        prior_daily_spend_usd: 0.0,
        role: "implement".to_string(),
        project: "test".to_string(),
        repository: "test".to_string(),
        session_id: "test-session".to_string(),
        attribution: None,
        cost_sink: None,
    }
}

fn _unused_explore_tool() -> ToolDefinition {
    ToolDefinition {
        id: "explore-q".to_string(),
        mechanism: ToolMechanism::Subagent {
            role: "explore".to_string(),
            internal_tools: default_internal_tools(),
            instructions: Some("You are a read-only explorer.".to_string()),
        },
        budget: ToolBudget {
            max_turns: 4,
            wall_clock_secs: Some(60),
            max_result_bytes: 8 * 1024,
        },
        visible_to: std::collections::BTreeSet::from(["implement".to_string()]),
    }
}

fn command_tool_def() -> ToolDefinition {
    ToolDefinition {
        id: "cmd".to_string(),
        mechanism: ToolMechanism::Command {
            argv: vec!["echo".to_string()],
        },
        budget: ToolBudget::bootstrap_defaults(),
        visible_to: std::collections::BTreeSet::new(),
    }
}

async fn serve(script: Vec<PlannedResponse>) -> OpenAiMockServer {
    OpenAiMockServer::serve(script).await.unwrap()
}

/// Wraps one action JSON in the fenced block the loop parses.
fn action_text(value: &serde_json::Value) -> String {
    format!("```json\n{value}\n```")
}

/// The terminating successful finish action.
fn finish_action(success: bool) -> PlannedResponse {
    PlannedResponse::NonStreaming {
        content: action_text(&serde_json::json!({
            "tool": "finish",
            "summary": "task finished",
            "success": success,
        })),
    }
}

#[test]
fn parent_failure_classes_map_budget_to_one_uniform_class() {
    assert_eq!(
        parent_failure_class(FailureClass::TurnBudgetExhausted),
        FailureClass::SubsessionBudgetExhausted
    );
    assert_eq!(
        parent_failure_class(FailureClass::WorkerTimeout),
        FailureClass::SubsessionBudgetExhausted
    );
    assert_eq!(
        parent_failure_class(FailureClass::Stalled),
        FailureClass::SubsessionBudgetExhausted
    );
    assert_eq!(
        parent_failure_class(FailureClass::ProviderError),
        FailureClass::SubsessionFailed
    );
    assert_eq!(
        parent_failure_class(FailureClass::SubsessionDepthExceeded),
        FailureClass::SubsessionDepthExceeded
    );
}

#[test]
fn not_a_subagent_tool_is_a_typed_error() {
    // Synchronous check of the guard: command tools cannot be spawned as
    // sub-sessions.
    let parent = parent(0);
    let def = command_tool_def();
    // run_subsession is async; drive it on a minimal runtime.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap();
    let sim = rt.block_on(serve(vec![]));
    let transport = test_transport(sim.base_url());
    let error = rt
        .block_on(run_subsession(
            &parent,
            &def,
            None,
            &transport,
            ("neuralwatt", "glm-5.3-flash"),
            None,
        ))
        .unwrap_err();
    assert!(matches!(error, SubsessionError::NotSubagent { tool_id } if tool_id == "cmd"));
}

// --- CR remediation regressions (PR #539 review findings) ---------------------

/// Builds the real Neuralwatt transport pointed at the simulator with an
/// explicit key (mirrors `tests/simulated_run.rs`: `build_bootstrap_transport`
/// resolves auth through the keyring/env and is not hermetic).
fn test_transport(base_url: &str) -> orchestraitor_provider_neuralwatt::NeuralwattTransport {
    let config = orchestraitor_provider_neuralwatt::NeuralwattConfig::with_endpoint(
        base_url.to_string(),
        "secret://env/UNUSED".to_string(),
    )
    .unwrap();
    orchestraitor_provider_neuralwatt::NeuralwattTransport::with_key(
        config,
        secrecy::SecretString::from("simulator-test-key"),
    )
    .unwrap()
}

fn explore_tool() -> ToolDefinition {
    ToolDefinition {
        id: "explore-q".to_string(),
        mechanism: ToolMechanism::Subagent {
            role: "explore".to_string(),
            internal_tools: default_internal_tools(),
            instructions: Some("You are a read-only explorer.".to_string()),
        },
        budget: ToolBudget {
            max_turns: 4,
            wall_clock_secs: Some(60),
            max_result_bytes: 8 * 1024,
        },
        visible_to: std::collections::BTreeSet::from(["implement".to_string()]),
    }
}

#[tokio::test]
async fn child_write_file_attempt_under_read_only_allowlist_is_refused_and_effects_free() {
    // Adversarial negative (CR finding #2, CWE-863): a sub-session spawned
    // with the read-only default allowlist attempts write_file. The child's
    // executor must refuse it with the typed reason, and the forbidden
    // effect did NOT happen — no file in the worktree.
    let script = vec![
        PlannedResponse::NonStreaming {
            content: action_text(&serde_json::json!({
                "tool": "write_file",
                "path": "pwned.txt",
                "content": "child escaped the allowlist"
            })),
        },
        finish_action(true),
    ];
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    let mut parent = parent(0);
    parent.worktree_root = worktree.path().to_path_buf();

    let outcome = run_subsession(
        &parent,
        &explore_tool(),
        Some("try to write a file"),
        &transport,
        ("neuralwatt", "glm-5.3-flash"),
        None,
    )
    .await
    .unwrap();

    // The child run itself completes (a refusal is an observation, not a
    // run failure), but the receipt stream shows the typed admission
    // refusal.
    assert_eq!(outcome.status, crate::result::RunStatus::Completed);
    let refused: Vec<_> = outcome
        .receipts
        .iter()
        .filter(|receipt| receipt.outcome == "refused")
        .collect();
    assert!(
        refused
            .iter()
            .any(|receipt| receipt.tool == "write_file"
                && receipt.reason == Some("tool-not-allowed")),
        "write_file under a read-only allowlist must be typed-refused: {:?}",
        outcome.receipts
    );
    // The forbidden effect did NOT happen: the worktree holds no such file.
    assert!(
        !worktree.path().join("pwned.txt").exists(),
        "a refused write must leave the worktree untouched"
    );
    // And nothing was reported as written.
    assert!(outcome.untrusted_writes.is_empty());
}

#[tokio::test]
async fn child_bash_is_refused_unless_allowlisted() {
    // Adversarial negative (CR finding #2): bash is reachable ONLY when the
    // allowlist names it. The explore allowlist (read_file, search) must
    // refuse a bash attempt with the typed reason — never reach a mediator.
    let script = vec![
        PlannedResponse::NonStreaming {
            content: action_text(&serde_json::json!({
                "tool": "bash",
                "script": "echo pwned > pwned.txt"
            })),
        },
        finish_action(true),
    ];
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    let mut parent = parent(0);
    parent.worktree_root = worktree.path().to_path_buf();

    let outcome = run_subsession(
        &parent,
        &explore_tool(),
        Some("try to run bash"),
        &transport,
        ("neuralwatt", "glm-5.3-flash"),
        None,
    )
    .await
    .unwrap();

    let refused: Vec<_> = outcome
        .receipts
        .iter()
        .filter(|receipt| receipt.outcome == "refused")
        .collect();
    assert!(
        refused
            .iter()
            .any(|receipt| receipt.tool == "bash" && receipt.reason == Some("tool-not-allowed")),
        "bash without an allowlist entry must be typed-refused: {:?}",
        outcome.receipts
    );
    assert!(
        !worktree.path().join("pwned.txt").exists(),
        "a refused bash call must have no effect"
    );
}

#[tokio::test]
async fn child_usage_aggregates_into_the_parent_view_via_the_outcome() {
    // CR finding #3: the child's token usage rides the outcome (the parent
    // loop aggregates it into state.usage and re-evaluates the soft cap).
    // Here: the outcome must carry non-zero usage for a real child run.
    let script = vec![
        PlannedResponse::NonStreaming {
            content: action_text(&serde_json::json!({
                "tool": "search", "pattern": "needle"
            })),
        },
        finish_action(true),
    ];
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());

    let outcome = run_subsession(
        &parent(0),
        &explore_tool(),
        Some("find the needle"),
        &transport,
        ("neuralwatt", "glm-5.3-flash"),
        None,
    )
    .await
    .unwrap();

    assert_eq!(outcome.status, crate::result::RunStatus::Completed);
    assert!(
        outcome.usage.output_tokens > 0,
        "child usage must be observable to the parent: {:?}",
        outcome.usage
    );
}

#[tokio::test]
async fn child_summary_is_truncated_by_bytes_not_chars() {
    // CR finding #5 regression: a multibyte summary must respect the
    // tool's max_result_bytes as a BYTE bound (a char cut would yield up
    // to ~3× the bound for 3-byte chars).
    let long_summary = "水".repeat(10_000); // 30_000 bytes, 10_000 chars
    let script = vec![
        PlannedResponse::NonStreaming {
            content: action_text(&serde_json::json!({
                "tool": "search", "pattern": "水水水"
            })),
        },
        PlannedResponse::NonStreaming {
            content: action_text(&serde_json::json!({
                "tool": "finish",
                "summary": long_summary,
                "success": true
            })),
        },
    ];
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());

    let mut tool = explore_tool();
    tool.budget.max_result_bytes = 2_048;
    let outcome = run_subsession(
        &parent(0),
        &tool,
        None,
        &transport,
        ("neuralwatt", "glm-5.3-flash"),
        None,
    )
    .await
    .unwrap();

    let summary = outcome.summary.expect("completed run must carry a summary");
    let marker = "\n[truncated]";
    let body_len = summary.strip_suffix(marker).map_or(summary.len(), str::len);
    assert!(
        body_len <= 2_048,
        "byte cap must hold: body {body_len} bytes > 2048"
    );
    assert!(
        summary.ends_with(marker),
        "a capped summary must carry the truncation marker"
    );
}
