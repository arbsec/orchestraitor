use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use gix::bstr::ByteSlice;
use gix::object::tree::EntryKind;

use crate::types::{Result, WorkspaceError};

/// History diff returned by controller-backed RPC methods.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HistoryDiff {
    /// Left revision.
    pub old_revision: String,
    /// Right revision.
    pub new_revision: String,
    /// Per-path changes observed between tree snapshots.
    pub changes: Vec<PathChange>,
}

/// Path-level change in a history diff.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathChange {
    /// Path relative to the repository root.
    pub path: PathBuf,
    /// Change classification.
    pub kind: PathChangeKind,
}

/// Kind of path-level history change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathChangeKind {
    /// Path was added.
    Added,
    /// Path was removed.
    Removed,
    /// Path content or mode changed.
    Modified,
}

/// Commit summary returned by history log RPCs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LogEntry {
    /// Commit id.
    pub id: String,
    /// First line of the commit message.
    pub summary: String,
}

/// Line attribution returned by the blame RPC.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlameLine {
    /// One-based line number.
    pub line_number: usize,
    /// Commit currently responsible for this line.
    pub commit_id: String,
    /// Line text without a trailing newline.
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TreeEntryData {
    pub(crate) mode: EntryKind,
    pub(crate) data: Vec<u8>,
}

pub(crate) fn collect_revision_entries(
    repo: &gix::Repository,
    revision: &str,
) -> Result<BTreeMap<PathBuf, TreeEntryData>> {
    let commit_id =
        repo.rev_parse_single(revision)
            .map_err(|source| WorkspaceError::ResolveRevision {
                revision: revision.to_owned(),
                source: Box::new(source),
            })?;
    let commit = commit_id
        .object()
        .map_err(|source| WorkspaceError::LoadObject {
            object: revision.to_owned(),
            source: Box::new(source),
        })?
        .try_into_commit()
        .map_err(|source| WorkspaceError::LoadObject {
            object: revision.to_owned(),
            source: Box::new(source),
        })?;
    let tree = commit.tree().map_err(|source| WorkspaceError::LoadObject {
        object: format!("{revision}^{{tree}}"),
        source: Box::new(source),
    })?;
    let mut entries = BTreeMap::new();
    collect_tree_entries(repo, &tree, Path::new(""), &mut entries)?;
    Ok(entries)
}

pub(crate) fn safe_entry_name(name: &gix::bstr::BStr) -> Result<&str> {
    let text = name.to_str().map_err(|_| WorkspaceError::UnsafeTreePath {
        path: name.to_string(),
    })?;
    let path = Path::new(text);
    let safe = !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if safe && text != ".git" {
        Ok(text)
    } else {
        Err(WorkspaceError::UnsafeTreePath {
            path: text.to_owned(),
        })
    }
}

fn collect_tree_entries(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    prefix: &Path,
    entries: &mut BTreeMap<PathBuf, TreeEntryData>,
) -> Result<()> {
    for entry in tree.iter() {
        let entry = entry.map_err(|source| WorkspaceError::LoadObject {
            object: prefix.display().to_string(),
            source: Box::new(source),
        })?;
        let rel_path = prefix.join(safe_entry_name(entry.filename())?);
        match entry.mode().kind() {
            EntryKind::Tree => {
                let child = repo.find_tree(entry.object_id()).map_err(|source| {
                    WorkspaceError::LoadObject {
                        object: rel_path.display().to_string(),
                        source: Box::new(source),
                    }
                })?;
                collect_tree_entries(repo, &child, &rel_path, entries)?;
            }
            EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => {
                let mut blob = entry
                    .object()
                    .map_err(|source| WorkspaceError::LoadObject {
                        object: rel_path.display().to_string(),
                        source: Box::new(source),
                    })?
                    .try_into_blob()
                    .map_err(|source| WorkspaceError::LoadObject {
                        object: rel_path.display().to_string(),
                        source: Box::new(source),
                    })?;
                entries.insert(
                    rel_path,
                    TreeEntryData {
                        mode: entry.mode().kind(),
                        data: blob.take_data(),
                    },
                );
            }
            EntryKind::Commit => {
                entries.insert(
                    rel_path,
                    TreeEntryData {
                        mode: EntryKind::Commit,
                        data: entry.object_id().as_bytes().to_vec(),
                    },
                );
            }
        }
    }
    Ok(())
}
