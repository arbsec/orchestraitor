//! Task metadata generated from a specification (spec §9.33.2).
//!
//! Each backlog task is a thin vertical slice carrying everything the runner,
//! reviewers, and the audit trail need: traceability to spec requirements,
//! DAG edges, routing and retry policy references, and the evidence that must
//! exist before the task counts as done. Validity of the *content* (does the
//! named check exist in CI, does the domain exist in the agent catalog) is
//! enforced by the consumers; this module enforces structural validity.

use serde::{Deserialize, Serialize};
use thiserror::Error;

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

/// Risk classification (mirrors the delivery board's Risk field).
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Autonomy {
    /// Runs without human interaction; may still hit Arbitraitor approval gates.
    Full,
    /// Autonomous execution with human-visible checkpoints.
    #[default]
    Guided,
    /// Every step is gated on the operator.
    Manual,
}

/// Named required-verification check (spec §21.10 registry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRef {
    /// Registry name of the check (e.g. `cargo-clippy`, `nextest-workspace`).
    pub name: String,
    /// Invocation override when the registry default does not suffice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

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
    /// The task identifier is empty.
    #[error("task id must not be empty")]
    EmptyId,
    /// The title is empty.
    #[error("task title must not be empty")]
    EmptyTitle,
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
    /// The domain is empty.
    #[error("task {id} must carry a domain classification")]
    EmptyDomain {
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
    /// Security-reviewer requirement is mandatory for critical risk (spec §9.33.4).
    #[error("critical-risk task {id} must require the security reviewer domain")]
    CriticalWithoutSecurityReview {
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
    /// Risk classification driving review strictness and human gates.
    pub risk: RiskClass,
    /// Files or components the change is expected to touch (review scoping).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_files: Vec<String>,
    /// Named checks that must pass before review (spec §9.33.2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_verification: Vec<VerificationRef>,
    /// Reviewer domains that must cover the change set (spec §9.33.4).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_reviewer_domains: Vec<DomainId>,
    /// Autonomy level (default `guided`).
    #[serde(default)]
    pub autonomy: Autonomy,
    /// Explicit model route override; `None` defers to the §9.19.2 chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<String>,
    /// Named §9.26 retry/budget policy profile (default `default`).
    #[serde(default = "default_retry_policy")]
    pub retry_policy: String,
    /// Evidence that must exist before the task counts as done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completion_evidence: Vec<CompletionEvidence>,
}

fn default_retry_policy() -> String {
    "default".to_string()
}

impl TaskMetadata {
    /// Validates structural invariants (spec §9.33.2 fields and §9.33.4 rules).
    ///
    /// # Errors
    ///
    /// Returns the first [`MetadataError`] encountered.
    pub fn validate(&self) -> Result<(), MetadataError> {
        if self.id.as_str().is_empty() {
            return Err(MetadataError::EmptyId);
        }
        let id = self.id.to_string();
        if self.title.is_empty() {
            return Err(MetadataError::EmptyTitle);
        }
        if self.spec_refs.is_empty() {
            return Err(MetadataError::NoSpecRefs { id });
        }
        if self.acceptance_criteria.is_empty() {
            return Err(MetadataError::NoAcceptanceCriteria { id });
        }
        if self.domain.as_str().is_empty() {
            return Err(MetadataError::EmptyDomain { id });
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
        if self.risk == RiskClass::Critical
            && !self
                .required_reviewer_domains
                .iter()
                .any(|d| d.as_str() == "security")
        {
            return Err(MetadataError::CriticalWithoutSecurityReview { id });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

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
            expected_files: vec!["crates/orchestraitor-delivery/src/dag.rs".to_string()],
            required_verification: vec![VerificationRef {
                name: "nextest-workspace".to_string(),
                command: None,
            }],
            required_reviewer_domains: vec![DomainId::new("backend")],
            autonomy: Autonomy::Guided,
            routing: None,
            retry_policy: default_retry_policy(),
            completion_evidence: vec![CompletionEvidence::Verification {
                name: "nextest-workspace".to_string(),
            }],
        }
    }

    #[test]
    fn metadata_round_trips_through_json() {
        let task = sample();
        let json = serde_json::to_string_pretty(&task).unwrap();
        let back: TaskMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(task, back);
    }

    #[test]
    fn defaults_are_applied_on_deserialize() {
        let json = r#"{
            "id": "review-convergence",
            "spec_refs": ["9.33.4"],
            "title": "Convergence check",
            "objective": "One full generation with no new findings converges",
            "acceptance_criteria": ["no noteworthy findings converges"],
            "dependencies": [],
            "domain": "testing",
            "risk": "high"
        }"#;
        let task: TaskMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(task.autonomy, Autonomy::Guided);
        assert_eq!(task.retry_policy, "default");
        assert!(task.required_verification.is_empty());
        assert!(task.completion_evidence.is_empty());
    }

    #[test]
    fn valid_metadata_passes_validation() {
        sample().validate().unwrap();
    }

    #[test]
    fn empty_id_is_rejected() {
        let mut task = sample();
        task.id = BacklogTaskId::new("");
        assert_eq!(task.validate(), Err(MetadataError::EmptyId));
    }

    #[test]
    fn empty_title_is_rejected() {
        let mut task = sample();
        task.title.clear();
        assert_eq!(task.validate(), Err(MetadataError::EmptyTitle));
    }

    #[test]
    fn missing_spec_refs_break_traceability() {
        let mut task = sample();
        task.spec_refs.clear();
        assert_eq!(
            task.validate(),
            Err(MetadataError::NoSpecRefs {
                id: "delivery-dag-toposort".to_string()
            })
        );
    }

    #[test]
    fn missing_acceptance_criteria_is_rejected() {
        let mut task = sample();
        task.acceptance_criteria.clear();
        assert!(matches!(
            task.validate(),
            Err(MetadataError::NoAcceptanceCriteria { .. })
        ));
    }

    #[test]
    fn empty_domain_is_rejected() {
        let mut task = sample();
        task.domain = DomainId::new("");
        assert!(matches!(
            task.validate(),
            Err(MetadataError::EmptyDomain { .. })
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
    fn critical_risk_requires_security_reviewer() {
        let mut task = sample();
        task.risk = RiskClass::Critical;
        assert_eq!(
            task.validate(),
            Err(MetadataError::CriticalWithoutSecurityReview {
                id: "delivery-dag-toposort".to_string()
            })
        );
        task.required_reviewer_domains
            .push(DomainId::new("security"));
        task.validate().unwrap();
    }
}
