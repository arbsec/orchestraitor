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
/// reasons (spec `30-model-routing.md` §9.45 "Routing decision records"). `confidence` is the
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

/// One alternative a [`DecisionProvider`] considered and skipped (spec `30-model-routing.md` §9.45
/// "Routing decision records": per-alternative skip reasons).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionAlternative {
    /// Alternative provider id.
    pub provider: ProviderId,
    /// Alternative model id.
    pub model: String,
    /// Machine-readable skip reason (for example `skipped-because-quota`,
    /// spec `30-model-routing.md` §9.46).
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

/// A typed structured-output task-split proposal (spec `30-model-routing.md` §9.45 decision
/// surface "task splitting"): whether the decision model would decompose one
/// ready task and, when yes, the ordered subtasks it proposes. The proposal
/// grants no authority — the campaign applies it only through the
/// deterministic downstream acceptance path (code lands in a follow-up
/// slice; the typed surface and records land here).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "TaskSplitProposalRaw")]
pub struct TaskSplitProposal {
    /// The deterministic worker task id the split proposal is about.
    pub task_id: String,
    /// Whether the decision model proposes a split at all.
    pub split: bool,
    /// Ordered subtasks when `split` is `true`; empty otherwise.
    pub subtasks: Vec<TaskSplitSubtask>,
    /// Calibrated confidence for the split decision, `0.0..=1.0` (validated).
    pub confidence: f64,
    /// Why the decision model split (or declined to split) the task.
    pub reason: String,
}

/// Raw deserialization shape for [`TaskSplitProposal`]: identical fields,
/// funneled through the same validation as [`TaskSplitProposal::new`].
#[derive(Debug, Deserialize)]
struct TaskSplitProposalRaw {
    task_id: String,
    split: bool,
    #[serde(default)]
    subtasks: Vec<TaskSplitSubtask>,
    confidence: f64,
    #[serde(default)]
    reason: Option<String>,
}

impl TryFrom<TaskSplitProposalRaw> for TaskSplitProposal {
    type Error = DecisionProviderError;

    fn try_from(raw: TaskSplitProposalRaw) -> DecisionResult<Self> {
        Self::new(
            raw.task_id,
            raw.split,
            raw.subtasks,
            raw.confidence,
            raw.reason,
        )
    }
}

/// One proposed subtask of a [`TaskSplitProposal`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSplitSubtask {
    /// Human-readable subtask title (inert data, never executed).
    pub title: String,
    /// Why this subtask is a separate unit of work.
    pub rationale: String,
}

impl TaskSplitProposal {
    /// Validates and constructs a split proposal, rejecting confidence
    /// outside `0.0..=1.0` (including NaN) and subtasks on a no-split
    /// proposal.
    ///
    /// # Errors
    ///
    /// Returns [`DecisionProviderError::InvalidConfidence`] when `confidence`
    /// is NaN, negative, or greater than `1.0`, and
    /// [`DecisionProviderError::MalformedOutput`] when `split` is `false`
    /// but subtasks are present (or `true` with none).
    pub fn new(
        task_id: impl Into<String>,
        split: bool,
        subtasks: Vec<TaskSplitSubtask>,
        confidence: f64,
        reason: Option<String>,
    ) -> DecisionResult<Self> {
        if !(0.0..=1.0).contains(&confidence) {
            return Err(DecisionProviderError::InvalidConfidence { value: confidence });
        }
        if !split && !subtasks.is_empty() {
            return Err(DecisionProviderError::MalformedOutput(
                "task-split proposal with `split: false` must not carry subtasks".to_string(),
            ));
        }
        if split && subtasks.is_empty() {
            return Err(DecisionProviderError::MalformedOutput(
                "task-split proposal with `split: true` must carry at least one subtask"
                    .to_string(),
            ));
        }
        Ok(Self {
            task_id: task_id.into(),
            split,
            subtasks,
            confidence,
            reason: reason.unwrap_or_default(),
        })
    }
}

/// The query context for a tool-selection decision (spec `30-model-routing.md` §9.45 decision
/// surface "tool discovery"): the state a decision model evaluates. Titles
/// and descriptions are inert data carried to the decision model, never
/// executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolQueryContext {
    /// The task or query the tool is selected for.
    pub query: String,
    /// Available tool names with inert descriptions.
    pub available_tools: Vec<ToolDescriptor>,
}

/// One tool a [`ToolQueryContext`] exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// Tool name (for example an MCP tool id).
    pub name: String,
    /// Inert description of what the tool does.
    pub description: String,
}

/// A typed structured-output tool-selection decision (spec `30-model-routing.md` §9.45 decision
/// surface "tool discovery"). `selected_tools` name tools from the query
/// context; the caller validates them against the actual tool registry
/// before any use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ToolSelectionRaw")]
pub struct ToolSelection {
    /// Names of the selected tools, in the order the model ranked them.
    pub selected_tools: Vec<String>,
    /// Calibrated confidence for the selection, `0.0..=1.0` (validated).
    pub confidence: f64,
}

/// Raw deserialization shape for [`ToolSelection`]: identical fields,
/// funneled through the same confidence validation as [`ToolSelection::new`].
#[derive(Debug, Deserialize)]
struct ToolSelectionRaw {
    selected_tools: Vec<String>,
    confidence: f64,
}

impl TryFrom<ToolSelectionRaw> for ToolSelection {
    type Error = DecisionProviderError;

    fn try_from(raw: ToolSelectionRaw) -> DecisionResult<Self> {
        Self::new(raw.selected_tools, raw.confidence)
    }
}

impl ToolSelection {
    /// Validates and constructs a tool selection, rejecting confidence
    /// outside `0.0..=1.0` (including NaN).
    ///
    /// # Errors
    ///
    /// Returns [`DecisionProviderError::InvalidConfidence`] when `confidence`
    /// is NaN, negative, or greater than `1.0`.
    pub fn new(selected_tools: Vec<String>, confidence: f64) -> DecisionResult<Self> {
        if !(0.0..=1.0).contains(&confidence) {
            return Err(DecisionProviderError::InvalidConfidence { value: confidence });
        }
        Ok(Self {
            selected_tools,
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
///
/// The trait covers three decision surfaces: role resolution, campaign
/// task selection, and — as typed surfaces with code landing in follow-up
/// slices — task splitting and tool discovery/selection. Implementors
/// SHOULD support the split/selection surfaces; the defaults below return
/// [`DecisionProviderError::Unsupported`] so adding a method never breaks
/// an existing implementation, and callers treat that error exactly like
/// any other unavailability (deterministic fallback, no loop error).
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

    /// Proposes whether one ready task should be split into subtasks (spec
    /// §9.45 decision surface "task splitting").
    ///
    /// The default returns [`DecisionProviderError::Unsupported`] so
    /// adding this surface never breaks an existing implementation.
    ///
    /// # Errors
    ///
    /// Returns a [`DecisionProviderError`] when the provider cannot produce
    /// a typed proposal. Callers keep the unsplit task.
    async fn propose_task_split(&self, task: &TaskSummary) -> DecisionResult<TaskSplitProposal> {
        let _ = task;
        Err(DecisionProviderError::Unsupported {
            provider_id: self.id().clone(),
            capability: "task-splitting",
        })
    }

    /// Proposes which tools apply to a query (spec `30-model-routing.md` §9.45 decision surface
    /// "tool discovery").
    ///
    /// The default returns [`DecisionProviderError::Unsupported`] so
    /// adding this surface never breaks an existing implementation.
    ///
    /// # Errors
    ///
    /// Returns a [`DecisionProviderError`] when the provider cannot produce
    /// a typed selection. Callers use the static tool set.
    async fn propose_tool_selection(
        &self,
        query: &ToolQueryContext,
    ) -> DecisionResult<ToolSelection> {
        let _ = query;
        Err(DecisionProviderError::Unsupported {
            provider_id: self.id().clone(),
            capability: "tool-selection",
        })
    }
}

/// A minimal summary of one ready task handed to a decision provider for
/// task-splitting (spec `30-model-routing.md` §9.45). Titles are inert data, never executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    /// The deterministic worker task id.
    pub task_id: String,
    /// Human-readable task title.
    pub title: String,
    /// Inert description of the task's goal.
    pub description: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::float_cmp)]

    use super::*;
    use crate::decision_fixture::FixtureDecisionProvider;

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

    #[tokio::test]
    async fn default_split_and_tool_methods_are_unsupported() {
        let fixture = FixtureDecisionProvider::new();
        let task = TaskSummary {
            task_id: "task-a".to_string(),
            title: "write the file".to_string(),
            description: "inert description".to_string(),
        };
        let error = fixture.propose_task_split(&task).await.unwrap_err();
        assert!(matches!(
            error,
            DecisionProviderError::Unsupported {
                capability: "task-splitting",
                ..
            }
        ));
        let query = ToolQueryContext {
            query: "find tests".to_string(),
            available_tools: vec![ToolDescriptor {
                name: "board.query".to_string(),
                description: "inert".to_string(),
            }],
        };
        let error = fixture.propose_tool_selection(&query).await.unwrap_err();
        assert!(matches!(
            error,
            DecisionProviderError::Unsupported {
                capability: "tool-selection",
                ..
            }
        ));
    }

    #[test]
    fn task_split_proposal_validates_shape_and_confidence() {
        let subtask = TaskSplitSubtask {
            title: "part one".to_string(),
            rationale: "independent module".to_string(),
        };
        assert!(TaskSplitProposal::new("task-a", true, vec![subtask.clone()], 0.9, None).is_ok());
        // `split: false` with subtasks is malformed.
        let error =
            TaskSplitProposal::new("task-a", false, vec![subtask.clone()], 0.9, None).unwrap_err();
        assert!(matches!(error, DecisionProviderError::MalformedOutput(_)));
        // `split: true` without subtasks is malformed.
        let error = TaskSplitProposal::new("task-a", true, Vec::new(), 0.9, None).unwrap_err();
        assert!(matches!(error, DecisionProviderError::MalformedOutput(_)));
        // Confidence still validated at the boundary.
        assert!(TaskSplitProposal::new("task-a", false, Vec::new(), 1.5, None).is_err());
    }

    #[test]
    fn split_and_tool_selection_round_trip_through_json() {
        let proposal = TaskSplitProposal::new(
            "task-a",
            true,
            vec![TaskSplitSubtask {
                title: "part one".to_string(),
                rationale: "independent module".to_string(),
            }],
            0.8,
            Some("two independent modules".to_string()),
        )
        .unwrap();
        let json = serde_json::to_string(&proposal).unwrap();
        let parsed: TaskSplitProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, proposal);

        let selection = ToolSelection::new(vec!["board.query".to_string()], 0.7).unwrap();
        let json = serde_json::to_string(&selection).unwrap();
        let parsed: ToolSelection = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, selection);
    }

    #[test]
    fn tool_selection_rejects_out_of_range_confidence() {
        assert!(ToolSelection::new(Vec::new(), 1.01).is_err());
        assert!(ToolSelection::new(Vec::new(), f64::NAN).is_err());
        assert!(ToolSelection::new(vec!["t".to_string()], 0.0).is_ok());
    }
}
