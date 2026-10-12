//! Lifecycle checkpoint and event-preservation integration tests.

#![allow(
    clippy::unwrap_used,
    reason = "tests use unwraps for direct Given/When/Then assertions"
)]

mod common;

use std::time::Duration;

use common::{key, op};
use orchestraitor_events::{AuditStore, EventCategory, EventQuery, InMemoryAuditStore};
use orchestraitor_lifecycle::{
    CheckpointCursor, CheckpointPolicy, CheckpointTrigger, EventCursor, PartialResult,
    PartialResultKind, PartialResultStore, ReplayPlan,
};
use orchestraitor_model::SessionState;
use serde_json::json;

#[test]
fn checkpoints_emit_after_tool_calls_or_time_budget_and_enable_replay() {
    // Given: a policy with two tool calls or 30s time budget.
    let policy = CheckpointPolicy::new(2, Duration::from_secs(30));
    let mut cursor = CheckpointCursor::new(100);

    // When: tool calls and time budget are observed.
    let first = cursor.record_tool_call(101, policy);
    let second = cursor.record_tool_call(102, policy);
    let time = cursor.check_time_budget(132, policy);

    // Then: checkpoints trigger on threshold and replay carries completed operations.
    assert_eq!(first, None);
    assert_eq!(second, Some(CheckpointTrigger::ToolCalls));
    assert_eq!(time, Some(CheckpointTrigger::TimeBudget));

    let checkpoint = orchestraitor_lifecycle::Checkpoint {
        checkpoint_id: op("checkpoint"),
        state: SessionState::Running,
        completed_tool_calls: vec![op("tool")],
        model_responses: vec![op("model")],
        partial_patches: vec![op("patch")],
    };
    let replay = ReplayPlan::from_checkpoint(&checkpoint);
    assert_eq!(replay.resume_state, SessionState::Running);
    assert_eq!(replay.completed_tool_calls, vec![op("tool")]);
}

#[test]
fn partial_results_are_preserved_in_event_store() {
    // Given: an in-memory audit store and a partial model response.
    let mut audit = InMemoryAuditStore::default();
    let mut partials = PartialResultStore::new(&mut audit, EventCursor::empty());

    // When: the partial result is preserved.
    let result = partials
        .preserve(PartialResult {
            operation_id: op("model-response"),
            idempotency_key: key("model-response"),
            kind: PartialResultKind::ModelResponse,
            partial: true,
            payload: json!({ "chunk": "hello" }),
        })
        .unwrap();

    // Then: the event store contains a model response metadata event with partial marker.
    assert_eq!(result, op("model-response"));
    let records = audit
        .query(&EventQuery {
            category: Some(EventCategory::ModelResponseMetadata),
            since_seq: None,
            until_seq: None,
            include_uninterpreted: false,
        })
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].envelope.payload["partial"], true);
}
