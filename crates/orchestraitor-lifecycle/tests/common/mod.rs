#![allow(
    dead_code,
    reason = "integration test helper module is shared across test targets"
)]

use orchestraitor_lifecycle::{IdempotencyKey, TransitionAttempt};
use orchestraitor_model::{OperationId, SessionState};

pub fn op(suffix: &str) -> OperationId {
    OperationId::from_string(format!("op_{suffix}"))
}

pub fn key(suffix: &str) -> IdempotencyKey {
    IdempotencyKey::new(format!("idem_{suffix}"))
}

pub fn attempt(suffix: &str, to: SessionState) -> TransitionAttempt {
    TransitionAttempt {
        operation_id: op(suffix),
        idempotency_key: key(suffix),
        to,
        reason: format!("transition {suffix}"),
    }
}
