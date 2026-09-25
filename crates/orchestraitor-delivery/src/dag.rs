//! Backlog dependency DAG (spec §9.33.2–9.33.3): construction-time validation,
//! deterministic topological ordering, and dependency-satisfied eligibility.
//!
//! The DAG is structural only: scheduling constraints (concurrency, budgets,
//! review capacity) are layered on top by the runner, and nothing here makes
//! or implies a security decision (§9.33.7).

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::metadata::{BacklogTaskId, MetadataError, TaskMetadata};

/// DAG construction or ordering failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DagError {
    /// A task carries structurally invalid metadata (§9.33.2 invariants).
    #[error("invalid metadata for task {id}: {source}")]
    InvalidMetadata {
        /// Offending task.
        id: String,
        /// Underlying metadata validation failure.
        source: MetadataError,
    },
    /// Two tasks share one stable ID.
    #[error("duplicate task id {id}")]
    DuplicateTask {
        /// Duplicated identifier.
        id: String,
    },
    /// A dependency edge points at a task absent from the backlog.
    #[error("task {id} depends on unknown task {dep}")]
    UnknownDependency {
        /// Offending task.
        id: String,
        /// Missing dependency.
        dep: String,
    },
    /// The backlog contains a dependency cycle.
    #[error("tasks blocked by a dependency cycle: {}", path.join(", "))]
    Cycle {
        /// Tasks that remain after Kahn elimination, sorted by ID.
        path: Vec<String>,
    },
}

/// Validated backlog DAG: task metadata keyed by stable ID.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskDag {
    tasks: BTreeMap<BacklogTaskId, TaskMetadata>,
}

impl TaskDag {
    /// Builds a DAG from task metadata, validating each task and every edge.
    ///
    /// # Errors
    ///
    /// Returns [`DagError`] for invalid metadata, duplicate IDs, or edges to
    /// unknown tasks.
    pub fn new(tasks: impl IntoIterator<Item = TaskMetadata>) -> Result<Self, DagError> {
        let mut map = BTreeMap::new();
        for task in tasks {
            task.validate()
                .map_err(|source| DagError::InvalidMetadata {
                    id: task.id.to_string(),
                    source,
                })?;
            let id = task.id.to_string();
            if map.insert(task.id.clone(), task).is_some() {
                return Err(DagError::DuplicateTask { id });
            }
        }
        for (id, task) in &map {
            for dep in &task.dependencies {
                if !map.contains_key(dep) {
                    return Err(DagError::UnknownDependency {
                        id: id.to_string(),
                        dep: dep.to_string(),
                    });
                }
            }
        }
        Ok(Self { tasks: map })
    }

    /// Returns the task metadata for `id`, if present.
    #[must_use]
    pub fn get(&self, id: &BacklogTaskId) -> Option<&TaskMetadata> {
        self.tasks.get(id)
    }

    /// Returns the number of tasks in the DAG.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Returns true when the DAG holds no tasks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Iterates over tasks in stable ID order.
    pub fn iter(&self) -> impl Iterator<Item = (&BacklogTaskId, &TaskMetadata)> {
        self.tasks.iter()
    }

    /// Computes a deterministic topological order. Ties break by task ID so a
    /// given backlog always orders identically (§9.33.2 stable IDs).
    ///
    /// # Errors
    ///
    /// Returns [`DagError::Cycle`] listing the tasks that cannot be ordered.
    pub fn topological_order(&self) -> Result<Vec<BacklogTaskId>, DagError> {
        // Kahn's algorithm over indegree, with a sorted ready set.
        let mut indegree: BTreeMap<&BacklogTaskId, usize> = BTreeMap::new();
        let mut dependents: BTreeMap<&BacklogTaskId, Vec<&BacklogTaskId>> = BTreeMap::new();
        for (id, task) in &self.tasks {
            indegree.entry(id).or_insert(0);
            for dep in &task.dependencies {
                *indegree.entry(id).or_insert(0) += 1;
                dependents.entry(dep).or_default().push(id);
            }
        }
        let mut ready: BTreeSet<BacklogTaskId> = indegree
            .iter()
            .filter(|&(_, &deg)| deg == 0)
            .map(|(id, _)| (*id).clone())
            .collect();
        let mut order = Vec::with_capacity(self.tasks.len());
        let mut ordered: BTreeSet<BacklogTaskId> = BTreeSet::new();
        while let Some(id) = ready.iter().next().cloned() {
            ready.remove(&id);
            ordered.insert(id.clone());
            order.push(id.clone());
            if let Some(children) = dependents.get(&id) {
                for child in children {
                    if let Some(deg) = indegree.get_mut(child) {
                        *deg -= 1;
                        if *deg == 0 {
                            ready.insert((*child).clone());
                        }
                    }
                }
            }
        }
        if order.len() != self.tasks.len() {
            // Members of, or tasks downstream of, an unresolved dependency
            // cycle: Kahn elimination emits `ordered` first, so anything left
            // over is blocked by one.
            let path = self
                .tasks
                .keys()
                .filter(|id| !ordered.contains(id))
                .map(ToString::to_string)
                .collect();
            return Err(DagError::Cycle { path });
        }
        Ok(order)
    }

    /// Returns tasks eligible to start: every dependency is in `completed`
    /// and the task itself is not (spec §9.33.3 dependency-satisfied
    /// eligibility), in stable ID order.
    #[must_use]
    pub fn eligible(&self, completed: &BTreeSet<BacklogTaskId>) -> Vec<BacklogTaskId> {
        self.tasks
            .iter()
            .filter(|(id, task)| {
                !completed.contains(*id)
                    && task.dependencies.iter().all(|dep| completed.contains(dep))
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::metadata::{
        Autonomy, CompletionEvidence, DomainId, RiskClass, SpecRef, TaskMetadata, VerificationRef,
    };
    use orchestraitor_model::DataSensitivity;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn task(id: &str, deps: &[&str]) -> TaskMetadata {
        TaskMetadata {
            id: BacklogTaskId::new(id),
            spec_refs: vec![SpecRef::new("9.33.3")],
            title: format!("task {id}"),
            objective: format!("objective {id}"),
            acceptance_criteria: vec!["green".to_string()],
            dependencies: deps.iter().map(|d| BacklogTaskId::new(*d)).collect(),
            domain: DomainId::new("backend"),
            risk: RiskClass::Low,
            data_sensitivity: DataSensitivity::Internal,
            expected_files: Vec::new(),
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

    fn ids(order: &[BacklogTaskId]) -> Vec<String> {
        order.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn empty_dag_orders_to_nothing() -> TestResult {
        let dag = TaskDag::default();
        assert_eq!(dag.topological_order()?, Vec::<BacklogTaskId>::new());
        assert!(dag.eligible(&BTreeSet::new()).is_empty());
        assert!(dag.is_empty());
        Ok(())
    }

    #[test]
    fn linear_chain_orders_dependencies_first() -> TestResult {
        let dag = TaskDag::new([task("c", &["b"]), task("a", &[]), task("b", &["a"])])?;
        assert_eq!(ids(&dag.topological_order()?), ["a", "b", "c"]);
        Ok(())
    }

    #[test]
    fn diamond_breaks_ties_by_task_id() -> TestResult {
        let dag = TaskDag::new([
            task("d", &["b", "c"]),
            task("a", &[]),
            task("c", &["a"]),
            task("b", &["a"]),
        ])?;
        assert_eq!(ids(&dag.topological_order()?), ["a", "b", "c", "d"]);
        Ok(())
    }

    #[test]
    fn insertion_order_does_not_change_output_order() -> TestResult {
        let forward = TaskDag::new([task("x", &[]), task("y", &[]), task("z", &[])])?;
        let reverse = TaskDag::new([task("z", &[]), task("y", &[]), task("x", &[])])?;
        assert_eq!(forward.topological_order(), reverse.topological_order());
        Ok(())
    }

    #[test]
    fn cycle_is_detected_and_lists_remaining_tasks() -> TestResult {
        let dag = TaskDag::new([task("a", &["c"]), task("b", &["a"]), task("c", &["b"])])?;
        match dag.topological_order() {
            Err(DagError::Cycle { path }) => assert_eq!(path, ["a", "b", "c"]),
            Ok(order) => {
                return Err(format!("expected cycle error, got order {order:?}").into());
            }
            Err(other) => {
                return Err(format!("expected cycle error, got {other}").into());
            }
        }
        Ok(())
    }

    #[test]
    fn unknown_dependency_is_rejected_at_construction() {
        let result = TaskDag::new([task("a", &["missing"])]);
        assert_eq!(
            result.map(|dag| dag.len()),
            Err(DagError::UnknownDependency {
                id: "a".to_string(),
                dep: "missing".to_string(),
            })
        );
    }

    #[test]
    fn duplicate_task_id_is_rejected() {
        let result = TaskDag::new([task("a", &[]), task("a", &[])]);
        assert!(matches!(
            result.map(|dag| dag.len()),
            Err(DagError::DuplicateTask { .. })
        ));
    }

    #[test]
    fn invalid_metadata_is_rejected_at_construction() {
        let mut broken = task("a", &[]);
        broken.title.clear();
        let result = TaskDag::new([broken]);
        assert!(matches!(
            result.map(|dag| dag.len()),
            Err(DagError::InvalidMetadata { .. })
        ));
    }

    #[test]
    fn eligibility_unlocks_only_when_all_dependencies_complete() -> TestResult {
        let dag = TaskDag::new([
            task("a", &[]),
            task("b", &["a"]),
            task("c", &["a", "b"]),
            task("d", &[]),
        ])?;
        let mut completed: BTreeSet<BacklogTaskId> = BTreeSet::new();
        assert_eq!(ids(&dag.eligible(&completed)), ["a", "d"]);
        completed.insert(BacklogTaskId::new("a"));
        assert_eq!(ids(&dag.eligible(&completed)), ["b", "d"]);
        completed.insert(BacklogTaskId::new("b"));
        assert_eq!(ids(&dag.eligible(&completed)), ["c", "d"]);
        completed.insert(BacklogTaskId::new("c"));
        completed.insert(BacklogTaskId::new("d"));
        assert!(dag.eligible(&completed).is_empty());
        Ok(())
    }

    #[test]
    fn get_returns_metadata() -> TestResult {
        let dag = TaskDag::new([task("a", &[])])?;
        let meta = dag.get(&BacklogTaskId::new("a")).ok_or("missing task a")?;
        assert_eq!(meta.title, "task a");
        assert!(dag.get(&BacklogTaskId::new("nope")).is_none());
        assert_eq!(dag.len(), 1);
        Ok(())
    }
}
