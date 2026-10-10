//! Lifecycle error types.

use orchestraitor_events::EventError;
use orchestraitor_model::SessionState;
use thiserror::Error;

use crate::state_machine::IdempotencyKey;

/// Errors produced by lifecycle orchestration logic.
#[derive(Debug, Error)]
pub enum LifecycleError {
    /// A requested transition is not legal for the current state.
    #[error("invalid lifecycle transition from {from:?} to {to:?}: {reason}")]
    InvalidTransition {
        /// State before the attempted transition.
        from: SessionState,
        /// Requested target state.
        to: SessionState,
        /// Clear human-readable reason.
        reason: &'static str,
    },
    /// An idempotency key was replayed with different operation data.
    #[error("idempotency key `{key}` was reused for a different lifecycle operation")]
    IdempotencyConflict {
        /// Stable idempotency key that conflicted.
        key: IdempotencyKey,
    },
    /// Event-store append failed while preserving partial results.
    #[error("event store rejected lifecycle event")]
    EventStore(#[from] EventError),
    /// The system clock could not produce a timestamp usable in an event.
    #[error("system clock is before the Unix epoch")]
    ClockBeforeUnixEpoch,
}
