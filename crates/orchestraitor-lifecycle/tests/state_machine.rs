//! Lifecycle state-machine integration tests.

#![allow(
    clippy::unwrap_used,
    reason = "tests use unwraps for direct Given/When/Then assertions"
)]

mod common;

use common::{attempt, key, op};
use orchestraitor_lifecycle::{LifecycleError, StateMachine, TransitionAttempt};
use orchestraitor_model::SessionState;
use serde_json::json;

#[test]
fn covers_all_eleven_lifecycle_states_when_serialized_from_model() {
    // Given: spec §9.24.1's full state list as the model-owned enum.
    let states = [
        SessionState::Queued,
        SessionState::Running,
        SessionState::InputRequired,
        SessionState::ApprovalRequired,
        SessionState::AuthenticationRequired,
        SessionState::Paused,
        SessionState::Completed,
        SessionState::Failed,
        SessionState::Cancelled,
        SessionState::Rejected,
        SessionState::Orphaned,
    ];

    // When: states are serialized for durable lifecycle records.
    let rendered = serde_json::to_value(states).unwrap();

    // Then: every spec spelling is present exactly once.
    assert_eq!(
        rendered,
        json!([
            "queued",
            "running",
            "input-required",
            "approval-required",
            "authentication-required",
            "paused",
            "completed",
            "failed",
            "cancelled",
            "rejected",
            "orphaned"
        ])
    );
}

#[test]
fn validates_allowed_transitions_and_rejects_invalid_terminal_transition() {
    // Given: a queued task state machine.
    let mut machine = StateMachine::queued();

    // When: the task runs and completes.
    machine
        .transition(attempt("run", SessionState::Running))
        .unwrap();
    machine
        .transition(attempt("complete", SessionState::Completed))
        .unwrap();
    let result = machine.transition(attempt("rerun", SessionState::Running));

    // Then: terminal states reject future transitions with a clear error.
    assert!(matches!(
        result,
        Err(LifecycleError::InvalidTransition {
            from: SessionState::Completed,
            to: SessionState::Running,
            ..
        })
    ));
}

#[test]
fn replays_same_idempotency_key_as_noop_and_conflicts_on_different_operation() {
    // Given: a running transition with a stable idempotency key.
    let mut machine = StateMachine::queued();
    let first = attempt("run", SessionState::Running);
    let replay = first.clone();

    // When: the same operation is replayed and then the key is reused differently.
    machine.transition(first).unwrap();
    machine.transition(replay).unwrap();
    let conflict = machine
        .transition(TransitionAttempt {
            operation_id: op("different"),
            idempotency_key: key("run"),
            to: SessionState::Paused,
            reason: String::from("different transition"),
        })
        .is_err();

    // Then: exact replay is a no-op and conflicting reuse is rejected.
    assert_eq!(machine.record().transitions.len(), 1);
    assert!(conflict);
}
