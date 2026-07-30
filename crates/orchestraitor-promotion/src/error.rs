//! Error taxonomy for the promotion pipeline and transaction graph.
//!
//! Errors never contain secrets, headers, cookies, signed URLs, or approval
//! tokens (spec §9.23.4).

use thiserror::Error;

/// Failures produced by classification, promotion, and transaction-graph operations.
#[derive(Debug, Error)]
pub enum PromotionError {
    /// A path could not be classified.
    #[error("promotion classification failed for path `{path}`")]
    Classification {
        /// Repository-relative path that failed classification.
        path: String,
    },
    /// The trusted controller rejected or failed to apply a change.
    #[error("trusted controller failed to apply change for path `{path}`")]
    ApplyFailed {
        /// Repository-relative path that failed to apply.
        path: String,
    },
    /// A transaction-graph node was not found.
    #[error("transaction graph node `{node}` not found")]
    NodeNotFound {
        /// Node identifier that was not found.
        node: String,
    },
    /// A transaction-graph operation was invalid for the current graph state.
    #[error("transaction graph operation invalid: {reason}")]
    InvalidOperation {
        /// Human-readable reason the operation is invalid.
        reason: String,
    },
    /// The audit store rejected an event or failed hash-chain validation.
    #[error("audit store error")]
    Audit(#[source] orchestraitor_events::EventError),
    /// Canonical JSON serialization for a graph event failed.
    #[error("event serialization failed")]
    EventSerialize(#[source] serde_json::Error),
}
