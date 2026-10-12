//! The watch daemon's reconcile pass (spec `10-orchestrator.md` §9.36
//! thin slice, issue #503): board-wins divergence recording and
//! unblocked-task promotion on every poll tick.
//!
//! Every `orcd watch` tick is a reconcile pass with board-wins semantics
//! (§9.43): where local durable state and the board disagree, the board
//! wins and the divergence is recorded as a `board-diverged` event
//! (§9.40's stale-blocker drift is the same class). Promotion (§9.40) is a
//! reconcile effect, not an event subscription: a task whose blockers all
//! landed becomes selectable at the next tick — the campaign pass's ready
//! queue already computes that from the fresh snapshot; what this module
//! adds is the typed record of the promotion transition and the
//! divergence detection against the local run state.
//!
//! This crate implements no security primitive: reconcile is orchestration
//! state bookkeeping (spec §9.27.3); the board remains the single
//! authority for work-item state (§9.43 — the local read-cache is never
//! authoritative).

use orchestraitor_board::ReadyItem;

use crate::error::CampaignError;
use crate::run_state::{RunRow, RunRowStatus};
use crate::session::{BoardSnapshot, task_id_for};

/// One reconcile observation. Events carry identifiers only — never board
/// content or task payloads (the same log-safety rule as
/// [`crate::LoopEvent`]).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReconcileEvent {
    /// Local run state disagreed with the board: the board wins (§9.43).
    /// A restart-recovered (`orphaned`) task — crashed-run liveness the
    /// §9.24.2 recovery reaped — is no longer in the board's open set:
    /// it was completed, cancelled, or moved out of scope elsewhere while
    /// the run was down. The local row keeps its status; the divergence
    /// is the record. Live `running` slots are excluded on purpose (a
    /// worker closing its own item mid-run is normal supervision).
    BoardDiverged {
        /// Worker task id whose local state the board overruled.
        task_id: String,
        /// The local row's status before the board-wins reconcile.
        local_status: RunRowStatus,
    },
    /// A blocked task's last blocker landed and the task is now on the
    /// ready queue (§9.40): the promotion is observed and recorded, never
    /// silently taken for granted.
    UnblockedTaskPromoted {
        /// Worker task id of the promoted task.
        task_id: String,
        /// The promotion's issue number.
        number: u64,
        /// The promoted item's repository.
        repo: String,
    },
}

/// One reconcile pass over a fresh snapshot.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ReconcileOutcome {
    /// Observed events, in detection order.
    pub events: Vec<ReconcileEvent>,
    /// The ready-queue task ids after the pass (the kick-off input).
    pub ready_task_ids: Vec<String>,
}

/// Applies one tick's reconcile pass (spec §9.36): board-wins divergence
/// recording over the local run state and unblocked-task promotion.
///
/// Divergence: a terminal local row for a task the board no longer lists
/// as open work is expected history — a completed row is the happy path,
/// not a divergence. A divergence is a row the board contradicted out
/// from under a run that can no longer speak for itself: the recovered
/// rows of this invocation's restart (`recovered` — rows the supervisor
/// never reached a terminal status for, now `orphaned`) whose task is
/// absent from the fresh snapshot's open items. The board cancelled,
/// completed, or moved the item while the crashed run was down; the
/// event records the overrule so the operator sees the work was
/// superseded, not silently dropped. Live slots are never in scope: the
/// current invocation's supervised rows are owned by the reaper, and a
/// worker legitimately closing its own item must not false-positive
/// (a PR #564 review finding).
///
/// Promotion: a task on the ready queue this pass that the previous pass
/// saw only as a blocked candidate is a newly unblocked promotion. The
/// caller supplies the previous pass's blocked candidates; the fresh
/// snapshot supplies the current ready queue.
///
/// # Errors
///
/// Always `Ok` today: the pass reads only its arguments and never
/// touches the store. The `Result` keeps the signature stable for a
/// future store-reading pass without churning every caller.
pub fn reconcile(
    snapshot: &BoardSnapshot,
    previous_blocked: &[ReadyItem],
    recovered_orphaned: &[RunRow],
) -> Result<ReconcileOutcome, CampaignError> {
    let mut events = Vec::new();

    // Board-wins divergence (§9.43): this invocation's recovered rows
    // (crashed-run liveness the restart recovery reaped) whose task the
    // board no longer lists as open. The snapshot's open items are the
    // authority; the row keeps its `orphaned` status (restart recovery
    // owns it) and the event records the overrule.
    let open_task_ids: std::collections::HashSet<String> = snapshot
        .open
        .iter()
        .map(|facts| task_id_for(&facts.repo, facts.number))
        .collect();
    for row in recovered_orphaned {
        if !open_task_ids.contains(&row.task_id) {
            events.push(ReconcileEvent::BoardDiverged {
                task_id: row.task_id.clone(),
                local_status: row.status,
            });
        }
    }

    // Promotion (§9.40): a previously blocked candidate now on the ready
    // queue is a newly unblocked task.
    let ready_ids: std::collections::HashSet<String> = snapshot
        .ready
        .iter()
        .map(|item| task_id_for(&item.repo, item.number))
        .collect();
    for candidate in previous_blocked {
        let candidate_id = task_id_for(&candidate.repo, candidate.number);
        if ready_ids.contains(&candidate_id) {
            events.push(ReconcileEvent::UnblockedTaskPromoted {
                task_id: candidate_id,
                number: candidate.number,
                repo: candidate.repo.clone(),
            });
        }
    }

    Ok(ReconcileOutcome {
        ready_task_ids: snapshot
            .ready
            .iter()
            .map(|item| task_id_for(&item.repo, item.number))
            .collect(),
        events,
    })
}
