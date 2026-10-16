//! Worker budget configuration (issue #310 acceptance criteria).
//!
//! Every budget is a typed, owner-adjustable field; [`WorkerBudgets::bootstrap_defaults`]
//! pins the bootstrap values from the issue: attempts 3, re-plan 2, worker
//! timeout 45m, concurrency 2, stall 10m, backoff 10s·2^n capped at 5m, spend
//! $10/day soft cap, run budget 4h. The loop enforces the count and time
//! bounds itself; `max_concurrent_workers` is the scheduler-facing bound (a
//! one-shot `orc worker run` executes exactly one worker) and the spend cap is
//! a *soft* cap: exceeding it is recorded on the run result, never a hard stop
//! (the run budget and worker timeout are the hard bounds).

use std::time::Duration;

use serde::Serialize;

/// Bootstrap default: maximum task attempts (issue #310).
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;
/// Bootstrap default: re-plans permitted between attempts (issue #310).
pub const DEFAULT_MAX_REPLANS: u32 = 2;
/// Bootstrap default: hard wall-clock bound for one worker run.
pub const DEFAULT_WORKER_TIMEOUT: Duration = Duration::from_mins(45);
/// Bootstrap default: workers the scheduler may run concurrently. The
/// one-shot CLI runs exactly one; the bound is consumed by the orchestration
/// scheduler lane.
pub const DEFAULT_MAX_CONCURRENT_WORKERS: u32 = 2;
/// Bootstrap default: no-progress bound; a turn that produces no tool
/// progress within this window fails the attempt as stalled.
pub const DEFAULT_STALL_TIMEOUT: Duration = Duration::from_mins(10);
/// Bootstrap default: provider-retry backoff base (10s·2^n, capped).
pub const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_secs(10);
/// Bootstrap default: provider-retry backoff cap (5m).
pub const DEFAULT_RETRY_MAX_DELAY: Duration = Duration::from_mins(5);
/// Bootstrap default: provider-call retries per model call before the attempt
/// fails. Worker-side bound; the issue fixes the backoff schedule, not a
/// retry count, so the schedule is bounded at three retries (10s/20s/40s).
pub const DEFAULT_MAX_PROVIDER_RETRIES: u32 = 3;
/// Bootstrap default: daily spend soft cap in USD (issue #310).
pub const DEFAULT_DAILY_SPEND_SOFT_CAP_USD: f64 = 10.0;
/// Bootstrap default: hard wall-clock budget for the whole run.
pub const DEFAULT_RUN_BUDGET: Duration = Duration::from_hours(4);
/// Bootstrap default: per-attempt conversation bound. The issue bounds
/// attempts, re-plans, and wall clock; a per-attempt turn bound keeps one
/// attempt finite without depending on wall-clock in tests (mini-swe-agent
/// step-limit analog).
pub const DEFAULT_MAX_TURNS_PER_ATTEMPT: u32 = 25;
/// Bootstrap default: consecutive malformed model responses that fail an
/// attempt (bounded format-error handling, mini-swe-agent analog).
pub const DEFAULT_MAX_CONSECUTIVE_FORMAT_ERRORS: u32 = 3;

/// Owner-adjustable worker budgets (issue #310 budget set).
#[derive(Clone, Debug)]
pub struct WorkerBudgets {
    /// Maximum task attempts; exhaustion is a typed failure, never a retry.
    pub max_attempts: u32,
    /// Re-plans permitted between attempts.
    pub max_replans: u32,
    /// Hard wall-clock bound for one worker run.
    pub worker_timeout: Duration,
    /// Scheduler-facing concurrency bound (one-shot CLI runs one worker).
    pub max_concurrent_workers: u32,
    /// No-progress bound; a turn window without tool progress is a stall.
    pub stall_timeout: Duration,
    /// Provider-retry backoff base delay (`base·2^n`, capped).
    pub retry_base_delay: Duration,
    /// Provider-retry backoff cap.
    pub retry_max_delay: Duration,
    /// Provider-call retries per model call before the attempt fails.
    pub max_provider_retries: u32,
    /// Daily spend soft cap in USD; exceeding is recorded, never a hard stop.
    pub daily_spend_soft_cap_usd: f64,
    /// Hard wall-clock budget for the whole run (combined with
    /// `worker_timeout`, whichever elapses first).
    pub run_budget: Duration,
    /// Per-attempt conversation bound (model calls within one attempt).
    pub max_turns_per_attempt: u32,
    /// Consecutive malformed responses that fail an attempt.
    pub max_consecutive_format_errors: u32,
    /// Per-token USD estimate feeding the soft-cap check. `0.0` disables
    /// in-run spend accrual (provider pricing wires in with the cost-ledger
    /// lane); the cap is still checked against prior daily spend.
    pub usd_per_token_estimate: f64,
}

impl WorkerBudgets {
    /// Returns the issue-#310 bootstrap budget set.
    #[must_use]
    pub fn bootstrap_defaults() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            max_replans: DEFAULT_MAX_REPLANS,
            worker_timeout: DEFAULT_WORKER_TIMEOUT,
            max_concurrent_workers: DEFAULT_MAX_CONCURRENT_WORKERS,
            stall_timeout: DEFAULT_STALL_TIMEOUT,
            retry_base_delay: DEFAULT_RETRY_BASE_DELAY,
            retry_max_delay: DEFAULT_RETRY_MAX_DELAY,
            max_provider_retries: DEFAULT_MAX_PROVIDER_RETRIES,
            daily_spend_soft_cap_usd: DEFAULT_DAILY_SPEND_SOFT_CAP_USD,
            run_budget: DEFAULT_RUN_BUDGET,
            max_turns_per_attempt: DEFAULT_MAX_TURNS_PER_ATTEMPT,
            max_consecutive_format_errors: DEFAULT_MAX_CONSECUTIVE_FORMAT_ERRORS,
            usd_per_token_estimate: 0.0,
        }
    }

    /// Returns the run deadline bound: `min(worker_timeout, run_budget)`.
    #[must_use]
    pub fn run_deadline(&self) -> Duration {
        self.worker_timeout.min(self.run_budget)
    }

    /// Serializable snapshot echoed into the run result as budget evidence.
    #[must_use]
    pub fn echo(&self) -> BudgetEcho {
        BudgetEcho {
            max_attempts: self.max_attempts,
            max_replans: self.max_replans,
            worker_timeout_secs: self.worker_timeout.as_secs(),
            max_concurrent_workers: self.max_concurrent_workers,
            stall_timeout_secs: self.stall_timeout.as_secs(),
            retry_base_delay_ms: u64::try_from(self.retry_base_delay.as_millis())
                .unwrap_or(u64::MAX),
            retry_max_delay_secs: self.retry_max_delay.as_secs(),
            max_provider_retries: self.max_provider_retries,
            daily_spend_soft_cap_usd: self.daily_spend_soft_cap_usd,
            run_budget_secs: self.run_budget.as_secs(),
            max_turns_per_attempt: self.max_turns_per_attempt,
            max_consecutive_format_errors: self.max_consecutive_format_errors,
        }
    }
}

/// Serializable budget snapshot embedded in the run result.
#[derive(Clone, Debug, Serialize)]
pub struct BudgetEcho {
    /// Maximum task attempts.
    pub max_attempts: u32,
    /// Re-plans permitted between attempts.
    pub max_replans: u32,
    /// Worker timeout in whole seconds.
    pub worker_timeout_secs: u64,
    /// Scheduler-facing concurrency bound.
    pub max_concurrent_workers: u32,
    /// Stall timeout in whole seconds.
    pub stall_timeout_secs: u64,
    /// Backoff base delay in whole milliseconds.
    pub retry_base_delay_ms: u64,
    /// Backoff cap in whole seconds.
    pub retry_max_delay_secs: u64,
    /// Provider retries per model call.
    pub max_provider_retries: u32,
    /// Daily spend soft cap in USD.
    pub daily_spend_soft_cap_usd: f64,
    /// Run budget in whole seconds.
    pub run_budget_secs: u64,
    /// Per-attempt turn bound.
    pub max_turns_per_attempt: u32,
    /// Consecutive malformed-response bound.
    pub max_consecutive_format_errors: u32,
}

/// Computes the provider-retry backoff delay: `base·2^n` capped at the
/// configured maximum (issue #310: 10s·2^n capped at 5m).
///
/// The shift saturates: absurdly large `retry_index` values yield the cap.
#[must_use]
pub fn backoff_delay(budgets: &WorkerBudgets, retry_index: u32) -> Duration {
    let factor = 2_u32.saturating_pow(retry_index.min(31));
    budgets
        .retry_base_delay
        .saturating_mul(factor)
        .min(budgets.retry_max_delay)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_defaults_match_the_issue_budget_set() {
        let budgets = WorkerBudgets::bootstrap_defaults();
        assert_eq!(budgets.max_attempts, 3);
        assert_eq!(budgets.max_replans, 2);
        assert_eq!(budgets.worker_timeout, Duration::from_mins(45));
        assert_eq!(budgets.max_concurrent_workers, 2);
        assert_eq!(budgets.stall_timeout, Duration::from_mins(10));
        assert_eq!(budgets.retry_base_delay, Duration::from_secs(10));
        assert_eq!(budgets.retry_max_delay, Duration::from_mins(5));
        assert!(
            (budgets.daily_spend_soft_cap_usd - 10.0).abs() < f64::EPSILON,
            "daily spend soft cap must be $10"
        );
        assert_eq!(budgets.run_budget, Duration::from_hours(4));
    }

    #[test]
    fn backoff_schedule_doubles_and_caps() {
        let budgets = WorkerBudgets::bootstrap_defaults();
        assert_eq!(backoff_delay(&budgets, 0), Duration::from_secs(10));
        assert_eq!(backoff_delay(&budgets, 1), Duration::from_secs(20));
        assert_eq!(backoff_delay(&budgets, 2), Duration::from_secs(40));
        assert_eq!(backoff_delay(&budgets, 3), Duration::from_secs(80));
        // Cap: 10s·2^6 = 640s > 300s cap.
        assert_eq!(backoff_delay(&budgets, 6), Duration::from_mins(5));
        assert_eq!(backoff_delay(&budgets, 30), Duration::from_mins(5));
        // Saturating shift: no overflow panic on absurd indices.
        assert_eq!(backoff_delay(&budgets, u32::MAX), Duration::from_mins(5));
    }

    #[test]
    fn run_deadline_is_the_tighter_of_worker_timeout_and_run_budget() {
        let budgets = WorkerBudgets::bootstrap_defaults();
        assert_eq!(budgets.run_deadline(), Duration::from_mins(45));
        let tightened = WorkerBudgets {
            run_budget: Duration::from_mins(5),
            ..budgets
        };
        assert_eq!(tightened.run_deadline(), Duration::from_mins(5));
    }
}
