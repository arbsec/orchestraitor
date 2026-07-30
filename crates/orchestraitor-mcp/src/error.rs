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
}

impl From<std::io::Error> for McpGatewayError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
