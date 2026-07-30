//! Error types for event validation and audit persistence.

use thiserror::Error;

/// Failures produced by event normalization, hash-chain validation, and import/export.
#[derive(Debug, Error)]
pub enum EventError {
    /// Canonical JSON serialization failed.
    #[error("event canonicalization failed")]
    CanonicalJson(#[source] serde_json::Error),
    /// JSON parsing or serialization failed.
    #[error("event JSON document is invalid")]
    Json(#[source] serde_json::Error),
    /// A sensitive field name was rejected before it could enter the audit log.
    #[error("event field `{field}` is sensitive and must be redacted at source")]
    SensitiveField {
        /// Sensitive field name that was rejected.
        field: String,
    },
    /// The event sequence was not contiguous.
    #[error("event sequence gap: expected {expected}, observed {observed}")]
    SequenceGap {
        /// Expected monotonic sequence number.
        expected: u64,
        /// Observed monotonic sequence number.
        observed: u64,
    },
    /// The previous-record hash pointer does not match the hash-chain state.
    #[error("event previous hash mismatch at sequence {sequence}")]
    PreviousHashMismatch {
        /// Event sequence whose `prev_hash` did not match.
        sequence: u64,
    },
    /// The stored record hash does not match canonical bytes for the envelope.
    #[error("event record hash mismatch at sequence {sequence}")]
    RecordHashMismatch {
        /// Event sequence whose record hash was invalid.
        sequence: u64,
    },
}

impl From<serde_json::Error> for EventError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
