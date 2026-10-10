//! Repository filesystem traversal for supported source files.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ContextError;
use crate::language::spec_for_path;

/// Returns supported source files under `repo_path` in stable order.
pub(crate) fn repository_files(repo_path: &Path) -> Result<Vec<PathBuf>, ContextError> {
    let mut files = Vec::new();
    visit(repo_path, repo_path, &mut files)?;
    files.sort();
    Ok(files)
}

/// Converts an absolute path to a repository-relative path.
pub(crate) fn relative_path(root: &Path, path: &Path) -> Result<PathBuf, ContextError> {
    path.strip_prefix(root).map(Path::to_path_buf).map_err(|_| {
        ContextError::PathOutsideRepository {
            path: path.to_path_buf(),
        }
    })
}

fn visit(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), ContextError> {
    let entries = fs::read_dir(directory).map_err(|source| ContextError::RepositoryPath {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ContextError::RepositoryPath {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if is_ignored(root, &path) {
            continue;
        }
        if path.is_dir() {
            visit(root, &path, files)?;
        } else if spec_for_path(&path).is_some() {
            files.push(path);
        }
    }
    Ok(())
}

fn is_ignored(root: &Path, path: &Path) -> bool {
    relative_path(root, path).is_ok_and(|relative| {
        relative.components().any(|component| {
            let part = component.as_os_str().to_string_lossy();
            matches!(
                part.as_ref(),
                ".git" | "target" | "node_modules" | "dist" | "build"
            )
        })
    })
}
