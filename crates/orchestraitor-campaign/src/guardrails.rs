//! Anti-stuck guardrail configuration `[loop]` (spec
//! `10-orchestrator.md` §9.27.1, §9.36 detection).
//!
//! Defaults are ACTIVE: a churn or no-progress worker burns its whole
//! budget invisibly today, so the guards ship on. Every threshold also
//! accepts an explicit `0` to disable its guard — a deliberate opt-out the
//! loop warns about (fail-loud, not fail-closed-reject: a one-line
//! `tool_repeat_count = 0` in a hobby config is the operator's call, and a
//! hard reject would just push the edit into a `#[allow]`-style comment
//! nobody reads). Absent keys inherit the bootstrap defaults.

use std::time::Duration;

/// Bootstrap default: consecutive identical worktree fingerprints that fail
/// an attempt as `no-progress`.
pub const DEFAULT_NO_PROGRESS_TURNS: u32 = 5;
/// Bootstrap default: repetitions of one tool-call shape within the window
/// that kill an attempt as `tool-loop-churn`.
pub const DEFAULT_TOOL_REPEAT_COUNT: u32 = 4;
/// Bootstrap default: churn sliding-window length in tool turns.
pub const DEFAULT_TOOL_REPEAT_WINDOW: u32 = 8;
/// Bootstrap default: cumulative poll-shaped bash wall-clock per attempt.
pub const DEFAULT_CI_POLL_BUDGET: Duration = Duration::from_mins(30);
/// Bootstrap default: cross-invocation attempts per task before the task is
/// marked `stuck` (matches the worker's own attempt bound).
pub const DEFAULT_MAX_TASK_ATTEMPTS: u32 = 3;
/// Bootstrap default: re-selection backoff after a task attempt fails.
pub const DEFAULT_TASK_RETRY_BACKOFF: Duration = Duration::from_mins(15);
/// Bootstrap default (whole seconds): cumulative CI-poll wall-clock.
pub const DEFAULT_CI_POLL_BUDGET_SECS: u64 = 30 * 60;
/// Bootstrap default (whole seconds): task re-selection backoff.
pub const DEFAULT_TASK_RETRY_BACKOFF_SECS: u64 = 15 * 60;

/// The parsed `[loop]` config block (serde shape; every key
/// optional — absent keys inherit the bootstrap defaults). Durations are
/// whole seconds in config spelling.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct GuardrailsConfigValues {
    /// Consecutive identical worktree fingerprints that fail an attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_progress_turns: Option<u32>,
    /// Repetitions of one tool-call shape within the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_repeat_count: Option<u32>,
    /// Churn sliding-window length in tool turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_repeat_window: Option<u32>,
    /// Cumulative poll-shaped bash wall-clock per attempt (seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_poll_budget_secs: Option<u64>,
    /// Cross-invocation attempts per task before a `stuck` row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_task_attempts: Option<u32>,
    /// Re-selection backoff after a failed task attempt (seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_retry_backoff_secs: Option<u64>,
}

/// Resolved guardrail thresholds for one `orc loop` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardrailsSettings {
    /// No-progress fingerprint streak threshold (`0` disables the guard).
    pub no_progress_turns: u32,
    /// Tool-shape repetition count (`0` disables the guard).
    pub tool_repeat_count: u32,
    /// Churn window length in tool turns (`0` disables the guard).
    pub tool_repeat_window: u32,
    /// Cumulative poll wall-clock per attempt (`0` disables the guard).
    pub ci_poll_budget: Duration,
    /// Cross-invocation attempts per task before a `stuck` row
    /// (`0` disables the task budget — unlimited re-selection).
    pub max_task_attempts: u32,
    /// Re-selection backoff after a failed task attempt (`0` disables it).
    pub task_retry_backoff: Duration,
}

impl Default for GuardrailsSettings {
    fn default() -> Self {
        Self {
            no_progress_turns: DEFAULT_NO_PROGRESS_TURNS,
            tool_repeat_count: DEFAULT_TOOL_REPEAT_COUNT,
            tool_repeat_window: DEFAULT_TOOL_REPEAT_WINDOW,
            ci_poll_budget: DEFAULT_CI_POLL_BUDGET,
            max_task_attempts: DEFAULT_MAX_TASK_ATTEMPTS,
            task_retry_backoff: DEFAULT_TASK_RETRY_BACKOFF,
        }
    }
}

impl GuardrailsSettings {
    /// Builds settings from the parsed `[loop]` config block:
    /// absent keys inherit the bootstrap defaults; explicit zeros disable
    /// their guard and are reported in the returned warning list (loud
    /// opt-out, never silent).
    #[must_use]
    pub fn from_config(config: Option<&crate::GuardrailsConfigValues>) -> (Self, Vec<String>) {
        let mut settings = Self::default();
        let mut warnings = Vec::new();
        let Some(config) = config else {
            return (settings, warnings);
        };
        if let Some(value) = config.no_progress_turns {
            if value == 0 {
                warnings.push(
                    "loop.no_progress_turns = 0 disables the no-progress guard; \
                     this is a deliberate opt-out"
                        .to_string(),
                );
            }
            settings.no_progress_turns = value;
        }
        if let Some(value) = config.tool_repeat_count {
            if value == 0 {
                warnings.push(
                    "loop.tool_repeat_count = 0 disables the tool-loop churn guard; \
                     this is a deliberate opt-out"
                        .to_string(),
                );
            } else if value == 1 {
                // The churn guard needs at least 2 repeats to be meaningful
                // (a single call is never a loop): 1 is silently equivalent
                // to 0, so it is warned as the opt-out it actually is.
                warnings.push(
                    "loop.tool_repeat_count = 1 disables the tool-loop churn guard \
                     (a single call can never be a loop); treat it as 0"
                        .to_string(),
                );
            }
            settings.tool_repeat_count = value;
        }
        if let Some(value) = config.tool_repeat_window {
            if value == 0 {
                warnings.push(
                    "loop.tool_repeat_window = 0 disables the tool-loop churn guard; \
                     this is a deliberate opt-out"
                        .to_string(),
                );
            }
            settings.tool_repeat_window = value;
        }
        if let Some(value) = config.ci_poll_budget_secs {
            if value == 0 {
                warnings.push(
                    "loop.ci_poll_budget_secs = 0 disables the CI-poll budget; \
                     this is a deliberate opt-out"
                        .to_string(),
                );
            }
            settings.ci_poll_budget = Duration::from_secs(value);
        }
        if let Some(value) = config.max_task_attempts {
            if value == 0 {
                warnings.push(
                    "loop.max_task_attempts = 0 disables the cross-invocation task \
                     retry budget; this is a deliberate opt-out"
                        .to_string(),
                );
            }
            settings.max_task_attempts = value;
        }
        if let Some(value) = config.task_retry_backoff_secs {
            if value == 0 {
                warnings.push(
                    "loop.task_retry_backoff_secs = 0 disables the task re-selection \
                     backoff; this is a deliberate opt-out"
                        .to_string(),
                );
            }
            settings.task_retry_backoff = Duration::from_secs(value);
        }
        (settings, warnings)
    }

    /// The worker-side guard config these settings imply.
    #[must_use]
    pub fn worker_guardrails(&self) -> orchestraitor_worker::GuardrailsConfig {
        orchestraitor_worker::GuardrailsConfig {
            no_progress_turns: self.no_progress_turns,
            tool_repeat_count: self.tool_repeat_count,
            tool_repeat_window: self.tool_repeat_window,
            ci_poll_budget: self.ci_poll_budget,
        }
    }
}

/// Typed reason a task was excluded from selection by a cross-invocation
/// guard. Carried on the pass decision record — never a silent skip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskSkipReason {
    /// The task's durable `attempts_total` reached `max_task_attempts`.
    AttemptBudget,
    /// The task's `backoff_until` is in the future.
    Backoff,
}

impl std::fmt::Display for TaskSkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AttemptBudget => write!(f, "attempt-budget"),
            Self::Backoff => write!(f, "backoff"),
        }
    }
}
