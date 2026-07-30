//! Promotion pipeline orchestration (spec §9.14).
//!
//! The pipeline implements the developer workflow:
//!
//! ```text
//! worker change
//!   -> classify changed path and content
//!   -> detect trust-sensitive destination
//!   -> generate semantic and textual diff
//!   -> run policy          (delegated to Arbitraitor via PolicyGate)
//!   -> prompt when required (delegated to the trusted UI via PromptSink)
//!   -> copy/apply through trusted controller
//!   -> emit promotion receipt
//! ```
//!
//! This crate **never** makes a security decision. [`PolicyGate`] is a trait
//! that Arbitraitor implements; [`PromptSink`] is a trait the trusted UI
//! implements; [`TrustedController`](crate::TrustedController) is a trait the
//! workspace controller implements.

use orchestraitor_events::{
    AuditStore, EventCategory, EventEnvelope, EventEnvelopeInput, EventQuery,
};
use orchestraitor_model::{
    Digest, Finding, FindingSeverity, PromotedPath, PromotionReceipt, RepositoryId, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Digest as _;
use std::path::{Path, PathBuf};

use crate::PromotionError;
use crate::classify::{Classification, ClassificationInput, classify};
use crate::controller::{AppliedChange, TrustedController};
use crate::destination::{DestinationSensitivity, detect_sensitivity};
use crate::diff::{SemanticDiff, compute_semantic_diff};

/// A single change to promote through the quarantine pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// Repository-relative path of the changed artifact.
    pub path: PathBuf,
    /// New content for the path. Empty content signals deletion.
    pub content: Vec<u8>,
    /// Filesystem metadata from the trusted controller.
    pub metadata: crate::classify::FileMetadata,
}

/// A policy decision returned by Arbitraitor (implemented externally).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    /// Promotion is allowed without user interaction.
    Allow,
    /// The user must be prompted before promotion.
    Prompt,
    /// Promotion is rejected; the change stays quarantined.
    Reject,
}

/// Policy evaluation gate — implemented by Arbitraitor.
///
/// Orchestraitor provides classification and diff context; Arbitraitor owns the
/// allow/deny/prompt decision (spec §9.14, §2.2).
pub trait PolicyGate {
    /// Evaluates whether a classified change may be promoted.
    fn evaluate(
        &self,
        classification: &Classification,
        sensitivity: DestinationSensitivity,
        diff: &SemanticDiff,
    ) -> PolicyDecision;
}

/// A prompt sink — implemented by the trusted UI (spec §6.4).
pub trait PromptSink {
    /// Prompts the user with a message and returns whether promotion was approved.
    fn prompt(&self, message: &str) -> bool;
}

/// Input for building a [`PromotionReceipt`] without parameter bloat.
pub struct PromotionInput<'a> {
    /// Workspace the promotion originated from.
    pub workspace_id: WorkspaceId,
    /// Digest of the captured workspace snapshot.
    pub source_digest: Digest,
    /// Repository receiving the promoted artifacts.
    pub target_repository: RepositoryId,
    /// The changes to promote.
    pub changes: Vec<Change>,
    /// Existing content per path (for diffing). Called with repository-relative paths.
    pub old_content: &'a dyn Fn(&Path) -> Option<Vec<u8>>,
}

/// Promotes a set of changes through the quarantine pipeline.
///
/// Classifies each change, detects trust-sensitive destinations, generates
/// diffs, delegates policy to `gate`, prompts through `prompt` when required,
/// applies through `controller`, and returns a [`PromotionReceipt`]. When
/// `store` is provided, appends an `OutputPromotion` event to the audit log.
///
/// # Errors
///
/// Returns [`PromotionError`] when classification, application, or audit
/// recording fails. Rejected changes produce a receipt with no promoted paths
/// for that change rather than an error.
pub fn promote(
    input: &PromotionInput<'_>,
    gate: &dyn PolicyGate,
    prompt: &dyn PromptSink,
    controller: &mut dyn TrustedController,
    store: Option<&mut dyn AuditStore>,
) -> Result<PromotionReceipt, PromotionError> {
    let mut pipeline = PromotionPipeline {
        controller,
        gate,
        prompt,
    };
    pipeline.promote(input, store)
}

/// The promotion pipeline, borrowing its collaborators.
pub struct PromotionPipeline<'a> {
    /// The trusted controller that owns atomic copy/apply.
    pub controller: &'a mut dyn TrustedController,
    /// The policy gate (Arbitraitor).
    pub gate: &'a dyn PolicyGate,
    /// The prompt sink (trusted UI).
    pub prompt: &'a dyn PromptSink,
}

/// A per-change result collected during promotion.
struct ChangeResult {
    classification: Classification,
    applied: Option<AppliedChange>,
}

impl PromotionPipeline<'_> {
    /// Runs the full promotion pipeline (see [`promote`]).
    ///
    /// # Errors
    ///
    /// Returns [`PromotionError`] on classification or application failure.
    pub fn promote(
        &mut self,
        input: &PromotionInput<'_>,
        store: Option<&mut dyn AuditStore>,
    ) -> Result<PromotionReceipt, PromotionError> {
        let mut promoted = Vec::with_capacity(input.changes.len());
        let mut findings = Vec::new();

        for change in &input.changes {
            let result = self.process_change(change, input.old_content)?;
            if let Some(applied) = &result.applied {
                promoted.push(PromotedPath {
                    path: change.path.clone(),
                    source_digest: compute_change_digest(change),
                    target_digest: applied.target_digest.clone(),
                    output_class: result.classification.output_class,
                });
            }
            if result.classification.credential_shaped {
                findings.push(Finding {
                    severity: FindingSeverity::High,
                    evidence: "credential-shaped content detected".to_string(),
                    affected_paths: vec![change.path.to_string_lossy().into_owned()],
                    violated_rule: "credential_shaped_content".to_string(),
                    proposed_remediation: "Remove secrets before promotion".to_string(),
                });
            }
        }

        let receipt = PromotionReceipt {
            workspace_id: input.workspace_id.clone(),
            source_digest: input.source_digest.clone(),
            target_repository: input.target_repository.clone(),
            paths: promoted,
            findings,
            approvals: Vec::new(),
            resulting_commit: None,
        };

        if let Some(store) = store {
            record_promotion_event(store, &receipt)?;
        }
        Ok(receipt)
    }

    fn process_change(
        &mut self,
        change: &Change,
        old_content: &dyn Fn(&Path) -> Option<Vec<u8>>,
    ) -> Result<ChangeResult, PromotionError> {
        let classification = classify(&ClassificationInput {
            path: &change.path,
            content: Some(&change.content),
            metadata: Some(&change.metadata),
        })?;
        let sensitivity = detect_sensitivity(&change.path);
        let old = old_content(&change.path).unwrap_or_default();
        let semantic = compute_semantic_diff(change.path.clone(), &old, &change.content);

        let decision = self.gate.evaluate(&classification, sensitivity, &semantic);
        let applied = match decision {
            PolicyDecision::Allow => Some(self.controller.apply(&change.path, &change.content)?),
            PolicyDecision::Prompt => {
                let approved = self.prompt.prompt(&format!(
                    "Promote {} ({:?}) to {}?",
                    change.path.display(),
                    classification.output_class,
                    if sensitivity.is_trust_sensitive() {
                        "trust-sensitive destination"
                    } else {
                        "ordinary destination"
                    }
                ));
                if approved {
                    Some(self.controller.apply(&change.path, &change.content)?)
                } else {
                    None
                }
            }
            PolicyDecision::Reject => None,
        };

        Ok(ChangeResult {
            classification,
            applied,
        })
    }
}

fn record_promotion_event(
    store: &mut dyn AuditStore,
    receipt: &PromotionReceipt,
) -> Result<(), PromotionError> {
    let (seq, prev_hash) = {
        let records = store
            .query(&EventQuery::default())
            .map_err(PromotionError::Audit)?;
        let seq = u64::try_from(records.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let prev = records.last().map(|r| r.hash.clone());
        (seq, prev)
    };
    let payload = json!({
        "workspace_id": receipt.workspace_id.as_str(),
        "target_repository": receipt.target_repository.as_str(),
        "promoted_paths": receipt.paths.len(),
        "findings": receipt.findings.len(),
    });
    let envelope = EventEnvelope::try_new(EventEnvelopeInput {
        schema_version: orchestraitor_events::CURRENT_SCHEMA_VERSION,
        monotonic_seq: seq,
        wall_clock_ts: chrono::Utc::now().to_rfc3339(),
        correlation_id: orchestraitor_model::OperationId::from_string("op_promote".to_string()),
        parent_op_id: None,
        category: EventCategory::OutputPromotion,
        payload,
        prev_hash,
    })
    .map_err(PromotionError::Audit)?;
    store.append(envelope).map_err(PromotionError::Audit)?;
    Ok(())
}

fn compute_change_digest(change: &Change) -> Digest {
    let hash = sha2::Sha256::digest(&change.content);
    Digest::new(hex::encode(hash))
}
