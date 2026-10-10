//! Serializable DTOs for built-in filesystem tools.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// SHA-256 digest for file content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct FileDigest(String);

impl FileDigest {
    /// Creates a file digest string wrapper.
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// Returns the digest string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Project-relative path accepted by built-in fs tools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ProjectPath(pub(crate) String);

impl ProjectPath {
    /// Returns the path string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Digest mismatch details returned by optimistic concurrency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DigestMismatch {
    /// Expected digest.
    pub expected: String,
    /// Actual digest.
    pub actual: String,
}

/// Apply-patch request data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ApplyPatchRequest {
    /// File to patch.
    pub path: String,
    /// Digest obtained from the prior `fs.read`.
    pub expected_digest: String,
    /// Unified diff patch body.
    pub patch: String,
}

/// Result from `fs.read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadResult {
    /// UTF-8 file content.
    pub content: String,
    /// SHA-256 digest.
    pub digest: FileDigest,
}

/// Result from `fs.stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatResult {
    /// Whether the path is a file.
    pub is_file: bool,
    /// Whether the path is a directory.
    pub is_dir: bool,
    /// Byte length.
    pub len: u64,
}

/// Result from `fs.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListResult {
    /// Sorted direct child names.
    pub entries: Vec<String>,
}

/// Result from `fs.search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    /// Matching file paths.
    pub matches: Vec<String>,
}

/// Result from a write transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WriteResult {
    /// Project-relative path.
    pub path: String,
    /// Write status.
    pub status: String,
    /// Final digest after normalization.
    pub digest: FileDigest,
    /// Normalization report.
    pub normalization: NormalizationResult,
    /// Secondary changed paths.
    pub secondary_changes: Vec<String>,
    /// Diagnostics emitted by normalization.
    pub diagnostics: Vec<String>,
}

/// Normalization delta summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NormalizationResult {
    /// Formatter name, when configured.
    pub formatter: Option<String>,
    /// Safe fixers applied.
    pub fixers: Vec<String>,
    /// Whether normalization changed content.
    pub changed: bool,
    /// Bounded normalization patch, when present.
    pub patch: Option<String>,
}

/// Result from `fs.rename`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RenameResult {
    /// Source path.
    pub from: String,
    /// Destination path.
    pub to: String,
    /// Operation status.
    pub status: String,
}

/// Result from `fs.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoveResult {
    /// Project-relative path.
    pub path: String,
    /// Operation status.
    pub status: String,
}

impl WriteResult {
    pub(crate) fn created(path: &str, digest: FileDigest) -> Self {
        Self {
            path: path.to_string(),
            status: "created".to_string(),
            digest,
            normalization: NormalizationResult::default(),
            secondary_changes: Vec::new(),
            diagnostics: Vec::new(),
        }
    }
}
