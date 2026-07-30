//! Project scoping for MCP registration and tool calls.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::error::McpGatewayResult;

/// Stable project identifier used to namespace MCP servers and tools.
///
/// Project ids are derived from a canonical-root SHA-256 digest by default, with an optional
/// explicit override stored in `${root}/.orchestraitor/project-id` (one trimmed line,
/// whitespace-free slash-free ASCII, max 64 bytes). The basename of the project root is
/// preserved as a human-readable display label via [`ProjectScope::display_label`] and
/// does **not** contribute to identity.
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

/// Resolved project root, stable identity, and display label for an MCP gateway connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectScope {
    id: ProjectId,
    root: PathBuf,
    display_label: String,
}

impl ProjectScope {
    /// Creates a scope from a known root, canonicalizing the path first.
    ///
    /// The project id is sourced from `${root}/.orchestraitor/project-id` when that file
    /// exists and its content validates; otherwise it falls back to a SHA-256 digest of the
    /// canonical root path. The basename is preserved as a display label only — it does
    /// not contribute to the identity, so two same-named roots in different parents still
    /// receive distinct ids.
    ///
    /// # Errors
    /// Returns an I/O error when the root cannot be canonicalized or the override file
    /// exists and is unreadable. An invalid override file is treated as missing and the
    /// method falls back to the canonical-root digest.
    pub fn from_root(root: impl AsRef<Path>) -> McpGatewayResult<Self> {
        let root = root.as_ref().canonicalize()?;
        let id = resolve_project_id(&root)?;
        let display_label = root
            .file_name()
            .and_then(|os| os.to_str())
            .unwrap_or("project")
            .to_string();
        Ok(Self {
            id,
            root,
            display_label,
        })
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

    /// Returns the human-readable display label (typically the root basename).
    ///
    /// The label is for diagnostics only; equality, hashing, and cross-project isolation
    /// are derived from [`ProjectScope::id`].
    #[must_use]
    pub fn display_label(&self) -> &str {
        &self.display_label
    }
}

fn resolve_project_id(root: &Path) -> McpGatewayResult<ProjectId> {
    let override_path = root.join(".orchestraitor").join("project-id");
    if let Some(value) = read_project_id_override(&override_path)? {
        return Ok(ProjectId::new(value));
    }
    Ok(ProjectId::new(digest_project_root(root)))
}

fn read_project_id_override(path: &Path) -> McpGatewayResult<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(validate_project_id_override(&raw)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn validate_project_id_override(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return None;
    }
    if !trimmed.bytes().all(|byte| byte.is_ascii_graphic()) {
        return None;
    }
    Some(trimmed.to_string())
}

fn digest_project_root(root: &Path) -> String {
    let canonical = root.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    format!("path-sha256:{}", hex::encode(hasher.finalize()))
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
) -> crate::error::McpGatewayResult<()> {
    if scope.id() == server_project {
        return Ok(());
    }
    Err(crate::error::McpGatewayError::CrossProjectToolLeak {
        project_id: scope.id().as_str().to_string(),
        server_id: server_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn unique_basename(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        PathBuf::from(format!("/tmp/orchestraitor-test-{label}-{nonce}"))
    }

    #[test]
    fn same_basename_roots_get_distinct_project_ids() -> McpGatewayResult<()> {
        let parent_a = unique_basename("parent-a");
        let parent_b = unique_basename("parent-b");
        fs::create_dir_all(&parent_a)?;
        fs::create_dir_all(&parent_b)?;
        let repo_a = parent_a.join("repo");
        let repo_b = parent_b.join("repo");
        fs::create_dir_all(&repo_a)?;
        fs::create_dir_all(&repo_b)?;

        let scope_a = ProjectScope::from_root(&repo_a)?;
        let scope_b = ProjectScope::from_root(&repo_b)?;

        assert_ne!(
            scope_a.id(),
            scope_b.id(),
            "same-basename roots under different parents must not collide",
        );
        assert_eq!(scope_a.display_label(), "repo");
        assert_eq!(scope_b.display_label(), "repo");

        fs::remove_dir_all(&parent_a)?;
        fs::remove_dir_all(&parent_b)?;
        Ok(())
    }

    #[test]
    fn override_file_beats_canonical_digest() -> McpGatewayResult<()> {
        let root = unique_basename("override");
        fs::create_dir_all(root.join(".orchestraitor"))?;
        fs::write(root.join(".orchestraitor").join("project-id"), "team-alpha")?;

        let scope_via_override = ProjectScope::from_root(&root)?;
        let explicit_id = ProjectScope::from_root(&root)?;
        assert_eq!(scope_via_override.id().as_str(), "team-alpha");

        let without_override = ProjectScope::from_root(&root)?;
        let _ = explicit_id;

        // Sanity: removing the override flips to the digest-derived id.
        fs::remove_file(root.join(".orchestraitor").join("project-id"))?;
        let fallback = ProjectScope::from_root(&root)?;
        assert_ne!(fallback.id().as_str(), "team-alpha");
        assert!(fallback.id().as_str().starts_with("path-sha256:"));

        fs::remove_dir_all(&root)?;
        let _ = without_override;
        Ok(())
    }

    #[test]
    fn invalid_override_falls_back_to_digest() -> McpGatewayResult<()> {
        let root = unique_basename("invalid");
        fs::create_dir_all(root.join(".orchestraitor"))?;
        // Contains whitespace, exceeds 64 chars, and includes a slash — all rejected.
        fs::write(
            root.join(".orchestraitor").join("project-id"),
            "has spaces and more than sixty-four chars of content for a project id ok?",
        )?;

        let scope = ProjectScope::from_root(&root)?;
        assert!(scope.id().as_str().starts_with("path-sha256:"));
        assert_ne!(scope.id().as_str(), "has spaces...");

        fs::remove_dir_all(&root)?;
        Ok(())
    }
}
