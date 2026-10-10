//! Index snapshot persistence: serde round-trip of a [`ContextIndex`] to
//! `.orchestraitor/codegraph.json` under a project root.
//!
//! The index derives `Serialize`/`Deserialize` and all record types are
//! serializable, so the snapshot is lossless by construction. Callers that
//! only need symbol/call-graph queries can reload a snapshot without
//! re-running tree-sitter over the repository.

use std::fs;
use std::path::Path;

use crate::error::ContextError;
use crate::index::ContextIndex;

/// Snapshot file path relative to a project root.
pub const INDEX_FILE: &str = ".orchestraitor/codegraph.json";

/// Writes `index` to `<project_root>/.orchestraitor/codegraph.json`,
/// creating the parent directory when missing.
///
/// # Errors
/// Returns [`ContextError::Io`] when directory creation or the write fails.
pub fn store_index(project_root: &Path, index: &ContextIndex) -> Result<(), ContextError> {
    let path = project_root.join(INDEX_FILE);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| ContextError::Persistence {
            operation: format!("create {}: {error}", parent.display()),
        })?;
    }
    let bytes = serde_json::to_vec_pretty(index).map_err(|error| ContextError::Persistence {
        operation: format!("serialize index: {error}"),
    })?;
    fs::write(&path, bytes).map_err(|error| ContextError::Persistence {
        operation: format!("write {}: {error}", path.display()),
    })
}

/// Loads a [`ContextIndex`] snapshot from
/// `<project_root>/.orchestraitor/codegraph.json`.
///
/// # Errors
/// Returns [`ContextError::Io`] when the file is missing or unreadable and
/// [`ContextError::IndexCorrupt`] when it fails to parse.
pub fn load_index(project_root: &Path) -> Result<ContextIndex, ContextError> {
    let path = project_root.join(INDEX_FILE);
    let bytes = fs::read(&path).map_err(|error| ContextError::Persistence {
        operation: format!("read {}: {error}", path.display()),
    })?;
    serde_json::from_slice(&bytes).map_err(|error| ContextError::Persistence {
        operation: format!("parse {}: {error}", path.display()),
    })
}
