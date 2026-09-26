//! Structured worker result types (the `orc worker run --json` output shape)
//! and the per-run configuration.

use orchestraitor_model::{ModelId, ProviderId};
use serde::Serialize;

use crate::budget::WorkerBudgets;
use crate::delivery::DeliveryOutcome;
use crate::tools::ToolReceipt;

/// Configuration for one worker run.
#[derive(Clone, Debug)]
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
}

impl WorkerConfig {
    /// Creates a config with zero prior daily spend.
    #[must_use]
    pub fn new(provider_id: ProviderId, model_id: ModelId, budgets: WorkerBudgets) -> Self {
        Self {
            provider_id,
            model_id,
            budgets,
            prior_daily_spend_usd: 0.0,
        }
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
    /// Provider call failed after bounded retries, or streamed invalid events.
    ProviderError,
    /// The mediation boundary refused or failed a bash call (fail closed).
    MediationRefused,
    /// The model declared the task not completable on every attempt.
    TaskNotCompleted,
    /// The delivery seam rejected a completed task.
    DeliveryFailed,
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
    /// Effective budgets (evidence echo).
    pub budgets: crate::budget::BudgetEcho,
}
