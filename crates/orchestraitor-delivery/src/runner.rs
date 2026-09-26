//! Backlog runner execution loop (spec §9.33.3, wiring §9.33.5 and §9.26).
//!
//! The runner drives the validated backlog DAG ([`crate::dag::TaskDag`]) and
//! continues until one of the §9.33.3 stop conditions fires: the backlog is
//! empty ([`StopReason::BacklogEmpty`]), no task is eligible
//! ([`StopReason::NoEligibleTasks`]), the configured attempt budget is
//! exhausted ([`StopReason::BudgetExhausted`]), an approval or user decision
//! is required ([`StopReason::ApprovalRequired`]), repeated failures exhaust
//! the escalation ladder ([`StopReason::FailuresBlocked`]), a security
//! invariant or policy blocks progress ([`StopReason::SecurityBlock`]), or
//! the user pauses the run ([`StopReason::Paused`]).
//!
//! This is a deterministic, synchronous, I/O-free state machine. Every effect
//! (provider call, tool invocation, agent spawn) happens in the runtime
//! layer, which feeds each started attempt's outcome back through
//! [`BacklogRunner::tick`] as an injectable [`AttemptOutcome`]. There is no
//! clock: retry delays and rate-limit `retry-after` hints are plain data the
//! runtime stamps per §9.26.2, and the tick boundary is the runner's only
//! time model — a scheduled retry or hold becomes due on the next tick.
//!
//! Decision chain for every failed attempt (§9.33.5, applied verbatim):
//!
//! ```text
//! AttemptOutcome::Failed
//!   -> failures::classify            (propose a RetryDecision)
//!   -> retry_rules::RetryGate::evaluate  (re-assert §9.26.3 + non-retriable invariants)
//!   -> runner executes the gated decision:
//!        Retry        -> RetryScheduled, bounded RetrySchedule backoff
//!        Hold         -> RetryHeld, honoring the provider's retry-after hint
//!        FixRootCause -> FixRootCause, then re-enter via a root-cause fix spawn
//!        Reprompt     -> Reprompt, fresh-context re-prompt bounded by
//!                        max_reprompt_attempts
//!        Escalate     -> walk escalation::EscalationState one ladder step
//!        AwaitUser    -> block the task, stop the run for user action
//! ```
//!
//! Retry gate wiring (§9.33.5 "NEVER blindly retry side-effecting actions"):
//! an [`AttemptOutcome::Failed`] without an [`IdempotencyProof`] is treated
//! as unproven, so a tool/process failure is overridden to
//! [`RetryDecision::FixRootCause`] and never retried blindly. Policy denials,
//! approval requirements, and non-retriable configuration or security
//! failures pass through the same gate and can never be laundered into a
//! retry (§9.33.5).
//!
//! §9.33.4 hard rule (the centerpiece): reaching the configured per-step
//! attempt budget walks [`crate::escalation::EscalationState`] one ladder
//! step; repeated failures walk same-agent-fresh-context → alternate model →
//! domain expert → revised plan until the terminal
//! [`EscalationStep::HumanEscalation`], which journals
//! [`RunnerEvent::HumanEscalation`], blocks the task, and stops the run with
//! [`StopReason::FailuresBlocked`] — an explicit blocked/needs-human state,
//! never silent approval. No code path in this module can mark an escalated
//! task completed or turn a stopped run into an approval.
//!
//! Fresh-context rule (§9.33.1, §7.3): fresh context per spawn is the
//! runner's responsibility to trigger and the runtime's to construct. The
//! runner only records *which* attempt runs next; every
//! [`RunnerEvent::Started`] attempt, reprompt, or escalation step is a fresh,
//! minimal context derived from the spec, task metadata, workspace state, and
//! dependency outputs — never accumulated conversation (§7.3 context
//! poisoning). This module threads no conversation state anywhere.
//!
//! Concurrency wiring (spec §9.33.3: "Parallel execution MUST respect
//! configurable concurrency, repository conflicts, resource budgets
//! (§9.27), provider limits (§9.19.5-§9.19.6), and review capacity."):
//! [`RunnerInput::scheduler_config`] is validated at construction (a zero
//! cap can never dispatch, so construction fails closed) and every
//! [`BacklogRunner::tick`] dispatch intersects its startable set with the
//! cap-constrained [`crate::schedule::ParallelScheduler::select`].
//! Enforced here: global `max_concurrent` (in-flight = the `running` set of
//! dispatched attempts whose outcomes have not been consumed; a slot is
//! free while `running.len()` < `max_concurrent`), per-domain
//! `max_per_domain` (every DAG task carries a [`crate::metadata::DomainId`]),
//! expected-file repository-conflict exclusion, and running
//! implementations' share of `review_capacity` (every running attempt is a
//! change set on its way to review). Deferred to the runtime layer: the
//! under-review change-set count `review_capacity` also bounds (owned by
//! the change-set review pipeline of §9.33.4 — the runner passes 0 and the
//! pipeline applies it when wiring), provider limits (§9.19.5–§9.19.6,
//! surfaced through outcomes such as [`FailureClass::RateLimit`]), and
//! resource budgets beyond the attempt budget (§9.27). Cap exhaustion is
//! never a stop reason: skipped tasks stay eligible and re-compete on the
//! next tick in stable task-ID order, so a full cap stalls neither the run
//! nor its event stream. A slot the scheduler reserves for a retry
//! scheduled during the same tick's outcome consumption idles for that
//! tick — the tick-boundary rule (a retry never re-fires within its own
//! tick) outranks slot utilization.
//!
//! Security boundary (spec §2.2, §9.33.7): the runner proposes, schedules,
//! and stops — it makes no security decisions (allow/deny/verdict), bypasses
//! no approval requirement, and owns no enforcement. Promotion remains with
//! the runner/policy layer's caller and Arbitraitor; this engine never emits
//! an approve/promote decision — stopping is its only authority.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::dag::TaskDag;
use crate::escalation::{EscalationPolicy, EscalationPolicyError, EscalationState, EscalationStep};
use crate::failures::{FailureClass, RetryDecision, classify};
use crate::metadata::BacklogTaskId;
use crate::retry_rules::{IdempotencyProof, RetryGate, RetrySchedule};
use crate::review_loop::{ReviewLoopConfig, ReviewLoopConfigError};
use crate::schedule::{ParallelScheduler, SchedulerConfig, SchedulerConfigError};

/// What the environment reports for one spawned task attempt
/// (§9.33.3–§9.33.5).
///
/// Plain data only: the runtime layer reports the outcome and the runner
/// records it verbatim in [`RunnerEvent::AttemptRecorded`] before acting on
/// it. Serialization is the `snake_case` form persisted in the §9.33.6
/// durable store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    /// The attempt finished; the task's §9.33.2 completion evidence exists.
    /// The runner records completion only — merging or promotion is decided
    /// by the caller and Arbitraitor, never by this runner (§9.33.7).
    Completed,
    /// The attempt failed. The runner classifies `class` and gates the
    /// proposed decision through [`RetryGate::evaluate`] before acting.
    Failed {
        /// Normalized §9.33.5 failure class.
        class: FailureClass,
        /// Provider `retry-after` hint in milliseconds, meaningful for
        /// [`FailureClass::RateLimit`] (§9.33.5 "honor retry-after").
        retry_after_ms: Option<u64>,
        /// §9.26.3 idempotency evidence for [`FailureClass::ToolOrProcess`]
        /// failures. `None` is treated as [`IdempotencyProof::Unproven`]: a
        /// side-effecting failure without proof is never retried blindly.
        proof: Option<IdempotencyProof>,
    },
    /// The attempt stopped because an approval or user decision is required
    /// (§9.24 `approval-required` state; §9.33.3 stop condition). The
    /// approval itself is owned by Arbitraitor (§2.2, §9.9); the runner only
    /// blocks the task and stops the run.
    ApprovalRequired,
    /// The attempt was refused by policy (§9.33.3 "a security invariant or
    /// organization policy blocks progress"). The refusal is owned by
    /// Arbitraitor (§2.2); the runner records it, blocks the task, and stops
    /// the run.
    PolicyDenied,
    /// The attempt was interrupted by a user/admin pause (§9.24 `paused`
    /// state; §9.33.3 stop condition). The attempt stays open in the runner's
    /// running set: after [`BacklogRunner::resume`], the runtime resumes from
    /// checkpoint or delivers a fresh outcome for it (§9.24.2).
    Paused,
}

/// Why the runner stopped (§9.33.3 stop condition list).
///
/// Stopping is the runner's only authority: there is no approve/pass stop
/// reason. `BacklogEmpty` means every task completed; every other variant
/// leaves tasks unfinished and requires resolution (§9.33.8: "a blocked
/// backlog is a visible state requiring resolution, not an excuse to loop
/// forever").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Every task in the backlog completed (§9.33.3 "the backlog is empty";
    /// §9.33.8 "an empty backlog is success").
    BacklogEmpty,
    /// Tasks remain but none is eligible: dependencies unsatisfiable (a
    /// dependency cycle) or every remaining task blocked (§9.33.3 "no task is
    /// currently eligible").
    NoEligibleTasks,
    /// The configured attempt budget reached zero while eligible work
    /// remained (§9.33.3 "a configured budget or time limit is reached").
    BudgetExhausted,
    /// An approval or user decision is required (§9.33.3, §9.24
    /// `approval-required`). The approval belongs to Arbitraitor (§2.2).
    ApprovalRequired,
    /// Repeated failures walked the §9.33.5 escalation ladder to its terminal
    /// human-escalation step — the explicit blocked/needs-human state §9.33.4
    /// mandates, never silent approval.
    FailuresBlocked,
    /// A policy denial or non-retriable security failure blocked progress
    /// (§9.33.3 "a security invariant or organization policy blocks
    /// progress"). Resolution belongs to Arbitraitor (§2.2).
    SecurityBlock,
    /// The user paused the run (§9.33.3 "the user pauses or cancels the run";
    /// §9.24 `paused` state). Resumable via [`BacklogRunner::resume`].
    Paused,
}

/// Why a task is blocked (§9.33.8 "a blocked backlog is a visible state").
///
/// Every block reason pairs with a run-level [`StopReason`]: the runner
/// blocks the task, records the visible state, and stops. No reason here is
/// an approval, and no blocked task ever becomes eligible again on its own —
/// resolution flows through the user and Arbitraitor (§2.2, §9.33.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockReason {
    /// The §9.33.5 escalation ladder reached its terminal
    /// [`EscalationStep::HumanEscalation`] step (§9.33.4 explicit
    /// `blocked`/`needs-human` state). Pairs with
    /// [`StopReason::FailuresBlocked`].
    HumanEscalation,
    /// A policy denial or non-retriable security failure blocked the task;
    /// resolution belongs to Arbitraitor (§2.2, §9.33.5). Pairs with
    /// [`StopReason::SecurityBlock`].
    SecurityBlock,
    /// The task awaits an approval or user decision (§9.24
    /// `approval-required`). Pairs with [`StopReason::ApprovalRequired`].
    ApprovalRequired,
}

/// One entry of the runner's durable decision journal (§9.33.6).
///
/// [`BacklogRunner::tick`] and [`BacklogRunner::run`] return the events they
/// appended; [`BacklogRunner::journal`] is the append-only ordered record of
/// everything the runner did and decided. Serialization is the `snake_case`
/// form persisted in the §9.33.6 durable store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerEvent {
    /// An attempt was dispatched to the runtime: fresh context per spawn is
    /// the runner's responsibility at this point (§9.33.1) and costs one
    /// unit of the configured attempt budget (§9.33.3).
    Started {
        /// Task whose attempt was dispatched.
        task: BacklogTaskId,
    },
    /// An environment-reported [`AttemptOutcome`] was consumed for a running
    /// attempt. `attempt` pairs the outcome to its 1-based [`Started`]
    /// dispatch for the task.
    AttemptRecorded {
        /// Task the outcome belongs to.
        task: BacklogTaskId,
        /// 1-based attempt number of the run this outcome completes.
        attempt: u32,
        /// The verbatim outcome reported by the environment.
        outcome: AttemptOutcome,
    },
    /// A transient or proven-idempotent tool/process retry was scheduled
    /// ([`RetryDecision::Retry`], gated per §9.26.3). `delay_ms` comes from
    /// the task's bounded [`RetrySchedule`]; the runtime stamps actual
    /// §9.26.2 timing (with jitter) on top.
    RetryScheduled {
        /// Task to retry on the next tick.
        task: BacklogTaskId,
        /// Proposed backoff delay in milliseconds.
        delay_ms: u64,
    },
    /// A rate-limited task is held for the provider's `retry-after` hint
    /// ([`RetryDecision::Hold`], §9.33.5 "retryable, honor retry-after").
    RetryHeld {
        /// Task to retry once the hold elapses (next tick).
        task: BacklogTaskId,
        /// Provider-supplied hold in milliseconds.
        retry_after_ms: u64,
    },
    /// A verification failure, merge conflict, or unproven side-effecting
    /// tool failure must fix its root cause or resolve its conflict first
    /// ([`RetryDecision::FixRootCause`], §9.33.5) — never a blind retry. The
    /// task re-enters through a root-cause fix spawn (fresh context,
    /// §9.33.1); repeated occurrences walk the escalation ladder.
    FixRootCause {
        /// Task whose attempt must fix a root cause.
        task: BacklogTaskId,
    },
    /// Invalid agent output is re-prompted with a fresh context
    /// ([`RetryDecision::Reprompt`], §9.33.5), bounded by the configured
    /// reprompt limit.
    Reprompt {
        /// Task to re-prompt.
        task: BacklogTaskId,
        /// 1-based attempt number of the next fresh-context run.
        attempt: u32,
    },
    /// Repeated failures exhausted the current ladder step's attempt budget,
    /// so the runner advanced [`crate::escalation::EscalationState`] to
    /// `step`. The next spawn at this step is a fresh context (§9.33.1,
    /// §7.3) — never the accumulated conversation of prior attempts.
    EscalationStep {
        /// Task being escalated.
        task: BacklogTaskId,
        /// The ladder step whose agent/model/profile spawns next.
        step: EscalationStep,
    },
    /// Terminal ladder proposal (§9.33.5): escalate to a human. Never a
    /// permission grant — the task is blocked and the run stops with
    /// [`StopReason::FailuresBlocked`] awaiting human direction (§9.33.4).
    HumanEscalation {
        /// Task handed to a human.
        task: BacklogTaskId,
    },
    /// A task was parked with a visible [`BlockReason`] (§9.33.8).
    Blocked {
        /// Task that is blocked.
        task: BacklogTaskId,
        /// Visible reason for the block.
        reason: BlockReason,
    },
    /// The run was paused (§9.24 `paused` state; §9.33.6 `pause` control).
    /// No attempts start and no outcomes are consumed while paused.
    Paused,
    /// The run was resumed after a pause (§9.33.6 `resume` control).
    Resumed,
    /// The configured attempt budget reached zero while eligible work
    /// remained (§9.33.3). Always followed by
    /// [`RunnerEvent::Stopped`] with [`StopReason::BudgetExhausted`].
    BudgetExhausted,
    /// The run stopped with the given [`StopReason`]. Terminal: further
    /// ticks record nothing unless the reason was [`StopReason::Paused`] and
    /// the run is resumed.
    Stopped {
        /// Why the runner stopped.
        reason: StopReason,
    },
}

/// Construction-time runner failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RunnerError {
    /// The review-loop configuration is structurally invalid (§9.33.4).
    #[error("invalid review loop configuration: {0}")]
    InvalidReviewLoopConfig(#[from] ReviewLoopConfigError),
    /// The scheduler configuration is structurally invalid (§9.33.3): a
    /// zero cap can never dispatch anything, so construction fails closed.
    #[error("invalid scheduler configuration: {0}")]
    InvalidSchedulerConfig(#[from] SchedulerConfigError),
    /// The escalation policy is structurally invalid (§9.33.5).
    #[error("invalid escalation policy: {0}")]
    InvalidEscalationPolicy(#[from] EscalationPolicyError),
}

/// Plain-data inputs for one [`BacklogRunner`] run (§9.33 "sane defaults,
/// configurable through §9.22").
///
/// Everything the runner needs is injected here: the validated backlog DAG,
/// the review-loop configuration, the scheduler configuration, and the
/// budget knobs. There are no tokio, clock, provider, or agent inputs —
/// effects stay in the runtime layer.
#[derive(Debug)]
pub struct RunnerInput<'a> {
    /// Validated backlog DAG (§9.33.2). Eligibility derives from
    /// [`TaskDag::eligible`] minus blocked, running, and pending tasks.
    pub dag: &'a TaskDag,
    /// Review-loop configuration (§9.33.4). Validated at construction so the
    /// run never starts under parameters that could not render the mandated
    /// blocked/needs-human states. Review-loop execution itself belongs to
    /// the change-set review pipeline, not this runner.
    pub config: &'a ReviewLoopConfig,
    /// Scheduler configuration (§9.33.3 "configurable concurrency"): the
    /// [`ParallelScheduler`] the runner clones into its dispatch loop and
    /// consults on every tick. Validated at construction — a zero cap can
    /// never schedule anything, so [`BacklogRunner::new`] fails closed with
    /// [`RunnerError::InvalidSchedulerConfig`]. The runner enforces the
    /// global and per-domain concurrency caps, expected-file
    /// repository-conflict exclusion, and running implementations' share of
    /// `review_capacity`; the under-review change-set count the capacity
    /// also bounds is owned by the change-set review pipeline (runtime
    /// layer), which applies it on top when wiring the runner.
    pub scheduler_config: SchedulerConfig,
    /// Total attempt budget (§9.33.3 "a configured budget or time limit is
    /// reached"): every [`RunnerEvent::Started`] attempt costs one unit, so
    /// `attempt_budget` bounds the maximum number of spawned attempts.
    pub attempt_budget: u32,
    /// Maximum bounded fresh-context reprompts for
    /// [`FailureClass::InvalidAgentOutput`] before escalation (§9.33.5,
    /// §9.26.2). Fed verbatim to [`classify`].
    pub max_reprompt_attempts: u32,
    /// Failed attempts tolerated per escalation ladder step before the runner
    /// advances to the next step (§9.33.5; per-step attempt budgets are
    /// retry-policy configuration owned by the runner per §9.26/§9.22.1).
    /// Zero escalates on the first failure.
    pub max_attempts_per_step: u32,
    /// Escalation ladder policy (§9.33.5): the full five-step ladder by
    /// default ([`EscalationPolicy::default`]). Validated at construction; a
    /// policy may shorten the ordered ladder but never reorder it (§9.22.4).
    pub escalation_policy: EscalationPolicy,
}

/// Deterministic backlog execution loop (spec §9.33.3).
///
/// One [`BacklogRunner::tick`] consumes the outcomes reported since the last
/// tick, applies the gated failure decisions, dispatches newly eligible
/// attempts, and emits the run-level stop conditions. [`BacklogRunner::run`]
/// loops `tick` until a [`StopReason`] fires or the tick bound is reached.
/// The durable record of everything the runner did is the append-only
/// [`BacklogRunner::journal`].
///
/// Outcome feeding contract: outcomes are consumed only for tasks the runner
/// dispatched and still holds running; outcomes for unknown, completed, or
/// blocked tasks are ignored — the journal records only what the runner acted
/// on. When a run-level stop fires mid-tick, consumption halts: the earliest
/// stop in stable task-ID order wins.
pub struct BacklogRunner<'a> {
    /// Validated backlog DAG (borrowed; the runner never mutates it).
    dag: &'a TaskDag,
    /// Validated review-loop configuration (§9.33.4).
    config: &'a ReviewLoopConfig,
    /// Deterministic scheduler consulted on every dispatch (§9.33.3).
    scheduler: ParallelScheduler,
    /// Remaining attempt budget; each started attempt spends one unit.
    budget_remaining: u32,
    /// Bounded reprompt limit for [`FailureClass::InvalidAgentOutput`].
    max_reprompt_attempts: u32,
    /// Failed attempts tolerated per escalation step before advancing.
    max_attempts_per_step: u32,
    /// Append-only durable journal of runner events (§9.33.6).
    journal: Vec<RunnerEvent>,
    /// Tasks whose completion evidence exists (§9.33.2).
    completed: BTreeSet<BacklogTaskId>,
    /// Tasks parked with a visible reason; never eligible again.
    blocked: BTreeMap<BacklogTaskId, BlockReason>,
    /// Dispatched attempts awaiting an environment-reported outcome.
    running: BTreeSet<BacklogTaskId>,
    /// Tasks due to (re-)attempt on the next tick: scheduled retries, holds,
    /// reprompts, root-cause fix re-entries, and escalation re-attempts.
    pending: BTreeSet<BacklogTaskId>,
    /// Per-task monotonic 1-based attempt counter.
    attempts: BTreeMap<BacklogTaskId, u32>,
    /// Per-task bounded backoff schedule; reset when the ladder advances.
    retry_schedules: BTreeMap<BacklogTaskId, RetrySchedule>,
    /// Per-task escalation ladder position, created for every DAG task at
    /// construction.
    escalation: BTreeMap<BacklogTaskId, EscalationState>,
    /// User/admin pause flag (§9.24 `paused` state).
    paused: bool,
    /// The run-level stop reason once fired; terminal except for
    /// [`StopReason::Paused`], which [`BacklogRunner::resume`] clears.
    stop_reason: Option<StopReason>,
}

impl std::fmt::Debug for BacklogRunner<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BacklogRunner")
            .field("budget_remaining", &self.budget_remaining)
            .field("scheduler", &self.scheduler)
            .field("completed", &self.completed)
            .field("blocked", &self.blocked)
            .field("running", &self.running)
            .field("pending", &self.pending)
            .field("paused", &self.paused)
            .field("stop_reason", &self.stop_reason)
            .finish_non_exhaustive()
    }
}

impl<'a> BacklogRunner<'a> {
    /// Creates a runner over `input`, validating the review-loop
    /// configuration, the scheduler configuration, and the per-task
    /// escalation states it derives from the escalation policy (§9.33.3,
    /// §9.33.4, §9.33.5).
    ///
    /// # Errors
    ///
    /// Returns [`RunnerError::InvalidReviewLoopConfig`] when the review-loop
    /// configuration is structurally invalid,
    /// [`RunnerError::InvalidSchedulerConfig`] when the scheduler
    /// configuration could never dispatch anything, or
    /// [`RunnerError::InvalidEscalationPolicy`] when the escalation policy
    /// cannot produce valid per-task ladder states.
    pub fn new(input: &RunnerInput<'a>) -> Result<Self, RunnerError> {
        input.config.validate()?;
        input.scheduler_config.validate()?;
        input.escalation_policy.validate()?;
        let mut escalation = BTreeMap::new();
        for (id, _) in input.dag.iter() {
            escalation.insert(
                id.clone(),
                EscalationState::new(input.escalation_policy.clone())?,
            );
        }
        Ok(Self {
            dag: input.dag,
            config: input.config,
            scheduler: ParallelScheduler::new(input.scheduler_config.clone()),
            budget_remaining: input.attempt_budget,
            max_reprompt_attempts: input.max_reprompt_attempts,
            max_attempts_per_step: input.max_attempts_per_step,
            journal: Vec::new(),
            completed: BTreeSet::new(),
            blocked: BTreeMap::new(),
            running: BTreeSet::new(),
            pending: BTreeSet::new(),
            attempts: BTreeMap::new(),
            retry_schedules: BTreeMap::new(),
            escalation,
            paused: false,
            stop_reason: None,
        })
    }

    /// The review-loop configuration this runner was constructed with
    /// (§9.33.4).
    #[must_use]
    pub const fn config(&self) -> &ReviewLoopConfig {
        self.config
    }

    /// The scheduler configuration this runner dispatches under (§9.33.3).
    #[must_use]
    pub const fn scheduler_config(&self) -> &SchedulerConfig {
        self.scheduler.config()
    }

    /// The append-only ordered journal of everything the runner did and
    /// decided (§9.33.6).
    #[must_use]
    pub fn journal(&self) -> &[RunnerEvent] {
        &self.journal
    }

    /// Tasks completed so far.
    #[must_use]
    pub fn completed(&self) -> &BTreeSet<BacklogTaskId> {
        &self.completed
    }

    /// Blocked tasks with their visible reasons (§9.33.8).
    #[must_use]
    pub fn blocked(&self) -> &BTreeMap<BacklogTaskId, BlockReason> {
        &self.blocked
    }

    /// Attempt budget remaining (§9.33.3).
    #[must_use]
    pub const fn budget_remaining(&self) -> u32 {
        self.budget_remaining
    }

    /// Whether the run is paused (§9.24 `paused` state).
    #[must_use]
    pub const fn is_paused(&self) -> bool {
        self.paused
    }

    /// The stop reason once the run has stopped.
    #[must_use]
    pub const fn stop_reason(&self) -> Option<StopReason> {
        self.stop_reason
    }

    /// User/admin pause control (§9.33.6 `pause`): journals
    /// [`RunnerEvent::Paused`]. No outcomes are consumed and no attempts
    /// start while paused; the next tick stops the run with
    /// [`StopReason::Paused`]. Idempotent.
    pub fn pause(&mut self) {
        if self.paused {
            return;
        }
        self.paused = true;
        self.push_event(RunnerEvent::Paused);
    }

    /// User/admin resume control (§9.33.6 `resume`): journals
    /// [`RunnerEvent::Resumed`], clears the pause flag and a
    /// [`StopReason::Paused`] stop so the run can continue. Attempts open at
    /// pause time stay running; their checkpoint resume or re-delivery is the
    /// runtime's job (§9.24.2). Idempotent.
    pub fn resume(&mut self) {
        if !self.paused {
            return;
        }
        self.paused = false;
        self.push_event(RunnerEvent::Resumed);
        if self.stop_reason == Some(StopReason::Paused) {
            self.stop_reason = None;
        }
    }

    /// Runs one deterministic step (§9.33.3) and returns the events it
    /// appended to the journal:
    ///
    /// 1. consume `outcomes` for running tasks (stable task-ID order),
    ///    recording each and applying the gated classification decision;
    /// 2. stop immediately when a run-level condition fired;
    /// 3. dispatch due retries and newly dependency-satisfied tasks
    ///    (eligibility = [`TaskDag::eligible`] minus blocked, running, and
    ///    pending tasks), intersected with the scheduler's cap-constrained
    ///    selection per §9.33.3 and spending one budget unit per
    ///    [`RunnerEvent::Started`];
    /// 4. fire [`StopReason::BacklogEmpty`] / [`StopReason::NoEligibleTasks`]
    ///    when nothing can ever start again.
    ///
    /// A stopped runner records nothing further. A paused runner stops with
    /// [`StopReason::Paused`] without consuming or starting anything.
    #[must_use]
    pub fn tick(&mut self, outcomes: &BTreeMap<BacklogTaskId, AttemptOutcome>) -> Vec<RunnerEvent> {
        let start = self.journal.len();
        if self.stop_reason.is_some() {
            return Vec::new();
        }
        if self.paused {
            self.set_stop(StopReason::Paused);
            return self.events_since(start);
        }

        // Due (re-)attempts from earlier ticks; retries scheduled below join
        // the pending set for the NEXT tick — the tick boundary is the only
        // time model, so a retry never re-fires within its own tick.
        let due: BTreeSet<BacklogTaskId> = std::mem::take(&mut self.pending);

        for (task, outcome) in outcomes {
            if self.stop_reason.is_some() {
                // Run-level stop: the earliest stop wins; later outcomes are
                // not consumed this tick.
                break;
            }
            if !self.running.remove(task) {
                // Outcome for a task not currently running (unknown,
                // completed, or blocked): ignored, never journaled.
                continue;
            }
            self.consume_outcome(task, outcome);
        }

        if self.stop_reason.is_some() {
            // Restore due tasks so a resumed run can still dispatch them.
            self.pending.extend(due);
            return self.events_since(start);
        }

        // Dispatch: due re-attempts first, then newly eligible tasks, all in
        // stable task-ID order.
        let mut startable = due;
        for id in self.dag.eligible(&self.completed) {
            if !self.blocked.contains_key(&id)
                && !self.running.contains(&id)
                && !self.pending.contains(&id)
            {
                startable.insert(id);
            }
        }
        // §9.33.3: "Parallel execution MUST respect configurable concurrency,
        // repository conflicts, resource budgets (§9.27), provider limits
        // (§9.19.5-§9.19.6), and review capacity." The scheduler's selection
        // caps this tick's starts against the running set; a task the caps
        // skip is neither dropped nor blocked — it stays eligible and
        // re-competes on the next tick. The under-review change-set count
        // that ParallelScheduler::select also bounds is runtime state owned
        // by the change-set review pipeline (§9.33.4), so the runner passes
        // 0 here (see module docs for the enforced/deferred split).
        let running: Vec<BacklogTaskId> = self.running.iter().cloned().collect();
        let selection: BTreeSet<BacklogTaskId> = self
            .scheduler
            .select(self.dag, &self.completed, &running, 0)
            .into_iter()
            .collect();
        for task in &startable {
            if !selection.contains(task) {
                continue;
            }
            if self.budget_remaining == 0 {
                self.push_event(RunnerEvent::BudgetExhausted);
                self.set_stop(StopReason::BudgetExhausted);
                break;
            }
            self.budget_remaining = self.budget_remaining.saturating_sub(1);
            let attempts = self.attempts.entry(task.clone()).or_insert(0);
            *attempts = attempts.saturating_add(1);
            self.running.insert(task.clone());
            self.push_event(RunnerEvent::Started { task: task.clone() });
        }

        if self.stop_reason.is_none() && self.running.is_empty() && self.pending.is_empty() {
            // Nothing in flight, nothing pending, nothing startable: the run
            // can never make progress again.
            if self.completed.len() == self.dag.len() {
                self.set_stop(StopReason::BacklogEmpty);
            } else {
                let eligible_remaining = self
                    .dag
                    .eligible(&self.completed)
                    .into_iter()
                    .filter(|id| !self.blocked.contains_key(id))
                    .count();
                if eligible_remaining == 0 {
                    self.set_stop(StopReason::NoEligibleTasks);
                }
            }
        }
        self.events_since(start)
    }

    /// Loops [`BacklogRunner::tick`] until a [`StopReason`] fires or
    /// `max_ticks` ticks have run, returning every appended event.
    ///
    /// `max_ticks = 0` runs nothing and dispatches no attempts. Reaching
    /// `max_ticks` without a stop reason returns the collected events as-is:
    /// the runner stays alive and resumable — an explicit state, never a
    /// silent approve (§9.33.8). A tick that changes nothing ends the loop
    /// defensively, so `run` never spins forever.
    #[must_use]
    pub fn run(
        &mut self,
        outcomes: &BTreeMap<BacklogTaskId, AttemptOutcome>,
        max_ticks: u32,
    ) -> Vec<RunnerEvent> {
        let mut emitted = Vec::new();
        for _ in 0..max_ticks {
            if self.stop_reason.is_some() {
                break;
            }
            let events = self.tick(outcomes);
            let progressed = !events.is_empty();
            emitted.extend(events);
            if !progressed {
                break;
            }
        }
        emitted
    }

    /// Records `outcome` for `task` and applies the gated decision.
    fn consume_outcome(&mut self, task: &BacklogTaskId, outcome: &AttemptOutcome) {
        let attempt = self.attempts.get(task).copied().unwrap_or(1);
        self.push_event(RunnerEvent::AttemptRecorded {
            task: task.clone(),
            attempt,
            outcome: outcome.clone(),
        });
        match outcome {
            AttemptOutcome::Completed => {
                self.completed.insert(task.clone());
                self.retry_schedules.remove(task);
            }
            AttemptOutcome::Paused => {
                self.paused = true;
                self.push_event(RunnerEvent::Paused);
                self.set_stop(StopReason::Paused);
                // The attempt stays open in `running`: after resume the
                // runtime resumes from checkpoint or re-delivers a fresh
                // outcome (§9.24.2).
                self.running.insert(task.clone());
            }
            AttemptOutcome::ApprovalRequired => {
                self.blocked
                    .insert(task.clone(), BlockReason::ApprovalRequired);
                self.push_event(RunnerEvent::Blocked {
                    task: task.clone(),
                    reason: BlockReason::ApprovalRequired,
                });
                self.set_stop(StopReason::ApprovalRequired);
            }
            AttemptOutcome::PolicyDenied => {
                self.blocked
                    .insert(task.clone(), BlockReason::SecurityBlock);
                self.push_event(RunnerEvent::Blocked {
                    task: task.clone(),
                    reason: BlockReason::SecurityBlock,
                });
                self.set_stop(StopReason::SecurityBlock);
            }
            AttemptOutcome::Failed {
                class,
                retry_after_ms,
                proof,
            } => {
                self.handle_failure(task, *class, *retry_after_ms, proof.as_ref(), attempt);
            }
        }
    }

    /// Applies the §9.33.5 decision chain to one failed attempt:
    /// [`classify`] proposes, [`RetryGate::evaluate`] re-asserts, the runner
    /// executes.
    fn handle_failure(
        &mut self,
        task: &BacklogTaskId,
        class: FailureClass,
        retry_after_ms: Option<u64>,
        proof: Option<&IdempotencyProof>,
        failing_attempt: u32,
    ) {
        // Count the failure at the task's current ladder step.
        if let Some(state) = self.escalation.get_mut(task) {
            state.record_failure();
        }
        let attempts_at_step = self
            .escalation
            .get(task)
            .map_or(u32::MAX, EscalationState::attempts_at_current_step);

        // An absent proof is an unproven side-effecting operation (§9.26.3).
        let proof = proof.cloned().unwrap_or(IdempotencyProof::Unproven);
        let proposed = classify(
            class,
            failing_attempt,
            retry_after_ms,
            self.max_reprompt_attempts,
        );
        let gated = RetryGate::evaluate(class, &proof, &proposed);

        match gated {
            RetryDecision::Retry { .. } => {
                if attempts_at_step < self.max_attempts_per_step {
                    let delay_ms = {
                        let schedule = self.retry_schedules.entry(task.clone()).or_default();
                        let delay_ms = schedule.next_delay();
                        schedule.attempt = schedule.attempt.saturating_add(1);
                        delay_ms
                    };
                    self.push_event(RunnerEvent::RetryScheduled {
                        task: task.clone(),
                        delay_ms,
                    });
                    self.pending.insert(task.clone());
                } else {
                    self.escalate(task);
                }
            }
            RetryDecision::Hold { retry_after_ms } => {
                if attempts_at_step < self.max_attempts_per_step {
                    self.push_event(RunnerEvent::RetryHeld {
                        task: task.clone(),
                        retry_after_ms,
                    });
                    self.pending.insert(task.clone());
                } else {
                    self.escalate(task);
                }
            }
            RetryDecision::FixRootCause => {
                self.push_event(RunnerEvent::FixRootCause { task: task.clone() });
                if attempts_at_step < self.max_attempts_per_step {
                    // Re-enter through a root-cause fix spawn (fresh context,
                    // §9.33.1) — never a blind re-execution (§9.33.5).
                    self.pending.insert(task.clone());
                } else {
                    self.escalate(task);
                }
            }
            RetryDecision::Reprompt { attempt } => {
                self.push_event(RunnerEvent::Reprompt {
                    task: task.clone(),
                    attempt,
                });
                self.pending.insert(task.clone());
            }
            RetryDecision::AwaitUser => {
                self.blocked
                    .insert(task.clone(), BlockReason::ApprovalRequired);
                self.push_event(RunnerEvent::Blocked {
                    task: task.clone(),
                    reason: BlockReason::ApprovalRequired,
                });
                self.set_stop(StopReason::ApprovalRequired);
            }
            RetryDecision::Escalate => match class {
                FailureClass::PolicyDenial | FailureClass::NonRetriableConfigOrSecurity => {
                    self.blocked
                        .insert(task.clone(), BlockReason::SecurityBlock);
                    self.push_event(RunnerEvent::Blocked {
                        task: task.clone(),
                        reason: BlockReason::SecurityBlock,
                    });
                    self.set_stop(StopReason::SecurityBlock);
                }
                _ => self.escalate(task),
            },
        }
    }

    /// Advances the task's escalation ladder one step and schedules the
    /// fresh-context re-attempt, or — at the terminal
    /// [`EscalationStep::HumanEscalation`] — journals the explicit
    /// blocked/needs-human state §9.33.4 mandates and stops the run. Never
    /// marks the task completed: no path here carries approval semantics
    /// (§9.33.5, §9.33.7).
    fn escalate(&mut self, task: &BacklogTaskId) {
        let (advanced_to, exhausted) = match self.escalation.get_mut(task) {
            Some(state) => (state.advance(), state.exhausted()),
            // Not a DAG task: unreachable through the public API (stray
            // outcomes are ignored), and fail-closed here — treat as the
            // terminal ladder state rather than silently continuing.
            None => (None, true),
        };
        if exhausted {
            self.push_event(RunnerEvent::HumanEscalation { task: task.clone() });
            self.blocked
                .insert(task.clone(), BlockReason::HumanEscalation);
            self.push_event(RunnerEvent::Blocked {
                task: task.clone(),
                reason: BlockReason::HumanEscalation,
            });
            self.set_stop(StopReason::FailuresBlocked);
        } else if let Some(step) = advanced_to {
            // A new ladder step is a new agent/model/profile with a fresh
            // context (§9.33.1, §7.3): its bounded backoff restarts too.
            self.retry_schedules.remove(task);
            self.push_event(RunnerEvent::EscalationStep {
                task: task.clone(),
                step,
            });
            self.pending.insert(task.clone());
        }
    }

    /// Appends one event to the durable journal.
    fn push_event(&mut self, event: RunnerEvent) {
        self.journal.push(event);
    }

    /// Fires the run-level stop once; the earliest stop wins.
    fn set_stop(&mut self, reason: StopReason) {
        if self.stop_reason.is_none() {
            self.stop_reason = Some(reason);
            self.push_event(RunnerEvent::Stopped { reason });
        }
    }

    /// Events appended to the journal since `start`.
    fn events_since(&self, start: usize) -> Vec<RunnerEvent> {
        self.journal[start..].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::TaskDag;
    use crate::metadata::{
        Autonomy, CompletionEvidence, DomainId, RiskClass, SpecRef, TaskMetadata, VerificationRef,
    };
    use crate::review_loop::ReviewLoopConfig;
    use orchestraitor_model::DataSensitivity;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn task(id: &str, deps: &[&str]) -> TaskMetadata {
        TaskMetadata {
            id: BacklogTaskId::new(id),
            spec_refs: vec![SpecRef::new("9.33.3")],
            title: format!("task {id}"),
            objective: format!("objective {id}"),
            acceptance_criteria: vec!["green".to_string()],
            dependencies: deps.iter().map(|d| BacklogTaskId::new(*d)).collect(),
            domain: DomainId::new("backend"),
            risk: RiskClass::Low,
            data_sensitivity: DataSensitivity::Internal,
            expected_files: Vec::new(),
            required_verification: vec![VerificationRef("nextest-workspace".to_string())],
            required_reviewer_domains: vec![DomainId::new("backend")],
            autonomy: Autonomy::Guided,
            routing: None,
            retry_policy: "default".to_string(),
            completion_evidence: vec![CompletionEvidence::Verification {
                name: "nextest-workspace".to_string(),
            }],
        }
    }

    fn dag(tasks: &[(&str, &[&str])]) -> Result<TaskDag, crate::dag::DagError> {
        let metadata: Vec<TaskMetadata> = tasks.iter().map(|(id, deps)| task(id, deps)).collect();
        TaskDag::new(metadata)
    }

    fn tid(id: &str) -> BacklogTaskId {
        BacklogTaskId::new(id)
    }

    fn input<'a>(dag: &'a TaskDag, config: &'a ReviewLoopConfig) -> RunnerInput<'a> {
        RunnerInput {
            dag,
            config,
            attempt_budget: 64,
            max_reprompt_attempts: 3,
            max_attempts_per_step: 2,
            escalation_policy: EscalationPolicy::default(),
            scheduler_config: SchedulerConfig::default(),
        }
    }

    fn runner<'a>(
        dag: &'a TaskDag,
        config: &'a ReviewLoopConfig,
    ) -> Result<BacklogRunner<'a>, RunnerError> {
        BacklogRunner::new(&RunnerInput {
            escalation_policy: EscalationPolicy::default(),
            ..input(dag, config)
        })
    }

    fn caps(max_concurrent: usize) -> SchedulerConfig {
        SchedulerConfig {
            max_concurrent,
            max_per_domain: None,
            review_capacity: 8,
        }
    }

    fn capped_runner<'a>(
        dag: &'a TaskDag,
        config: &'a ReviewLoopConfig,
        scheduler_config: SchedulerConfig,
    ) -> Result<BacklogRunner<'a>, RunnerError> {
        BacklogRunner::new(&RunnerInput {
            scheduler_config,
            ..input(dag, config)
        })
    }

    fn no_outcomes() -> BTreeMap<BacklogTaskId, AttemptOutcome> {
        BTreeMap::new()
    }

    fn completed_outcome(id: &str) -> (BacklogTaskId, AttemptOutcome) {
        (tid(id), AttemptOutcome::Completed)
    }

    fn failed(
        class: FailureClass,
        retry_after_ms: Option<u64>,
        proof: Option<IdempotencyProof>,
    ) -> AttemptOutcome {
        AttemptOutcome::Failed {
            class,
            retry_after_ms,
            proof,
        }
    }

    fn started_events(events: &[RunnerEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                RunnerEvent::Started { task } => Some(task.to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn linear_dag_completes_in_dependency_order() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &["a"]), ("c", &["b"])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let events = runner.tick(&no_outcomes());
        assert_eq!(started_events(&events), ["a"]);

        let events = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        assert!(events.contains(&RunnerEvent::AttemptRecorded {
            task: tid("a"),
            attempt: 1,
            outcome: AttemptOutcome::Completed,
        }));
        assert_eq!(started_events(&events), ["b"]);

        let _ = runner.tick(&BTreeMap::from([completed_outcome("b")]));
        let events = runner.tick(&BTreeMap::from([completed_outcome("c")]));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            })
        );
        assert_eq!(runner.stop_reason(), Some(StopReason::BacklogEmpty));
        assert_eq!(runner.completed().len(), 3);
        Ok(())
    }

    #[test]
    fn diamond_dag_runs_dependencies_before_dependents() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &["a"]), ("c", &["a"]), ("d", &["b", "c"])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        assert_eq!(started_events(&runner.tick(&no_outcomes())), ["a"]);
        let events = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        // b and c become eligible together, dispatched in stable ID order.
        assert_eq!(started_events(&events), ["b", "c"]);
        // Completing only b does not unlock d.
        let events = runner.tick(&BTreeMap::from([completed_outcome("b")]));
        assert_eq!(started_events(&events), Vec::<String>::new());
        let events = runner.tick(&BTreeMap::from([completed_outcome("c")]));
        assert_eq!(started_events(&events), ["d"]);
        let events = runner.tick(&BTreeMap::from([completed_outcome("d")]));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            })
        );
        Ok(())
    }

    #[test]
    fn empty_backlog_is_success() -> TestResult {
        let dag = TaskDag::default();
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;
        let events = runner.run(&no_outcomes(), 8);
        // "An empty backlog is success" (§9.33.8).
        assert_eq!(
            events,
            vec![RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            }]
        );
        Ok(())
    }

    #[test]
    fn cycle_yields_no_eligible_tasks() -> TestResult {
        // TaskDag construction accepts a cycle; eligibility can never unlock.
        let dag = dag(&[("a", &["b"]), ("b", &["a"])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;
        let events = runner.run(&no_outcomes(), 8);
        assert_eq!(started_events(&events), Vec::<String>::new());
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::NoEligibleTasks,
            })
        );
        assert!(runner.completed().is_empty());
        Ok(())
    }

    #[test]
    fn transient_failures_retry_with_bounded_backoff_inside_step_budget() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        // Shortened ladder: one fresh-context retry step, then human.
        let mut runner = BacklogRunner::new(&RunnerInput {
            max_attempts_per_step: 3,
            escalation_policy: EscalationPolicy {
                steps: vec![
                    EscalationStep::SameAgentFreshContext,
                    EscalationStep::HumanEscalation,
                ],
            },
            ..input(&dag, &config)
        })?;

        let _ = runner.tick(&no_outcomes());
        let transient = || {
            BTreeMap::from([(
                tid("a"),
                failed(FailureClass::TransientProviderOrNetwork, None, None),
            )])
        };
        // The failure lands while the attempt is running; the retry becomes
        // due on the next tick and re-dispatches as a fresh attempt.
        let first = runner.tick(&transient());
        assert!(first.contains(&RunnerEvent::RetryScheduled {
            task: tid("a"),
            delay_ms: 200,
        }));
        let _ = runner.tick(&no_outcomes());
        let second = runner.tick(&transient());
        assert!(second.contains(&RunnerEvent::RetryScheduled {
            task: tid("a"),
            delay_ms: 400,
        }));
        let _ = runner.tick(&no_outcomes());
        // The third failure exhausts the step budget and advances the ladder,
        // which is immediately terminal under the shortened policy.
        let third = runner.tick(&transient());
        assert!(
            !third
                .iter()
                .any(|e| matches!(e, RunnerEvent::RetryScheduled { .. }))
        );
        assert!(third.contains(&RunnerEvent::HumanEscalation { task: tid("a") }));
        assert_eq!(runner.stop_reason(), Some(StopReason::FailuresBlocked));
        Ok(())
    }

    #[test]
    fn rate_limit_hold_honors_retry_after_hint() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let _ = runner.tick(&no_outcomes());
        let events = runner.tick(&BTreeMap::from([(
            tid("a"),
            failed(FailureClass::RateLimit, Some(1_500), None),
        )]));
        assert!(events.contains(&RunnerEvent::RetryHeld {
            task: tid("a"),
            retry_after_ms: 1_500,
        }));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, RunnerEvent::RetryScheduled { .. }))
        );
        // The held attempt becomes due on the next tick and can complete.
        let _ = runner.tick(&no_outcomes());
        let events = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            })
        );
        Ok(())
    }

    #[test]
    fn rate_limit_without_hint_falls_back_to_bounded_backoff() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let _ = runner.tick(&no_outcomes());
        let events = runner.tick(&BTreeMap::from([(
            tid("a"),
            failed(FailureClass::RateLimit, None, None),
        )]));
        assert!(events.contains(&RunnerEvent::RetryScheduled {
            task: tid("a"),
            delay_ms: 200,
        }));
        Ok(())
    }

    #[test]
    fn invalid_output_reprompts_with_fresh_context_then_escalates() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = BacklogRunner::new(&RunnerInput {
            max_reprompt_attempts: 2,
            max_attempts_per_step: 2,
            escalation_policy: EscalationPolicy {
                steps: vec![
                    EscalationStep::SameAgentFreshContext,
                    EscalationStep::HumanEscalation,
                ],
            },
            ..input(&dag, &config)
        })?;

        let _ = runner.tick(&no_outcomes());
        let invalid = || {
            BTreeMap::from([(
                tid("a"),
                failed(FailureClass::InvalidAgentOutput, None, None),
            )])
        };
        let first = runner.tick(&invalid());
        assert!(first.contains(&RunnerEvent::Reprompt {
            task: tid("a"),
            attempt: 2,
        }));
        let _ = runner.tick(&no_outcomes());
        // Reprompt budget exhausted at attempt 2: escalate, which terminates
        // at human escalation — never a retry, never a silent pass.
        let second = runner.tick(&invalid());
        assert!(second.contains(&RunnerEvent::HumanEscalation { task: tid("a") }));
        assert_eq!(
            runner.blocked().get(&tid("a")),
            Some(&BlockReason::HumanEscalation)
        );
        assert_eq!(runner.stop_reason(), Some(StopReason::FailuresBlocked));
        assert!(runner.completed().is_empty());
        Ok(())
    }

    #[test]
    fn verification_failure_fixes_root_cause_never_blind_retries() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let _ = runner.tick(&no_outcomes());
        let events = runner.tick(&BTreeMap::from([(
            tid("a"),
            failed(FailureClass::Verification, None, None),
        )]));
        assert!(events.contains(&RunnerEvent::FixRootCause { task: tid("a") }));
        assert!(!events.iter().any(|e| matches!(
            e,
            RunnerEvent::RetryScheduled { .. } | RunnerEvent::RetryHeld { .. }
        )));
        // The root-cause fix re-enters as a new attempt (fresh context) and
        // can then complete.
        let events = runner.tick(&no_outcomes());
        assert_eq!(started_events(&events), ["a"]);
        let events = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            })
        );
        Ok(())
    }

    #[test]
    fn merge_conflict_demands_resolution_not_retry() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let _ = runner.tick(&no_outcomes());
        let events = runner.tick(&BTreeMap::from([(
            tid("a"),
            failed(FailureClass::MergeConflict, None, None),
        )]));
        assert!(events.contains(&RunnerEvent::FixRootCause { task: tid("a") }));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, RunnerEvent::RetryScheduled { .. }))
        );
        Ok(())
    }

    #[test]
    fn unproven_tool_failure_is_never_retried() -> TestResult {
        // §9.33.5 "NEVER blindly retry side-effecting actions": an absent
        // proof and an explicit Unproven proof both force FixRootCause via
        // RetryGate, overriding the class's retry proposal.
        for proof in [None, Some(IdempotencyProof::Unproven)] {
            let dag = dag(&[("a", &[])])?;
            let config = ReviewLoopConfig::default();
            let mut runner = runner(&dag, &config)?;
            let _ = runner.tick(&no_outcomes());
            let events = runner.tick(&BTreeMap::from([(
                tid("a"),
                failed(FailureClass::ToolOrProcess, None, proof.clone()),
            )]));
            assert!(
                events.contains(&RunnerEvent::FixRootCause { task: tid("a") }),
                "proof {proof:?} must force FixRootCause"
            );
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, RunnerEvent::RetryScheduled { .. })),
                "proof {proof:?} must never retry"
            );
        }
        Ok(())
    }

    #[test]
    fn proven_idempotent_tool_failure_retries() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let _ = runner.tick(&no_outcomes());
        let events = runner.tick(&BTreeMap::from([(
            tid("a"),
            failed(
                FailureClass::ToolOrProcess,
                None,
                Some(IdempotencyProof::CheckpointResume),
            ),
        )]));
        assert!(events.contains(&RunnerEvent::RetryScheduled {
            task: tid("a"),
            delay_ms: 200,
        }));
        Ok(())
    }

    #[test]
    fn policy_denied_outcome_stops_with_security_block() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let _ = runner.tick(&no_outcomes());
        let events = runner.tick(&BTreeMap::from([
            completed_outcome("b"),
            (tid("a"), AttemptOutcome::PolicyDenied),
        ]));
        assert!(events.contains(&RunnerEvent::Blocked {
            task: tid("a"),
            reason: BlockReason::SecurityBlock,
        }));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::SecurityBlock,
            })
        );
        assert_eq!(runner.stop_reason(), Some(StopReason::SecurityBlock));
        assert_eq!(
            runner.blocked().get(&tid("a")),
            Some(&BlockReason::SecurityBlock)
        );
        Ok(())
    }

    #[test]
    fn policy_denial_failure_is_never_retried_and_stops_the_run() -> TestResult {
        // QA failure-injection anchor (bootstrap plan todo-31, scenario 3):
        // a patch that retries a policy denial as though it were transient
        // MUST fail this test.
        for class in [
            FailureClass::PolicyDenial,
            FailureClass::NonRetriableConfigOrSecurity,
        ] {
            let dag = dag(&[("a", &[])])?;
            let config = ReviewLoopConfig::default();
            let mut runner = runner(&dag, &config)?;
            let _ = runner.tick(&no_outcomes());
            let events = runner.tick(&BTreeMap::from([(tid("a"), failed(class, None, None))]));
            let journal = runner.journal();
            assert!(
                !journal.iter().any(|e| matches!(
                    e,
                    RunnerEvent::RetryScheduled { .. }
                        | RunnerEvent::RetryHeld { .. }
                        | RunnerEvent::Reprompt { .. }
                )),
                "{class:?} was laundered into a retry: {journal:?}"
            );
            assert!(events.contains(&RunnerEvent::Blocked {
                task: tid("a"),
                reason: BlockReason::SecurityBlock,
            }));
            assert_eq!(runner.stop_reason(), Some(StopReason::SecurityBlock));
            assert!(runner.completed().is_empty());
        }
        Ok(())
    }

    #[test]
    fn approval_required_stops_the_run_without_retrying() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let _ = runner.tick(&no_outcomes());
        let events = runner.tick(&BTreeMap::from([(
            tid("a"),
            AttemptOutcome::ApprovalRequired,
        )]));
        assert!(events.contains(&RunnerEvent::Blocked {
            task: tid("a"),
            reason: BlockReason::ApprovalRequired,
        }));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::ApprovalRequired,
            })
        );
        assert_eq!(
            runner.blocked().get(&tid("a")),
            Some(&BlockReason::ApprovalRequired)
        );
        assert!(
            !runner
                .journal()
                .iter()
                .any(|e| matches!(e, RunnerEvent::RetryScheduled { .. }))
        );
        Ok(())
    }

    #[test]
    fn repeated_failures_walk_the_ladder_to_human_escalation() -> TestResult {
        // QA failure-injection anchor (bootstrap plan todo-31, scenario 1):
        // a patch that auto-approves after repeated failures — marks the task
        // completed instead of producing the explicit blocked state — MUST
        // fail this test (§9.33.4 "never silently approve").
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = BacklogRunner::new(&RunnerInput {
            max_attempts_per_step: 1,
            ..input(&dag, &config)
        })?;

        let _ = runner.tick(&no_outcomes());
        let fail = || {
            BTreeMap::from([(
                tid("a"),
                failed(FailureClass::TransientProviderOrNetwork, None, None),
            )])
        };
        let mut events = Vec::new();
        // Each failure exhausts its step's single-attempt budget and advances
        // the ladder; the escalated attempt re-dispatches on the next tick.
        for _ in 0..4 {
            events = runner.tick(&fail());
            if runner.stop_reason().is_some() {
                break;
            }
            let _ = runner.tick(&no_outcomes());
        }

        let journal = runner.journal();
        for step in [
            EscalationStep::AlternateModel,
            EscalationStep::DomainExpert,
            EscalationStep::RevisedPlan,
        ] {
            assert!(
                journal.contains(&RunnerEvent::EscalationStep {
                    task: tid("a"),
                    step
                }),
                "ladder walk must visit {step:?}"
            );
        }
        assert!(events.contains(&RunnerEvent::HumanEscalation { task: tid("a") }));
        assert!(events.contains(&RunnerEvent::Blocked {
            task: tid("a"),
            reason: BlockReason::HumanEscalation,
        }));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::FailuresBlocked,
            })
        );
        // The §9.33.4 invariant: escalation can never complete the task, and
        // no completion outcome was ever recorded for it.
        assert!(runner.completed().is_empty());
        assert!(!journal.iter().any(|e| matches!(
            e,
            RunnerEvent::AttemptRecorded {
                outcome: AttemptOutcome::Completed,
                ..
            }
        )));
        // A stopped runner makes no further progress.
        assert!(
            runner
                .tick(&BTreeMap::from([completed_outcome("a")]))
                .is_empty()
        );
        assert!(runner.completed().is_empty());
        Ok(())
    }

    #[test]
    fn budget_exhaustion_stops_mid_dag() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &["a"])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = BacklogRunner::new(&RunnerInput {
            attempt_budget: 1,
            ..input(&dag, &config)
        })?;

        let _ = runner.tick(&no_outcomes());
        let events = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        assert!(events.contains(&RunnerEvent::BudgetExhausted));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BudgetExhausted,
            })
        );
        assert_eq!(runner.stop_reason(), Some(StopReason::BudgetExhausted));
        assert_eq!(runner.budget_remaining(), 0);
        // Only a completed; b never started and never will on its own.
        assert_eq!(runner.completed(), &BTreeSet::from([tid("a")]));
        assert!(
            !runner
                .journal()
                .iter()
                .any(|e| matches!(e, RunnerEvent::Started { task } if task == &tid("b")))
        );
        Ok(())
    }

    #[test]
    fn pause_stop_and_resume_controls_journal_pause_lifecycle() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        runner.pause();
        assert!(runner.is_paused());
        let events = runner.tick(&no_outcomes());
        // No attempts while paused; the run stops as Paused.
        assert_eq!(started_events(&events), Vec::<String>::new());
        assert_eq!(
            events,
            vec![RunnerEvent::Stopped {
                reason: StopReason::Paused,
            }]
        );

        runner.resume();
        assert!(!runner.is_paused());
        assert_eq!(runner.stop_reason(), None);
        // Journal order: Paused, Stopped{Paused}, Resumed.
        assert!(matches!(
            runner.journal(),
            [
                RunnerEvent::Paused,
                RunnerEvent::Stopped {
                    reason: StopReason::Paused,
                },
                RunnerEvent::Resumed,
            ]
        ));

        let events = runner.tick(&no_outcomes());
        assert_eq!(started_events(&events), ["a"]);
        let events = runner.run(&BTreeMap::from([completed_outcome("a")]), 8);
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            })
        );
        Ok(())
    }

    #[test]
    fn task_paused_outcome_pauses_the_run_and_resume_continues() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let _ = runner.tick(&no_outcomes());
        let events = runner.tick(&BTreeMap::from([(tid("a"), AttemptOutcome::Paused)]));
        assert!(events.contains(&RunnerEvent::Paused));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::Paused,
            })
        );
        assert!(runner.is_paused());

        runner.resume();
        // The paused attempt stayed open: the runtime re-delivers a fresh
        // outcome (§9.24.2 checkpoint resume), and the sibling completes.
        let events = runner.tick(&BTreeMap::from([completed_outcome("b")]));
        // b completing does not finish the backlog (a still open), so no stop.
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, RunnerEvent::Stopped { .. }))
        );
        let events = runner.run(&BTreeMap::from([completed_outcome("a")]), 8);
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            })
        );
        Ok(())
    }

    #[test]
    fn max_ticks_zero_dispatches_nothing_and_runner_stays_resumable() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let outcomes = BTreeMap::from([completed_outcome("a")]);
        assert!(runner.run(&outcomes, 0).is_empty());
        assert!(runner.journal().is_empty());
        assert_eq!(runner.stop_reason(), None);

        // One tick dispatches the attempt; the next consumes the outcome.
        assert_eq!(started_events(&runner.run(&outcomes, 1)), ["a"]);
        let events = runner.run(&outcomes, 1);
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            })
        );
        Ok(())
    }

    #[test]
    fn stray_outcomes_for_unknown_or_unrunning_tasks_are_ignored() -> TestResult {
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        // Unknown task: nothing consumed, journal holds only the start.
        let events = runner.tick(&BTreeMap::from([completed_outcome("ghost")]));
        assert_eq!(started_events(&events), ["a"]);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, RunnerEvent::AttemptRecorded { .. }))
        );

        // Completed task can never receive another attempt outcome.
        let _ = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        let journal_len = runner.journal().len();
        let events = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, RunnerEvent::AttemptRecorded { .. }))
        );
        assert_eq!(runner.journal().len(), journal_len + events.len());
        Ok(())
    }

    #[test]
    fn journal_is_append_only_and_matches_tick_returns() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &["a"])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = runner(&dag, &config)?;

        let mut emitted = Vec::new();
        let len_before = runner.journal().len();
        emitted.extend(runner.tick(&no_outcomes()));
        // Every tick return is exactly the journal growth.
        assert_eq!(runner.journal()[len_before..], emitted[..]);

        let before_second = runner.journal().len();
        let second = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        assert_eq!(runner.journal()[before_second..], second[..]);
        emitted.extend(second);

        emitted.extend(runner.tick(&no_outcomes()));
        emitted.extend(runner.tick(&BTreeMap::from([completed_outcome("b")])));
        // The concatenation of all returns IS the full journal, in order.
        assert_eq!(runner.journal(), &emitted[..]);
        Ok(())
    }

    #[test]
    fn same_inputs_produce_identical_journals() -> TestResult {
        let script = |runner: &mut BacklogRunner<'_>| {
            let _ = runner.tick(&no_outcomes());
            let _ = runner.tick(&BTreeMap::from([(
                tid("a"),
                failed(FailureClass::TransientProviderOrNetwork, None, None),
            )]));
            let _ = runner.tick(&no_outcomes());
            let _ = runner.tick(&BTreeMap::from([(
                tid("a"),
                failed(FailureClass::RateLimit, Some(750), None),
            )]));
            let _ = runner.tick(&no_outcomes());
            let _ = runner.run(&BTreeMap::from([completed_outcome("a")]), 8);
        };
        let dag = dag(&[("a", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut first = runner(&dag, &config)?;
        let mut second = runner(&dag, &config)?;
        script(&mut first);
        script(&mut second);
        assert_eq!(first.journal(), second.journal());
        assert_eq!(first.stop_reason(), second.stop_reason());
        Ok(())
    }

    #[test]
    fn construction_rejects_invalid_config_and_policy() {
        let dag = TaskDag::default();
        let config = ReviewLoopConfig::default();
        let bad_policy = RunnerInput {
            escalation_policy: EscalationPolicy { steps: vec![] },
            ..input(&dag, &config)
        };
        assert!(matches!(
            BacklogRunner::new(&bad_policy),
            Err(RunnerError::InvalidEscalationPolicy(_))
        ));

        let invalid_config = ReviewLoopConfig {
            max_review_loops: 0,
            ..ReviewLoopConfig::default()
        };
        assert!(matches!(
            BacklogRunner::new(&RunnerInput {
                config: &invalid_config,
                escalation_policy: EscalationPolicy::default(),
                ..input(&dag, &config)
            }),
            Err(RunnerError::InvalidReviewLoopConfig(_))
        ));
    }

    #[test]
    fn attempt_outcome_serde_round_trip_and_names() -> TestResult {
        let outcomes = [
            (AttemptOutcome::Completed, "completed"),
            (AttemptOutcome::ApprovalRequired, "approval_required"),
            (AttemptOutcome::PolicyDenied, "policy_denied"),
            (AttemptOutcome::Paused, "paused"),
        ];
        for (outcome, name) in outcomes {
            let json = serde_json::to_string(&outcome)?;
            assert_eq!(json, format!("\"{name}\""));
            let back: AttemptOutcome = serde_json::from_str(&json)?;
            assert_eq!(back, outcome);
        }
        let failed_outcome = failed(
            FailureClass::RateLimit,
            Some(1_500),
            Some(IdempotencyProof::CheckpointResume),
        );
        let json = serde_json::to_string(&failed_outcome)?;
        assert!(json.contains("\"failed\""));
        assert!(json.contains("\"rate_limit\""));
        let back: AttemptOutcome = serde_json::from_str(&json)?;
        assert_eq!(back, failed_outcome);
        Ok(())
    }

    #[test]
    fn stop_reason_and_block_reason_serde_round_trip() -> TestResult {
        let stops = [
            (StopReason::BacklogEmpty, "backlog_empty"),
            (StopReason::NoEligibleTasks, "no_eligible_tasks"),
            (StopReason::BudgetExhausted, "budget_exhausted"),
            (StopReason::ApprovalRequired, "approval_required"),
            (StopReason::FailuresBlocked, "failures_blocked"),
            (StopReason::SecurityBlock, "security_block"),
            (StopReason::Paused, "paused"),
        ];
        for (reason, name) in stops {
            let json = serde_json::to_string(&reason)?;
            assert_eq!(json, format!("\"{name}\""));
            let back: StopReason = serde_json::from_str(&json)?;
            assert_eq!(back, reason);
        }
        let blocks = [
            (BlockReason::HumanEscalation, "human_escalation"),
            (BlockReason::SecurityBlock, "security_block"),
            (BlockReason::ApprovalRequired, "approval_required"),
        ];
        for (reason, name) in blocks {
            let json = serde_json::to_string(&reason)?;
            assert_eq!(json, format!("\"{name}\""));
            let back: BlockReason = serde_json::from_str(&json)?;
            assert_eq!(back, reason);
        }
        Ok(())
    }

    #[test]
    fn runner_event_serde_round_trip() -> TestResult {
        let events = [
            RunnerEvent::Started { task: tid("a") },
            RunnerEvent::AttemptRecorded {
                task: tid("a"),
                attempt: 2,
                outcome: failed(FailureClass::ToolOrProcess, None, None),
            },
            RunnerEvent::RetryScheduled {
                task: tid("a"),
                delay_ms: 400,
            },
            RunnerEvent::RetryHeld {
                task: tid("a"),
                retry_after_ms: 1_500,
            },
            RunnerEvent::FixRootCause { task: tid("a") },
            RunnerEvent::Reprompt {
                task: tid("a"),
                attempt: 2,
            },
            RunnerEvent::EscalationStep {
                task: tid("a"),
                step: EscalationStep::DomainExpert,
            },
            RunnerEvent::HumanEscalation { task: tid("a") },
            RunnerEvent::Blocked {
                task: tid("a"),
                reason: BlockReason::HumanEscalation,
            },
            RunnerEvent::Paused,
            RunnerEvent::Resumed,
            RunnerEvent::BudgetExhausted,
            RunnerEvent::Stopped {
                reason: StopReason::FailuresBlocked,
            },
        ];
        for event in events {
            let json = serde_json::to_string(&event)?;
            let back: RunnerEvent = serde_json::from_str(&json)?;
            assert_eq!(back, event);
        }
        // Snake_case variant tags anchor the §9.33.6 journal format.
        assert_eq!(
            serde_json::to_string(&RunnerEvent::BudgetExhausted)?,
            "\"budget_exhausted\""
        );
        assert_eq!(serde_json::to_string(&RunnerEvent::Paused)?, "\"paused\"");
        Ok(())
    }

    #[test]
    fn concurrency_cap_respected_on_wide_dag() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &[]), ("c", &[]), ("d", &[]), ("e", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = capped_runner(&dag, &config, caps(2))?;

        // Tick 1: exactly cap-sized dispatch; c/d/e stay eligible.
        assert_eq!(started_events(&runner.tick(&no_outcomes())), ["a", "b"]);
        // Tick 2: the running set is full — no starts, and the full cap is
        // NOT a stop reason.
        assert!(runner.tick(&no_outcomes()).is_empty());
        assert_eq!(runner.stop_reason(), None);
        // Draining the running set releases the next cap-sized batch.
        let events = runner.tick(&BTreeMap::from([
            completed_outcome("a"),
            completed_outcome("b"),
        ]));
        assert_eq!(started_events(&events), ["c", "d"]);
        let events = runner.tick(&BTreeMap::from([
            completed_outcome("c"),
            completed_outcome("d"),
        ]));
        assert_eq!(started_events(&events), ["e"]);
        let events = runner.tick(&BTreeMap::from([completed_outcome("e")]));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            })
        );
        // Dispatch order across the whole run is the stable task-ID order.
        assert_eq!(started_events(runner.journal()), ["a", "b", "c", "d", "e"]);
        Ok(())
    }

    #[test]
    fn cap_competition_between_pending_retry_and_newly_eligible_is_deterministic() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = capped_runner(&dag, &config, caps(1))?;

        assert_eq!(started_events(&runner.tick(&no_outcomes())), ["a"]);
        // The transient failure schedules a retry due on the NEXT tick; the
        // slot the scheduler may reserve for it idles this tick (the
        // tick-boundary rule outranks slot utilization), so b waits.
        let events = runner.tick(&BTreeMap::from([(
            tid("a"),
            failed(FailureClass::TransientProviderOrNetwork, None, None),
        )]));
        assert!(events.contains(&RunnerEvent::RetryScheduled {
            task: tid("a"),
            delay_ms: 200,
        }));
        assert!(started_events(&events).is_empty());
        // Pending retry a competes with newly eligible b for the single
        // slot; stable task-ID order decides, and b is never dropped or
        // blocked while it waits.
        assert_eq!(started_events(&runner.tick(&no_outcomes())), ["a"]);
        assert!(runner.blocked().is_empty());
        let events = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        assert_eq!(started_events(&events), ["b"]);
        let events = runner.tick(&BTreeMap::from([completed_outcome("b")]));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            })
        );
        assert_eq!(started_events(runner.journal()), ["a", "a", "b"]);
        assert_eq!(runner.completed(), &BTreeSet::from([tid("a"), tid("b")]));
        Ok(())
    }

    #[test]
    fn running_set_drain_releases_dispatch_slots() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &[]), ("c", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = capped_runner(&dag, &config, caps(2))?;

        assert_eq!(started_events(&runner.tick(&no_outcomes())), ["a", "b"]);
        // A full running set yields an empty-but-alive tick.
        assert!(runner.tick(&no_outcomes()).is_empty());
        assert_eq!(runner.stop_reason(), None);
        // Consuming one outcome frees exactly one slot.
        let events = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        assert_eq!(started_events(&events), ["c"]);
        // c joined the running set: b completing frees a slot with nothing
        // left to start — no starts, no stop.
        let events = runner.tick(&BTreeMap::from([completed_outcome("b")]));
        assert!(started_events(&events).is_empty());
        assert_eq!(runner.stop_reason(), None);
        let events = runner.tick(&BTreeMap::from([completed_outcome("c")]));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BacklogEmpty,
            })
        );
        Ok(())
    }

    #[test]
    fn construction_rejects_invalid_scheduler_config() {
        let dag = TaskDag::default();
        let config = ReviewLoopConfig::default();

        for (scheduler_config, error) in [
            (
                SchedulerConfig {
                    max_concurrent: 0,
                    max_per_domain: None,
                    review_capacity: 8,
                },
                SchedulerConfigError::ZeroMaxConcurrent,
            ),
            (
                SchedulerConfig {
                    max_concurrent: 2,
                    max_per_domain: Some(0),
                    review_capacity: 8,
                },
                SchedulerConfigError::ZeroMaxPerDomain,
            ),
            (
                SchedulerConfig {
                    max_concurrent: 2,
                    max_per_domain: None,
                    review_capacity: 0,
                },
                SchedulerConfigError::ZeroReviewCapacity,
            ),
        ] {
            let result = BacklogRunner::new(&RunnerInput {
                scheduler_config,
                ..input(&dag, &config)
            });
            assert_eq!(
                result.map(|_| ()),
                Err(RunnerError::InvalidSchedulerConfig(error))
            );
        }
    }

    #[test]
    fn in_flight_tasks_are_never_dropped_or_blocked_by_the_cap() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = capped_runner(&dag, &config, caps(1))?;

        assert_eq!(started_events(&runner.tick(&no_outcomes())), ["a"]);
        // With the running set full, b waits on the cap: repeated ticks
        // neither drop a from the running set nor turn the wait into a
        // block, a stop, or a false event stream.
        for _ in 0..3 {
            assert!(runner.tick(&no_outcomes()).is_empty());
            assert_eq!(runner.stop_reason(), None);
        }
        assert!(runner.blocked().is_empty());
        assert!(runner.completed().is_empty());
        // a is still in flight: its outcome is consumed and recorded, then b
        // immediately takes the released slot.
        let events = runner.tick(&BTreeMap::from([completed_outcome("a")]));
        assert!(events.contains(&RunnerEvent::AttemptRecorded {
            task: tid("a"),
            attempt: 1,
            outcome: AttemptOutcome::Completed,
        }));
        assert_eq!(started_events(&events), ["b"]);
        let _ = runner.tick(&BTreeMap::from([completed_outcome("b")]));
        assert_eq!(runner.completed(), &BTreeSet::from([tid("a"), tid("b")]));
        assert!(runner.blocked().is_empty());
        Ok(())
    }

    #[test]
    fn budget_and_cap_both_bound_dispatch_and_the_smaller_wins() -> TestResult {
        let dag = dag(&[("a", &[]), ("b", &[]), ("c", &[]), ("d", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut runner = BacklogRunner::new(&RunnerInput {
            attempt_budget: 3,
            scheduler_config: caps(2),
            ..input(&dag, &config)
        })?;

        // Cap smaller than budget: the cap binds, spending two of three
        // units. Cap exhaustion produces no stop reason.
        assert_eq!(started_events(&runner.tick(&no_outcomes())), ["a", "b"]);
        assert_eq!(runner.budget_remaining(), 1);
        assert_eq!(runner.stop_reason(), None);
        // Budget smaller than the released slots: the budget binds and the
        // BudgetExhausted stop is unchanged by the cap wiring.
        let events = runner.tick(&BTreeMap::from([
            completed_outcome("a"),
            completed_outcome("b"),
        ]));
        assert_eq!(started_events(&events), ["c"]);
        assert!(events.contains(&RunnerEvent::BudgetExhausted));
        assert_eq!(
            events.last(),
            Some(&RunnerEvent::Stopped {
                reason: StopReason::BudgetExhausted,
            })
        );
        // d never started and never will on its own.
        assert!(
            !runner
                .journal()
                .iter()
                .any(|e| matches!(e, RunnerEvent::Started { task } if task == &tid("d")))
        );
        Ok(())
    }

    #[test]
    fn capped_run_produces_identical_journals_for_identical_inputs() -> TestResult {
        let script = |runner: &mut BacklogRunner<'_>| {
            let _ = runner.tick(&no_outcomes());
            let _ = runner.tick(&BTreeMap::from([(
                tid("a"),
                failed(FailureClass::TransientProviderOrNetwork, None, None),
            )]));
            let _ = runner.tick(&no_outcomes());
            let _ = runner.tick(&BTreeMap::from([
                completed_outcome("a"),
                completed_outcome("b"),
            ]));
            let _ = runner.tick(&BTreeMap::from([completed_outcome("c")]));
        };
        let dag = dag(&[("a", &[]), ("b", &[]), ("c", &[])])?;
        let config = ReviewLoopConfig::default();
        let mut first = capped_runner(&dag, &config, caps(2))?;
        let mut second = capped_runner(&dag, &config, caps(2))?;
        script(&mut first);
        script(&mut second);
        assert_eq!(first.stop_reason(), Some(StopReason::BacklogEmpty));
        assert_eq!(first.journal(), second.journal());
        assert_eq!(first.stop_reason(), second.stop_reason());
        Ok(())
    }
}
