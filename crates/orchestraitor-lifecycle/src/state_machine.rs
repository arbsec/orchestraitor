//! Replay-safe lifecycle state machine.

use std::collections::HashMap;

use orchestraitor_model::{OperationId, SessionState};
use serde::{Deserialize, Serialize};

use crate::LifecycleError;

/// Stable idempotency key attached to every lifecycle transition (spec §9.24.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Wraps a caller-provided stable idempotency key.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// Returns the underlying key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for IdempotencyKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Input for one lifecycle transition attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionAttempt {
    /// Stable operation id for correlation across stores and retries.
    pub operation_id: OperationId,
    /// Stable idempotency key for replay-safe transition deduplication.
    pub idempotency_key: IdempotencyKey,
    /// Requested target state.
    pub to: SessionState,
    /// Non-secret reason for the transition.
    pub reason: String,
}

/// Durable lifecycle transition record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionRecord {
    /// Operation id supplied by the caller.
    pub operation_id: OperationId,
    /// Idempotency key supplied by the caller.
    pub idempotency_key: IdempotencyKey,
    /// Previous state.
    pub from: SessionState,
    /// New state.
    pub to: SessionState,
    /// Non-secret reason for audit display.
    pub reason: String,
}

/// Durable lifecycle state plus replay metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleRecord {
    /// Current lifecycle state.
    pub state: SessionState,
    /// Transition history in applied order.
    pub transitions: Vec<TransitionRecord>,
}

impl LifecycleRecord {
    /// Creates a lifecycle record in the given initial state.
    #[must_use]
    pub fn new(state: SessionState) -> Self {
        Self {
            state,
            transitions: Vec::new(),
        }
    }
}

/// Validates lifecycle transitions and makes replayed operations no-ops.
#[derive(Debug, Clone)]
pub struct StateMachine {
    record: LifecycleRecord,
    applied: HashMap<IdempotencyKey, TransitionRecord>,
}

impl StateMachine {
    /// Starts a state machine from a durable record.
    #[must_use]
    pub fn from_record(record: LifecycleRecord) -> Self {
        let applied = record
            .transitions
            .iter()
            .map(|transition| (transition.idempotency_key.clone(), transition.clone()))
            .collect();
        Self { record, applied }
    }

    /// Starts a state machine in `queued`.
    #[must_use]
    pub fn queued() -> Self {
        Self::from_record(LifecycleRecord::new(SessionState::Queued))
    }

    /// Returns the current state.
    #[must_use]
    pub const fn state(&self) -> SessionState {
        self.record.state
    }

    /// Returns the durable record.
    #[must_use]
    pub const fn record(&self) -> &LifecycleRecord {
        &self.record
    }

    /// Applies a transition or returns the prior record when the idempotency key is replayed.
    ///
    /// # Errors
    /// Returns [`LifecycleError::InvalidTransition`] for disallowed edges and
    /// [`LifecycleError::IdempotencyConflict`] when a key is reused for different data.
    pub fn transition(
        &mut self,
        attempt: TransitionAttempt,
    ) -> Result<&TransitionRecord, LifecycleError> {
        if self.applied.contains_key(&attempt.idempotency_key) {
            return self.replay(attempt);
        }
        let from = self.record.state;
        validate_transition(from, attempt.to)?;
        let record = TransitionRecord {
            operation_id: attempt.operation_id,
            idempotency_key: attempt.idempotency_key,
            from,
            to: attempt.to,
            reason: attempt.reason,
        };
        self.record.state = record.to;
        self.record.transitions.push(record.clone());
        self.applied.insert(record.idempotency_key.clone(), record);
        let index = self.record.transitions.len().saturating_sub(1);
        Ok(&self.record.transitions[index])
    }

    fn replay(&self, attempt: TransitionAttempt) -> Result<&TransitionRecord, LifecycleError> {
        let Some(record) = self.applied.get(&attempt.idempotency_key) else {
            return Err(LifecycleError::IdempotencyConflict {
                key: attempt.idempotency_key,
            });
        };
        if record.operation_id == attempt.operation_id
            && record.to == attempt.to
            && record.reason == attempt.reason
        {
            return Ok(record);
        }
        Err(LifecycleError::IdempotencyConflict {
            key: attempt.idempotency_key,
        })
    }
}

fn validate_transition(from: SessionState, to: SessionState) -> Result<(), LifecycleError> {
    if transition_allowed(from, to) {
        return Ok(());
    }
    Err(LifecycleError::InvalidTransition {
        from,
        to,
        reason: invalid_reason(from),
    })
}

fn invalid_reason(from: SessionState) -> &'static str {
    match from {
        SessionState::Queued => {
            "queued tasks may run, pause, cancel, or be rejected before execution"
        }
        SessionState::Running => "running tasks may pause, wait, complete, fail, cancel, or orphan",
        SessionState::InputRequired
        | SessionState::ApprovalRequired
        | SessionState::AuthenticationRequired
        | SessionState::Paused => "waiting tasks may resume, cancel, or be rejected",
        SessionState::Orphaned => "orphaned tasks may resume, pause for reconnect, fail, or cancel",
        SessionState::Completed
        | SessionState::Failed
        | SessionState::Cancelled
        | SessionState::Rejected => "terminal states cannot transition",
    }
}

const fn transition_allowed(from: SessionState, to: SessionState) -> bool {
    match from {
        SessionState::Queued => matches!(
            to,
            SessionState::Running
                | SessionState::Paused
                | SessionState::Cancelled
                | SessionState::Rejected
        ),
        SessionState::Running => matches!(
            to,
            SessionState::InputRequired
                | SessionState::ApprovalRequired
                | SessionState::AuthenticationRequired
                | SessionState::Paused
                | SessionState::Completed
                | SessionState::Failed
                | SessionState::Cancelled
                | SessionState::Orphaned
        ),
        SessionState::InputRequired
        | SessionState::ApprovalRequired
        | SessionState::AuthenticationRequired
        | SessionState::Paused => matches!(
            to,
            SessionState::Running | SessionState::Cancelled | SessionState::Rejected
        ),
        SessionState::Orphaned => matches!(
            to,
            SessionState::Running
                | SessionState::Paused
                | SessionState::Failed
                | SessionState::Cancelled
        ),
        SessionState::Completed
        | SessionState::Failed
        | SessionState::Cancelled
        | SessionState::Rejected => false,
    }
}
