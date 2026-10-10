use std::collections::BTreeMap;
use std::path::PathBuf;

/// Result alias for workspace-controller operations.
pub type Result<T> = std::result::Result<T, WorkspaceError>;

/// Errors produced by the workspace controller.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// Opening the trusted source repository failed.
    #[error("failed to open trusted source repository")]
    OpenRepository(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Resolving a revision failed.
    #[error("failed to resolve revision {revision}")]
    ResolveRevision {
        /// Revision string supplied by the caller.
        revision: String,
        /// Underlying `gix` error.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Loading a Git object failed.
    #[error("failed to load git object {object}")]
    LoadObject {
        /// Object id or path being loaded.
        object: String,
        /// Underlying `gix` error.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Filesystem operation failed.
    #[error("filesystem operation failed at {path}")]
    Filesystem {
        /// Filesystem path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// A Git tree entry contained a path the controller will not materialize.
    #[error("unsafe tree path rejected: {path}")]
    UnsafeTreePath {
        /// Rejected path text.
        path: String,
    },
    /// A symlink target would escape the materialized workspace root.
    #[error("symlink {link} escapes workspace root via target {target}")]
    EscapingSymlink {
        /// Workspace-relative link path.
        link: PathBuf,
        /// Link target stored in Git.
        target: PathBuf,
    },
    /// The destination would expose Git metadata to the worker.
    #[error("snapshot destination contains forbidden .git entry")]
    DotGitExposed,
}

/// Snapshot creation policy.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SnapshotOptions {
    /// Optional sparse path prefixes visible to the worker.
    pub sparse_paths: Vec<PathBuf>,
}

/// A materialized worker snapshot and controller-side reconciliation data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    /// Commit exported into the worker directory.
    pub base_commit: String,
    /// Trusted repository `HEAD` when the snapshot was created.
    pub source_head_at_creation: String,
    /// Worker directory that contains no `.git` metadata.
    pub workspace_root: PathBuf,
    /// Controller-side digest manifest for external mutation detection.
    pub manifest: BTreeMap<PathBuf, FileDigest>,
}

/// SHA-256 digest and byte length for a materialized file or symlink payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileDigest {
    /// Hex-encoded SHA-256 digest.
    pub sha256: String,
    /// Byte length hashed into `sha256`.
    pub len: u64,
}

/// Reconciliation report for base-branch drift and external workspace edits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconciliationReport {
    /// True when the trusted source `HEAD` moved after snapshot creation.
    pub base_branch_drifted: bool,
    /// Current trusted source `HEAD`.
    pub current_source_head: String,
    /// Worker paths whose digest differs from the snapshot manifest.
    pub external_mutations: Vec<PathBuf>,
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> FileDigest {
    use sha2::{Digest as ShaDigest, Sha256};

    let digest = Sha256::digest(bytes);
    FileDigest {
        sha256: hex::encode(digest),
        len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    }
}
