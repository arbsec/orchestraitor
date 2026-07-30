//! Filesystem content-addressed storage compatible with Arbitraitor layout.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use orchestraitor_model::Digest;
use sha2::{Digest as _, Sha256};

use crate::store::{StoreError, StoreResult};

/// Filesystem CAS rooted at `<root>/objects/<first-two-hex>/<digest>`.
#[derive(Debug, Clone)]
pub struct CasDirectory {
    root: PathBuf,
}

impl CasDirectory {
    /// Opens or creates a CAS directory.
    ///
    /// # Errors
    /// Returns [`StoreError`] when the objects directory cannot be created.
    pub fn open(root: impl AsRef<Path>) -> StoreResult<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("objects"))?;
        Ok(Self { root })
    }

    /// Returns the CAS root path.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Stores bytes by SHA-256 digest and returns the content digest.
    ///
    /// # Errors
    /// Returns [`StoreError`] when writing or atomically installing the blob fails.
    pub fn store_bytes(&self, bytes: &[u8]) -> StoreResult<Digest> {
        let hex_digest = hex::encode(Sha256::digest(bytes));
        let digest = Digest::new(hex_digest);
        let path = self.path_for_digest(&digest)?;
        if path.is_file() {
            return Ok(digest);
        }
        let shard = path
            .parent()
            .ok_or_else(|| StoreError::InvalidDigest(digest.to_string()))?;
        fs::create_dir_all(shard)?;
        let temp = shard.join(format!(".{digest}.tmp"));
        fs::write(&temp, bytes)?;
        match fs::rename(&temp, &path) {
            Ok(()) => Ok(digest),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                remove_temp(&temp)?;
                Ok(digest)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Retrieves bytes for a digest and verifies their SHA-256 matches the address.
    ///
    /// # Errors
    /// Returns [`StoreError::InvalidDigest`] when the digest text is malformed,
    /// [`StoreError::Io`] when the blob cannot be read, and
    /// [`StoreError::DigestMismatch`] when the bytes on disk do not hash to the
    /// requested digest.
    pub fn load_bytes(&self, digest: &Digest) -> StoreResult<Vec<u8>> {
        let bytes = fs::read(self.path_for_digest(digest)?)?;
        let actual = Digest::new(hex::encode(Sha256::digest(&bytes)));
        if actual != *digest {
            return Err(StoreError::DigestMismatch {
                expected: digest.clone(),
                actual,
            });
        }
        Ok(bytes)
    }

    /// Returns the canonical path for a digest in the CAS layout.
    ///
    /// # Errors
    /// Returns [`StoreError::InvalidDigest`] when the digest text is malformed.
    pub fn path_for_digest(&self, digest: &Digest) -> StoreResult<PathBuf> {
        let value = digest.as_str();
        let shard = value
            .get(0..2)
            .ok_or_else(|| StoreError::InvalidDigest(value.to_owned()))?;
        if value.len() != 64 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(StoreError::InvalidDigest(value.to_owned()));
        }
        Ok(self.root.join("objects").join(shard).join(value))
    }
}

fn remove_temp(path: &Path) -> StoreResult<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
