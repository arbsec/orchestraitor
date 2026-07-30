//! Project scoping for MCP registration and tool calls.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{McpGatewayError, McpGatewayResult};

/// Stable project identifier used to namespace MCP servers and tools.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ProjectId(String);

impl ProjectId {
    /// Creates a project id from a caller-supplied stable string.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the project id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Resolved project root and id for an MCP gateway connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectScope {
    id: ProjectId,
    root: PathBuf,
}

impl ProjectScope {
    /// Creates a scope from a known root.
    ///
    /// # Errors
    /// Returns an I/O error when the root cannot be canonicalized.
    pub fn from_root(root: impl AsRef<Path>) -> McpGatewayResult<Self> {
        let root = root.as_ref().canonicalize()?;
        let id = root.file_name().and_then(OsStr::to_str).map_or_else(
            || ProjectId::new(root.display().to_string()),
            ProjectId::new,
        );
        Ok(Self { id, root })
    }

    /// Resolves the nearest `.orchestraitor/` owner or git root from `cwd`.
    ///
    /// # Errors
    /// Returns an I/O error when `cwd` cannot be canonicalized.
    pub fn auto_detect(cwd: impl AsRef<Path>) -> McpGatewayResult<Self> {
        let start = cwd.as_ref().canonicalize()?;
        let root = nearest_marker_root(&start).unwrap_or(start);
        Self::from_root(root)
    }

    /// Returns the stable project id.
    #[must_use]
    pub const fn id(&self) -> &ProjectId {
        &self.id
    }

    /// Returns the project root path.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn nearest_marker_root(start: &Path) -> Option<PathBuf> {
    for candidate in start.ancestors() {
        if candidate.join(".orchestraitor").is_dir() || candidate.join(".git").exists() {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

pub(crate) fn require_server_project(
    scope: &ProjectScope,
    server_project: &ProjectId,
    server_id: &str,
) -> McpGatewayResult<()> {
    if scope.id() == server_project {
        return Ok(());
    }
    Err(McpGatewayError::CrossProjectToolLeak {
        project_id: scope.id().as_str().to_string(),
        server_id: server_id.to_string(),
    })
}
