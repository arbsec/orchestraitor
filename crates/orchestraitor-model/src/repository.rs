//! Repository domain type (spec §18.1).

use crate::digest::Digest;
use crate::ids::RepositoryId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Repository identity — stable metadata about a Git repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryIdentity {
    /// Canonical remote URL, if available.
    pub remote_url: Option<String>,
    /// Default branch name observed on the remote.
    pub default_branch: Option<String>,
    /// Owning organization, if derivable from the remote.
    pub organization: Option<String>,
}

/// Index state for a repository's context index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum IndexState {
    /// Not yet indexed.
    NotIndexed,
    /// Indexing in progress.
    Indexing,
    /// Indexed at the given digest.
    Indexed {
        /// Digest of the indexed repository state.
        digest: Digest,
        /// Number of blobs captured in the index.
        blob_count: u64,
    },
    /// Index is stale (changes since last index).
    Stale {
        /// Digest of the last up-to-date index snapshot.
        last_digest: Digest,
    },
}

/// A reference to a policy (by digest).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PolicyRef(pub Digest);

/// Repository model (spec §18.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    /// Stable identifier for this repository.
    pub id: RepositoryId,
    /// Canonical on-disk path of the repository checkout.
    pub canonical_path: PathBuf,
    /// Stable metadata describing the Git repository.
    pub identity: RepositoryIdentity,
    /// Default policy applied to sessions in this repository.
    pub default_policy: PolicyRef,
    /// State of the context index for this repository.
    pub index_state: IndexState,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::digest::Digest;
    use crate::ids::RepositoryId;

    #[test]
    fn repository_round_trips() {
        let repo = Repository {
            id: RepositoryId::new(),
            canonical_path: PathBuf::from("/tmp/repo"),
            identity: RepositoryIdentity {
                remote_url: Some("https://github.com/arbsec/orchestraitor.git".into()),
                default_branch: Some("main".into()),
                organization: Some("arbsec".into()),
            },
            default_policy: PolicyRef(Digest::new("b".repeat(64))),
            index_state: IndexState::NotIndexed,
        };
        let json = serde_json::to_string_pretty(&repo).unwrap();
        let back: Repository = serde_json::from_str(&json).unwrap();
        assert_eq!(repo, back);
    }
}
