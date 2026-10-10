use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::materialize::MaterializeContext;
use crate::types::{Result, WorkspaceError, digest_bytes};

pub(crate) fn materialize_symlink(
    entry: &gix::object::tree::EntryRef<'_, '_>,
    rel_path: &Path,
    context: &MaterializeContext<'_>,
) -> Result<()> {
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
    let target = PathBuf::from(String::from_utf8_lossy(&blob.take_data()).to_string());
    ensure_confined_symlink(rel_path, &target)?;
    create_symlink(context.root, rel_path, &target)?;
    context.manifest.borrow_mut().insert(
        rel_path.to_path_buf(),
        digest_bytes(target.as_os_str().as_encoded_bytes()),
    );
    Ok(())
}

fn ensure_confined_symlink(link: &Path, target: &Path) -> Result<()> {
    if target.is_absolute() {
        return Err(escaping_symlink(link, target));
    }
    let mut depth = link.components().count().saturating_sub(1);
    for component in target.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(_) => depth = depth.saturating_add(1),
            Component::ParentDir if depth == 0 => return Err(escaping_symlink(link, target)),
            Component::ParentDir => depth = depth.saturating_sub(1),
            Component::RootDir | Component::Prefix(_) => {
                return Err(escaping_symlink(link, target));
            }
        }
    }
    Ok(())
}

fn escaping_symlink(link: &Path, target: &Path) -> WorkspaceError {
    WorkspaceError::EscapingSymlink {
        link: link.to_path_buf(),
        target: target.to_path_buf(),
    }
}

#[cfg(unix)]
fn create_symlink(root: &Path, link: &Path, target: &Path) -> Result<()> {
    use std::os::unix::fs::symlink;

    let full_link = root.join(link);
    if let Some(parent) = full_link.parent() {
        fs::create_dir_all(parent).map_err(|source| WorkspaceError::Filesystem {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    symlink(target, &full_link).map_err(|source| WorkspaceError::Filesystem {
        path: full_link,
        source,
    })
}

#[cfg(not(unix))]
fn create_symlink(root: &Path, link: &Path, target: &Path) -> Result<()> {
    let dest = crate::materialize::open_dest(root)?;
    if let Some(parent) = link
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
        .create(link)
        .map_err(|source| WorkspaceError::Filesystem {
            path: root.join(link),
            source,
        })?;
    use std::io::Write;
    file.write_all(target.as_os_str().as_encoded_bytes())
        .map_err(|source| WorkspaceError::Filesystem {
            path: root.join(link),
            source,
        })
}
