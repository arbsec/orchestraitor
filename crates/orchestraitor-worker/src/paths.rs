//! Worktree path confinement for the file tools.
//!
//! Model-supplied paths are untrusted input (spec `40-arbitraitor-integration.md`
//! §6.1). Confinement follows the `orchestraitor-workspace` symlink discipline
//! (`symlink.rs`: lexical `..` accounting, no absolute targets) and tightens it
//! for untrusted writes: any existing symlink component on the path — including
//! the final component — fails closed, so no read, write, or search can escape
//! the task worktree through a planted link.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// A rejected worktree-relative path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PathRejection {
    /// The path is empty or names the worktree root itself.
    Empty,
    /// The path is absolute (or carries a platform prefix).
    Absolute,
    /// `..` components climb above the worktree root.
    EscapesWorktree,
    /// An existing component on the path is a symlink.
    SymlinkComponent,
    /// A path component could not be inspected (I/O).
    Inaccessible,
}

impl PathRejection {
    /// Static receipt/log reason code for this rejection.
    pub(crate) const fn reason_code(self) -> &'static str {
        match self {
            Self::Empty => "path-empty",
            Self::Absolute => "path-absolute",
            Self::EscapesWorktree => "path-escape",
            Self::SymlinkComponent => "path-symlink",
            Self::Inaccessible => "path-inaccessible",
        }
    }
}

/// Resolves a model-supplied relative path against the (canonicalized)
/// worktree root, failing closed on escapes and on any existing symlink
/// component. Writing through a symlink follows it, so an existing final
/// symlink is refused for writes exactly as for reads.
pub(crate) fn resolve_confined(root: &Path, relative: &str) -> Result<PathBuf, PathRejection> {
    let parts = normalize_relative(relative)?;
    refuse_symlink_components(root, &parts)?;
    let mut resolved = root.to_path_buf();
    for part in &parts {
        resolved.push(part);
    }
    Ok(resolved)
}

/// Lexically normalizes a relative path into its components, refusing
/// absolute paths and `..` escapes (mirrors `symlink.rs` depth accounting).
fn normalize_relative(relative: &str) -> Result<Vec<OsString>, PathRejection> {
    if relative.is_empty() {
        return Err(PathRejection::Empty);
    }
    let mut parts: Vec<OsString> = Vec::new();
    for component in Path::new(relative).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_owned()),
            Component::ParentDir => {
                if parts.pop().is_none() {
                    return Err(PathRejection::EscapesWorktree);
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err(PathRejection::Absolute),
        }
    }
    if parts.is_empty() {
        return Err(PathRejection::Empty);
    }
    Ok(parts)
}

/// Refuses when any *existing* component under the root is a symlink.
///
/// Fail-closed tightening over the workspace crate's target-confinement
/// check: a symlink anywhere on the path (worktree-internal links included) is
/// refused, because untrusted writes must not follow planted links even to
/// destinations inside the worktree (a link can be re-pointed between the
/// check and a later promotion step).
fn refuse_symlink_components(root: &Path, parts: &[OsString]) -> Result<(), PathRejection> {
    let mut current = root.to_path_buf();
    for part in parts {
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(PathRejection::SymlinkComponent);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // The remaining components cannot exist; nothing left to check.
                return Ok(());
            }
            Err(_) => return Err(PathRejection::Inaccessible),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn accepts_nested_relative_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let resolved = resolve_confined(root, "src/deep/file.rs").unwrap();
        assert_eq!(resolved, root.join("src/deep/file.rs"));
    }

    #[test]
    fn rejects_absolute_and_escaping_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        for bad in [
            "/etc/passwd",
            "/abs",
            "..",
            "../escape",
            "a/../../escape",
            "",
        ] {
            assert!(
                resolve_confined(root, bad).is_err(),
                "path {bad:?} must be refused"
            );
        }
        assert_eq!(
            resolve_confined(root, "/etc/passwd"),
            Err(PathRejection::Absolute)
        );
        assert_eq!(
            resolve_confined(root, "../escape"),
            Err(PathRejection::EscapesWorktree)
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_components_on_the_path() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("real")).unwrap();
        symlink("real", root.join("link")).unwrap();

        // A symlinked intermediate component is refused even when it points
        // inside the worktree.
        assert_eq!(
            resolve_confined(root, "link/file.rs"),
            Err(PathRejection::SymlinkComponent)
        );
        // A symlinked final component is refused for writes (a write follows
        // the link).
        symlink("../outside", root.join("planted")).unwrap();
        assert_eq!(
            resolve_confined(root, "planted"),
            Err(PathRejection::SymlinkComponent)
        );
    }

    #[test]
    fn missing_components_are_not_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        assert!(resolve_confined(root, "does/not/exist/yet.txt").is_ok());
    }
}
