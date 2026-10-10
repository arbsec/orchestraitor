//! Error types for cost-ledger operations.

use orchestraitor_core::Retryability;
use thiserror::Error;

/// Result alias for cost-ledger operations.
pub type LedgerResult<T> = Result<T, LedgerError>;

/// Cost-ledger failure taxonomy.
#[derive(Debug, Error)]
pub enum LedgerError {
    /// `SQLite` storage failed.
    #[error("cost ledger SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
    /// Stored timestamp text was not a valid RFC 3339 timestamp.
    #[error("cost ledger timestamp is invalid")]
    Timestamp(#[from] chrono::ParseError),
    /// Integer conversion would overflow the `SQLite` storage type.
    #[error("cost ledger integer value is outside the supported SQLite range")]
    IntegerRange,
    /// Stored enum text was not recognized by this crate version.
    #[error("cost ledger stored value is invalid: {0}")]
    InvalidStoredValue(String),
    /// Numeric text failed to parse.
    #[error("cost ledger numeric value is invalid")]
    Float(#[from] std::num::ParseFloatError),
}

impl LedgerError {
    /// Returns the retry classification for this error.
    #[must_use]
    pub const fn retryability(&self) -> Retryability {
        match self {
            Self::Sqlite(_)
            | Self::Timestamp(_)
            | Self::IntegerRange
            | Self::InvalidStoredValue(_)
            | Self::Float(_) => Retryability::NotRetriable,
        }
    }
}
