//! Parallel scheduling over the backlog DAG (spec §9.33.3).
//!
//! Pure, deterministic selection of which eligible tasks may start. It
//! enforces orchestration-side constraints that live in Orchestraitor:
//! global concurrency, per-domain parallelism caps, review capacity, and
//! expected-file overlap between in-flight tasks (repository conflicts).
//!
//! What is deliberately NOT here: security verdicts (§2.2, §9.33.7) and
//! provider/token budget enforcement (§9.19.5–9.19.6, §9.27). The runner
//! consults the cost-ledger and provider registry before dispatching; this
//! module only orders and caps.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::dag::TaskDag;
use crate::metadata::{BacklogTaskId, DomainId};

/// Scheduler configuration; parameterized per §9.33 ("sane defaults,
/// configurable through §9.22").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchedulerConfig {
    /// Maximum tasks implementing concurrently.
    pub max_concurrent: usize,
    /// Per-domain concurrency cap; `None` bounds by `max_concurrent` only.
    pub max_per_domain: Option<usize>,
    /// Maximum in-flight change sets — running implementations plus those
    /// awaiting review. Every running implementation is on its way to
    /// becoming a change set, so both count against the bound; starting new
    /// implementations pauses while in-flight work reaches capacity
    /// (spec §9.33.3 review capacity).
    pub review_capacity: usize,
}

/// Scheduler configuration validation failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SchedulerConfigError {
    /// `max_concurrent` of zero can never schedule anything.
    #[error("max_concurrent must be >= 1")]
    ZeroMaxConcurrent,
    /// `max_per_domain` of zero, when set, starves every domain.
    #[error("max_per_domain must be >= 1 when set")]
    ZeroMaxPerDomain,
    /// `review_capacity` of zero blocks all new implementations.
    #[error("review_capacity must be >= 1")]
    ZeroReviewCapacity,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 4,
            max_per_domain: None,
            review_capacity: 8,
        }
    }
}

impl SchedulerConfig {
    /// Validates the configuration.
    ///
    /// # Errors
    ///
    /// Returns the first [`SchedulerConfigError`] when the configuration
    /// cannot schedule anything.
    pub fn validate(&self) -> Result<(), SchedulerConfigError> {
        if self.max_concurrent == 0 {
            return Err(SchedulerConfigError::ZeroMaxConcurrent);
        }
        if self.review_capacity == 0 {
            return Err(SchedulerConfigError::ZeroReviewCapacity);
        }
        if self.max_per_domain.is_some_and(|cap| cap == 0) {
            return Err(SchedulerConfigError::ZeroMaxPerDomain);
        }
        Ok(())
    }
}

/// Deterministic parallel scheduler over a [`TaskDag`].
#[derive(Debug, Clone, Default)]
pub struct ParallelScheduler {
    config: SchedulerConfig,
}

impl ParallelScheduler {
    /// Creates a scheduler with the given config.
    #[must_use]
    pub const fn new(config: SchedulerConfig) -> Self {
        Self { config }
    }

    /// Returns the config in effect.
    #[must_use]
    pub const fn config(&self) -> &SchedulerConfig {
        &self.config
    }

    /// Selects the subset of dependency-satisfied tasks that may start now,
    /// in stable ID order.
    ///
    /// A task is selected only when: the review-capacity bound holds
    /// (running implementations plus change sets awaiting review stay below
    /// `review_capacity`, since every running implementation is on its way
    /// to review), the concurrency caps hold (global and per-domain), and
    /// none of its `expected_files` overlap with an already-selected or
    /// already-running task's `expected_files` (repository-conflict
    /// avoidance).
    #[must_use]
    pub fn select(
        &self,
        dag: &TaskDag,
        completed: &BTreeSet<BacklogTaskId>,
        running: &[BacklogTaskId],
        under_review: usize,
    ) -> Vec<BacklogTaskId> {
        let Some(mut slots) = self
            .config
            .max_concurrent
            .checked_sub(running.len())
            .filter(|slots| *slots > 0)
        else {
            return Vec::new();
        };
        let review_headroom = self
            .config
            .review_capacity
            .saturating_sub(under_review.saturating_add(running.len()));
        slots = slots.min(review_headroom);
        if slots == 0 {
            return Vec::new();
        }
        let mut domain_load: BTreeMap<DomainId, usize> = BTreeMap::new();
        let mut claimed_files: BTreeSet<String> = BTreeSet::new();
        for id in running {
            if let Some(meta) = dag.get(id) {
                *domain_load.entry(meta.domain.clone()).or_insert(0) += 1;
                claimed_files.extend(meta.expected_files.iter().cloned());
            }
        }
        let running_set: BTreeSet<&BacklogTaskId> = running.iter().collect();
        let mut selected = Vec::new();
        for id in dag.eligible(completed) {
            if slots == 0 {
                break;
            }
            if running_set.contains(&id) {
                continue;
            }
            let Some(meta) = dag.get(&id) else {
                continue;
            };
            if self
                .config
                .max_per_domain
                .is_some_and(|cap| domain_load.get(&meta.domain).copied().unwrap_or(0) >= cap)
            {
                continue;
            }
            if meta
                .expected_files
                .iter()
                .any(|path| claimed_files.contains(path))
            {
                continue;
            }
            *domain_load.entry(meta.domain.clone()).or_insert(0) += 1;
            claimed_files.extend(meta.expected_files.iter().cloned());
            selected.push(id);
            slots -= 1;
        }
        selected
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::metadata::{
        Autonomy, CompletionEvidence, RiskClass, SpecRef, TaskMetadata, VerificationRef,
    };
    use orchestraitor_model::DataSensitivity;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn task(id: &str, deps: &[&str], domain: &str, files: &[&str]) -> TaskMetadata {
        TaskMetadata {
            id: BacklogTaskId::new(id),
            spec_refs: vec![SpecRef::new("9.33.3")],
            title: format!("task {id}"),
            objective: format!("objective {id}"),
            acceptance_criteria: vec!["green".to_string()],
            dependencies: deps.iter().map(|d| BacklogTaskId::new(*d)).collect(),
            domain: DomainId::new(domain),
            risk: RiskClass::Low,
            data_sensitivity: DataSensitivity::Internal,
            expected_files: files.iter().map(|f| (*f).to_string()).collect(),
            required_verification: vec![VerificationRef("nextest-workspace".to_string())],
            required_reviewer_domains: vec![DomainId::new(domain)],
            autonomy: Autonomy::Guided,
            routing: None,
            retry_policy: "default".to_string(),
            completion_evidence: vec![CompletionEvidence::Verification {
                name: "nextest-workspace".to_string(),
            }],
        }
    }

    fn strings(selected: &[BacklogTaskId]) -> Vec<String> {
        selected.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn config_validation() {
        assert_eq!(SchedulerConfig::default().validate(), Ok(()));
        let cfg = SchedulerConfig {
            max_concurrent: 0,
            ..SchedulerConfig::default()
        };
        assert_eq!(cfg.validate(), Err(SchedulerConfigError::ZeroMaxConcurrent));
        let cfg = SchedulerConfig {
            max_concurrent: 2,
            max_per_domain: Some(0),
            review_capacity: 4,
        };
        assert_eq!(cfg.validate(), Err(SchedulerConfigError::ZeroMaxPerDomain));
        let cfg = SchedulerConfig {
            max_concurrent: 2,
            max_per_domain: Some(1),
            review_capacity: 0,
        };
        assert_eq!(
            cfg.validate(),
            Err(SchedulerConfigError::ZeroReviewCapacity)
        );
    }

    #[test]
    fn concurrency_cap_limits_selection() -> TestResult {
        let dag = TaskDag::new([
            task("a", &[], "backend", &[]),
            task("b", &[], "backend", &[]),
            task("c", &[], "backend", &[]),
        ])?;
        let sched = ParallelScheduler::new(SchedulerConfig {
            max_concurrent: 2,
            max_per_domain: None,
            review_capacity: 8,
        });
        let selected = sched.select(&dag, &BTreeSet::new(), &[], 0);
        assert_eq!(strings(&selected), ["a", "b"]);
        Ok(())
    }

    #[test]
    fn running_tasks_count_against_concurrency() -> TestResult {
        let dag = TaskDag::new([
            task("a", &[], "backend", &[]),
            task("b", &[], "backend", &[]),
        ])?;
        let sched = ParallelScheduler::default();
        let selected = sched.select(&dag, &BTreeSet::new(), &[BacklogTaskId::new("a")], 0);
        assert_eq!(strings(&selected), ["b"]);
        Ok(())
    }

    #[test]
    fn full_concurrency_selects_nothing() -> TestResult {
        let dag = TaskDag::new([task("a", &[], "backend", &[])])?;
        let sched = ParallelScheduler::new(SchedulerConfig {
            max_concurrent: 1,
            max_per_domain: None,
            review_capacity: 8,
        });
        assert!(
            sched
                .select(&dag, &BTreeSet::new(), &[BacklogTaskId::new("a")], 0)
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn full_review_backlog_selects_nothing() -> TestResult {
        let dag = TaskDag::new([task("a", &[], "backend", &[])])?;
        let sched = ParallelScheduler::default();
        assert!(sched.select(&dag, &BTreeSet::new(), &[], 8).is_empty());
        Ok(())
    }

    #[test]
    fn review_capacity_counts_running_implementations() -> TestResult {
        let dag = TaskDag::new([
            task("a", &[], "backend", &[]),
            task("b", &[], "backend", &[]),
            task("c", &[], "backend", &[]),
        ])?;
        // Capacity 2 reached by 1 running + 1 under review → nothing starts.
        let sched = ParallelScheduler::new(SchedulerConfig {
            max_concurrent: 4,
            max_per_domain: None,
            review_capacity: 2,
        });
        assert!(
            sched
                .select(&dag, &BTreeSet::new(), &[BacklogTaskId::new("a")], 1)
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn review_capacity_headroom_allows_partial_selection() -> TestResult {
        let dag = TaskDag::new([
            task("a", &[], "backend", &[]),
            task("b", &[], "backend", &[]),
            task("c", &[], "backend", &[]),
        ])?;
        // Capacity 3 with 1 running + 1 under review leaves one change-set
        // slot even though global concurrency allows more.
        let sched = ParallelScheduler::new(SchedulerConfig {
            max_concurrent: 4,
            max_per_domain: None,
            review_capacity: 3,
        });
        let selected = sched.select(&dag, &BTreeSet::new(), &[BacklogTaskId::new("a")], 1);
        assert_eq!(strings(&selected), ["b"]);
        Ok(())
    }

    #[test]
    fn per_domain_cap_limits_selection() -> TestResult {
        let dag = TaskDag::new([
            task("a", &[], "backend", &[]),
            task("b", &[], "backend", &[]),
            task("c", &[], "frontend", &[]),
        ])?;
        let sched = ParallelScheduler::new(SchedulerConfig {
            max_concurrent: 4,
            max_per_domain: Some(1),
            review_capacity: 8,
        });
        let selected = sched.select(&dag, &BTreeSet::new(), &[], 0);
        assert_eq!(strings(&selected), ["a", "c"]);
        Ok(())
    }

    #[test]
    fn overlapping_expected_files_are_not_scheduled_together() -> TestResult {
        let dag = TaskDag::new([
            task("a", &[], "backend", &["src/lib.rs"]),
            task("b", &[], "backend", &["src/lib.rs", "src/main.rs"]),
            task("c", &[], "backend", &["src/main.rs"]),
            task("d", &[], "backend", &["README.md"]),
        ])?;
        let sched = ParallelScheduler::default();
        let selected = sched.select(&dag, &BTreeSet::new(), &[], 0);
        // a claims lib.rs+; b conflicts with a, c conflicts with b's files but
        // b was skipped — c claims main.rs, d is clean.
        assert_eq!(strings(&selected), ["a", "c", "d"]);
        Ok(())
    }

    #[test]
    fn running_task_seeds_per_domain_cap() -> TestResult {
        // A running backend task fills the domain slot: the next eligible
        // backend task is skipped while a frontend task may start.
        let dag = TaskDag::new([
            task("a", &[], "backend", &[]),
            task("b", &[], "backend", &[]),
            task("c", &[], "frontend", &[]),
        ])?;
        let sched = ParallelScheduler::new(SchedulerConfig {
            max_concurrent: 4,
            max_per_domain: Some(1),
            review_capacity: 8,
        });
        let selected = sched.select(&dag, &BTreeSet::new(), &[BacklogTaskId::new("a")], 0);
        assert_eq!(strings(&selected), ["c"]);
        Ok(())
    }

    #[test]
    fn running_task_seeds_file_conflict_exclusion() -> TestResult {
        // A running task touching src/lib.rs blocks any eligible task with
        // overlapping expected files; a disjoint task may start.
        let dag = TaskDag::new([
            task("a", &[], "backend", &["src/lib.rs"]),
            task("b", &[], "backend", &["src/lib.rs"]),
            task("c", &[], "backend", &["src/main.rs"]),
        ])?;
        let sched = ParallelScheduler::default();
        let selected = sched.select(&dag, &BTreeSet::new(), &[BacklogTaskId::new("a")], 0);
        assert_eq!(strings(&selected), ["c"]);
        Ok(())
    }

    #[test]
    fn selection_respects_dependency_satisfaction() -> TestResult {
        let dag = TaskDag::new([
            task("a", &[], "backend", &[]),
            task("b", &["a"], "backend", &[]),
        ])?;
        let sched = ParallelScheduler::default();
        let selected = sched.select(&dag, &BTreeSet::new(), &[], 0);
        assert_eq!(strings(&selected), ["a"]);
        let mut completed = BTreeSet::new();
        completed.insert(BacklogTaskId::new("a"));
        let selected = sched.select(&dag, &completed, &[], 0);
        assert_eq!(strings(&selected), ["b"]);
        Ok(())
    }
}
