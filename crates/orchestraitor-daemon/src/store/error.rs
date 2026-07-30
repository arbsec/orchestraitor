//! Error taxonomy for daemon persistence.

use orchestraitor_core::Retryability;
use thiserror::Error;

/// Result alias for daemon store operations.
pub type StoreResult<T> = Result<T, StoreError>;

/// Failure modes for `SQLite` metadata and filesystem CAS operations.
#[derive(Debug, Error)]
pub enum StoreError {
    /// `SQLite` returned an error while opening, migrating, or querying the store.
    #[error("daemon store SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
    /// Filesystem access failed while managing database parents or CAS blobs.
    #[error("daemon store filesystem operation failed")]
    Io(#[from] std::io::Error),
    /// JSON payload serialization or parsing failed.
    #[error("daemon store JSON payload is invalid")]
    Json(#[from] serde_json::Error),
    /// Normalized event validation or hash-chain verification failed.
    #[error(transparent)]
    Event(#[from] orchestraitor_events::EventError),
    /// Digest text was not a lowercase SHA-256 hex digest.
    #[error("daemon store digest is invalid: {0}")]
    InvalidDigest(String),
    /// Integer conversion would overflow `SQLite`'s signed integer storage.
    #[error("daemon store integer value is outside the supported SQLite range")]
    IntegerRange,
}

impl StoreError {
    /// Returns the retry classification for this error.
    #[must_use]
    pub const fn retryability(&self) -> Retryability {
        match self {
            Self::Sqlite(_)
            | Self::Io(_)
            | Self::Json(_)
            | Self::Event(_)
            | Self::InvalidDigest(_)
            | Self::IntegerRange => Retryability::NotRetriable,
        }
    }
}
