use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use gix::object::tree::EntryKind;

use crate::history::safe_entry_name;
use crate::types::{FileDigest, Result, SnapshotOptions, WorkspaceError, digest_bytes};

pub(crate) fn prepare_destination(dest_dir: &Path) -> Result<()> {
    fs::create_dir_all(dest_dir).map_err(|source| WorkspaceError::Filesystem {
        path: dest_dir.to_path_buf(),
        source,
    })?;
    ensure_no_dot_git(dest_dir)
}

pub(crate) fn open_dest(dest_dir: &Path) -> Result<Dir> {
    Dir::open_ambient_dir(dest_dir, ambient_authority()).map_err(|source| {
        WorkspaceError::Filesystem {
            path: dest_dir.to_path_buf(),
            source,
        }
    })
}

pub(crate) fn ensure_no_dot_git(root: &Path) -> Result<()> {
    if root.join(".git").exists() {
        return Err(WorkspaceError::DotGitExposed);
    }
    Ok(())
}

pub(crate) fn materialize_tree(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    dest: &Dir,
    prefix: &Path,
    context: &MaterializeContext<'_>,
) -> Result<()> {
    // Validate the complete plan before writing anything, so a refused entry
    // (such as a link) never leaves a partially materialized snapshot behind.
    let mut plan = Vec::new();
    plan_tree(repo, tree, prefix, context.options, &mut plan)?;
    write_plan(repo, dest, &plan, context)
}

/// One validated tree entry scheduled for materialization.
enum PlanEntry {
    /// Directory that must exist before its children are written.
    Dir { path: PathBuf },
    /// Regular file content stored as a Git blob.
    Blob {
        path: PathBuf,
        object_id: gix::ObjectId,
        kind: EntryKind,
    },
    /// Submodule placeholder file.
    Submodule { path: PathBuf, marker: String },
}

fn plan_tree(
    repo: &gix::Repository,
    tree: &gix::Tree<'_>,
    prefix: &Path,
    options: &SnapshotOptions,
    plan: &mut Vec<PlanEntry>,
) -> Result<()> {
    for entry in tree.iter() {
        let entry = entry.map_err(|source| WorkspaceError::LoadObject {
            object: prefix.display().to_string(),
            source: Box::new(source),
        })?;
        let rel_path = prefix.join(safe_entry_name(entry.filename())?);
        if !is_sparse_visible(&rel_path, options) {
            continue;
        }
        match entry.mode().kind() {
            EntryKind::Tree => {
                plan.push(PlanEntry::Dir {
                    path: rel_path.clone(),
                });
                let child = repo.find_tree(entry.object_id()).map_err(|source| {
                    WorkspaceError::LoadObject {
                        object: rel_path.display().to_string(),
                        source: Box::new(source),
                    }
                })?;
                plan_tree(repo, &child, &rel_path, options, plan)?;
            }
            EntryKind::Blob | EntryKind::BlobExecutable => plan.push(PlanEntry::Blob {
                path: rel_path,
                object_id: entry.object_id(),
                kind: entry.mode().kind(),
            }),
            EntryKind::Link => {
                // Arbitraitor owns the symlink-projection decision (spec §2.2,
                // §9.4.2). Its projection capability does not exist yet
                // (arbsec/arbitraitor#872), so the plan is refused as a whole.
                return Err(WorkspaceError::ProjectionCapabilityUnavailable {
                    path: rel_path.display().to_string(),
                    link: link_name(&rel_path),
                    target: link_target(&entry, &rel_path)?,
                });
            }
            EntryKind::Commit => plan.push(PlanEntry::Submodule {
                path: rel_path,
                marker: format!("submodule {}\n", entry.object_id().to_hex()),
            }),
        }
    }
    Ok(())
}

fn link_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Cap for the diagnostic-only link target echoed in an error message.
const LINK_TARGET_DIAGNOSTIC_LIMIT: usize = 4096;

fn link_target(entry: &gix::object::tree::EntryRef<'_, '_>, rel_path: &Path) -> Result<String> {
    // The read is bounded by the link blob's own size and the value is
    // diagnostic-only, so truncate it instead of growing the error unboundedly.
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
    let data = blob.take_data();
    let text = String::from_utf8_lossy(&data);
    Ok(truncate_on_char_boundary(
        &text,
        LINK_TARGET_DIAGNOSTIC_LIMIT,
    ))
}

fn truncate_on_char_boundary(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let mut truncated = text[..end].to_owned();
    truncated.push_str("… (truncated)");
    truncated
}

fn write_plan(
    repo: &gix::Repository,
    dest: &Dir,
    plan: &[PlanEntry],
    context: &MaterializeContext<'_>,
) -> Result<()> {
    for entry in plan {
        match entry {
            PlanEntry::Dir { path } => {
                dest.create_dir_all(path)
                    .map_err(|source| WorkspaceError::Filesystem {
                        path: context.root.join(path),
                        source,
                    })?;
            }
            PlanEntry::Blob {
                path,
                object_id,
                kind,
            } => {
                let mut blob = repo
                    .find_object(*object_id)
                    .map_err(|source| WorkspaceError::LoadObject {
                        object: path.display().to_string(),
                        source: Box::new(source),
                    })?
                    .try_into_blob()
                    .map_err(|source| WorkspaceError::LoadObject {
                        object: path.display().to_string(),
                        source: Box::new(source),
                    })?;
                let data = blob.take_data();
                write_file(dest, context.root, path, &data, *kind)?;
                context
                    .manifest
                    .borrow_mut()
                    .insert(path.clone(), digest_bytes(&data));
            }
            PlanEntry::Submodule { path, marker } => {
                write_file(dest, context.root, path, marker.as_bytes(), EntryKind::Blob)?;
                context
                    .manifest
                    .borrow_mut()
                    .insert(path.clone(), digest_bytes(marker.as_bytes()));
            }
        }
    }
    Ok(())
}

pub(crate) struct MaterializeContext<'a> {
    pub(crate) root: &'a Path,
    pub(crate) options: &'a SnapshotOptions,
    pub(crate) manifest: &'a std::cell::RefCell<BTreeMap<PathBuf, FileDigest>>,
}

pub(crate) fn digest_path(path: &Path) -> Result<FileDigest> {
    let metadata = fs::symlink_metadata(path).map_err(|source| WorkspaceError::Filesystem {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|source| WorkspaceError::Filesystem {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(digest_bytes(target.as_os_str().as_encoded_bytes()))
    } else {
        let data = fs::read(path).map_err(|source| WorkspaceError::Filesystem {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(digest_bytes(&data))
    }
}

fn is_sparse_visible(path: &Path, options: &SnapshotOptions) -> bool {
    options.sparse_paths.is_empty()
        || options
            .sparse_paths
            .iter()
            .any(|prefix| path.starts_with(prefix) || prefix.starts_with(path))
}

fn write_file(
    dest: &Dir,
    root: &Path,
    rel_path: &Path,
    data: &[u8],
    kind: EntryKind,
) -> Result<()> {
    if let Some(parent) = rel_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        dest.create_dir_all(parent)
            .map_err(|source| WorkspaceError::Filesystem {
                path: root.join(parent),
                source,
            })?;
    }
    let mut file = dest
        .create(rel_path)
        .map_err(|source| WorkspaceError::Filesystem {
            path: root.join(rel_path),
            source,
        })?;
    file.write_all(data)
        .map_err(|source| WorkspaceError::Filesystem {
            path: root.join(rel_path),
            source,
        })?;
    if kind == EntryKind::BlobExecutable {
        set_executable(&root.join(rel_path))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)
        .map_err(|source| WorkspaceError::Filesystem {
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).map_err(|source| WorkspaceError::Filesystem {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<()> {
    Ok(())
}
