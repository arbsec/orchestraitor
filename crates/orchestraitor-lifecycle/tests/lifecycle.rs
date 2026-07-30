//! Lifecycle integration tests.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::expect_used,
    reason = "tests use direct assertions and unwraps for focused Given/When/Then checks"
)]

use std::time::Duration;

use orchestraitor_events::{AuditStore, EventCategory, EventQuery, InMemoryAuditStore};
use orchestraitor_lifecycle::{
    CancellationController, CancellationOutcome, CheckpointCursor, CheckpointPolicy,
    CheckpointTrigger, CleanupReport, EventCursor, IdempotencyKey, Lease, LeaseTtl,
    LifecycleConfig, LifecycleError, LifecycleRecord, PartialResult, PartialResultKind,
    PartialResultStore, RecoveryAction, RecoveryDecision, ReplayPlan, ResourceId, StateMachine,
    TransitionAttempt,
};
use orchestraitor_model::{OperationId, SessionState};
use serde_json::json;

fn op(suffix: &str) -> OperationId {
    OperationId::from_string(format!("op_{suffix}"))
}

fn key(suffix: &str) -> IdempotencyKey {
    IdempotencyKey::new(format!("idem_{suffix}"))
}

fn attempt(suffix: &str, to: SessionState) -> TransitionAttempt {
    TransitionAttempt {
        operation_id: op(suffix),
        idempotency_key: key(suffix),
        to,
        reason: format!("transition {suffix}"),
    }
}

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

#[test]
fn restart_recovery_keeps_waiting_states_and_marks_running_orphaned() {
    // Given: durable states observed after daemon kill -9 and restart.
    let paused = LifecycleRecord::new(SessionState::Paused);
    let approval = LifecycleRecord::new(SessionState::ApprovalRequired);
    let input = LifecycleRecord::new(SessionState::InputRequired);
    let running = LifecycleRecord::new(SessionState::Running);

    // When: restart recovery runs.
    let paused_decision = RecoveryDecision::after_restart(&paused);
    let approval_decision = RecoveryDecision::after_restart(&approval);
    let input_decision = RecoveryDecision::after_restart(&input);
    let running_decision = RecoveryDecision::after_restart(&running);

    // Then: paused/input/approval stay; running becomes orphaned.
    assert_eq!(paused_decision.after, SessionState::Paused);
    assert_eq!(approval_decision.after, SessionState::ApprovalRequired);
    assert_eq!(input_decision.after, SessionState::InputRequired);
    assert_eq!(running_decision.after, SessionState::Orphaned);
    assert_eq!(running_decision.action, RecoveryAction::MarkOrphaned);
}

#[test]
fn orphan_recovery_timer_defaults_to_thirty_seconds() {
    // Given: default lifecycle recovery config.
    let config = LifecycleConfig::default();

    // When: an orphan has been pending for 29s and then 30s.
    let before = RecoveryDecision::orphan_recovery_due(Duration::from_secs(29), &config);
    let due = RecoveryDecision::orphan_recovery_due(Duration::from_secs(30), &config);

    // Then: policy recovery becomes due within the required 30s default.
    assert!(!before);
    assert!(due);
}

#[test]
fn lease_expiry_marks_active_task_orphaned_not_failed() {
    // Given: a running task with a 10s lease.
    let record = LifecycleRecord::new(SessionState::Running);
    let lease = Lease::with_ttl(100, LeaseTtl::from_duration(Duration::from_secs(10)));

    // When: lease expiry is checked at the expiry boundary.
    let decision = RecoveryDecision::after_lease_check(&record, lease, 110);

    // Then: expiry routes to orphaned for extension/recovery, not failed.
    assert_eq!(decision.after, SessionState::Orphaned);
    assert_eq!(decision.action, RecoveryAction::LeaseExpired);
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_releases_resources_within_bounded_grace() {
    // Given: a cancellation controller and worker token.
    let controller = CancellationController::new(Duration::from_millis(50));
    let mut token = controller.token();
    let worker = tokio::spawn(async move {
        token.cancelled().await;
        CleanupReport {
            released: vec![ResourceId::new("process:1")],
            unreleased: Vec::new(),
        }
    });

    // When: cancellation is requested and cleanup completes.
    let (outcome, report) = controller
        .cancel_with_cleanup(vec![ResourceId::new("process:1")], async move {
            worker.await.unwrap()
        })
        .await;

    // Then: resources are released inside grace with no unreleased set.
    assert_eq!(outcome, CancellationOutcome::Released);
    assert!(report.completed_within_grace);
    assert!(report.cleanup.unreleased.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_records_unreleased_set_after_grace_expiry() {
    // Given: cleanup that cannot finish within grace.
    let controller = CancellationController::new(Duration::from_millis(10));
    let resource = ResourceId::new("socket:blocked");

    // When: cancellation times out.
    let (outcome, report) = controller
        .cancel_with_cleanup(vec![resource.clone()], async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            CleanupReport::default()
        })
        .await;

    // Then: unreleased resources are preserved for audit visibility.
    assert_eq!(outcome, CancellationOutcome::GraceExpired);
    assert!(!report.completed_within_grace);
    assert_eq!(report.cleanup.unreleased, vec![resource]);
}

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
