use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use gix::object::tree::EntryKind;

use crate::history::safe_entry_name;
use crate::symlink::materialize_symlink;
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
    for entry in tree.iter() {
        let entry = entry.map_err(|source| WorkspaceError::LoadObject {
            object: prefix.display().to_string(),
            source: Box::new(source),
        })?;
        let rel_path = prefix.join(safe_entry_name(entry.filename())?);
        if !is_sparse_visible(&rel_path, context.options) {
            continue;
        }
        match entry.mode().kind() {
            EntryKind::Tree => {
                dest.create_dir_all(&rel_path)
                    .map_err(|source| WorkspaceError::Filesystem {
                        path: context.root.join(&rel_path),
                        source,
                    })?;
                let child = repo.find_tree(entry.object_id()).map_err(|source| {
                    WorkspaceError::LoadObject {
                        object: rel_path.display().to_string(),
                        source: Box::new(source),
                    }
                })?;
                materialize_tree(repo, &child, dest, &rel_path, context)?;
            }
            EntryKind::Blob | EntryKind::BlobExecutable => {
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
                write_file(dest, context.root, &rel_path, &data, entry.mode().kind())?;
                context
                    .manifest
                    .borrow_mut()
                    .insert(rel_path, digest_bytes(&data));
            }
            EntryKind::Link => materialize_symlink(&entry, &rel_path, context)?,
            EntryKind::Commit => {
                let marker = format!("submodule {}\n", entry.object_id().to_hex());
                write_file(
                    dest,
                    context.root,
                    &rel_path,
                    marker.as_bytes(),
                    EntryKind::Blob,
                )?;
                context
                    .manifest
                    .borrow_mut()
                    .insert(rel_path, digest_bytes(marker.as_bytes()));
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
