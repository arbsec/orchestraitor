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

/// The parent context a sub-session is carved from.
#[derive(Clone)]
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
    /// Per-invocation ordinal set by the parent's dispatch (the parent's
    /// subsession event count): makes the child's cost-attribution session
    /// id unique for each invocation of the same tool, so every child model
    /// call gets a distinct ledger `request_id` (the ledger primary key —
    /// duplicate keys are dropped, losing spend attribution).
    pub spawn_seq: u64,
    /// The parent run's cost attribution context; the child's model calls
    /// inherit it with the sub-session role and a tool-suffixed session
    /// stem (CR finding #3: child spend must land in the ledger).
    pub attribution: Option<orchestraitor_provider_neuralwatt::cost::CostAttribution>,
    /// The §13.5.1 profile label inherited from the parent run's config;
    /// recorded on the child's cost rows so A/B grouping spans sub-sessions.
    pub profile: Option<String>,
    /// The parent's cost sink; the child's per-call rows land in the same
    /// ledger (CR finding #3).
    pub cost_sink: Option<std::sync::Arc<dyn orchestraitor_provider_neuralwatt::CostSink>>,
}

impl std::fmt::Debug for SubsessionParent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The cost sink is a bare trait object (no `Debug`); echo presence,
        // mirroring `WorkerConfig`'s debug shape.
        f.debug_struct("SubsessionParent")
            .field("worktree_root", &self.worktree_root)
            .field("remaining", &self.remaining)
            .field("depth", &self.depth)
            .field("prior_daily_spend_usd", &self.prior_daily_spend_usd)
            .field("role", &self.role)
            .field("project", &self.project)
            .field("repository", &self.repository)
            .field("session_id", &self.session_id)
            .field("spawn_seq", &self.spawn_seq)
            .field("attribution", &self.attribution)
            .field("profile", &self.profile)
            .field("cost_sink", &self.cost_sink.is_some())
            .finish()
    }
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
    /// Worktree-relative paths the CHILD wrote through its (allowlisted)
    /// tool surface (CR finding #2: dropped writes would hide mutations of
    /// the shared parent worktree from the untrusted-output pipeline).
    pub untrusted_writes: Vec<String>,
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
        // Anti-stuck guardrail kills are fatal to the child attempt with no
        // rescue path; the parent sees them as a sub-session failure (the
        // child's churn/no-progress/poll state never carries over).
        FailureClass::ToolLoopChurn
        | FailureClass::NoProgress
        | FailureClass::PollBudgetExhausted => crate::guardrails::guardrail_child_class(),
    }
}

/// The internal-tool allowlist carved from a tool definition (empty for
/// command tools — they are never spawned as sub-sessions, but the type
/// must stay total).
fn allowlist_of(def: &ToolDefinition) -> std::collections::BTreeSet<InternalTool> {
    match &def.mechanism {
        ToolMechanism::Subagent { internal_tools, .. } => internal_tools.clone(),
        ToolMechanism::Command { .. } => std::collections::BTreeSet::new(),
    }
}

/// Builds the child run's config: depth+1, the carved allowlist ENFORCED by
/// the child's executor (CR finding #2), the parent's prior daily spend and
/// cost attribution/sink (CR finding #3 — the child's rows land in the same
/// ledger under a tool-suffixed session stem), and the parent's progress
/// channel (CR finding #4 — the child's beats land on the same supervisor
/// stream, so a hung child call cannot masquerade as a stalled parent).
fn child_worker_config(
    parent: &SubsessionParent,
    def: &ToolDefinition,
    role: &str,
    routed: &RoleRoutingEvidence,
    budgets: crate::budget::WorkerBudgets,
    allowed_internal: std::collections::BTreeSet<InternalTool>,
    progress: Option<&tokio::sync::watch::Sender<u64>>,
) -> WorkerConfig {
    let mut config = WorkerConfig::new(
        ProviderId::from_string(routed.provider.clone()),
        ModelId::from_string(routed.model.clone()),
        budgets,
    )
    .with_subsession_depth(parent.depth.saturating_add(1))
    .with_subsession_allowed_internal(allowed_internal)
    .with_prior_daily_spend(parent.prior_daily_spend_usd)
    // Structured-only finish (issue #535 §9.45): the child's system prompt
    // demands a fielded summary payload instead of prose when the tool
    // declares it.
    .with_structured_summary(
        def.budget.structured_summary || def.structured_summary.unwrap_or(false),
    )
    // Reasoning-effort tier (issue #535 §9.45): the child's model calls
    // send it — never a silent drop back to the routing default.
    .with_effort(def.effort)
    // The §13.5.1 profile label: the child's cost rows must group with the
    // parent run's, so the label inherits (never reset mid-run).
    .with_profile(parent.profile.clone());
    if let (Some(attribution), Some(sink)) = (&parent.attribution, &parent.cost_sink) {
        let child_attribution = orchestraitor_provider_neuralwatt::cost::CostAttribution {
            agent_domain_id: attribution.agent_domain_id.clone(),
            role: role.to_string(),
            project: parent.project.clone(),
            session: orchestraitor_model::SessionId::from_string(format!(
                "{}/tool-{}-{}",
                attribution.session.as_str(),
                def.id,
                parent.spawn_seq
            )),
            repository: orchestraitor_model::RepositoryId::from_string(parent.repository.clone()),
        };
        config = config.with_cost_tracking(child_attribution, sink.clone());
    }
    if let Some(sender) = progress {
        config = config.with_progress(sender.clone());
    }
    config
}

/// Runs one sub-session for one declared `subagent` tool (issue #535, T3).
///
/// `question` is the parent model's question (untrusted data, carried as
/// data); `instructions` come from the tool definition (trusted layer). The
/// child run inherits the guardrail posture through `run_worker` reuse.
/// `routed` is the CONTROL PLANE's resolution for the sub-session role (the
/// parent never chooses the model — spec §9.19.2), carrying the §9.35
/// decision-record evidence; it rides the outcome for the decision record.
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
    routed: &RoleRoutingEvidence,
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
    // `validate_budget` rejects 0 and the registry defaults an absent value
    // to 12; the configured bound is the enforced bound — a clamp would be
    // a silent drop of a trusted-layer setting (never a silent drop).
    budgets.max_turns_per_attempt = def.budget.max_turns.max(1);
    // One invocation = one attempt: `max_turns` bounds the WHOLE child run
    // ("maximum conversation turns for one sub-session invocation"), never
    // `max_attempts × max_turns` through re-plans — the carved budget is
    // the enforced budget, not a per-attempt ceiling.
    budgets.max_attempts = 1;
    budgets.max_replans = 0;
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

    // The child's execution surface is built STRICTLY from the allowlist
    // (CR finding #2, CWE-863): a bash mediator exists only when the
    // allowlist names `bash` — read-only sub-sessions never probe the
    // sandbox preflight — and the executor's admission gate refuses every
    // non-allowlisted internal tool (see `ToolExecutor::admit_internal`).
    let allowed_internal = allowlist_of(def);
    let bash_mediator: Option<MediatedBashMediator> = allowed_internal
        .contains(&InternalTool::Bash)
        .then(MediatedBashMediator::new);
    let bash: &dyn BashMediator = bash_mediator
        .as_ref()
        .map_or(&ReadOnlyBashStub, |mediator| mediator as &dyn BashMediator);

    let child_config = child_worker_config(
        parent,
        def,
        role,
        routed,
        budgets,
        allowed_internal,
        progress,
    );

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

    Ok(outcome_from_run(
        def.id.clone(),
        routed,
        // The summary cap is `max_summary_bytes` when set (issue #535 §9.45),
        // falling back to the budget's result cap — the tool-level override
        // MUST take effect, never a silent drop back to the default cap.
        def.max_summary_bytes.unwrap_or(def.budget.max_result_bytes),
        run,
    ))
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
    routed: &RoleRoutingEvidence,
    max_result_bytes: u64,
    run: WorkerRun,
) -> SubsessionOutcome {
    let failure: Option<TypedFailure> = run.failure.map(|failure| TypedFailure {
        class: parent_failure_class(failure.class),
        reason: failure.reason,
    });
    let cap = usize::try_from(max_result_bytes).unwrap_or(usize::MAX);
    SubsessionOutcome {
        tool_id,
        status: run.status,
        // Size cap in BYTES, not chars (CR finding #5): a char cut lets
        // multibyte text exceed the declared byte bound up to ~4×.
        summary: run
            .summary
            .as_ref()
            .map(|summary| crate::search::truncate_bytes(summary, cap)),
        failure: failure.filter(|_| run.status == RunStatus::Failed),
        usage: run.usage,
        receipts: run.receipts,
        untrusted_writes: run.untrusted_writes,
        // The control plane's own evidence (precedence path, fallback
        // reason) rides the record verbatim — the decision record is
        // replayable as §9.35 requires, not a hardcoded path.
        routing: RoleRoutingEvidence {
            role: routed.role.clone(),
            provider: routed.provider.clone(),
            model: routed.model.clone(),
            precedence_path: routed.precedence_path.clone(),
            fallback_reason: routed.fallback_reason.clone(),
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
