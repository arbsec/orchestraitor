//! Lifecycle recovery and lease integration tests.

mod common;

use std::time::Duration;

use orchestraitor_lifecycle::{Lease, LeaseTtl, LifecycleConfig, LifecycleRecord};
use orchestraitor_lifecycle::{RecoveryAction, RecoveryDecision};
use orchestraitor_model::SessionState;

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
