//! Trusted controller abstraction for atomic copy/apply and rollback.
//!
//! The trusted controller owns the authoritative filesystem mutation path
//! (spec §9.5, §9.14). The workspace controller in the daemon crate implements
//! [`TrustedController`]; this crate defines the trait and an in-memory
//! implementation for tests. Orchestraitor never lets the worker write directly
//! to the trusted checkout — promotion always flows through this controller.

use orchestraitor_model::Digest;
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::PromotionError;

/// A file state captured at a transaction-graph node.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileState {
    /// Repository-relative path.
    pub path: PathBuf,
    /// File content at this node.
    pub content: Vec<u8>,
    /// SHA-256 content digest (content-addressed storage key, spec §9.4.3).
    pub digest: Digest,
}

impl FileState {
    /// Creates a file state from path and content, computing the digest.
    #[must_use]
    pub fn new(path: PathBuf, content: Vec<u8>) -> Self {
        let digest = compute_digest(&content);
        Self {
            path,
            content,
            digest,
        }
    }
}

/// The result of applying a change through the trusted controller.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppliedChange {
    /// Repository-relative path that was applied.
    pub path: PathBuf,
    /// Digest of the content written to the target.
    pub target_digest: Digest,
}

/// Owns atomic copy/apply and rollback for the trusted checkout.
///
/// Implementations MUST guarantee atomicity: a multi-file apply either lands
/// the workspace in the pre-apply or post-apply state, never a half-applied
/// intermediate (spec §9.4.3, §9.5).
pub trait TrustedController {
    /// Atomically applies content to a path in the trusted checkout.
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError::ApplyFailed`] when the write fails.
    fn apply(&mut self, path: &Path, content: &[u8]) -> Result<AppliedChange, PromotionError>;

    /// Atomically restores a set of file states (rollback / restore).
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError::ApplyFailed`] when the restore fails.
    fn restore(&mut self, files: &[FileState]) -> Result<(), PromotionError>;

    /// Reads the current content of a path in the trusted checkout, if present.
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError::ApplyFailed`] when the read fails.
    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, PromotionError>;
}

/// In-memory controller for tests and embedded callers.
#[derive(Debug, Default)]
pub struct InMemoryController {
    files: HashMap<PathBuf, Vec<u8>>,
}

impl InMemoryController {
    /// Creates an empty controller.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds the controller with an initial file set.
    pub fn seed(&mut self, path: &Path, content: Vec<u8>) {
        self.files.insert(path.to_path_buf(), content);
    }

    /// Returns the number of tracked files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Returns `true` when no files are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl TrustedController for InMemoryController {
    fn apply(&mut self, path: &Path, content: &[u8]) -> Result<AppliedChange, PromotionError> {
        self.files.insert(path.to_path_buf(), content.to_vec());
        Ok(AppliedChange {
            path: path.to_path_buf(),
            target_digest: compute_digest(content),
        })
    }

    fn restore(&mut self, files: &[FileState]) -> Result<(), PromotionError> {
        self.files.clear();
        for file in files {
            self.files.insert(file.path.clone(), file.content.clone());
        }
        Ok(())
    }

    fn read(&self, path: &Path) -> Result<Option<Vec<u8>>, PromotionError> {
        Ok(self.files.get(path).cloned())
    }
}

/// Computes a SHA-256 content digest for content-addressed storage keys.
///
/// This is NOT a security operation — it produces storage keys only (spec §9.4.3,
/// §9.5). Security-relevant digests originate from Arbitraitor.
fn compute_digest(content: &[u8]) -> Digest {
    let hash = Sha256::digest(content);
    Digest::new(hex::encode(hash))
}
