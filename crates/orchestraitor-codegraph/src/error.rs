//! Codegraph error type: typed, log-safe, no index content in messages.

use thiserror::Error;

/// Errors from the codegraph server and persistence layer.
#[derive(Debug, Error)]
pub enum CodegraphError {
    /// The index file is missing or unreadable.
    #[error("codegraph index not found at {path}: run `orc codegraph index` first")]
    IndexMissing {
        /// Path that was attempted.
        path: String,
    },
    /// The index file failed to parse.
    #[error("codegraph index at {path} is corrupt: {message}")]
    IndexCorrupt {
        /// Path that failed to parse.
        path: String,
        /// Underlying serialization error message.
        message: String,
    },
    /// A queried symbol id is absent from the index.
    #[error("symbol not found: {id}")]
    SymbolNotFound {
        /// The missing symbol id.
        id: String,
    },
    /// The MCP transport failed.
    #[error("codegraph server transport failed: {0}")]
    Transport(String),
    /// An index persistence or query operation failed.
    #[error(transparent)]
    Context(#[from] orchestraitor_context::ContextError),
}
