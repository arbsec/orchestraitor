//! Structured worker result types (the `orc worker run --json` output shape)
//! and the per-run configuration.

use orchestraitor_model::{ModelId, ProviderId};
use orchestraitor_provider_neuralwatt::CostSink;
use orchestraitor_provider_neuralwatt::cost::CostAttribution;
use serde::Serialize;

use crate::budget::WorkerBudgets;
use crate::delivery::DeliveryOutcome;
use crate::tools::ToolReceipt;

/// Configuration for one worker run.
pub struct WorkerConfig {
    /// Routed provider id (the control plane routes; the worker never
    /// self-selects — issue #310 non-goals).
    pub provider_id: ProviderId,
    /// Routed model id.
    pub model_id: ModelId,
    /// Owner-adjustable budgets (issue #310 set).
    pub budgets: WorkerBudgets,
    /// Daily spend already accrued before this run, in USD, fed into the
    /// soft-cap check. The one-shot CLI passes `0.0`; the scheduler/cost
    /// lanes supply real values when they wire in.
    pub prior_daily_spend_usd: f64,
    /// Optional progress-beat channel: a monotonic beat counter is sent at
    /// every turn boundary and immediately before every tool dispatch. The
    /// loop runner (`orc loop`, issue #314) watches beat *staleness* to
    /// detect a worker wedged inside a hung transport call — a case the
    /// worker-internal stall check cannot see, because it only fires between
    /// turns. `None` (the default) disables emission entirely; the payload
    /// is an opaque sequence number, not a turn count.
    pub progress: Option<tokio::sync::watch::Sender<u64>>,
    /// Anti-stuck guardrail thresholds (churn window, no-progress
    /// fingerprint streak, CI-poll budget). Defaults are active; the loop
    /// layer feeds the `[loop.guardrails]` config block through.
    pub guardrails: crate::guardrails::GuardrailsConfig,
    /// Cost attribution context for this run: which agent (task), role,
    /// session (run), project, and repository the model calls belong to.
    /// `None` (the default) means no per-call cost entries are recorded.
    pub attribution: Option<CostAttribution>,
    /// Cost sink receiving one `CostEntry` per completed model call when
    /// `attribution` is `Some`. `None` (the default) records nothing.
    pub cost_sink: Option<std::sync::Arc<dyn CostSink>>,
    /// Monotonic per-run model-call counter backing unique cost-row keys
    /// (`request_id` is the ledger primary key). Shared mutable state on
    /// the config because `call_model` takes `&WorkerConfig`.
    pub(crate) model_call_sequence: std::sync::atomic::AtomicU64,
    /// Declared tools available to this run (issue #535, T2): the resolved
    /// `[tools.<id>]` definitions visible to the run's role. Empty = the
    /// bootstrap default (no declared tools).
    pub tools: Vec<crate::tooldef::ToolDefinition>,
    /// Sub-session depth of this run: 0 for a top-level worker, 1 inside a
    /// sub-session. Declared-tool dispatch is refused at depth >= 1; there
    /// is no config path to raise it (plan C.1/S5).
    pub subsession_depth: u8,
    /// The internal tools a sub-session run may dispatch (the allowlist
    /// carved from the spawning tool definition). Empty = no internal tool
    /// is admissible inside a sub-session; top-level runs ignore this field
    /// (their bootstrap four-tool surface is not re-admitted here — the
    /// depth-0 path bypasses the internal admission gate). The executor
    /// ENFORCES this set on every internal dispatch (CR finding #2).
    pub subsession_allowed_internal: std::collections::BTreeSet<crate::tooldef::InternalTool>,
    /// The routing the control plane resolved for each sub-session role
    /// (issue #535, T3): the parent never chooses a child model. Keyed by
    /// orchestration role id; the value carries the §9.35 decision-record
    /// evidence (precedence path, fallback reason) so the spawn decision
    /// record is replayable. A subagent tool whose role is missing here
    /// fails typed at dispatch (`subsession-role-unrouted`).
    pub subsession_routing:
        std::collections::BTreeMap<String, crate::subsession::RoleRoutingEvidence>,
    /// Reasoning-effort tier for this run's model calls (issue #535 §9.45):
    /// carried from the spawning tool definition. `None` = the routing
    /// default (no explicit effort on the wire).
    pub effort: Option<crate::tooldef::ReasoningEffort>,
    /// Named configuration profile the run resolved through (spec §9.22.5);
    /// recorded on every cost entry the run writes so §13.5.1 A/B
    /// comparisons can group by profile. `None` = no profile label.
    pub profile: Option<String>,
    /// Structured-only finish (issue #535 §9.45): the system prompt demands
    /// a compact fielded summary payload instead of prose narrative.
    pub structured_summary: bool,
}

impl std::fmt::Debug for WorkerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerConfig")
            .field("provider_id", &self.provider_id)
            .field("model_id", &self.model_id)
            .field("budgets", &self.budgets)
            .field("prior_daily_spend_usd", &self.prior_daily_spend_usd)
            .field("progress", &self.progress)
            .field("guardrails", &self.guardrails)
            .field("attribution", &self.attribution)
            .field("cost_sink", &self.cost_sink.is_some())
            .field(
                "model_calls_recorded",
                &self
                    .model_call_sequence
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
            .field(
                "tools",
                &self.tools.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            )
            .field("subsession_depth", &self.subsession_depth)
            .field(
                "subsession_allowed_internal",
                &self
                    .subsession_allowed_internal
                    .iter()
                    .map(|tool| tool.tool_name())
                    .collect::<Vec<_>>(),
            )
            .field(
                "subsession_routing",
                &self.subsession_routing.keys().collect::<Vec<_>>(),
            )
            .field("effort", &self.effort)
            .field("profile", &self.profile)
            .field("structured_summary", &self.structured_summary)
            .finish()
    }
}

impl WorkerConfig {
    /// Creates a config with zero prior daily spend and no progress channel.
    #[must_use]
    pub fn new(provider_id: ProviderId, model_id: ModelId, budgets: WorkerBudgets) -> Self {
        Self {
            provider_id,
            model_id,
            budgets,
            prior_daily_spend_usd: 0.0,
            progress: None,
            guardrails: crate::guardrails::GuardrailsConfig::bootstrap_defaults(),
            attribution: None,
            cost_sink: None,
            model_call_sequence: std::sync::atomic::AtomicU64::new(0),
            tools: Vec::new(),
            subsession_depth: 0,
            subsession_allowed_internal: std::collections::BTreeSet::new(),
            subsession_routing: std::collections::BTreeMap::new(),
            effort: None,
            profile: None,
            structured_summary: false,
        }
    }

    /// Sets the declared tools visible to this run and the sub-session
    /// depth (issue #535, T2). Callers map the core `ToolRegistry` (already
    /// layer-gated and role-filtered) into definitions here.
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<crate::tooldef::ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    /// Sets the sub-session depth (1 inside a sub-session; never raised by
    /// configuration).
    #[must_use]
    pub const fn with_subsession_depth(mut self, depth: u8) -> Self {
        self.subsession_depth = depth;
        self
    }

    /// Sets the reasoning-effort tier for this run's model calls (issue
    /// #535 §9.45).
    #[must_use]
    pub fn with_effort(mut self, effort: Option<crate::tooldef::ReasoningEffort>) -> Self {
        self.effort = effort;
        self
    }

    /// Sets the profile label recorded on this run's cost entries (spec
    /// §9.22.5, §13.5.1).
    #[must_use]
    pub fn with_profile(mut self, profile: Option<String>) -> Self {
        self.profile = profile;
        self
    }

    /// Sets the structured-only finish requirement (issue #535 §9.45).
    #[must_use]
    pub const fn with_structured_summary(mut self, structured_summary: bool) -> Self {
        self.structured_summary = structured_summary;
        self
    }

    /// Sets the internal-tool allowlist for a sub-session run (CR finding
    /// #2): the executor refuses every internal dispatch outside this set.
    #[must_use]
    pub fn with_subsession_allowed_internal(
        mut self,
        allowed: std::collections::BTreeSet<crate::tooldef::InternalTool>,
    ) -> Self {
        self.subsession_allowed_internal = allowed;
        self
    }

    /// Attaches a progress-beat channel (see [`WorkerConfig::progress`]).
    #[must_use]
    pub fn with_progress(mut self, progress: tokio::sync::watch::Sender<u64>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Sets the daily spend already accrued before this run (fed into the
    /// soft-cap check; sub-session configs inherit the parent's value).
    #[must_use]
    pub fn with_prior_daily_spend(mut self, prior_daily_spend_usd: f64) -> Self {
        self.prior_daily_spend_usd = prior_daily_spend_usd;
        self
    }

    /// Attaches per-call cost attribution and the sink that receives the
    /// entries (see [`WorkerConfig::attribution`]). Both are set together:
    /// attribution without a sink (or the reverse) records nothing.
    #[must_use]
    pub fn with_cost_tracking(
        mut self,
        attribution: CostAttribution,
        sink: std::sync::Arc<dyn CostSink>,
    ) -> Self {
        self.attribution = Some(attribution);
        self.cost_sink = Some(sink);
        self
    }
}

/// Run status in the structured result.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    /// The task completed and was handed to the delivery seam.
    Completed,
    /// The run ended with a typed failure.
    Failed,
}

/// Typed failure classes (kebab-case in the result JSON).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureClass {
    /// Attempt budget exhausted (never an infinite retry).
    AttemptBudgetExhausted,
    /// Re-plan budget exhausted.
    ReplanBudgetExhausted,
    /// Worker timeout / run budget deadline elapsed.
    WorkerTimeout,
    /// No tool progress within the stall window.
    Stalled,
    /// Per-attempt turn bound exhausted.
    TurnBudgetExhausted,
    /// Consecutive malformed responses exhausted their bound.
    FormatErrorsExhausted,
    /// The same normalized tool-call shape repeated K times within the last
    /// W turns (anti-stuck churn guard, spec `10-orchestrator.md` §9.36 detection).
    ToolLoopChurn,
    /// N consecutive turns produced an identical worktree progress
    /// fingerprint (anti-stuck no-progress guard).
    NoProgress,
    /// Cumulative poll-shaped bash wall-clock (sleep + CI wait fingerprints)
    /// exceeded the per-attempt CI-poll budget; the task parks
    /// blocked-on-external instead of burning the session.
    PollBudgetExhausted,
    /// Provider call failed after bounded retries, or streamed invalid events.
    ProviderError,
    /// The mediation boundary refused or failed a bash call (fail closed).
    MediationRefused,
    /// The model declared the task not completable on every attempt.
    TaskNotCompleted,
    /// The delivery seam rejected a completed task.
    DeliveryFailed,
    /// A declared-tool sub-session exhausted its carved budget (turns,
    /// wall clock, or deadline) or failed its bounded guardrails. The child
    /// class detail rides the reason code; the parent sees one uniform
    /// budget-exhaustion class (issue #535, T3).
    SubsessionBudgetExhausted,
    /// A declared-tool dispatch was refused at the sub-session depth limit
    /// (sub-sessions cannot spawn sub-sessions; issue #535, T3).
    SubsessionDepthExceeded,
    /// A declared-tool sub-session run failed for a non-budget reason
    /// (provider error, mediation refusal, task-not-completed). The child
    /// class detail rides the reason code (issue #535, T3).
    SubsessionFailed,
}

/// A typed run failure: class plus static reason code.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TypedFailure {
    /// Failure class.
    pub class: FailureClass,
    /// Static reason code (log-safe, spec §9.23.4).
    pub reason: &'static str,
}

/// Accumulated token usage for the run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct UsageTotals {
    /// Input tokens across all model calls.
    pub input_tokens: u64,
    /// Output tokens across all model calls.
    pub output_tokens: u64,
}

/// The structured worker result (the `--json` output shape).
#[derive(Clone, Debug, Serialize)]
pub struct WorkerRun {
    /// Task id echoed from the request.
    pub task_id: String,
    /// Final status.
    pub status: RunStatus,
    /// Process-style exit code: 0 on success, 1 on typed failure.
    pub exit_code: i32,
    /// Completion summary (on success).
    pub summary: Option<String>,
    /// Typed failure (on failure).
    pub failure: Option<TypedFailure>,
    /// Delivery outcome (on success).
    pub delivery: Option<DeliveryOutcome>,
    /// Attempts consumed.
    pub attempts: u32,
    /// Re-plans consumed.
    pub replans: u32,
    /// Model-call turns consumed across all attempts.
    pub turns: u32,
    /// Model calls issued (including retried calls).
    pub model_calls: u32,
    /// Token usage totals.
    pub usage: UsageTotals,
    /// Whether the daily spend soft cap was exceeded (soft: recorded only).
    pub spend_soft_cap_exceeded: bool,
    /// Worktree-relative paths written by the worker (untrusted output, spec
    /// §9.14).
    pub untrusted_writes: Vec<String>,
    /// Per-tool-call receipts.
    pub receipts: Vec<ToolReceipt>,
    /// Sub-session decision events (issue #535): one per subagent-tool
    /// invocation (outcome or pre-run refusal), carrying the routing and
    /// effort evidence the §9.35 decision records consume.
    pub subsession_events: Vec<crate::model::SubsessionEvent>,
    /// Effective budgets (evidence echo).
    pub budgets: crate::budget::BudgetEcho,
}
