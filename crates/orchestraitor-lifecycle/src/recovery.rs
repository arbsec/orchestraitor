//! Restart and orphan recovery policy.

use std::time::Duration;

use orchestraitor_model::SessionState;

use crate::{Lease, LifecycleRecord};

const DEFAULT_ORPHAN_RECOVERY_SECS: u64 = 30;
const DEFAULT_LEASE_TTL_SECS: u64 = 60 * 60;
const DEFAULT_CANCEL_GRACE_SECS: u64 = 10;
const DEFAULT_CHECKPOINT_TOOL_CALLS: u32 = 10;
const DEFAULT_CHECKPOINT_INTERVAL_SECS: u64 = 300;

/// Runtime lifecycle knobs with bounded defaults from spec §9.24.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleConfig {
    /// How quickly the controller marks missing running workers as orphaned.
    pub orphan_recovery_after: Duration,
    /// Default lease TTL. Expiry transitions to `orphaned`, not `failed`.
    pub default_lease_ttl: Duration,
    /// Bounded grace period for cancellation propagation.
    pub cancellation_grace_period: Duration,
    /// Tool-call interval for checkpoint emission.
    pub checkpoint_after_tool_calls: u32,
    /// Time interval for checkpoint emission.
    pub checkpoint_after: Duration,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            orphan_recovery_after: Duration::from_secs(DEFAULT_ORPHAN_RECOVERY_SECS),
            default_lease_ttl: Duration::from_secs(DEFAULT_LEASE_TTL_SECS),
            cancellation_grace_period: Duration::from_secs(DEFAULT_CANCEL_GRACE_SECS),
            checkpoint_after_tool_calls: DEFAULT_CHECKPOINT_TOOL_CALLS,
            checkpoint_after: Duration::from_secs(DEFAULT_CHECKPOINT_INTERVAL_SECS),
        }
    }
}

/// Recovery action chosen for one durable lifecycle record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// State remains unchanged across restart.
    Keep,
    /// Running worker disappeared; mark task orphaned.
    MarkOrphaned,
    /// Lease expired; mark task orphaned for user extension/recovery.
    LeaseExpired,
}

/// Recovery result for one task/session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryDecision {
    /// State observed before recovery.
    pub before: SessionState,
    /// State after recovery.
    pub after: SessionState,
    /// Recovery action selected.
    pub action: RecoveryAction,
}

impl RecoveryDecision {
    /// Computes restart recovery for a durable state.
    #[must_use]
    pub const fn after_restart(record: &LifecycleRecord) -> Self {
        match record.state {
            SessionState::Running => Self {
                before: record.state,
                after: SessionState::Orphaned,
                action: RecoveryAction::MarkOrphaned,
            },
            SessionState::Queued
            | SessionState::InputRequired
            | SessionState::ApprovalRequired
            | SessionState::AuthenticationRequired
            | SessionState::Paused
            | SessionState::Completed
            | SessionState::Failed
            | SessionState::Cancelled
            | SessionState::Rejected
            | SessionState::Orphaned => Self {
                before: record.state,
                after: record.state,
                action: RecoveryAction::Keep,
            },
        }
    }

    /// Computes lease-expiry recovery for a state and lease.
    #[must_use]
    pub const fn after_lease_check(
        record: &LifecycleRecord,
        lease: Lease,
        now_unix_secs: u64,
    ) -> Self {
        if lease.is_expired_at(now_unix_secs) && lease_expiry_orphans(record.state) {
            return Self {
                before: record.state,
                after: SessionState::Orphaned,
                action: RecoveryAction::LeaseExpired,
            };
        }
        Self {
            before: record.state,
            after: record.state,
            action: RecoveryAction::Keep,
        }
    }

    /// Returns true once an orphan has waited long enough for policy recovery.
    #[must_use]
    pub fn orphan_recovery_due(orphaned_for: Duration, config: &LifecycleConfig) -> bool {
        orphaned_for >= config.orphan_recovery_after
    }
}

const fn lease_expiry_orphans(state: SessionState) -> bool {
    match state {
        SessionState::Queued
        | SessionState::Running
        | SessionState::InputRequired
        | SessionState::ApprovalRequired
        | SessionState::AuthenticationRequired
        | SessionState::Paused
        | SessionState::Orphaned => true,
        SessionState::Completed
        | SessionState::Failed
        | SessionState::Cancelled
        | SessionState::Rejected => false,
    }
}
