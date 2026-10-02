//! The cron-shaped foreground loop runner (spec `10-orchestrator.md` §9.36
//! thin slice, issue #314): poll the board → run one campaign pass →
//! supervise in-flight workers → repeat, under the issue-#310 minimal guard
//! set.
//!
//! Guard ownership follows the worker-crate contract: the attempt and
//! re-plan budgets, the worker timeout, and the worker-internal stall check
//! are enforced *inside* `run_worker`; this runner enforces the
//! scheduler-facing bounds — the concurrency cap, the supervisor-side stall
//! kill (beat staleness over `WorkerBudgets::stall_timeout`), the
//! pass-pacing backoff, the daily spend soft cap, and the whole-run budget —
//! all from the same `WorkerBudgets` instance, so the two layers can never
//! drift apart. A weakened guard configuration is rejected fail-closed: a
//! zeroed or negative guard is a runaway loop.
//!
//! Supervision is tick-poll-based: every tick (at most 1 second, and at
//! least four ticks per stall window) the runner observes natural
//! completions, progress beats, guard violations, and the shutdown signal.
//! No wait in the loop is blind — every sleep is tick-sized, so SIGTERM is
//! honored well inside the five-second daemon budget even during backoff,
//! and a stalled worker is killed within one stall window.
//!
//! Termination arbitration is single-pointed: when the supervisor decides to
//! kill a run it records the intent *before* calling `abort()`, and the
//! final row status derives from what the handle then returns — a natural
//! `Ok(run)` always wins over a just-declared kill (abort-after-completion
//! is a no-op), a cancellation resolves to the recorded intent, and a panic
//! is a typed `failed` row, never a stall.
//!
//! Retry semantics: one worker run per task per loop invocation. A stalled,
//! failed, or killed task is never silently re-selected within the
//! invocation; any retry is a fresh board-driven decision in a later
//! invocation (the typed rows are the audit trail).
//!
//! This crate implements no security primitive: killing, capping, and
//! pacing are orchestration limits (spec §9.27.3); the sandbox boundary
//! remains Arbitraitor's.

use std::collections::HashSet;
use std::time::Duration;

use async_trait::async_trait;
use orchestraitor_agent_catalog::RoleRoutingDecision;
use orchestraitor_worker::{RunStatus, WorkerBudgets, WorkerError, WorkerRun};

use crate::decision::NoOpReason;
use crate::error::CampaignError;
use crate::run_state::{LoopRunStore, RunRowStatus, StartRun};
use crate::session::{BoardSnapshot, plan_pass, task_id_for};

/// The supervisor-visible handle of one in-flight worker run.
pub struct WorkerProcess {
    /// Progress-beat channel (opaque sequence numbers; staleness is the
    /// supervisor's stall signal — see `WorkerConfig::progress`).
    pub beats: tokio::sync::watch::Receiver<u64>,
    /// The worker run itself. Killing is `abort()`; the runner owns the
    /// handle and arbitrates the final status from its result. The inner
    /// `Result` carries the worker's fail-closed "no run could be produced
    /// at all" case (unusable worktree, transport construction failure) —
    /// it never carries classifiable task failures, which arrive as typed
    /// failures inside the produced run.
    pub run: tokio::task::JoinHandle<Result<WorkerRun, WorkerError>>,
}

/// Reads the reconciled board state for one pass. Production wraps the
/// #308 board provider; tests inject fixtures — the trait is the
/// hermeticity seam for the poll side.
#[async_trait]
pub trait BoardPoller: Send + Sync {
    /// Reads one board snapshot.
    ///
    /// # Errors
    ///
    /// Returns a log-safe error; poll failures are transient (the loop backs
    /// off and re-polls), never fatal.
    async fn poll(&self) -> Result<BoardSnapshot, CampaignError>;
}

/// Starts one worker run in the background. Production wires the bootstrap
/// transport on the daemon-less direct path (async — the runner already
/// executes inside a runtime); tests inject fakes. The prior daily spend is
/// fed through so the worker's soft-cap check sees the loop's accrual.
#[async_trait]
pub trait LoopWorkerStarter: Send + Sync {
    /// Starts one leaf task end-to-end in the background.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Spawn`] when no run can be started at all.
    async fn start(
        &self,
        task_id: &str,
        routing: &RoleRoutingDecision,
        prior_daily_spend_usd: f64,
    ) -> Result<WorkerProcess, CampaignError>;
}

/// Loop-only configuration: the shared guard set plus the knobs only a
/// foreground runner has. Composes — never duplicates — [`WorkerBudgets`],
/// so the loop's stall timeout, concurrency cap, backoff schedule, spend
/// cap, and run budget are the worker's own pinned values.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// The issue-#310 guard set shared with the worker.
    pub budgets: WorkerBudgets,
    /// Total budget for a graceful stop (SIGTERM/SIGINT or a budget stop
    /// that must reap in-flight work): stop the intake, give running runs
    /// the remaining window to finish, then abort stragglers and record
    /// them. The tech-stack daemon budget is 5s.
    pub shutdown_budget: Duration,
    /// Optional cycle bound (board polls) — a QA/evidence affordance; `None`
    /// runs until a terminal stop or shutdown.
    pub max_cycles: Option<u64>,
}

impl LoopConfig {
    /// Validates and returns the configuration. Fail-closed on any weakened
    /// guard (a zero stall timeout or concurrency cap, a zero shutdown
    /// budget, a negative spend cap, or a zero run deadline is a runaway or
    /// unkillable loop).
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Loop`] naming the rejected guard.
    pub fn new(
        budgets: WorkerBudgets,
        shutdown_budget: Duration,
        max_cycles: Option<u64>,
    ) -> Result<Self, CampaignError> {
        let config = Self {
            budgets,
            shutdown_budget,
            max_cycles,
        };
        config.validate()?;
        Ok(config)
    }

    /// The issue-#310 bootstrap set with the five-second daemon shutdown
    /// budget. Defaults are trusted constants; [`Self::validate`] still runs
    /// at runner construction.
    #[must_use]
    pub fn bootstrap_defaults() -> Self {
        Self {
            budgets: WorkerBudgets::bootstrap_defaults(),
            shutdown_budget: Duration::from_secs(5),
            max_cycles: None,
        }
    }

    /// Fail-closed guard validation.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Loop`] naming the first rejected guard.
    pub fn validate(&self) -> Result<(), CampaignError> {
        if self.budgets.max_concurrent_workers == 0 {
            return Err(CampaignError::Loop(
                "guard-weakening rejected: concurrency cap must be at least 1".to_string(),
            ));
        }
        if self.budgets.stall_timeout.is_zero() {
            return Err(CampaignError::Loop(
                "guard-weakening rejected: stall timeout must be positive".to_string(),
            ));
        }
        if self.shutdown_budget.is_zero() {
            return Err(CampaignError::Loop(
                "guard-weakening rejected: shutdown budget must be positive".to_string(),
            ));
        }
        if self.budgets.daily_spend_soft_cap_usd < 0.0 {
            return Err(CampaignError::Loop(
                "guard-weakening rejected: daily spend soft cap must be non-negative".to_string(),
            ));
        }
        if self.budgets.run_deadline().is_zero() {
            return Err(CampaignError::Loop(
                "guard-weakening rejected: worker run deadline must be positive".to_string(),
            ));
        }
        Ok(())
    }
}

/// Why the loop stopped. Terminal budget stops and clean shutdowns are
/// distinct: budget stops are recorded on the summary and the run-state
/// rows; shutdown is the operator's signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StopReason {
    /// The whole-run budget elapsed; in-flight runs were aborted and
    /// recorded.
    RunBudgetExhausted,
    /// The daily spend soft cap was reached; in-flight runs drained
    /// naturally and no new work was started.
    SpendSoftCap,
    /// The cycle bound from `LoopConfig::max_cycles` was reached; in-flight
    /// runs were reaped within the shutdown budget.
    CycleBudget,
    /// SIGTERM/SIGINT: clean stop within the shutdown budget.
    Shutdown,
}

/// One observed loop event (the journal the QA evidence renders). Events
/// carry identifiers only — never board content or task payloads.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoopEvent {
    /// A pass was planned: the decision record id, whether a task was
    /// selected, the typed no-op reason on no-op passes, and the selected
    /// worker task id when one was chosen.
    PassPlanned {
        /// Decision record id (exactly one per pass).
        decision_id: i64,
        /// Whether a task was selected.
        selected: bool,
        /// The typed no-op reason on no-op passes.
        no_op_reason: Option<NoOpReason>,
        /// The selected worker task id, when selected.
        task_id: Option<String>,
    },
    /// A worker was spawned and is now supervised.
    WorkerSpawned {
        /// Run-state row id.
        run_id: i64,
        /// Worker task id.
        task_id: String,
    },
    /// A run reached its natural end.
    WorkerFinished {
        /// Run-state row id.
        run_id: i64,
        /// Worker task id.
        task_id: String,
        /// Whether the run completed (vs a typed failure or panic).
        completed: bool,
    },
    /// The supervisor killed a run whose beats went stale.
    WorkerStallKilled {
        /// Run-state row id.
        run_id: i64,
        /// Worker task id.
        task_id: String,
    },
    /// The supervisor killed a run that overstayed the worker timeout while
    /// still beating.
    WorkerTimedOut {
        /// Run-state row id.
        run_id: i64,
        /// Worker task id.
        task_id: String,
    },
    /// The supervisor killed a run during a graceful stop.
    WorkerAbortedOnStop {
        /// Run-state row id.
        run_id: i64,
        /// Worker task id.
        task_id: String,
        /// Whether the stop was operator shutdown (vs a budget stop).
        shutdown: bool,
    },
    /// A board poll failed; the loop backs off and retries (transient).
    PollFailed {
        /// Store-assigned, log-safe poll error summary.
        message: String,
    },
    /// A pass produced no spawn; the next pass is paced out
    /// (`10s·2^n`, capped — the same schedule the worker uses for
    /// provider retries, pinned once by issue #310).
    BackingOff {
        /// Backoff schedule index.
        index: u32,
        /// The scheduled delay in whole seconds.
        delay_secs: u64,
    },
}

/// The end-of-run report: typed counts plus the event journal.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LoopSummary {
    /// Why the loop stopped.
    pub stop_reason: StopReason,
    /// Board polls executed.
    pub cycles: u64,
    /// Worker runs started.
    pub spawns: u64,
    /// Runs that completed.
    pub completed: u64,
    /// Runs that ended in a typed failure or panic.
    pub failed: u64,
    /// Runs killed for beat staleness.
    pub stalled: u64,
    /// Runs killed for overstaying the worker timeout.
    pub timed_out: u64,
    /// Runs aborted by a graceful stop (shutdown or a budget stop).
    pub aborted_on_stop: u64,
    /// Failed board polls.
    pub poll_failures: u64,
    /// Whole seconds of loop elapsed time (virtual under tests).
    pub elapsed_secs: u64,
    /// The event journal, in observation order.
    pub events: Vec<LoopEvent>,
}

/// Why the supervisor decided to kill a run (recorded pre-abort, consumed
/// by the arbitration when the handle resolves as cancelled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopIntent {
    /// Beat staleness exceeded the stall timeout.
    Stall,
    /// The worker timeout elapsed while the run kept beating.
    WorkerTimeout,
    /// A graceful stop reaped the run; the payload is the summary reason.
    Drain(StopReason),
}

/// One supervised slot: the process handle plus the beat bookkeeping.
struct Slot {
    run_id: i64,
    task_id: String,
    process: WorkerProcess,
    /// Elapsed time (since loop start) of the last observed beat.
    last_beat_elapsed: Duration,
    /// Elapsed time when the run started.
    started_elapsed: Duration,
    /// The kill intent, recorded before `abort()` is called.
    intent: Option<StopIntent>,
}

/// The intent for slots aborted by a graceful stop: a drain carries the
/// summary reason it served; a drain without a recorded reason cannot
/// happen and resolves to the conservative run-budget terminal.
fn drain_intent(stop: Option<StopReason>) -> StopIntent {
    match stop {
        Some(reason) => StopIntent::Drain(reason),
        None => StopIntent::Drain(StopReason::RunBudgetExhausted),
    }
}

/// The tick cadence: short enough that SIGTERM is honored inside the daemon
/// budget and kills land promptly, long enough to be cheap. One quarter of
/// the stall window, capped at one second.
fn tick_interval(budgets: &WorkerBudgets) -> Duration {
    budgets
        .stall_timeout
        .div_f64(4.0)
        .min(Duration::from_secs(1))
}

/// The abort reason detail recorded on run rows killed by a graceful stop.
fn drain_detail(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Shutdown => "shutdown-abort",
        StopReason::RunBudgetExhausted => "run-budget-abort",
        StopReason::CycleBudget => "cycle-budget-abort",
        // The spend soft cap never aborts; it drains naturally.
        StopReason::SpendSoftCap => "spend-cap-abort",
    }
}

/// The loop runner. Generic over the two seams; all state transitions go
/// through the run-state store, so the guards assert against durable data.
/// The stores are borrowed: the runner runs on one task (a `rusqlite`
/// connection is `Send` but not `Sync`), and the owner keeps reading rows
/// after the run.
pub struct LoopRunner<'a, P: BoardPoller, S: LoopWorkerStarter> {
    config: LoopConfig,
    poller: P,
    starter: S,
    decisions: &'a crate::decision::CampaignDecisionStore,
    runs: &'a LoopRunStore,
    invocation_id: String,
    routing: RoleRoutingDecision,
    start_unix_secs: u64,
    /// Loop-start origin on the TOKIO clock: under a paused runtime
    /// (virtual-clock tests) `std::time::Instant` would never advance, so
    /// every guard timing must read the same clock the sleeps do.
    started: tokio::time::Instant,
    backoff_index: u32,
    next_pass_allowed_at: Duration,
    slots: Vec<Slot>,
}

/// The loop runner's typed counters (summarized into [`LoopSummary`]).
#[derive(Debug, Default, Clone, Copy)]
struct Counters {
    cycles: u64,
    spawns: u64,
    completed: u64,
    failed: u64,
    stalled: u64,
    timed_out: u64,
    aborted_on_stop: u64,
    poll_failures: u64,
}

impl Counters {
    fn note(&mut self, reaped: &Reaped) {
        match reaped.status {
            RunRowStatus::Completed => self.completed += 1,
            RunRowStatus::Failed => self.failed += 1,
            RunRowStatus::Stalled => self.stalled += 1,
            RunRowStatus::TimedOut => self.timed_out += 1,
            RunRowStatus::AbortedShutdown => self.aborted_on_stop += 1,
            RunRowStatus::Running | RunRowStatus::AbortedCrash => {}
        }
    }
}

/// One reaped run: the event for the journal plus its terminal row status.
struct Reaped {
    event: LoopEvent,
    status: RunRowStatus,
}

impl<'a, P: BoardPoller, S: LoopWorkerStarter> LoopRunner<'a, P, S> {
    /// Builds a runner. Validation runs here and again at `run` — a
    /// weakened guard must never reach the supervision loop.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError::Loop`] when the configuration is invalid.
    #[expect(
        clippy::too_many_arguments,
        reason = "each dependency is a distinct seam (config, two board/worker hermeticity traits, two durable stores, routing identity, invocation identity, virtual clock origin); grouping them would hide which argument serves which invariant"
    )]
    pub fn new(
        config: LoopConfig,
        poller: P,
        starter: S,
        decisions: &'a crate::decision::CampaignDecisionStore,
        runs: &'a LoopRunStore,
        routing: RoleRoutingDecision,
        invocation_id: String,
        start_unix_secs: u64,
    ) -> Result<Self, CampaignError> {
        config.validate()?;
        Ok(Self {
            config,
            poller,
            starter,
            decisions,
            runs,
            invocation_id,
            routing,
            start_unix_secs,
            started: tokio::time::Instant::now(),
            backoff_index: 0,
            next_pass_allowed_at: Duration::ZERO,
            slots: Vec::new(),
        })
    }

    /// Unix seconds under the runner's clock: loop-start wall time plus the
    /// (virtual) elapsed time.
    fn now_secs(&self, elapsed: Duration) -> Result<u64, CampaignError> {
        self.start_unix_secs
            .checked_add(elapsed.as_secs())
            .ok_or_else(|| {
                CampaignError::Loop(
                    "clock overflow: loop start plus elapsed exceeds u64".to_string(),
                )
            })
    }

    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Runs the loop until a terminal stop or shutdown. The shutdown channel
    /// carries one increment per received signal (SIGTERM/SIGINT in the CLI
    /// production wiring): the first starts the graceful drain, a second
    /// short-circuits any remaining grace window.
    ///
    /// # Errors
    ///
    /// Returns [`CampaignError`] when a durable write fails, the
    /// configuration is invalid, or the clock overflows. Worker failures are
    /// NOT errors — they are recorded, typed run outcomes.
    pub async fn run(
        mut self,
        mut shutdown: tokio::sync::watch::Receiver<u64>,
    ) -> Result<LoopSummary, CampaignError> {
        self.config.validate()?;
        // Crash reconciliation before any guard reads slots: a previous
        // invocation's running rows would otherwise occupy the concurrency
        // cap forever.
        self.runs
            .reconcile_stale(&self.invocation_id, self.now_secs(self.elapsed())?)?;

        let tick = tick_interval(&self.config.budgets);
        let mut counters = Counters::default();
        let mut events = Vec::new();
        let mut stop: Option<StopReason> = None;
        let mut drain_deadline = Duration::ZERO;
        let mut signal_source_gone = false;

        // `loop`, not `while stop.is_none()`: setting `stop` must NOT exit
        // the loop — the drain (grace window, straggler aborts, final
        // reaps) runs on the following iterations and breaks explicitly
        // when the slots are empty. A `while` guard would terminate the
        // loop the moment a budget stop is declared, leaking running rows.
        loop {
            let elapsed = self.elapsed();
            let now = self.now_secs(elapsed)?;
            self.supervise(&mut counters, &mut events).await?;

            if stop.is_some() {
                self.drain_step(stop, &mut drain_deadline, &mut counters, &mut events)
                    .await?;
                if self.slots.is_empty() {
                    break;
                }
            } else if elapsed >= self.config.budgets.run_budget {
                stop = Some(StopReason::RunBudgetExhausted);
                drain_deadline = elapsed + self.config.shutdown_budget;
            } else {
                let daily = self.runs.daily_spend(now)?;
                if daily >= self.config.budgets.daily_spend_soft_cap_usd {
                    // Soft: seal the intake, let in-flight runs finish.
                    stop = Some(StopReason::SpendSoftCap);
                } else if self
                    .config
                    .max_cycles
                    .is_some_and(|max| counters.cycles >= max)
                {
                    stop = Some(StopReason::CycleBudget);
                    drain_deadline = elapsed + self.config.shutdown_budget;
                } else {
                    self.pass(&mut counters, &mut events, elapsed, now).await?;
                }
            }

            // -- Wait one tick. The shutdown channel stays armed for the
            //    whole run: the first signal starts the drain, a second
            //    short-circuits the remaining grace.
            if signal_source_gone {
                tokio::time::sleep(tick).await;
            } else {
                tokio::select! {
                    changed = shutdown.changed() => {
                        match changed {
                            Ok(()) => {
                                shutdown.borrow_and_update();
                                let elapsed = self.elapsed();
                                if stop.is_none() {
                                    stop = Some(StopReason::Shutdown);
                                    drain_deadline = elapsed + self.config.shutdown_budget;
                                } else if stop == Some(StopReason::Shutdown) {
                                    // Second signal: skip the remaining grace.
                                    drain_deadline = elapsed;
                                } else {
                                    // Shutdown preempts another drain's deadline.
                                    drain_deadline = drain_deadline.min(elapsed + self.config.shutdown_budget);
                                }
                            }
                            // Sender dropped: no signals will ever arrive.
                            Err(_) => signal_source_gone = true,
                        }
                    }
                    () = tokio::time::sleep(tick) => {}
                }
            }
        }

        let final_elapsed = self.elapsed();
        Ok(LoopSummary {
            stop_reason: stop.unwrap_or(StopReason::Shutdown),
            cycles: counters.cycles,
            spawns: counters.spawns,
            completed: counters.completed,
            failed: counters.failed,
            stalled: counters.stalled,
            timed_out: counters.timed_out,
            aborted_on_stop: counters.aborted_on_stop,
            poll_failures: counters.poll_failures,
            elapsed_secs: final_elapsed.as_secs(),
            events,
        })
    }

    /// One supervision step: reap natural completions, observe beats, apply
    /// the stall and worker-timeout kills (intent recorded pre-abort), and
    /// reap the kills. Natural completion is always observed first, so the
    /// arbitration favors a run that finished on its own.
    async fn supervise(
        &mut self,
        counters: &mut Counters,
        events: &mut Vec<LoopEvent>,
    ) -> Result<(), CampaignError> {
        self.record_reaps(counters, events).await?;
        let elapsed = self.elapsed();
        for slot in &mut self.slots {
            if slot
                .process
                .beats
                .has_changed()
                .is_ok_and(|changed| changed)
            {
                slot.process.beats.borrow_and_update();
                slot.last_beat_elapsed = elapsed;
            }
        }
        let stall_timeout = self.config.budgets.stall_timeout;
        self.kill_where(
            |slot, elapsed| {
                slot.intent.is_none()
                    && elapsed.saturating_sub(slot.last_beat_elapsed) >= stall_timeout
            },
            StopIntent::Stall,
        );
        let worker_timeout = self.config.budgets.worker_timeout;
        self.kill_where(
            |slot, elapsed| {
                slot.intent.is_none()
                    && elapsed.saturating_sub(slot.started_elapsed) >= worker_timeout
            },
            StopIntent::WorkerTimeout,
        );
        self.record_reaps(counters, events).await?;
        Ok(())
    }

    /// One drain step while a stop reason is set: within the grace window
    /// (or indefinitely for the spend soft cap, which never aborts) keep
    /// supervising; at the deadline abort stragglers and reap them.
    async fn drain_step(
        &mut self,
        stop: Option<StopReason>,
        drain_deadline: &mut Duration,
        counters: &mut Counters,
        events: &mut Vec<LoopEvent>,
    ) -> Result<(), CampaignError> {
        let elapsed = self.elapsed();
        // The spend soft cap drains naturally — no abort deadline; the
        // stall and worker-timeout guards still apply inside `supervise`.
        let deadline_reached = stop != Some(StopReason::SpendSoftCap) && elapsed >= *drain_deadline;
        if self.slots.is_empty() || deadline_reached {
            self.kill_where(|slot, _| slot.intent.is_none(), drain_intent(stop));
            self.record_reaps(counters, events).await?;
            if !self.slots.is_empty() {
                // Aborts land as cancellations; give them the next tick.
                *drain_deadline = elapsed + self.config.shutdown_budget;
            }
        }
        Ok(())
    }

    /// Records every finished run's terminal row into the counters and the
    /// journal.
    async fn record_reaps(
        &mut self,
        counters: &mut Counters,
        events: &mut Vec<LoopEvent>,
    ) -> Result<(), CampaignError> {
        for reaped in self.reap_finished().await? {
            counters.note(&reaped);
            events.push(reaped.event);
        }
        Ok(())
    }

    /// Aborts every slot whose predicate holds, recording the intent first
    /// (the pre-abort intent drives the arbitration).
    fn kill_where(&mut self, predicate: impl Fn(&Slot, Duration) -> bool, intent: StopIntent) {
        let elapsed = self.elapsed();
        for slot in &mut self.slots {
            if predicate(slot, elapsed) {
                slot.intent = Some(intent);
                slot.process.run.abort();
            }
        }
    }

    /// One campaign pass: poll, filter out this invocation's tasks, plan
    /// exactly one decision record, and spawn when a task was selected. The
    /// pacing gate (slot capacity + backoff) is checked before the cycle is
    /// counted; a gated-off step is a plain supervision tick.
    async fn pass(
        &mut self,
        counters: &mut Counters,
        events: &mut Vec<LoopEvent>,
        elapsed: Duration,
        now: u64,
    ) -> Result<(), CampaignError> {
        if self.slots.len() >= self.config.budgets.max_concurrent_workers as usize
            || elapsed < self.next_pass_allowed_at
        {
            return Ok(());
        }
        counters.cycles += 1;

        match self.poller.poll().await {
            Err(error) => {
                counters.poll_failures += 1;
                events.push(LoopEvent::PollFailed {
                    message: error.to_string(),
                });
                self.pace_no_spawn(events, elapsed);
            }
            Ok(mut snapshot) => {
                let excluded = self.excluded_tasks()?;
                snapshot
                    .ready
                    .retain(|item| !excluded.contains(&task_id_for(&item.repo, item.number)));
                let stored = plan_pass(&snapshot, &self.routing, self.decisions)?;
                let selected = stored.decision.selected.as_ref();
                events.push(LoopEvent::PassPlanned {
                    decision_id: stored.id,
                    selected: selected.is_some(),
                    no_op_reason: stored.decision.no_op_reason,
                    task_id: selected.map(|task| task.task_id.clone()),
                });
                if let Some(selected) = selected {
                    let spend = self.runs.daily_spend(now)?;
                    let process = self
                        .starter
                        .start(&selected.task_id, &self.routing, spend)
                        .await?;
                    let row = self.runs.start(&StartRun {
                        invocation_id: self.invocation_id.clone(),
                        decision_id: stored.id,
                        task_id: selected.task_id.clone(),
                        repo: selected.repo.clone(),
                        number: selected.number,
                        started_at_secs: now,
                    })?;
                    self.slots.push(Slot {
                        run_id: row.id,
                        task_id: selected.task_id.clone(),
                        process,
                        last_beat_elapsed: elapsed,
                        started_elapsed: elapsed,
                        intent: None,
                    });
                    counters.spawns += 1;
                    self.backoff_index = 0;
                    self.next_pass_allowed_at = elapsed;
                    events.push(LoopEvent::WorkerSpawned {
                        run_id: row.id,
                        task_id: selected.task_id.clone(),
                    });
                } else {
                    self.pace_no_spawn(events, elapsed);
                }
            }
        }
        Ok(())
    }

    /// Tasks this invocation already ran or is running (the
    /// never-silent-retry exclusion).
    fn excluded_tasks(&self) -> Result<HashSet<String>, CampaignError> {
        let mut excluded: HashSet<String> =
            self.slots.iter().map(|slot| slot.task_id.clone()).collect();
        // Rows of THIS invocation only: previous invocations' terminal rows
        // are board-visible audit history, not a silent suppression.
        for row in self.runs.runs_for_invocation(&self.invocation_id)? {
            excluded.insert(row.task_id);
        }
        Ok(excluded)
    }

    /// Paces the next pass after one that produced no spawn.
    fn pace_no_spawn(&mut self, events: &mut Vec<LoopEvent>, elapsed: Duration) {
        let delay = orchestraitor_worker::backoff_delay(&self.config.budgets, self.backoff_index);
        self.next_pass_allowed_at = elapsed.saturating_add(delay);
        events.push(LoopEvent::BackingOff {
            index: self.backoff_index,
            delay_secs: delay.as_secs(),
        });
        self.backoff_index = self.backoff_index.saturating_add(1);
    }

    /// Awaits every finished handle and records its terminal row. The
    /// arbitration is single-pointed: `Ok(run)` wins over any recorded
    /// intent (abort-after-completion is a no-op), cancellation resolves to
    /// the intent, and a panic is a typed failure — never a stall.
    async fn reap_finished(&mut self) -> Result<Vec<Reaped>, CampaignError> {
        let mut results = Vec::new();
        let mut index = 0;
        while index < self.slots.len() {
            if !self.slots[index].process.run.is_finished() {
                index += 1;
                continue;
            }
            let slot = self.slots.remove(index);
            let outcome = slot.process.run.await;
            let spend = spend_of(&self.config.budgets, &outcome);
            let (status, detail, completed_flag) = match outcome {
                Ok(Ok(run)) => match run.status {
                    RunStatus::Completed => (
                        RunRowStatus::Completed,
                        "natural-completion".to_string(),
                        true,
                    ),
                    RunStatus::Failed => {
                        let class = run.failure.as_ref().map_or_else(
                            || "typed-failure".to_string(),
                            |failure| format!("{:?}", failure.class),
                        );
                        (RunRowStatus::Failed, class, false)
                    }
                },
                // The worker produced no run at all (fail-closed infra
                // case); the error text is store-assigned and log-safe.
                Ok(Err(error)) => (
                    RunRowStatus::Failed,
                    format!("worker-loop-unusable: {error}"),
                    false,
                ),
                Err(join_error) if join_error.is_cancelled() => match slot.intent {
                    Some(StopIntent::Stall) => {
                        (RunRowStatus::Stalled, "stall-timeout".to_string(), false)
                    }
                    Some(StopIntent::WorkerTimeout) => {
                        (RunRowStatus::TimedOut, "worker-timeout".to_string(), false)
                    }
                    Some(StopIntent::Drain(reason)) => (
                        RunRowStatus::AbortedShutdown,
                        drain_detail(reason).to_string(),
                        false,
                    ),
                    None => (
                        RunRowStatus::Failed,
                        "aborted-without-intent".to_string(),
                        false,
                    ),
                },
                Err(_) => (RunRowStatus::Failed, "worker-panicked".to_string(), false),
            };
            let now = self.now_secs(self.elapsed())?;
            self.runs.finish(slot.run_id, status, now, spend, &detail)?;
            let event = match status {
                RunRowStatus::Completed | RunRowStatus::Failed => LoopEvent::WorkerFinished {
                    run_id: slot.run_id,
                    task_id: slot.task_id.clone(),
                    completed: completed_flag,
                },
                RunRowStatus::Stalled => LoopEvent::WorkerStallKilled {
                    run_id: slot.run_id,
                    task_id: slot.task_id.clone(),
                },
                RunRowStatus::TimedOut => LoopEvent::WorkerTimedOut {
                    run_id: slot.run_id,
                    task_id: slot.task_id.clone(),
                },
                RunRowStatus::AbortedShutdown => LoopEvent::WorkerAbortedOnStop {
                    run_id: slot.run_id,
                    task_id: slot.task_id.clone(),
                    shutdown: detail == "shutdown-abort",
                },
                // `finish` rejects non-terminal statuses, so these never
                // occur here.
                RunRowStatus::Running | RunRowStatus::AbortedCrash => LoopEvent::WorkerFinished {
                    run_id: slot.run_id,
                    task_id: slot.task_id.clone(),
                    completed: false,
                },
            };
            results.push(Reaped { event, status });
        }
        Ok(results)
    }
}

/// Estimated run spend: tokens × the configured per-token estimate (`0.0`
/// disables accrual — the documented default until the cost-ledger lane
/// wires provider pricing in).
fn spend_of(
    budgets: &WorkerBudgets,
    outcome: &Result<Result<WorkerRun, WorkerError>, tokio::task::JoinError>,
) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "token counts are far below 2^53; the estimate only feeds a soft-cap comparison"
    )]
    let tokens = match outcome {
        Ok(Ok(run)) => run.usage.input_tokens + run.usage.output_tokens,
        _ => 0,
    } as f64;
    tokens * budgets.usd_per_token_estimate
}
