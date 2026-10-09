//! One-shot campaign pass (spec `10-orchestrator.md` §9.35 thin slice,
//! issue #313).
//!
//! A campaign session is a fresh, short-lived manager invocation that reads
//! the reconciled board state (open items + ready queue, supplied by the
//! caller from [`orchestraitor_board`]), applies the minimal epic-focus rule
//! (items whose configured priority-field value is P0 first, stable issue-number order otherwise), selects at
//! most one eligible task, persists exactly ONE append-only decision record,
//! and — when a task was selected — spawns the worker through an injected
//! [`WorkerSpawner`] (the daemon-less direct path; tests inject a fake).
//!
//! No-op passes are typed: [`NoOpReason::EmptyQueue`], [`NoOpReason::AllBlocked`]
//! (with the blocked graph attached to the record), or [`NoOpReason::EpicExhausted`].
//! The session holds no state between invocations: every pass re-reads the
//! board and appends one row, so a crashed pass leaves nothing to roll back.
//!
//! This crate implements no security primitive: the worker loop, mediation,
//! and promotion boundaries live in `orchestraitor-worker` and Arbitraitor
//! (spec `40-arbitraitor-integration.md` §2.2). Board content is untrusted
//! input (spec §6.1): titles are carried as inert data, never executed.

#![forbid(unsafe_code)]

mod decision;
mod error;
mod loop_run;
mod run_state;
mod session;

pub use decision::{
    BlockedNode, CampaignDecision, CampaignDecisionStore, DecisionKind, NoOpReason, SelectedTask,
    SkipRecord, StoredCampaignDecision,
};
pub use error::CampaignError;
pub use loop_run::{
    BoardPoller, LoopConfig, LoopEvent, LoopRunner, LoopSummary, LoopWorkerStarter, StopReason,
    WorkerProcess,
};
pub use run_state::{LoopRunStore, RunRow, RunRowStatus, StartRun};
pub use session::{
    BoardSnapshot, CampaignOutcome, SelectorDecision, WorkerSpawner, compute_selection, plan_pass,
    plan_pass_with_selector, run_once, run_once_with_selector, task_id_for,
};
