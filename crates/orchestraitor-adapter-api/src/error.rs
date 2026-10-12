//! Adapter API error types.

use orchestraitor_model::SessionId;
use thiserror::Error;

/// Convenience result type for adapter operations.
pub type AdapterResult<T> = Result<T, AdapterError>;

/// Adapter boundary failures.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// Adapter is unavailable in the requested environment.
    #[error("adapter unavailable: {reason}")]
    Unavailable {
        /// Non-secret reason.
        reason: String,
    },
    /// Adapter session does not exist or no longer matches the supplied handle.
    #[error("adapter session not found: {session_id}")]
    SessionMissing {
        /// Missing session id.
        session_id: SessionId,
    },
    /// Adapter operation failed without exposing secrets.
    #[error("adapter operation `{operation}` failed: {message}")]
    OperationFailed {
        /// Operation name.
        operation: &'static str,
        /// Redacted message.
        message: String,
    },
}
