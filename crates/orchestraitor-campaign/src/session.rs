//! The pass itself: order, select, record, spawn. Pure over the board
//! snapshot supplied by the caller; the only side effects are the single
//! decision-store insert and the injected spawner call.

use orchestraitor_agent_catalog::RoleRoutingDecision;
use orchestraitor_board::{ItemFacts, ReadyItem, SkipWarning, WarningKind};
use orchestraitor_worker::WorkerRun;

use crate::decision::{
    BlockedNode, CampaignDecision, CampaignDecisionStore, DecisionKind, NoOpReason, SelectedTask,
    SkipRecord, StoredCampaignDecision,
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
/// configured repositories, the ready queue over it, the eligible candidates
/// blocked by unresolved dependencies, and the warning channel (closed items
/// surface as `NotOpen` warnings).
#[derive(Debug, Clone)]
pub struct BoardSnapshot {
    /// All open items in scope.
    pub open: Vec<ItemFacts>,
    /// Ready-queue items (leaf, MVP, Ready, no unresolved blockers), already
    /// sorted by issue number (as [`ready_queue`] produces them); the
    /// selection pass applies the P0-first epic-focus order on top.
    pub ready: Vec<ReadyItem>,
    /// Eligible candidates whose only disqualifier is unresolved blockers
    /// (spec §9.35 "all-blocked"), sorted by `(repo, issue number)`.
    pub blocked_candidates: Vec<ReadyItem>,
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

/// Worker-task-id charset limit from `orchestraitor-worker`'s id validation
/// (`task.rs`): the id becomes a file name, so it is capped at 64 bytes.
const TASK_ID_MAX_LEN: usize = 64;

/// Deterministic worker task id for a selected board item:
/// `board-<owner-repo>-<number>` (repo slug restricted to the worker id
/// charset). Issue numbers are per-repo, so the identity must carry the
/// repo — two configured repos with the same issue number must never load
/// the same fixture. When the slug would overflow the 64-byte file-name
/// bound, it is truncated AND suffixed with a stable 8-hex FNV-1a digest of
/// the full repo, keeping distinct repos collision-free.
#[must_use]
pub fn task_id_for(repo: &str, number: u64) -> String {
    let prefix = format!("board--{number}");
    let budget = TASK_ID_MAX_LEN.saturating_sub(prefix.len());
    let slug: String = repo
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if slug.len() <= budget {
        return format!("board-{slug}-{number}");
    }
    let digest = format!("{:08x}", fnv1a(repo));
    let truncated: String = slug.chars().take(budget.saturating_sub(9)).collect();
    format!("board-{truncated}-{digest}-{number}")
}

/// Stable 32-bit FNV-1a digest over the full repo string; pure ASCII output
/// keeps ids byte- and char-length identical.
fn fnv1a(value: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in value.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Applies the minimal epic-focus rule (spec §9.41): candidates whose
/// configured priority-field value is `P0` first, stable `(repo, issue
/// number)` order within each group. The priority field is authoritative —
/// labels are advisory per the board config.
#[must_use]
pub fn compute_selection(ready: &[ReadyItem], open: &[ItemFacts]) -> Vec<ReadyItem> {
    let mut ordered: Vec<&ReadyItem> = ready.iter().collect();
    ordered.sort_by_key(|item| {
        let facts = open
            .iter()
            .find(|facts| facts.repo == item.repo && facts.number == item.number);
        let p0 = facts.is_some_and(|facts| {
            facts
                .priority
                .as_deref()
                .is_some_and(|priority| priority.eq_ignore_ascii_case("p0"))
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
    let skipped = skipped_records(snapshot);
    let decision = if let Some(selected) = ordered.first() {
        let task_id = task_id_for(&selected.repo, selected.number);
        let alternatives = ordered
            .iter()
            .skip(1)
            .map(|item| {
                let facts = facts_for(snapshot, &item.repo, item.number);
                BlockedNode {
                    repo: item.repo.clone(),
                    number: item.number,
                    title: item.title.clone(),
                    open_blockers: facts.map_or(0, |facts| facts.open_blockers),
                    target: facts.and_then(|facts| facts.target.clone()),
                    status: facts.and_then(|facts| facts.status.clone()),
                }
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
            blocked_graph: Vec::new(),
            skipped,
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
            worker_args: Vec::new(),
            rationale,
            alternatives: Vec::new(),
            blocked_graph: blocked_candidate_nodes(snapshot),
            skipped,
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
    vec![
        "worker".to_string(),
        "run".to_string(),
        "--task".to_string(),
        task_id.to_string(),
        "--json".to_string(),
    ]
}

fn facts_for<'a>(snapshot: &'a BoardSnapshot, repo: &str, number: u64) -> Option<&'a ItemFacts> {
    snapshot
        .open
        .iter()
        .find(|facts| facts.repo == repo && facts.number == number)
}

fn skipped_records(snapshot: &BoardSnapshot) -> Vec<SkipRecord> {
    let mut records: Vec<SkipRecord> = snapshot
        .warnings
        .iter()
        .map(|warning| SkipRecord {
            number: warning.number,
            kind: match warning.kind {
                WarningKind::NotOpen => "not-open".to_string(),
                WarningKind::Malformed => "malformed".to_string(),
                WarningKind::Truncated => "truncated".to_string(),
            },
            reason: warning.reason.clone(),
        })
        .collect();
    records.sort_by(|left, right| (left.number, &left.kind).cmp(&(right.number, &right.kind)));
    records
}

fn blocked_candidate_nodes(snapshot: &BoardSnapshot) -> Vec<BlockedNode> {
    snapshot
        .blocked_candidates
        .iter()
        .map(|item| {
            let facts = facts_for(snapshot, &item.repo, item.number);
            BlockedNode {
                repo: item.repo.clone(),
                number: item.number,
                title: item.title.clone(),
                open_blockers: facts.map_or(0, |facts| facts.open_blockers),
                target: facts.and_then(|facts| facts.target.clone()),
                status: facts.and_then(|facts| facts.status.clone()),
            }
        })
        .collect()
}

/// Spec §9.35 discrimination: `all-blocked` only when eligible work exists
/// and every candidate is blocked; open items that merely are not Ready (no
/// blocked candidates) are `empty-queue` — "no eligible work exists at all".
/// `epic-exhausted` requires EVERY unevaluable item to be known-closed: a
/// malformed or truncated item's open-state is unknown, so claiming "every
/// tracked item is closed" would be fail-open.
fn no_op_classification(snapshot: &BoardSnapshot) -> (NoOpReason, String) {
    if snapshot.open.is_empty() {
        let skipped_note = if snapshot.warnings.is_empty() {
            String::new()
        } else {
            format!(
                "; {} item(s) skipped as unevaluable (fail-closed)",
                snapshot.warnings.len()
            )
        };
        let all_known_closed = !snapshot.warnings.is_empty()
            && snapshot
                .warnings
                .iter()
                .all(|warning| warning.kind == WarningKind::NotOpen);
        if all_known_closed {
            return (
                NoOpReason::EpicExhausted,
                format!("every tracked item is closed; nothing open remains{skipped_note}"),
            );
        }
        return (
            NoOpReason::EmptyQueue,
            format!("no open board items in the configured repositories{skipped_note}"),
        );
    }
    if snapshot.blocked_candidates.is_empty() {
        let skipped_note = if snapshot.warnings.is_empty() {
            String::new()
        } else {
            format!(
                "; {} item(s) skipped as unevaluable (fail-closed)",
                snapshot.warnings.len()
            )
        };
        return (
            NoOpReason::EmptyQueue,
            format!(
                "{} open item(s), none eligible under the ready predicate and none blocked{}",
                snapshot.open.len(),
                skipped_note
            ),
        );
    }
    (
        NoOpReason::AllBlocked,
        format!(
            "{} eligible candidate(s) blocked by unresolved dependencies; blocked graph attached",
            snapshot.blocked_candidates.len()
        ),
    )
}
