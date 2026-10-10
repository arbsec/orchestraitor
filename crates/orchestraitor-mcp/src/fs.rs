//! Project-scoped built-in filesystem tools.

use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::drift::sha256_digest;
use crate::error::{McpGatewayError, McpGatewayResult};
use crate::fs_types::{
    ApplyPatchRequest, FileDigest, ListResult, NormalizationResult, ProjectPath, ReadResult,
    RemoveResult, RenameResult, SearchResult, StatResult, WriteResult,
};
use crate::patch::apply_unified_patch;
use crate::project::ProjectScope;

/// Built-in filesystem tool implementation bound to a project scope.
#[derive(Debug, Clone)]
pub struct FileSystemTools {
    scope: ProjectScope,
}

impl FileSystemTools {
    /// Creates filesystem tools for a project.
    #[must_use]
    pub const fn new(scope: ProjectScope) -> Self {
        Self { scope }
    }

    /// Reads a UTF-8 file and returns content with its digest.
    ///
    /// # Errors
    /// Returns an error when the path is invalid or I/O fails.
    pub fn read(&self, path: &str) -> McpGatewayResult<ReadResult> {
        let resolved = self.resolve_existing_file(path)?;
        let content = fs::read_to_string(resolved)?;
        let digest = digest_text(&content);
        Ok(ReadResult { content, digest })
    }

    /// Returns metadata for a path.
    ///
    /// # Errors
    /// Returns an error when the path is invalid or I/O fails.
    pub fn stat(&self, path: &str) -> McpGatewayResult<StatResult> {
        let resolved = self.resolve_existing(path)?;
        let metadata = fs::metadata(resolved)?;
        Ok(StatResult {
            is_file: metadata.is_file(),
            is_dir: metadata.is_dir(),
            len: metadata.len(),
        })
    }

    /// Lists direct children of a directory.
    ///
    /// # Errors
    /// Returns an error when the path is invalid or I/O fails.
    pub fn list(&self, path: &str) -> McpGatewayResult<ListResult> {
        let resolved = self.resolve_existing(path)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(resolved)? {
            entries.push(entry?.file_name().to_string_lossy().into_owned());
        }
        entries.sort();
        Ok(ListResult { entries })
    }

    /// Searches UTF-8 files under a directory for a literal query.
    ///
    /// # Errors
    /// Returns an error when the path is invalid or I/O fails.
    pub fn search(&self, path: &str, query: &str) -> McpGatewayResult<SearchResult> {
        let root = self.resolve_existing(path)?;
        let mut matches = Vec::new();
        collect_literal_matches(&root, query, &mut matches)?;
        Ok(SearchResult { matches })
    }

    /// Applies a unified patch if the expected digest matches current content.
    ///
    /// # Errors
    /// Returns an error when optimistic concurrency fails, patch context does not match, or I/O fails.
    pub fn apply_patch(&self, request: &ApplyPatchRequest) -> McpGatewayResult<WriteResult> {
        let resolved = self.resolve_existing_file(&request.path)?;
        let original = fs::read_to_string(&resolved)?;
        let actual = digest_text(&original);
        if actual.as_str() != request.expected_digest {
            return Err(McpGatewayError::DigestMismatch {
                path: request.path.clone(),
                expected: request.expected_digest.clone(),
                actual: actual.as_str().to_string(),
            });
        }
        let patched = apply_unified_patch(&original, &request.patch).ok_or_else(|| {
            McpGatewayError::PatchRejected {
                path: request.path.clone(),
            }
        })?;
        fs::write(&resolved, patched.as_bytes())?;
        Ok(WriteResult {
            path: request.path.clone(),
            status: "written".to_string(),
            digest: digest_text(&patched),
            normalization: NormalizationResult::default(),
            secondary_changes: Vec::new(),
            diagnostics: Vec::new(),
        })
    }

    /// Creates a UTF-8 file.
    ///
    /// # Errors
    /// Returns an error when the path is invalid, already exists, or I/O fails.
    pub fn create(&self, path: &str, content: &str) -> McpGatewayResult<WriteResult> {
        let relative = ProjectPath::parse(path)?;
        let resolved = self.resolve_new_path(&relative)?;
        if resolved.exists() {
            return Err(McpGatewayError::AlreadyExists { path: resolved });
        }
        if let Some(parent) = resolved.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&resolved, content.as_bytes())?;
        Ok(WriteResult::created(path, digest_text(content)))
    }

    /// Moves a project-scoped path.
    ///
    /// # Errors
    /// Returns an error when either path is invalid or I/O fails.
    pub fn rename(&self, from: &str, to: &str) -> McpGatewayResult<RenameResult> {
        let from_path = self.resolve_existing(from)?;
        let to_path = self.resolve_new_path(&ProjectPath::parse(to)?)?;
        if let Some(parent) = to_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(from_path, to_path)?;
        Ok(RenameResult {
            from: from.to_string(),
            to: to.to_string(),
            status: "renamed".to_string(),
        })
    }

    /// Deletes a project-scoped file or directory tree.
    ///
    /// # Errors
    /// Returns an error when the path is invalid or I/O fails.
    pub fn remove(&self, path: &str) -> McpGatewayResult<RemoveResult> {
        let resolved = self.resolve_existing(path)?;
        let metadata = fs::metadata(&resolved)?;
        if metadata.is_dir() {
            fs::remove_dir_all(resolved)?;
        } else {
            fs::remove_file(resolved)?;
        }
        Ok(RemoveResult {
            path: path.to_string(),
            status: "removed".to_string(),
        })
    }

    fn resolve_existing(&self, path: &str) -> McpGatewayResult<PathBuf> {
        let relative = ProjectPath::parse(path)?;
        let resolved = self.scope.root().join(relative.as_str()).canonicalize()?;
        ensure_in_scope(self.scope.root(), &resolved)?;
        Ok(resolved)
    }

    fn resolve_existing_file(&self, path: &str) -> McpGatewayResult<PathBuf> {
        let resolved = self.resolve_existing(path)?;
        if resolved.is_file() {
            Ok(resolved)
        } else {
            Err(McpGatewayError::InvalidProjectPath {
                path: path.to_string(),
            })
        }
    }

    fn resolve_new_path(&self, relative: &ProjectPath) -> McpGatewayResult<PathBuf> {
        let resolved = self.scope.root().join(relative.as_str());
        let parent = resolved.parent().unwrap_or_else(|| self.scope.root());
        let canonical_parent = if parent.exists() {
            parent.canonicalize()?
        } else {
            self.scope.root().to_path_buf()
        };
        ensure_in_scope(self.scope.root(), &canonical_parent)?;
        Ok(resolved)
    }
}

impl ProjectPath {
    /// Creates a project-relative path after rejecting absolute and parent components.
    ///
    /// # Errors
    /// Returns an error when the path escapes the project root.
    pub fn parse(value: impl Into<String>) -> McpGatewayResult<Self> {
        let value = value.into();
        let path = Path::new(&value);
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(McpGatewayError::InvalidProjectPath { path: value });
        }
        Ok(Self(value))
    }
}

fn digest_text(content: &str) -> FileDigest {
    FileDigest::new(sha256_digest(content.as_bytes()).as_str().to_string())
}

fn ensure_in_scope(root: &Path, resolved: &Path) -> McpGatewayResult<()> {
    if resolved.starts_with(root) {
        Ok(())
    } else {
        Err(McpGatewayError::PathEscapesProject)
    }
}

fn collect_literal_matches(
    root: &Path,
    query: &str,
    matches: &mut Vec<String>,
) -> McpGatewayResult<()> {
    if root.is_file() {
        if fs::read_to_string(root).is_ok_and(|content| content.contains(query)) {
            matches.push(root.display().to_string());
        }
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_literal_matches(&path, query, matches)?;
        } else if fs::read_to_string(&path).is_ok_and(|content| content.contains(query)) {
            matches.push(path.display().to_string());
        }
    }
    matches.sort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_patch_digest_mismatch_fails() -> McpGatewayResult<()> {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("a.txt"), b"one\n")?;
        let scope = ProjectScope::from_root(temp.path())?;
        let tools = FileSystemTools::new(scope);
        let request = ApplyPatchRequest {
            path: "a.txt".to_string(),
            expected_digest: "sha256:bad".to_string(),
            patch: "@@ -1 +1 @@\n-one\n+two\n".to_string(),
        };
        let error = tools.apply_patch(&request).err();
        assert!(matches!(
            error,
            Some(McpGatewayError::DigestMismatch { .. })
        ));
        assert_eq!(fs::read_to_string(temp.path().join("a.txt"))?, "one\n");
        Ok(())
    }

    #[test]
    fn apply_patch_writes_when_digest_matches() -> McpGatewayResult<()> {
        let temp = tempfile::tempdir()?;
        fs::write(temp.path().join("a.txt"), b"one\n")?;
        let scope = ProjectScope::from_root(temp.path())?;
        let tools = FileSystemTools::new(scope);
        let digest = tools.read("a.txt")?.digest;
        let request = ApplyPatchRequest {
            path: "a.txt".to_string(),
            expected_digest: digest.as_str().to_string(),
            patch: "@@ -1 +1 @@\n-one\n+two\n".to_string(),
        };
        let result = tools.apply_patch(&request)?;
        assert_eq!(result.status, "written");
        assert_eq!(fs::read_to_string(temp.path().join("a.txt"))?, "two\n");
        Ok(())
    }
}
