//! Error types for the MCP gateway crate.

use std::path::PathBuf;

use thiserror::Error;

/// Result alias for MCP gateway operations.
pub type McpGatewayResult<T> = Result<T, McpGatewayError>;

/// Errors emitted by project-scoped MCP gateway logic.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpGatewayError {
    /// A filesystem path escaped the current project scope.
    #[error("path is outside the project scope")]
    PathEscapesProject,
    /// A requested project-relative path is invalid.
    #[error("invalid project-relative path `{path}`")]
    InvalidProjectPath {
        /// Caller-supplied path.
        path: String,
    },
    /// A target path already exists.
    #[error("path already exists: {path}")]
    AlreadyExists {
        /// Filesystem path.
        path: PathBuf,
    },
    /// Optimistic concurrency rejected a write.
    #[error("digest mismatch for `{path}`: expected {expected}, actual {actual}")]
    DigestMismatch {
        /// Project-relative path.
        path: String,
        /// Expected digest.
        expected: String,
        /// Actual digest.
        actual: String,
    },
    /// Patch syntax or context did not match the current file.
    #[error("patch could not be applied to `{path}`")]
    PatchRejected {
        /// Project-relative path.
        path: String,
    },
    /// A requested server belongs to a different project.
    #[error("MCP server `{server_id}` is not registered for project `{project_id}`")]
    CrossProjectToolLeak {
        /// Stable project id.
        project_id: String,
        /// Stable server id.
        server_id: String,
    },
    /// Imported server launch is blocked until Arbitraitor inspection grants it.
    #[error("MCP server `{server_id}` requires Arbitraitor inspection before launch")]
    ArbitraitorInspectionRequired {
        /// Stable server id.
        server_id: String,
    },
    /// TOML parsing failed.
    #[error("MCP TOML is invalid")]
    Toml(#[source] Box<toml::de::Error>),
    /// Canonical JSON serialization failed.
    #[error("canonical fingerprint serialization failed: {message}")]
    CanonicalJson {
        /// Redacted serialization failure message.
        message: String,
    },
    /// Filesystem operation failed.
    #[error("filesystem operation failed")]
    Io(#[source] std::io::Error),
    /// The `board.query` decision tool failed (provider transport or event
    /// store). Message text is a static, log-safe label — never board
    /// content (spec `40-arbitraitor-integration.md` §9.23.4).
    #[error("board.query failed: {0}")]
    BoardQuery(String),
    /// The `decision.record` decision tool failed (validation refusal,
    /// store failure, or event-recording failure). Message text is a
    /// static, log-safe label — never record content (§9.23.4).
    #[error("decision.record failed: {0}")]
    DecisionRecord(String),
    /// `decision.record` was called without a session-scoped decision
    /// store. The tool never fabricates persistence: appending into a
    /// per-invocation store and reporting success would violate the
    /// persist+replay guarantee (spec §9.35).
    #[error("decision.record is not configured with a session-scoped decision store")]
    DecisionRecordUnconfigured,
    /// The `board.move` decision tool failed (lease registry or event
    /// store). Message text is a static, log-safe label — never board
    /// content (spec `40-arbitraitor-integration.md` §9.23.4). Guard
    /// REFUSALS are not errors: they are structured outcomes.
    #[error("board.move failed: {0}")]
    BoardMove(String),
}

impl From<std::io::Error> for McpGatewayError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
