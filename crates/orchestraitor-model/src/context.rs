//! Context receipt domain type (spec §18.4, §9.15).

use crate::digest::Digest;
use crate::enums::{ContextOrigin, DataSensitivity, TrustClass};
use crate::ids::ContextRequestId;
use serde::{Deserialize, Serialize};

/// Classification of a task's domain (spec §9.19.1).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum TaskClass {
    /// Generic fallback domain — always enabled (spec §9.19.1).
    General,
    /// Frontend / UI work — web, desktop, mobile surfaces.
    Frontend,
    /// Backend services — APIs, daemons, queue workers.
    Backend,
    /// Data engineering — pipelines, warehouses, analytics.
    Data,
    /// DevOps / SRE — infrastructure, deployment, observability.
    Devops,
    /// Test authoring and maintenance.
    Testing,
    /// Documentation writing and editing.
    Documentation,
    /// Security-sensitive code — auth, crypto, sandbox, policy.
    Security,
}

/// A reference to a selected context item (spec §9.15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextItemRef {
    /// Repository-relative or session-scoped reference to the source.
    pub source_ref: String,
    /// Provenance origin of this item (spec §9.15.1).
    pub origin: ContextOrigin,
    /// Trust classification of this item (spec §9.15.1).
    pub trust_class: TrustClass,
    /// Data-sensitivity tier used for provider selection (spec §9.28.1).
    pub sensitivity: DataSensitivity,
    /// Content digest of the item (sha-256 hex).
    pub digest: Digest,
    /// Estimated token count when included in the prompt.
    pub token_estimate: u64,
}

/// Context receipt (spec §18.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextReceipt {
    /// Identifier of the context request that produced this receipt.
    pub request_id: ContextRequestId,
    /// Classifier used to scope policy selection (spec §9.19.1).
    pub task_class: TaskClass,
    /// Token budget imposed for the request.
    pub budget_tokens: u64,
    /// Total tokens of all candidate items before selection.
    pub candidate_tokens: u64,
    /// Tokens actually selected for the prompt.
    pub selected_tokens: u64,
    /// Items selected for inclusion in the prompt.
    pub selected_items: Vec<ContextItemRef>,
    /// Number of candidate items omitted from selection.
    pub omitted_count: u64,
    /// Digest of the index snapshot the selection ran against.
    pub index_digest: Digest,
    /// Digest of the policy that governed the selection.
    pub selection_policy_digest: Digest,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::digest::Digest;
    use crate::enums::{ContextOrigin, DataSensitivity, TrustClass};
    use crate::ids::ContextRequestId;

    #[test]
    fn context_receipt_round_trips() {
        let receipt = ContextReceipt {
            request_id: ContextRequestId::new(),
            task_class: TaskClass::Backend,
            budget_tokens: 100_000,
            candidate_tokens: 200_000,
            selected_tokens: 80_000,
            selected_items: vec![ContextItemRef {
                source_ref: "src/main.rs".into(),
                origin: ContextOrigin::RepositoryContent,
                trust_class: TrustClass::Untrusted,
                sensitivity: DataSensitivity::Internal,
                digest: Digest::new("d".repeat(64)),
                token_estimate: 500,
            }],
            omitted_count: 42,
            index_digest: Digest::new("e".repeat(64)),
            selection_policy_digest: Digest::new("f".repeat(64)),
        };
        let json = serde_json::to_string(&receipt).unwrap();
        let back: ContextReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(receipt.request_id, back.request_id);
        assert_eq!(receipt.task_class, back.task_class);
    }
}
