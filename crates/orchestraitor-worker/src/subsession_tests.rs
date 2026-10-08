//! Sub-session runtime tests (issue #535, T3): budget carving, depth
//! enforcement, child-await beats, typed failure mapping — over the
//! deterministic simulator (spec §21.3).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use super::{SubsessionError, SubsessionParent, parent_failure_class, run_subsession};
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
    }
}

fn _unused_explore_tool() -> ToolDefinition {
    ToolDefinition {
        id: "explore-q".to_string(),
        mechanism: ToolMechanism::Subagent {
            role: "explore".to_string(),
            internal_tools: super::default_internal_tools(),
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
    let transport =
        crate::bootstrap::build_bootstrap_transport(Some(sim.base_url().to_string())).unwrap();
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
