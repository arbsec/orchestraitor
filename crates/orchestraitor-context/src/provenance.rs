//! Provenance envelopes for all context items.

use orchestraitor_model::{ContextOrigin, DataSensitivity, Digest, TrustClass};
use serde::{Deserialize, Serialize};

use crate::types::SourceRef;

/// Staleness marker attached to a context item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextAge {
    /// Captured during the current index pass.
    Current,
    /// Reused from a previous index pass because the blob digest was unchanged.
    ReusedUnchanged,
}

/// Spec §9.15.1 provenance envelope attached to every emitted context item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceEnvelope {
    /// Source category for the item.
    pub origin: ContextOrigin,
    /// Content digest or repository blob digest.
    pub digest: Digest,
    /// Staleness / refresh marker.
    pub age: ContextAge,
    /// Data governance classification.
    pub sensitivity: DataSensitivity,
    /// Trust classification. Repository content remains untrusted.
    pub trust_class: TrustClass,
    /// Stable pointer to the origin.
    pub source_ref: SourceRef,
}

impl ProvenanceEnvelope {
    /// Builds an untrusted repository-content envelope for a blob-backed item.
    #[must_use]
    pub const fn repository_blob(digest: Digest, age: ContextAge, source_ref: SourceRef) -> Self {
        Self {
            origin: ContextOrigin::RepositoryContent,
            digest,
            age,
            sensitivity: DataSensitivity::Internal,
            trust_class: TrustClass::Untrusted,
            source_ref,
        }
    }
}
