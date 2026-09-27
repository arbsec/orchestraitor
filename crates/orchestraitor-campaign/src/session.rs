//! The pass itself: order, select, record, spawn. Pure over the board
//! snapshot supplied by the caller; the only side effects are the single
//! decision-store insert and the injected spawner call.

use orchestraitor_agent_catalog::RoleRoutingDecision;
use orchestraitor_board::{ItemFacts, ReadyItem, SkipWarning, WarningKind};
use orchestraitor_worker::WorkerRun;

use crate::decision::{
    BlockedNode, CampaignDecision, CampaignDecisionStore, DecisionKind, NoOpReason, SelectedTask,
    StoredCampaignDecision,
};
use crate::error::CampaignError;

/// Spawns the worker for a selected task on the daemon-less direct path.
/// Production wires the bootstrap transport (fixture task source, mediated
/// bash, pending delivery sink); tests inject a fake — the trait is the
/// hermeticity seam.
pub trait WorkerSpawner {
    /// Runs one leaf task end-to-end and returns the structured result.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Spawn`] when no run can be produced at all
    /// (unusable worktree, transport failure). Classifiable worker failures
    /// arrive inside the returned [`WorkerRun`] as typed failures.
    fn spawn(
        &self,
        task_id: &str,
        routing: &RoleRoutingDecision,
    ) -> Result<WorkerRun, CampaignError>;
}

/// The reconciled board state one pass reads: every open item in the
/// configured repositories, the ready queue over it, and the warning channel
/// (closed items surface as `NotOpen` warnings).
#[derive(Debug, Clone)]
pub struct BoardSnapshot {
    /// All open items in scope.
    pub open: Vec<ItemFacts>,
    /// Ready-queue items (leaf, MVP, Ready, no unresolved blockers), already
    /// sorted by issue number.
    pub ready: Vec<ReadyItem>,
    /// Warnings for items that could not be evaluated; `NotOpen` entries are
    /// what separates [`NoOpReason::EpicExhausted`] from
    /// [`NoOpReason::EmptyQueue`].
    pub warnings: Vec<SkipWarning>,
}

/// The result of one pass: the persisted decision plus the worker run, when a
/// task was selected and spawned.
#[derive(Debug)]
pub struct CampaignOutcome {
    /// The exactly-one decision record for this pass.
    pub decision: StoredCampaignDecision,
    /// The worker run, present iff the pass selected a task and the spawn
    /// produced a run.
    pub worker: Option<WorkerRun>,
}

/// Deterministic worker task id for a selected board item: `board-<number>`
/// (alphanumeric-led, fixture-file safe per the worker's id rules).
#[must_use]
pub fn task_id_for(repo: &str, number: u64) -> String {
    let _ = repo;
    format!("board-{number}")
}

/// Applies the minimal epic-focus rule: `P0`-labelled items first, stable
/// issue-number order within each group, cross-repo by `(repo, number)` for
/// determinism.
#[must_use]
pub fn compute_selection(ready: &[ReadyItem], open: &[ItemFacts]) -> Vec<ReadyItem> {
    let mut ordered: Vec<&ReadyItem> = ready.iter().collect();
    ordered.sort_by_key(|item| {
        let facts = open
            .iter()
            .find(|facts| facts.repo == item.repo && facts.number == item.number);
        let p0 = facts.is_some_and(|facts| {
            facts
                .labels
                .iter()
                .any(|label| label.eq_ignore_ascii_case("p0"))
        });
        (u8::from(!p0), item.repo.clone(), item.number)
    });
    ordered.into_iter().cloned().collect()
}

/// Runs exactly one pass: reads the snapshot, selects at most one task,
/// persists exactly one decision record, and — for a selection — spawns the
/// worker. No-op passes spawn nothing and still persist their typed record.
///
/// # Errors
///
/// Returns [`CampaignError::Store`] when the record cannot be persisted and
/// [`CampaignError::Spawn`] when the spawner fails to produce a run at all.
pub fn run_once(
    snapshot: &BoardSnapshot,
    routing: &RoleRoutingDecision,
    store: &CampaignDecisionStore,
    spawner: &dyn WorkerSpawner,
) -> Result<CampaignOutcome, CampaignError> {
    let ordered = compute_selection(&snapshot.ready, &snapshot.open);
    let blocked_graph = blocked_nodes(snapshot);
    let decision = if let Some(selected) = ordered.first() {
        let task_id = task_id_for(&selected.repo, selected.number);
        let alternatives = ordered
            .iter()
            .skip(1)
            .map(|item| BlockedNode {
                repo: item.repo.clone(),
                number: item.number,
                title: item.title.clone(),
                open_blockers: blockers_for(snapshot, &item.repo, item.number),
                target: None,
                status: None,
            })
            .collect();
        CampaignDecision {
            kind: DecisionKind::Selected,
            no_op_reason: None,
            selected: Some(SelectedTask {
                repo: selected.repo.clone(),
                number: selected.number,
                title: selected.title.clone(),
                url: selected.url.clone(),
                item_node_id: selected.item_id.clone(),
                task_id: task_id.clone(),
            }),
            role: routing.role.clone(),
            provider: routing.provider.clone(),
            model: routing.model.clone(),
            precedence_path: routing.precedence_path.clone(),
            fallback_reason: routing.fallback_reason.clone(),
            worker_args: worker_args(&task_id),
            rationale:
                "first eligible item in P0-first ready order (spec 10-orchestrator.md §9.35)"
                    .to_string(),
            alternatives,
            blocked_graph,
        }
    } else {
        let (reason, rationale) = no_op_classification(snapshot);
        CampaignDecision {
            kind: DecisionKind::NoOp,
            no_op_reason: Some(reason),
            selected: None,
            role: routing.role.clone(),
            provider: routing.provider.clone(),
            model: routing.model.clone(),
            precedence_path: routing.precedence_path.clone(),
            fallback_reason: routing.fallback_reason.clone(),
            worker_args: worker_args(""),
            rationale,
            alternatives: Vec::new(),
            blocked_graph,
        }
    };
    let stored = store.record(&decision)?;
    let worker = match &decision.selected {
        Some(selected) => Some(spawner.spawn(&selected.task_id, routing)?),
        None => None,
    };
    Ok(CampaignOutcome {
        decision: stored,
        worker,
    })
}

fn worker_args(task_id: &str) -> Vec<String> {
    let mut args = vec![
        "worker".to_string(),
        "run".to_string(),
        "--task".to_string(),
    ];
    if !task_id.is_empty() {
        args.push(task_id.to_string());
        args.push("--json".to_string());
    }
    args
}

fn blockers_for(snapshot: &BoardSnapshot, repo: &str, number: u64) -> u64 {
    snapshot
        .open
        .iter()
        .find(|facts| facts.repo == repo && facts.number == number)
        .map_or(0, |facts| facts.open_blockers)
}

fn blocked_nodes(snapshot: &BoardSnapshot) -> Vec<BlockedNode> {
    let ready_keys: Vec<(&str, u64)> = snapshot
        .ready
        .iter()
        .map(|item| (item.repo.as_str(), item.number))
        .collect();
    let mut nodes: Vec<BlockedNode> = snapshot
        .open
        .iter()
        .filter(|facts| {
            !ready_keys
                .iter()
                .any(|(repo, number)| *repo == facts.repo && *number == facts.number)
        })
        .map(|facts| BlockedNode {
            repo: facts.repo.clone(),
            number: facts.number,
            title: facts.title.clone(),
            open_blockers: facts.open_blockers,
            target: facts.target.clone(),
            status: facts.status.clone(),
        })
        .collect();
    nodes.sort_by(|left, right| (&left.repo, left.number).cmp(&(&right.repo, right.number)));
    nodes
}

fn no_op_classification(snapshot: &BoardSnapshot) -> (NoOpReason, String) {
    if snapshot.open.is_empty() {
        let epic_exhausted = snapshot
            .warnings
            .iter()
            .any(|warning| warning.kind == WarningKind::NotOpen);
        if epic_exhausted {
            return (
                NoOpReason::EpicExhausted,
                "every tracked item is closed; nothing open remains".to_string(),
            );
        }
        return (
            NoOpReason::EmptyQueue,
            "no open board items in the configured repositories".to_string(),
        );
    }
    (
        NoOpReason::AllBlocked,
        format!(
            "{} open item(s), none eligible under the ready predicate; blocked graph attached",
            snapshot.open.len()
        ),
    )
}
