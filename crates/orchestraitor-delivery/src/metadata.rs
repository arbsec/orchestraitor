//! Task metadata generated from a specification (spec §9.33.2).
//!
//! Each backlog task is a thin vertical slice carrying everything the runner,
//! reviewers, and the audit trail need: traceability to spec requirements,
//! DAG edges, domain and risk/data-sensitivity classification, routing and
//! retry policy references, and the evidence that must exist before the task
//! counts as done. Structural validity is enforced here; semantic validity
//! (does the named CI check exist, does the domain exist in the agent
//! catalog) is enforced by the consumers.
//!
//! Durable persistence note (spec §9.33.6): serializers that store
//! [`TaskMetadata`] MUST wrap it in a versioned envelope keyed by
//! [`SCHEMA_VERSION`] and MUST run [`TaskMetadata::validate`] immediately after
//! loading, so future schema changes fail visibly instead of silently
//! deserializing into wrong defaults.

use orchestraitor_model::DataSensitivity;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Current schema version of the task metadata record, for the versioned
/// persistence envelope used by durable storage (§9.33.6).
pub const SCHEMA_VERSION: u32 = 1;

/// Stable backlog task identifier (spec §9.33.2 "stable ID").
///
/// Unlike session or workspace identifiers, this is deterministic: the same
/// decomposition of the same specification MUST produce the same task ID, so
/// it is a human-assigned slug, not a generated UUID.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BacklogTaskId(String);

impl BacklogTaskId {
    /// Wraps an identifier produced by the task planner.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the underlying identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for BacklogTaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Reference to a spec requirement (e.g. `9.33.2`, `MVP-4`, `AGENTS.md`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpecRef(String);

impl SpecRef {
    /// Wraps a spec section anchor.
    #[must_use]
    pub fn new(section: impl Into<String>) -> Self {
        Self(section.into())
    }

    /// Returns the section anchor.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Agent-catalog domain identifier (spec §9.19.1; e.g. `backend`, `security`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DomainId(String);

impl DomainId {
    /// Wraps a catalog domain identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the underlying identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Delivery-board risk classification (Low–Critical), distinct from
/// [`DataSensitivity`]: risk drives review strictness and escalation, while
/// data sensitivity drives §9.28 routing constraints (§9.33.2 "domain and risk
/// classification ... maps to §9.19.1 domain + §9.28 data sensitivity").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    /// Reviewer defaults suffice.
    Low,
    /// Standard review.
    Medium,
    /// Demands extra reviewers or stricter verification.
    High,
    /// Human sign-off required before release (spec §21.1).
    Critical,
}

/// Autonomy level of the implementer for this task (spec §9.33.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Autonomy {
    /// Runs without human interaction; may still hit Arbitraitor approval gates.
    Full,
    /// Autonomous execution with human-visible checkpoints.
    Guided,
    /// Every step is gated on the operator.
    Manual,
}

/// Reference to a named required-verification check in the project-managed
/// verification registry (spec §21.10). Metadata carries the stable registry
/// key only; executable invocations live in the trusted registry, never in
/// backlog content (backlog metadata is untrusted input, spec §6.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VerificationRef(pub String);

/// Evidence that proves a task is done (spec §9.33.2 "completion evidence").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompletionEvidence {
    /// A named required-verification check passed and the record is retained.
    Verification {
        /// Registry name of the check.
        name: String,
    },
    /// An artifact was produced and is retained in the event/CAS store.
    Report {
        /// Path to the retained artifact, relative to the session evidence root.
        path: String,
    },
    /// Human-visible note recorded by the implementer or reviewer.
    Note {
        /// Free-text evidence summary (never secrets).
        text: String,
    },
}

/// Structural validation failure for [`TaskMetadata`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MetadataError {
    /// The task identifier is empty or whitespace.
    #[error("task id must not be empty")]
    EmptyId,
    /// The title is empty or whitespace.
    #[error("task title must not be empty")]
    EmptyTitle,
    /// The objective is empty or whitespace.
    #[error("task {id} must have a non-empty objective")]
    EmptyObjective {
        /// Offending task.
        id: String,
    },
    /// No spec requirement references; traceability (§9.33.2) requires ≥1.
    #[error("task {id} must reference at least one spec requirement")]
    NoSpecRefs {
        /// Offending task.
        id: String,
    },
    /// No acceptance criteria; the runner cannot decide completion.
    #[error("task {id} must define at least one acceptance criterion")]
    NoAcceptanceCriteria {
        /// Offending task.
        id: String,
    },
    /// The domain is empty or whitespace.
    #[error("task {id} must carry a domain classification")]
    EmptyDomain {
        /// Offending task.
        id: String,
    },
    /// No required-verification checks; completion is undecidable (§9.33.2).
    #[error("task {id} must require at least one named verification")]
    NoVerification {
        /// Offending task.
        id: String,
    },
    /// No required reviewer domains; review capacity cannot be planned (§9.33.4).
    #[error("task {id} must require at least one reviewer domain")]
    NoReviewerDomains {
        /// Offending task.
        id: String,
    },
    /// No completion evidence kinds; done-ness cannot be proven (§9.33.2).
    #[error("task {id} must name at least one completion-evidence kind")]
    NoCompletionEvidence {
        /// Offending task.
        id: String,
    },
    /// The named retry-policy profile is empty.
    #[error("task {id} must name a retry-policy profile")]
    EmptyRetryPolicy {
        /// Offending task.
        id: String,
    },
    /// Task depends on itself.
    #[error("task {id} must not depend on itself")]
    SelfDependency {
        /// Offending task.
        id: String,
    },
    /// Duplicate dependency edge.
    #[error("task {id} lists dependency {dep} more than once")]
    DuplicateDependency {
        /// Offending task.
        id: String,
        /// Duplicated dependency.
        dep: String,
    },
    /// Security-reviewer requirement is mandatory for security-relevant tasks
    /// (§9.33.4: task domain is `security`, risk is `Critical`, or data
    /// sensitivity is Confidential/Restricted).
    #[error("security-relevant task {id} must require the security reviewer domain")]
    SecurityReviewRequired {
        /// Offending task.
        id: String,
    },
}

/// Backlog task metadata (spec §9.33.2): one bounded, independently testable,
/// revertible unit of delivery work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskMetadata {
    /// Stable, deterministic task identifier.
    pub id: BacklogTaskId,
    /// Spec requirement references (traceability spec → task → evidence).
    pub spec_refs: Vec<SpecRef>,
    /// Short title.
    pub title: String,
    /// Objective in one bounded sentence.
    pub objective: String,
    /// Acceptance criteria the verifier checks.
    pub acceptance_criteria: Vec<String>,
    /// DAG edges: tasks that must complete first.
    pub dependencies: Vec<BacklogTaskId>,
    /// Agent-catalog domain for routing and reviewer selection.
    pub domain: DomainId,
    /// Delivery-board risk classification driving review strictness.
    pub risk: RiskClass,
    /// §9.28 data sensitivity of what the task reads/writes, used for routing.
    pub data_sensitivity: DataSensitivity,
    /// Files or components the change is expected to touch (review scoping).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_files: Vec<String>,
    /// Named registry checks that must pass before review (spec §9.33.2).
    pub required_verification: Vec<VerificationRef>,
    /// Reviewer domains that must cover the change set (spec §9.33.4).
    pub required_reviewer_domains: Vec<DomainId>,
    /// Autonomy level.
    pub autonomy: Autonomy,
    /// Explicit model route override; `None` defers to the §9.19.2 chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<String>,
    /// Named §9.26 retry/budget policy profile.
    pub retry_policy: String,
    /// Evidence that must exist before the task counts as done.
    pub completion_evidence: Vec<CompletionEvidence>,
}

impl TaskMetadata {
    /// Returns true when the task is security-relevant for reviewer selection
    /// (§9.33.4): security domain, critical risk, or confidential/restricted data.
    #[must_use]
    pub fn is_security_relevant(&self) -> bool {
        self.risk == RiskClass::Critical
            || self.data_sensitivity >= DataSensitivity::Confidential
            || self.domain.as_str() == "security"
    }

    /// Validates structural invariants (spec §9.33.2 fields and §9.33.4 rules).
    ///
    /// # Errors
    ///
    /// Returns the first [`MetadataError`] encountered.
    pub fn validate(&self) -> Result<(), MetadataError> {
        if self.id.as_str().trim().is_empty() {
            return Err(MetadataError::EmptyId);
        }
        let id = self.id.to_string();
        if self.title.trim().is_empty() {
            return Err(MetadataError::EmptyTitle);
        }
        if self.objective.trim().is_empty() {
            return Err(MetadataError::EmptyObjective { id });
        }
        if self.spec_refs.is_empty() {
            return Err(MetadataError::NoSpecRefs { id });
        }
        if self.spec_refs.iter().any(|r| r.as_str().trim().is_empty()) {
            return Err(MetadataError::NoSpecRefs { id });
        }
        if self.domain.as_str().trim().is_empty() {
            return Err(MetadataError::EmptyDomain { id });
        }
        if self.acceptance_criteria.is_empty()
            || self.acceptance_criteria.iter().any(|c| c.trim().is_empty())
        {
            return Err(MetadataError::NoAcceptanceCriteria { id });
        }
        if self.required_verification.is_empty()
            || self
                .required_verification
                .iter()
                .any(|v| v.0.trim().is_empty())
        {
            return Err(MetadataError::NoVerification { id });
        }
        if self.required_reviewer_domains.is_empty() {
            return Err(MetadataError::NoReviewerDomains { id });
        }
        if self.completion_evidence.is_empty() {
            return Err(MetadataError::NoCompletionEvidence { id });
        }
        if self.retry_policy.trim().is_empty() {
            return Err(MetadataError::EmptyRetryPolicy { id });
        }
        if self.dependencies.contains(&self.id) {
            return Err(MetadataError::SelfDependency { id });
        }
        let mut seen = Vec::with_capacity(self.dependencies.len());
        for dep in &self.dependencies {
            if seen.contains(dep) {
                return Err(MetadataError::DuplicateDependency {
                    id,
                    dep: dep.to_string(),
                });
            }
            seen.push(dep.clone());
        }
        if self.is_security_relevant()
            && !self
                .required_reviewer_domains
                .iter()
                .any(|d| d.as_str() == "security")
        {
            return Err(MetadataError::SecurityReviewRequired { id });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn sample() -> TaskMetadata {
        TaskMetadata {
            id: BacklogTaskId::new("delivery-dag-toposort"),
            spec_refs: vec![SpecRef::new("9.33.3")],
            title: "TaskDag topological sort".to_string(),
            objective: "Dependency-satisfied eligibility for the backlog".to_string(),
            acceptance_criteria: vec!["cycle detection errors deterministically".to_string()],
            dependencies: vec![BacklogTaskId::new("delivery-metadata")],
            domain: DomainId::new("backend"),
            risk: RiskClass::Medium,
            data_sensitivity: DataSensitivity::Internal,
            expected_files: vec!["crates/orchestraitor-delivery/src/dag.rs".to_string()],
            required_verification: vec![VerificationRef("nextest-workspace".to_string())],
            required_reviewer_domains: vec![DomainId::new("backend")],
            autonomy: Autonomy::Guided,
            routing: None,
            retry_policy: "default".to_string(),
            completion_evidence: vec![CompletionEvidence::Verification {
                name: "nextest-workspace".to_string(),
            }],
        }
    }

    #[test]
    fn metadata_round_trips_through_json() -> TestResult {
        let task = sample();
        let json = serde_json::to_string_pretty(&task)?;
        let back: TaskMetadata = serde_json::from_str(&json)?;
        assert_eq!(task, back);
        Ok(())
    }

    #[test]
    fn deserializing_old_or_partial_records_fails_visibly() {
        // No silent defaults: privacy-relevant and completion-contract fields
        // are mandatory in serialized form.
        let json = r#"{
            "id": "legacy-task",
            "spec_refs": ["9.33.2"],
            "title": "legacy",
            "objective": "pre-schema record",
            "acceptance_criteria": ["x"],
            "dependencies": [],
            "domain": "backend",
            "risk": "low"
        }"#;
        assert!(serde_json::from_str::<TaskMetadata>(json).is_err());
    }

    #[test]
    fn valid_internal_task_passes_validation() -> TestResult {
        sample().validate()?;
        Ok(())
    }

    #[test]
    fn empty_id_is_rejected() {
        let mut task = sample();
        task.id = BacklogTaskId::new("");
        assert_eq!(task.validate(), Err(MetadataError::EmptyId));
    }

    #[test]
    fn whitespace_id_is_rejected() {
        let mut task = sample();
        task.id = BacklogTaskId::new("\u{2003}");
        assert_eq!(task.validate(), Err(MetadataError::EmptyId));
    }

    #[test]
    fn blank_criterion_among_valid_ones_is_rejected() {
        let mut task = sample();
        task.acceptance_criteria = vec!["real criterion".to_string(), " ".to_string()];
        assert!(matches!(
            task.validate(),
            Err(MetadataError::NoAcceptanceCriteria { .. })
        ));
    }

    #[test]
    fn whitespace_title_is_rejected() {
        let mut task = sample();
        task.title = "   ".to_string();
        assert_eq!(task.validate(), Err(MetadataError::EmptyTitle));
    }

    #[test]
    fn empty_objective_is_rejected() {
        let mut task = sample();
        task.objective.clear();
        assert!(matches!(
            task.validate(),
            Err(MetadataError::EmptyObjective { .. })
        ));
    }

    #[test]
    fn missing_spec_refs_break_traceability() {
        let mut task = sample();
        task.spec_refs.clear();
        assert!(matches!(
            task.validate(),
            Err(MetadataError::NoSpecRefs { .. })
        ));
    }

    #[test]
    fn blank_acceptance_criteria_are_rejected() {
        let mut task = sample();
        task.acceptance_criteria = vec!["  ".to_string()];
        assert!(matches!(
            task.validate(),
            Err(MetadataError::NoAcceptanceCriteria { .. })
        ));
    }

    #[test]
    fn empty_acceptance_criteria_list_is_rejected() {
        let mut task = sample();
        task.acceptance_criteria.clear();
        assert!(matches!(
            task.validate(),
            Err(MetadataError::NoAcceptanceCriteria { .. })
        ));
    }

    #[test]
    fn missing_verification_is_rejected() {
        let mut task = sample();
        task.required_verification.clear();
        assert!(matches!(
            task.validate(),
            Err(MetadataError::NoVerification { .. })
        ));
    }

    #[test]
    fn missing_reviewer_domains_are_rejected() {
        let mut task = sample();
        task.required_reviewer_domains.clear();
        assert!(matches!(
            task.validate(),
            Err(MetadataError::NoReviewerDomains { .. })
        ));
    }

    #[test]
    fn missing_completion_evidence_is_rejected() {
        let mut task = sample();
        task.completion_evidence.clear();
        assert!(matches!(
            task.validate(),
            Err(MetadataError::NoCompletionEvidence { .. })
        ));
    }

    #[test]
    fn empty_retry_policy_is_rejected() {
        let mut task = sample();
        task.retry_policy.clear();
        assert!(matches!(
            task.validate(),
            Err(MetadataError::EmptyRetryPolicy { .. })
        ));
    }

    #[test]
    fn self_dependency_is_rejected() {
        let mut task = sample();
        task.dependencies.push(task.id.clone());
        assert!(matches!(
            task.validate(),
            Err(MetadataError::SelfDependency { .. })
        ));
    }

    #[test]
    fn duplicate_dependency_is_rejected() {
        let mut task = sample();
        task.dependencies
            .push(BacklogTaskId::new("delivery-metadata"));
        assert!(matches!(
            task.validate(),
            Err(MetadataError::DuplicateDependency { .. })
        ));
    }

    #[test]
    fn security_relevance_requires_security_reviewer() -> TestResult {
        // Critical risk.
        let mut task = sample();
        task.risk = RiskClass::Critical;
        assert!(matches!(
            task.validate(),
            Err(MetadataError::SecurityReviewRequired { .. })
        ));
        task.required_reviewer_domains
            .push(DomainId::new("security"));
        task.validate()?;

        // Security domain alone triggers it.
        let mut task = sample();
        task.domain = DomainId::new("security");
        assert!(matches!(
            task.validate(),
            Err(MetadataError::SecurityReviewRequired { .. })
        ));

        // Confidential/Restricted data triggers it; Internal does not.
        let mut task = sample();
        task.data_sensitivity = DataSensitivity::Confidential;
        assert!(matches!(
            task.validate(),
            Err(MetadataError::SecurityReviewRequired { .. })
        ));
        task.data_sensitivity = DataSensitivity::Internal;
        task.validate()?;
        Ok(())
    }
}
