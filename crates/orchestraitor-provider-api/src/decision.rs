//! The [`DecisionProvider`] trait: typed structured outputs with calibrated
//! confidence for decision-model-backed selection (spec `30-model-routing.md`
//! §9.45). A decision provider proposes role-to-model resolutions and the
//! campaign session's task-selection decision ([spec `10-orchestrator.md`
//! §9.35]); the heuristic routing table stays the default and the fallback
//! chain. This is a NEW provider class: a single-shot structured-output
//! request/response boundary, deliberately not a
//! [`crate::transport::ProviderTransport`] — no
//! message streams, no chat surface, and no capability coupling.
//!
//! Provider outputs are untrusted typed data: a proposal grants no authority,
//! it only names `(provider, model)` candidates the router may adopt after
//! validation. Confidence is a calibrated probability in `0.0..=1.0` validated
//! at the boundary — NaN, infinity, and out-of-range values are rejected as
//! typed errors, never clamped.

use async_trait::async_trait;
use orchestraitor_model::ProviderId;
use serde::{Deserialize, Serialize};

use crate::error::DecisionProviderError;

/// Convenience result type for decision provider operations.
pub type DecisionResult<T> = Result<T, DecisionProviderError>;

/// A typed structured-output proposal from a [`DecisionProvider`].
///
/// The proposal names a `(provider, model)` resolution for one role plus the
/// alternatives the decision model considered with per-alternative skip
/// reasons (spec §9.45 "Routing decision records"). `confidence` is the
/// provider's calibrated probability for the primary proposal, validated to
/// `0.0..=1.0` at construction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "DecisionProposalRaw")]
pub struct DecisionProposal {
    /// Orchestration role id the proposal resolves.
    pub role: String,
    /// Proposed provider id.
    pub provider: ProviderId,
    /// Proposed model id.
    pub model: String,
    /// Calibrated confidence for the primary proposal, `0.0..=1.0`.
    pub confidence: f64,
    /// Alternatives the decision model considered with skip reasons.
    pub alternatives: Vec<DecisionAlternative>,
}

/// Raw deserialization shape for [`DecisionProposal`]: identical fields, funneled
/// through the same confidence validation as [`DecisionProposal::new`] so no
/// serde path can construct a proposal with uncalibrated confidence.
#[derive(Debug, Deserialize)]
struct DecisionProposalRaw {
    role: String,
    provider: ProviderId,
    model: String,
    confidence: f64,
    #[serde(default)]
    alternatives: Vec<DecisionAlternative>,
}

impl TryFrom<DecisionProposalRaw> for DecisionProposal {
    type Error = DecisionProviderError;

    fn try_from(raw: DecisionProposalRaw) -> DecisionResult<Self> {
        Self::new(
            raw.role,
            raw.provider,
            raw.model,
            raw.confidence,
            raw.alternatives,
        )
    }
}

/// One alternative a [`DecisionProvider`] considered and skipped (spec §9.45
/// "Routing decision records": per-alternative skip reasons).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionAlternative {
    /// Alternative provider id.
    pub provider: ProviderId,
    /// Alternative model id.
    pub model: String,
    /// Machine-readable skip reason (for example `skipped-because-quota`,
    /// spec §9.46).
    pub skip_reason: String,
}

impl DecisionProposal {
    /// Validates and constructs a proposal, rejecting confidence outside
    /// `0.0..=1.0` (including NaN) as a typed error.
    ///
    /// # Errors
    ///
    /// Returns [`DecisionProviderError::InvalidConfidence`] when `confidence`
    /// is NaN, negative, or greater than `1.0`.
    pub fn new(
        role: impl Into<String>,
        provider: ProviderId,
        model: impl Into<String>,
        confidence: f64,
        alternatives: Vec<DecisionAlternative>,
    ) -> DecisionResult<Self> {
        if !(0.0..=1.0).contains(&confidence) {
            return Err(DecisionProviderError::InvalidConfidence { value: confidence });
        }
        Ok(Self {
            role: role.into(),
            provider,
            model: model.into(),
            confidence,
            alternatives,
        })
    }
}

/// A typed structured-output task-selection decision for a campaign session
/// (spec `10-orchestrator.md` §9.35). `task_id` is the deterministic worker
/// task id from the ready queue; the heuristic selector stays the fallback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "TaskSelectionRaw")]
pub struct TaskSelection {
    /// The selected deterministic worker task id.
    pub task_id: String,
    /// Calibrated confidence for the selection, `0.0..=1.0` (validated).
    pub confidence: f64,
}

/// Raw deserialization shape for [`TaskSelection`]: identical fields, funneled
/// through the same confidence validation as [`TaskSelection::new`].
#[derive(Debug, Deserialize)]
struct TaskSelectionRaw {
    task_id: String,
    confidence: f64,
}

impl TryFrom<TaskSelectionRaw> for TaskSelection {
    type Error = DecisionProviderError;

    fn try_from(raw: TaskSelectionRaw) -> DecisionResult<Self> {
        Self::new(raw.task_id, raw.confidence)
    }
}

impl TaskSelection {
    /// Validates and constructs a task selection, rejecting confidence
    /// outside `0.0..=1.0` (including NaN) as a typed error.
    ///
    /// # Errors
    ///
    /// Returns [`DecisionProviderError::InvalidConfidence`] when `confidence`
    /// is NaN, negative, or greater than `1.0`.
    pub fn new(task_id: impl Into<String>, confidence: f64) -> DecisionResult<Self> {
        if !(0.0..=1.0).contains(&confidence) {
            return Err(DecisionProviderError::InvalidConfidence { value: confidence });
        }
        Ok(Self {
            task_id: task_id.into(),
            confidence,
        })
    }
}

/// Project-owned decision-provider abstraction for decision-model-backed
/// selection (spec `30-model-routing.md` §9.45).
///
/// This is a separate provider class from [`crate::transport::ProviderTransport`]:
/// a decision provider answers single-shot structured-output requests — no
/// message streams, no chat surface. Implementations return typed outputs with
/// calibrated confidence; the heuristic table stays the default and the
/// fallback chain when a provider errors or is unavailable.
#[async_trait]
pub trait DecisionProvider: Send + Sync {
    /// Stable provider id used in decision records.
    fn id(&self) -> &ProviderId;

    /// Proposes a `(provider, model)` resolution for one orchestration role.
    ///
    /// # Errors
    ///
    /// Returns a [`DecisionProviderError`] when the provider cannot produce a
    /// typed proposal (unavailable, transport failure, or an unparseable
    /// response). Callers fall back to the heuristic table.
    async fn propose_role_resolution(&self, role: &str) -> DecisionResult<DecisionProposal>;

    /// Proposes the campaign session's task-selection decision (spec
    /// `10-orchestrator.md` §9.35) over the ready-queue task ids.
    ///
    /// # Errors
    ///
    /// Returns a [`DecisionProviderError`] when the provider cannot produce a
    /// typed selection. Callers fall back to the heuristic selector.
    async fn propose_task_selection(
        &self,
        ready_task_ids: &[String],
    ) -> DecisionResult<TaskSelection>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::float_cmp)]

    use super::*;

    fn provider_id(name: &str) -> ProviderId {
        ProviderId::from_string(name.to_string())
    }

    #[test]
    fn proposal_rejects_confidence_above_one() {
        let error = DecisionProposal::new(
            "implement",
            provider_id("neuralwatt"),
            "glm-5.2",
            1.5,
            Vec::new(),
        )
        .unwrap_err();
        assert!(
            matches!(error, DecisionProviderError::InvalidConfidence { value } if value == 1.5)
        );
    }

    #[test]
    fn proposal_rejects_negative_confidence() {
        let error =
            DecisionProposal::new("plan", provider_id("p"), "m", -0.1, Vec::new()).unwrap_err();
        assert!(
            matches!(error, DecisionProviderError::InvalidConfidence { value } if (value + 0.1).abs() < f64::EPSILON)
        );
    }

    #[test]
    fn proposal_rejects_nan_confidence() {
        let error =
            DecisionProposal::new("plan", provider_id("p"), "m", f64::NAN, Vec::new()).unwrap_err();
        assert!(matches!(
            error,
            DecisionProviderError::InvalidConfidence { value } if value.is_nan()
        ));
    }

    #[test]
    fn proposal_accepts_confidence_bounds() {
        for confidence in [0.0, 1.0] {
            let proposal = DecisionProposal::new(
                "plan",
                provider_id("p"),
                "m",
                confidence,
                vec![DecisionAlternative {
                    provider: provider_id("alt"),
                    model: "alt-model".to_string(),
                    skip_reason: "skipped-because-quota".to_string(),
                }],
            )
            .unwrap();
            assert!((proposal.confidence - confidence).abs() < f64::EPSILON);
            assert_eq!(proposal.alternatives.len(), 1);
        }
    }

    #[test]
    fn task_selection_rejects_out_of_range_and_nan_confidence() {
        assert!(TaskSelection::new("task-a", 1.01).is_err());
        assert!(TaskSelection::new("task-a", -1.0).is_err());
        assert!(TaskSelection::new("task-a", f64::NAN).is_err());
        assert!(TaskSelection::new("task-a", 0.5).is_ok());
    }

    #[test]
    fn proposal_round_trips_through_json() {
        let proposal = DecisionProposal::new(
            "implement",
            provider_id("neuralwatt"),
            "glm-5.2",
            0.95,
            vec![DecisionAlternative {
                provider: provider_id("neuralwatt"),
                model: "glm-5.2-flash".to_string(),
                skip_reason: "skipped-because-quota".to_string(),
            }],
        )
        .unwrap();
        let json = serde_json::to_string(&proposal).unwrap();
        let parsed: DecisionProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, proposal);
    }

    #[test]
    fn deserialization_rejects_confidence_above_one() {
        // serde_json's arbitrary_precision-less float parse accepts 2.0; the
        // try_from funnel must reject it as a typed error.
        let error = serde_json::from_str::<DecisionProposal>(
            r#"{"role":"plan","provider":"p","model":"m","confidence":2.0,"alternatives":[]}"#,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("invalid confidence 2"),
            "deserialization must reject confidence > 1.0: {error}"
        );
        let error =
            serde_json::from_str::<TaskSelection>(r#"{"task_id":"task-a","confidence":2.0}"#)
                .unwrap_err();
        assert!(
            error.to_string().contains("invalid confidence 2"),
            "task-selection deserialization must reject confidence > 1.0: {error}"
        );
    }

    #[test]
    fn deserialization_rejects_negative_confidence() {
        let error = serde_json::from_str::<DecisionProposal>(
            r#"{"role":"plan","provider":"p","model":"m","confidence":-1.0,"alternatives":[]}"#,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("invalid confidence -1"),
            "deserialization must reject negative confidence: {error}"
        );
        let error =
            serde_json::from_str::<TaskSelection>(r#"{"task_id":"task-a","confidence":-1.0}"#)
                .unwrap_err();
        assert!(
            error.to_string().contains("invalid confidence -1"),
            "task-selection deserialization must reject negative confidence: {error}"
        );
    }

    #[test]
    fn deserialization_rejects_non_finite_confidence() {
        // JSON has no NaN/Infinity literal, but the string form parses as a
        // float error and the `null` form must also be rejected; the serde
        // funnel guarantees no code path constructs an uncalibrated value.
        assert!(
            serde_json::from_str::<DecisionProposal>(
                r#"{"role":"plan","provider":"p","model":"m","confidence":"NaN","alternatives":[]}"#,
            )
            .is_err(),
            "string NaN must not deserialize as a valid confidence"
        );
        assert!(
            serde_json::from_str::<TaskSelection>(r#"{"task_id":"task-a","confidence":null}"#)
                .is_err()
        );
    }

    #[test]
    fn deserialization_preserves_in_range_confidence() {
        let parsed: DecisionProposal = serde_json::from_str(
            r#"{"role":"plan","provider":"p","model":"m","confidence":0.42,"alternatives":[]}"#,
        )
        .unwrap();
        assert!((parsed.confidence - 0.42).abs() < f64::EPSILON);
        let parsed: TaskSelection =
            serde_json::from_str(r#"{"task_id":"task-a","confidence":0.0}"#).unwrap();
        assert!((parsed.confidence - 0.0).abs() < f64::EPSILON);
    }
}
