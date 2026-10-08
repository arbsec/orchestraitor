//! The sub-session runtime (issue #535, T3): one declared `subagent` tool
//! invocation = one restricted `run_worker` re-entry.
//!
//! A sub-session is a full worker run with:
//! - the role-resolved `(provider, model)` from the §9.45 chain (the parent
//!   never chooses the model);
//! - a scoped internal-tool allowlist (read-only by default; a sub-session
//!   bash mediator is constructed only when the allowlist names `bash`);
//! - the `PendingDeliverySink` — a sub-session structurally cannot deliver;
//!   its `finish` summary is the result;
//! - carved budgets: `max_turns_per_attempt` from the tool budget, the
//!   deadline the lesser of the parent's remaining wall clock and the
//!   tool's own bound;
//! - `subsession_depth = parent + 1`; the executor refuses declared-tool
//!   dispatch at depth >= 1, so sub-sessions cannot spawn sub-sessions
//!   (plan C.1/S5: no config path raises the depth);
//! - beats emitted around the child await so the parent's supervisor-side
//!   stall detection (beat staleness, issue #314) covers the child window —
//!   a hung child transport call cannot stall detection.
//!
//! The result is a typed [`SubsessionOutcome`]; the summary is size-capped
//! and marker-wrapped as UNTRUSTED DATA by the parent loop (spec
//! `40-arbitraitor-integration.md` §9.14: sub-session output is worker
//! output; no "trusted-role" exception).

use std::path::Path;
use std::time::{Duration, Instant};

use orchestraitor_model::{ModelId, ProviderId};
use orchestraitor_provider_api::transport::ProviderTransport;
use serde::Serialize;

use crate::delivery::PendingDeliverySink;
use crate::mediator::{BashMediator, MediatedBashMediator, MediatedRun, MediationError};
use crate::result::{FailureClass, RunStatus, TypedFailure, WorkerConfig, WorkerRun};
use crate::run::run_worker;
use crate::task::WorkerTask;
use crate::tooldef::{InternalTool, ToolDefinition, ToolMechanism};

/// Default carve when the tool budget does not pin `max_turns`.
const DEFAULT_SUBSESSION_MAX_TURNS: u32 = 12;
/// Default carve when the tool budget does not pin `max_result_bytes`.
const DEFAULT_SUBSESSION_MAX_RESULT_BYTES: u64 = 8 * 1024;

/// The parent context a sub-session is carved from.
#[derive(Clone, Debug)]
pub struct SubsessionParent {
    /// The parent's worktree root (the child confines to the same tree).
    pub worktree_root: std::path::PathBuf,
    /// The parent's remaining wall clock; the child deadline is the lesser
    /// of this and the tool's own bound.
    pub remaining: Duration,
    /// The parent's sub-session depth (the child runs at `depth + 1`).
    pub depth: u8,
    /// The parent's remaining daily-spend soft cap input, forwarded so the
    /// child's soft-cap check sees the same day.
    pub prior_daily_spend_usd: f64,
    /// The orchestration role of the PARENT (visibility filtering already
    /// happened at registry build; carried for decision records).
    pub role: String,
    /// The project label for cost attribution.
    pub project: String,
    /// The repository label for cost attribution.
    pub repository: String,
    /// The session id stem for cost attribution (child sessions suffix the
    /// tool id and sequence).
    pub session_id: String,
}

/// One sub-session invocation's typed result.
#[derive(Clone, Debug, Serialize)]
pub struct SubsessionOutcome {
    /// The declared tool that was invoked.
    pub tool_id: String,
    /// The sub-session's run status.
    pub status: RunStatus,
    /// The size-capped finish summary (the result the parent feeds back as
    /// an untrusted observation). `None` on failure.
    pub summary: Option<String>,
    /// The typed failure, when the sub-session run failed.
    pub failure: Option<crate::result::TypedFailure>,
    /// Token usage totals accrued by the child run.
    pub usage: crate::result::UsageTotals,
    /// The child's receipts (count-capped by the parent's echo).
    pub receipts: Vec<crate::tools::ToolReceipt>,
    /// The role-resolved routing decision evidence (role, provider, model,
    /// precedence path, fallback reason) for the decision record.
    pub routing: RoleRoutingEvidence,
}

/// Routing evidence persisted with every sub-session spawn (the §9.35
/// decision-record shape, scoped to tool spawns). Carries the resolved
/// `(provider, model)` and the precedence path — enough to replay the
/// routing resolution.
#[derive(Clone, Debug, Serialize)]
pub struct RoleRoutingEvidence {
    /// The orchestration role the sub-session resolved as.
    pub role: String,
    /// Resolved provider id.
    pub provider: String,
    /// Resolved model id.
    pub model: String,
    /// Precedence path that produced the resolution.
    pub precedence_path: String,
    /// Documented fallback reason, when the routing fell back.
    pub fallback_reason: Option<String>,
}

/// Sub-session spawn failures that prevent any run from existing (as
/// opposed to typed run failures, which are results).
#[derive(Debug, thiserror::Error)]
pub enum SubsessionError {
    /// The tool definition is not a subagent mechanism.
    #[error("tool `{tool_id}` is not a subagent tool")]
    NotSubagent {
        /// The offending tool id.
        tool_id: String,
    },
    /// The child worktree root is unusable (canonicalization failed).
    #[error("sub-session worktree root is unusable: {reason}")]
    WorktreeRoot {
        /// Static reason code.
        reason: &'static str,
    },
}

/// Maps one typed child-run failure class to the parent-visible class. The
/// parent sees a single budget-exhaustion class so its own budget
/// accounting stays uniform (the detail rides the reason code).
#[must_use]
pub fn parent_failure_class(child: FailureClass) -> FailureClass {
    match child {
        FailureClass::AttemptBudgetExhausted
        | FailureClass::ReplanBudgetExhausted
        | FailureClass::TurnBudgetExhausted
        | FailureClass::WorkerTimeout
        | FailureClass::Stalled => FailureClass::SubsessionBudgetExhausted,
        FailureClass::SubsessionBudgetExhausted
        | FailureClass::SubsessionDepthExceeded
        | FailureClass::SubsessionFailed => child,
        FailureClass::FormatErrorsExhausted
        | FailureClass::ProviderError
        | FailureClass::MediationRefused
        | FailureClass::TaskNotCompleted
        | FailureClass::DeliveryFailed => FailureClass::SubsessionFailed,
    }
}

/// Runs one sub-session for one declared `subagent` tool (issue #535, T3).
///
/// `question` is the parent model's question (untrusted data, carried as
/// data); `instructions` come from the tool definition (trusted layer). The
/// child run inherits the guardrail posture through `run_worker` reuse.
/// `routed` is the `(provider, model)` the CONTROL PLANE resolved for the
/// sub-session role (the parent never chooses the model — spec §9.19.2);
/// the routing evidence rides the outcome for the decision record.
///
/// The beats around the child await close the supervision gap: the parent's
/// supervisor watches beat STALENESS, and the internal stall check cannot
/// fire while the child await hangs — so the parent emits a beat before and
/// after the child await (mirroring the pre-dispatch beat rationale).
///
/// # Errors
///
/// Returns [`SubsessionError`] only when no run can be produced at all; a
/// child-run failure is a typed [`SubsessionOutcome::failure`], never a
/// retry loop.
pub async fn run_subsession(
    parent: &SubsessionParent,
    def: &ToolDefinition,
    question: Option<&str>,
    transport: &dyn ProviderTransport,
    routed: (&str, &str),
    progress: Option<&tokio::sync::watch::Sender<u64>>,
) -> Result<SubsessionOutcome, SubsessionError> {
    let ToolMechanism::Subagent {
        role, instructions, ..
    } = &def.mechanism
    else {
        return Err(SubsessionError::NotSubagent {
            tool_id: def.id.clone(),
        });
    };

    // Carve the child budgets from the tool definition.
    let mut budgets = crate::budget::WorkerBudgets::bootstrap_defaults();
    budgets.max_turns_per_attempt = def.budget.max_turns.clamp(1, DEFAULT_SUBSESSION_MAX_TURNS);
    let wall_clock = def
        .budget
        .wall_clock_secs
        .map(Duration::from_secs)
        .map_or(parent.remaining, |bound| bound.min(parent.remaining));
    budgets.run_budget = wall_clock;
    budgets.worker_timeout = wall_clock;

    // The child task: instructions (operator-authored, trusted layer) plus
    // the parent's question (untrusted, carried as data).
    let mut description = String::new();
    if let Some(instructions) = instructions {
        description.push_str(instructions);
        description.push_str("\n\n");
    }
    if let Some(question) = question {
        description.push_str("Question from the parent session:\n");
        description.push_str(question);
        description.push('\n');
    } else {
        description.push_str("Complete the task your instructions define.\n");
    }
    let child_task = WorkerTask {
        id: format!("tool.{}", def.id),
        slug: format!("tool-{}", def.id),
        description,
    };

    // The child's mediator: constructed ONLY when the allowlist names bash
    // (read-only sub-sessions never probe the sandbox preflight).
    let bash_mediator: Option<MediatedBashMediator> = def
        .internal_tool_names()
        .contains(&"bash")
        .then(MediatedBashMediator::new);
    let bash: &dyn BashMediator = bash_mediator
        .as_ref()
        .map_or(&ReadOnlyBashStub, |mediator| mediator as &dyn BashMediator);

    let child_config = WorkerConfig::new(
        ProviderId::from_string(routed.0.to_string()),
        ModelId::from_string(routed.1.to_string()),
        budgets,
    )
    .with_subsession_depth(parent.depth.saturating_add(1));

    // BEAT: before the child await — the supervisor's staleness window
    // covers the child wait from here.
    if let Some(sender) = progress {
        let _ = sender.send(child_beat(sender));
    }
    let started = Instant::now();
    let child_run = run_worker(
        &child_task,
        Path::new(&parent.worktree_root),
        transport,
        bash,
        &PendingDeliverySink,
        &child_config,
    )
    .await;
    // BEAT: after the child await — the window is closed.
    if let Some(sender) = progress {
        let _ = sender.send(child_beat(sender));
    }
    let _ = started;
    let run = child_run.map_err(|error| match error {
        crate::error::WorkerError::WorktreeRoot { reason } => {
            SubsessionError::WorktreeRoot { reason }
        }
        // The child loop's only other errors are transport construction and
        // result serialization — neither occurs for a pre-bound transport;
        // any occurrence is a wiring defect surfaced as a typed error.
        crate::error::WorkerError::Transport { reason } => SubsessionError::WorktreeRoot { reason },
        crate::error::WorkerError::Serialize { .. } => SubsessionError::WorktreeRoot {
            reason: "child-serialize",
        },
    })?;

    Ok(outcome_from_run(def.id.clone(), role.clone(), routed, run))
}

/// Emits one opaque child-window beat through the parent's channel. The
/// payload is the channel's own counter contract (opaque sequence numbers);
/// the child window borrows the same channel.
fn child_beat(sender: &tokio::sync::watch::Sender<u64>) -> u64 {
    let current = *sender.borrow();
    current.wrapping_add(1)
}

/// Maps a child run into the typed outcome: capped summary, parent-visible
/// failure class, receipts, routing evidence.
fn outcome_from_run(
    tool_id: String,
    role: String,
    routed: (&str, &str),
    run: WorkerRun,
) -> SubsessionOutcome {
    let failure: Option<TypedFailure> = run.failure.map(|failure| TypedFailure {
        class: parent_failure_class(failure.class),
        reason: failure.reason,
    });
    SubsessionOutcome {
        tool_id,
        status: run.status,
        summary: run.summary.as_ref().map(|summary| {
            crate::search::truncate_chars(
                summary,
                usize::try_from(DEFAULT_SUBSESSION_MAX_RESULT_BYTES).unwrap_or(usize::MAX),
            )
        }),
        failure: failure.filter(|_| run.status == RunStatus::Failed),
        usage: run.usage,
        receipts: run.receipts,
        routing: RoleRoutingEvidence {
            role,
            provider: routed.0.to_string(),
            model: routed.1.to_string(),
            precedence_path: "subsession-spawn".to_string(),
            fallback_reason: None,
        },
    }
}

/// A bash mediator that refuses every call: the sub-session allowlist
/// without `bash` never gets an execution surface (and never probes the
/// sandbox preflight).
struct ReadOnlyBashStub;

#[async_trait::async_trait]
impl BashMediator for ReadOnlyBashStub {
    async fn run_bash(&self, _script: &str) -> Result<MediatedRun, MediationError> {
        Err(MediationError::Context {
            reason: "subsession-bash-not-allowed",
        })
    }
}

/// The default internal-tool allowlist applied when a `subagent` tool
/// declares none: read-only (plan A.2).
#[must_use]
pub fn default_internal_tools() -> std::collections::BTreeSet<InternalTool> {
    std::collections::BTreeSet::from([InternalTool::ReadFile, InternalTool::Search])
}

#[cfg(test)]
#[path = "subsession_tests.rs"]
mod tests;
