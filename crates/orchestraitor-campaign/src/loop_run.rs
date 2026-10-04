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
        // `NaN`/∞ fail every comparison with the recorded daily spend, so a
        // non-finite cap would silently disable the intake seal — fail
        // closed instead.
        if !self.budgets.daily_spend_soft_cap_usd.is_finite()
            || self.budgets.daily_spend_soft_cap_usd < 0.0
        {
            return Err(CampaignError::Loop(
                "guard-weakening rejected: daily spend soft cap must be finite and non-negative"
                    .to_string(),
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
    /// A graceful stop aborted the run while no summary stop reason was
    /// declared (a fatal-exit sweep). `Drain(_)` would fabricate a terminal
    /// budget cause that never held — the unattributed variant keeps the
    /// fabricated reason unrepresentable.
    Drained,
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
        // The spend soft cap never aborts on its own; an abort during its
        // drain only ever carries a shutdown that preempted the cap.
        StopReason::SpendSoftCap => "spend-cap-abort",
    }
}

/// Outcome of an observed shutdown signal.
enum SignalEffect {
    /// First observed signal: the graceful drain begins (or a budget drain
    /// is preempted by the shutdown). The caller records the stop.
    Begin,
    /// A further signal while already draining for shutdown: skip the
    /// remaining grace window.
    ShortCircuit,
}

/// What one tick wait observed.
enum TickSignal {
    /// The tick elapsed; nothing new on the shutdown channel.
    None,
    /// The shutdown sender was dropped — no signals will ever arrive.
    SourceGone,
}

/// What the pass's poll race observed.
enum PassPoll {
    /// The poll returned a snapshot.
    Snapshot(BoardSnapshot),
    /// The poll returned an error (a transient board failure — back off
    /// and retry; the loop stays alive).
    PollFailed(CampaignError),
    /// The shutdown signal won the race.
    Shutdown,
    /// The remaining run budget won the race (the board client's 60s
    /// timeout can outlast the budget's last seconds).
    BudgetExpired,
}

/// Applies one observed shutdown signal to the drain state.
///
/// `drain_reason` starts equal to the declared stop reason and converges to
/// [`StopReason::Shutdown`] once a signal is observed, so every kill intent
/// the drain records afterwards is a shutdown-abort; the summary-facing
/// `stop` reason is the caller's business and keeps the budget cause. The
/// deadline handling distinguishes three cases: the first signal with no
/// stop declared starts the grace window; the first signal during a budget
/// drain preempts it (the spend cap's unbounded natural drain is capped at
/// the budget; a bounded drain keeps its earlier deadline via `min`); a
/// further signal while already shutting down skips the remaining grace.
fn apply_signal(
    stop: Option<StopReason>,
    drain_reason: Option<&mut StopReason>,
    drain_deadline: &mut Duration,
    elapsed: Duration,
    budget: Duration,
) -> SignalEffect {
    match stop {
        Some(StopReason::Shutdown) => {
            *drain_deadline = elapsed;
            SignalEffect::ShortCircuit
        }
        Some(_) => {
            if let Some(drain) = drain_reason {
                *drain = StopReason::Shutdown;
            }
            // Preempt the drain: the spend cap's unbounded natural drain is
            // capped at the budget; a bounded drain keeps its earlier
            // deadline when that is sooner.
            *drain_deadline = (*drain_deadline).min(elapsed + budget);
            SignalEffect::Begin
        }
        None => {
            if let Some(drain) = drain_reason {
                *drain = StopReason::Shutdown;
            }
            *drain_deadline = elapsed + budget;
            SignalEffect::Begin
        }
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
    /// Clone of the shutdown channel, armed during `pass` so a signal that
    /// arrives mid-poll (the board client can block up to its 60s total
    /// timeout) interrupts the pass instead of waiting for it and the rest
    /// of the tick. `None` outside `run` — the field is seeded there.
    shutdown: Option<tokio::sync::watch::Receiver<u64>>,
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
    /// Counts a reaped worker outcome in the invocation summary.
    fn note(&mut self, reaped: &Reaped) {
        match reaped.status {
            RunRowStatus::Completed => self.completed += 1,
            RunRowStatus::Failed => self.failed += 1,
            RunRowStatus::Stalled => self.stalled += 1,
            RunRowStatus::TimedOut => self.timed_out += 1,
            RunRowStatus::AbortedShutdown => self.aborted_on_stop += 1,
            RunRowStatus::Running => {}
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
            shutdown: None,
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

    /// Returns elapsed time on the Tokio clock, including virtual test time.
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Waits one tick, racing the shutdown channel: a signal starts (or
    /// preempts) the drain via [`apply_signal`], a second short-circuits
    /// the remaining grace.
    async fn wait_tick(
        &self,
        shutdown: &mut tokio::sync::watch::Receiver<u64>,
        stop: &mut Option<StopReason>,
        drain_reason: &mut Option<StopReason>,
        drain_deadline: &mut Duration,
    ) -> TickSignal {
        let tick = tick_interval(&self.config.budgets);
        tokio::select! {
            changed = shutdown.changed() => {
                match changed {
                    Ok(()) => {
                        shutdown.borrow_and_update();
                        let elapsed = self.elapsed();
                        match apply_signal(
                            *stop,
                            drain_reason.as_mut(),
                            drain_deadline,
                            elapsed,
                            self.config.shutdown_budget,
                        ) {
                            SignalEffect::ShortCircuit => {}
                            SignalEffect::Begin => {
                                if stop.is_none() {
                                    *stop = Some(StopReason::Shutdown);
                                    *drain_reason = Some(StopReason::Shutdown);
                                }
                            }
                        }
                        TickSignal::None
                    }
                    // Sender dropped: no signals will ever arrive.
                    Err(_) => TickSignal::SourceGone,
                }
            }
            () = tokio::time::sleep(tick) => TickSignal::None,
        }
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
    /// NOT errors — they are recorded, typed run outcomes. Before returning
    /// a fatal error, in-flight slots are aborted and reaped so no row
    /// stays `running` after the process gives up on the run.
    #[expect(
        clippy::too_many_lines,
        reason = "the loop body is the guard state machine (supervise, budget, spend cap, cycle bound, pass, tick wait) read top-to-bottom in arbitration order; extracting arms would scatter the state transitions the spec reviews as one table"
    )]
    pub async fn run(
        mut self,
        mut shutdown: tokio::sync::watch::Receiver<u64>,
    ) -> Result<LoopSummary, CampaignError> {
        // Arm the pass-time race (see `pass`): the run loop holds the
        // original receiver; the pass watches a clone.
        self.shutdown = Some(shutdown.clone());
        self.config.validate()?;
        let tick = tick_interval(&self.config.budgets);
        let mut counters = Counters::default();
        let mut events = Vec::new();
        let mut stop: Option<StopReason> = None;
        // What the drain's kill intents record. Starts equal to the declared
        // stop reason; a shutdown signal converges it to `Shutdown` so the
        // preemption is visible in the recorded details, while `stop` keeps
        // the budget cause for the summary.
        let mut drain_reason: Option<StopReason> = None;
        // `Duration::MAX` = unbounded natural drain (the spend soft cap). A
        // shutdown signal always bounds it.
        let mut drain_deadline = Duration::MAX;
        let mut signal_source_gone = false;

        // The loop body propagates fatal (durable-state) errors; this guard
        // converts such an exit into the fail-safe terminal: abort every
        // in-flight slot (intent recorded pre-abort — the arbitration then
        // records them as aborted), reap what is reapable, and attach the
        // events to the fatal error's context via the counters above.
        let run_result: Result<(), CampaignError> = async {
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
                    self.drain_step(
                        drain_reason,
                        &mut drain_deadline,
                        &mut counters,
                        &mut events,
                    )
                    .await?;
                    if self.slots.is_empty() {
                        break;
                    }
                } else if elapsed >= self.config.budgets.run_budget {
                    stop = Some(StopReason::RunBudgetExhausted);
                    drain_reason = stop;
                    drain_deadline = elapsed + self.config.shutdown_budget;
                } else {
                    let daily = self.runs.daily_spend(now)?;
                    if daily >= self.config.budgets.daily_spend_soft_cap_usd {
                        // Soft: seal the intake, let in-flight runs finish.
                        stop = Some(StopReason::SpendSoftCap);
                        drain_reason = stop;
                        // Unbounded natural drain: no abort deadline. A
                        // shutdown signal bounds it (see `apply_signal`).
                        drain_deadline = Duration::MAX;
                    } else if self
                        .config
                        .max_cycles
                        .is_some_and(|max| counters.cycles >= max)
                    {
                        stop = Some(StopReason::CycleBudget);
                        drain_reason = stop;
                        drain_deadline = elapsed + self.config.shutdown_budget;
                    } else {
                        let signal_consumed = self
                            .pass(
                                &mut counters,
                                &mut events,
                                elapsed,
                                &mut stop,
                                &mut drain_reason,
                                &mut drain_deadline,
                            )
                            .await?;
                        if signal_consumed {
                            // The pass consumed the send through the clone;
                            // advance the MAIN receiver's seen version too, or
                            // `wait_tick` re-observes the same send and applies
                            // the second-signal short-circuit to a first signal
                            // (grace collapsing to one tick).
                            shutdown.borrow_and_update();
                        }
                    }
                }

                // -- Wait one tick. The shutdown channel stays armed for the
                //    whole run: the first signal starts (or preempts) the
                //    drain, a second short-circuits the remaining grace.
                if signal_source_gone {
                    tokio::time::sleep(tick).await;
                } else {
                    match self
                        .wait_tick(
                            &mut shutdown,
                            &mut stop,
                            &mut drain_reason,
                            &mut drain_deadline,
                        )
                        .await
                    {
                        TickSignal::None => {}
                        // Sender dropped: no signals will ever arrive.
                        TickSignal::SourceGone => signal_source_gone = true,
                    }
                }
            }
            Ok(())
        }
        .await;

        if run_result.is_err() && !self.slots.is_empty() {
            // Fatal exit: never strand a `running` row. Abort every
            // in-flight slot (intent recorded pre-abort — the arbitration
            // records them as aborted-unattributed: no summary stop reason
            // was declared, so inventing a budget cause on the audit row
            // would be a fabrication) and reap what is reapable. The sweep
            // is best-effort: its own clock/store failure cannot make things
            // more terminal than the fatal error already is, and the
            // original error is returned either way.
            self.kill_where(|slot, _| slot.intent.is_none(), StopIntent::Drained);
            // Remove the slots first: awaiting each handle needs the slot
            // owned, and `self.runs` borrows `self` immutably.
            let stranded = std::mem::take(&mut self.slots);
            for slot in stranded {
                // Wait for the aborted task to observe the cancellation
                // (the handle is dropped with the slot; awaiting it here
                // keeps the sweep synchronous with the terminal write).
                let _ignore = slot.process.run.await;
                // The fatal may BE the clock: clamp the terminal stamp to
                // what the store can represent (the sweep is best-effort —
                // a misdated row is still terminal).
                let stamp = self
                    .now_secs(self.elapsed())
                    .unwrap_or(i64::MAX as u64)
                    .min(i64::MAX as u64);
                let _ignore = self.runs.finish(
                    slot.run_id,
                    RunRowStatus::AbortedShutdown,
                    stamp,
                    0.0,
                    "unattributed-drain-abort",
                );
                counters.aborted_on_stop += 1;
                events.push(LoopEvent::WorkerAbortedOnStop {
                    run_id: slot.run_id,
                    task_id: slot.task_id.clone(),
                    shutdown: false,
                });
            }
        }

        run_result?;

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
        // Split borrows: collect (run_id, beat) pairs first, then persist —
        // `self.runs.heartbeat` needs `&self.runs` while `self.slots` is
        // mutably borrowed.
        let mut fresh_beats: Vec<(i64, u64)> = Vec::new();
        for slot in &mut self.slots {
            if slot
                .process
                .beats
                .has_changed()
                .is_ok_and(|changed| changed)
            {
                let beat = *slot.process.beats.borrow_and_update();
                slot.last_beat_elapsed = elapsed;
                fresh_beats.push((slot.run_id, beat));
            }
        }
        // Durable liveness: each observed beat is persisted so the loop.db
        // heartbeat columns stay current (see run_state). A stale row
        // (externally finished) would reject the beat — that failure
        // surfaces; it is never suppressed.
        let beat_secs = self.now_secs(elapsed)?;
        for (run_id, beat) in fresh_beats {
            self.runs.heartbeat(run_id, beat, beat_secs)?;
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
    /// (or indefinitely for the spend soft cap, whose deadline is
    /// `Duration::MAX` until a shutdown preempts it) keep supervising; at
    /// the deadline abort stragglers and reap them. The recorded intent is
    /// `drain_reason` — a shutdown that preempted a budget stop still
    /// records `shutdown-abort`.
    async fn drain_step(
        &mut self,
        drain_reason: Option<StopReason>,
        drain_deadline: &mut Duration,
        counters: &mut Counters,
        events: &mut Vec<LoopEvent>,
    ) -> Result<(), CampaignError> {
        let elapsed = self.elapsed();
        let deadline_reached = elapsed >= *drain_deadline;
        if self.slots.is_empty() || deadline_reached {
            // `drain_reason` is `Some` whenever a stop is declared (see
            // `run`) — the unattributed arm is unreachable, but the type
            // still refuses to invent a terminal reason here.
            let intent = match drain_reason {
                Some(reason) => StopIntent::Drain(reason),
                None => StopIntent::Drained,
            };
            self.kill_where(|slot, _| slot.intent.is_none(), intent);
            self.record_reaps(counters, events).await?;
            if !self.slots.is_empty() {
                // Aborts land as cancellations; give them the next tick.
                // A fresh unbounded cap drain cannot reach here (the abort
                // only fires at a real deadline), so the extension is
                // always bounded.
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
    ///
    /// The poll races the shutdown channel *and* the remaining run budget:
    /// `BoardClient` calls can block up to its 60s total timeout, far
    /// outside the five-second daemon budget, so a signal observed mid-poll
    /// drops the in-flight request future (`poll` takes `&self` and owns no
    /// kill-capable state) and declares the shutdown right here — the
    /// signal is never swallowed, and the pass never spawns work on a stale
    /// snapshot. A budget that expires mid-poll does the same for the run
    /// budget (typed `RunBudgetExhausted`), and a fresh check after the
    /// poll completes keeps a budget that expired during the request from
    /// ever reaching plan or spawn.
    ///
    /// Returns whether the pass consumed a signal (the interrupt arm fired):
    /// the caller must then advance the MAIN receiver's seen version, or the
    /// same send is re-observed by `wait_tick` and misread as a second
    /// signal (collapsing the grace window to one tick).
    async fn pass(
        &mut self,
        counters: &mut Counters,
        events: &mut Vec<LoopEvent>,
        elapsed: Duration,
        stop: &mut Option<StopReason>,
        drain_reason: &mut Option<StopReason>,
        drain_deadline: &mut Duration,
    ) -> Result<bool, CampaignError> {
        if self.slots.len() >= self.config.budgets.max_concurrent_workers as usize
            || elapsed < self.next_pass_allowed_at
        {
            return Ok(false);
        }
        counters.cycles += 1;

        // Scoped: the poll future borrows the poller, so the race — and
        // that borrow — must end before the pass mutates runner state.
        // The remaining run budget: the board client can block up to its
        // 60s total timeout, so the last seconds of the budget would
        // otherwise be spent blocked inside the poll — with no supervision
        // and no chance to enter the budget drain.
        let budget_left = self.config.budgets.run_budget.checked_sub(self.elapsed());
        let poll_result = {
            let poll = self.poller.poll();
            tokio::pin!(poll);
            // Field-disjoint from `poller`: the shutdown watch rides the
            // same borrow scope. `None` (before `run` seeds the clone)
            // never wakes.
            let shutdown = self.shutdown.as_mut();
            tokio::select! {
                result = &mut poll => match result {
                    Ok(snapshot) => PassPoll::Snapshot(snapshot),
                    // Transient board failure: recorded and paced below —
                    // never propagated (a propagated error would kill the
                    // run and orphan its in-flight workers).
                    Err(error) => PassPoll::PollFailed(error),
                },
                () = async move {
                    match shutdown {
                        // Ok(()) = a signal; Err = the sender was dropped,
                        // no signal will ever arrive — stay pending.
                        Some(receiver) => {
                            if receiver.changed().await.is_err() {
                                std::future::pending::<()>().await;
                            }
                        }
                        None => std::future::pending().await,
                    }
                } => PassPoll::Shutdown,
                // Absent when the budget is already spent: the caller
                // checked `elapsed < run_budget` this iteration.
                () = async {
                    match budget_left {
                        Some(remaining) => tokio::time::sleep(remaining).await,
                        None => std::future::pending().await,
                    }
                } => PassPoll::BudgetExpired,
            }
        };
        match poll_result {
            PassPoll::Snapshot(snapshot) => {
                // Fresh post-poll budget check: the poll may complete just
                // as the budget expires (the race above only bounds the
                // in-flight wait) — never plan or start work against a
                // spent budget; enter the same drain as the race arm.
                if self.elapsed() >= self.config.budgets.run_budget {
                    *stop = Some(StopReason::RunBudgetExhausted);
                    *drain_reason = Some(StopReason::RunBudgetExhausted);
                    *drain_deadline = self.elapsed() + self.config.shutdown_budget;
                    return Ok(false);
                }
                // Shutdown wins the poll-vs-signal race: tokio's `select!` is
                // unbiased, so a signal that arrived during the poll can lose
                // to the snapshot arm even though the documented behavior
                // seals the intake on the signal. A yield lets the signal
                // task's send (and the clone's waker registration) land
                // before the synchronous recheck below — a send that raced
                // the snapshot arm must still seal the intake before any
                // work is planned or spawned.
                tokio::task::yield_now().await;
                tokio::task::yield_now().await;
                if let Some(receiver) = self.shutdown.as_mut()
                    && matches!(receiver.has_changed(), Ok(true))
                {
                    self.shutdown_from_pass(stop, drain_reason, drain_deadline);
                    return Ok(true);
                }
                // Fresh post-poll clock: the poll can block for up to the
                // board client's timeout, so the pre-poll `elapsed`/`now`
                // would backdate the worker's heartbeat baseline and the
                // run row's started-at day. Re-read both before planning.
                let elapsed = self.elapsed();
                let now = self.now_secs(elapsed)?;
                self.plan_and_spawn(counters, events, elapsed, now, snapshot)
                    .await
            }
            PassPoll::PollFailed(error) => {
                // Transient board failure: count it, journal it, pace the
                // next pass — the loop stays alive and keeps supervising
                // any in-flight workers. Propagating would end the run
                // without a drain, leaving running rows and detached
                // worker tasks behind. The pace anchors at the *current*
                // elapsed (the poll itself may have consumed up to the
                // board client's timeout): pre-poll pacing would let a
                // slow failing poll eat the backoff and re-poll on the
                // next tick.
                counters.poll_failures += 1;
                events.push(LoopEvent::PollFailed {
                    message: error.to_string(),
                });
                self.pace_no_spawn(events, self.elapsed());
                Ok(false)
            }
            PassPoll::Shutdown => {
                // The poll future is dropped mid-request; the signal that
                // won the race declares the shutdown. The pass-time clone's
                // version is advanced here; the caller advances the MAIN
                // receiver too (returning `true` below), so the same send
                // is never re-observed as a second signal.
                self.mark_signal_seen();
                self.shutdown_from_pass(stop, drain_reason, drain_deadline);
                Ok(true)
            }
            PassPoll::BudgetExpired => {
                // The budget expired while the poll was blocked. The pass
                // consumes nothing — plan nothing, spawn nothing; the
                // caller's drain (with any in-flight slots) takes over.
                *stop = Some(StopReason::RunBudgetExhausted);
                *drain_reason = Some(StopReason::RunBudgetExhausted);
                *drain_deadline = self.elapsed() + self.config.shutdown_budget;
                Ok(false)
            }
        }
    }

    /// Declares the shutdown from inside a pass: seals the intake, arms the
    /// grace-window drain, and reports that a send was consumed (the caller
    /// advances the MAIN receiver so the send is never re-observed).
    fn shutdown_from_pass(
        &mut self,
        stop: &mut Option<StopReason>,
        drain_reason: &mut Option<StopReason>,
        drain_deadline: &mut Duration,
    ) {
        // Declared before the drain cells: `wait_tick`'s `Begin` branch
        // refuses to overwrite an existing stop, and the run loop's
        // drain iterations check `stop.is_some()`.
        *stop = Some(StopReason::Shutdown);
        *drain_reason = Some(StopReason::Shutdown);
        *drain_deadline = self.elapsed() + self.config.shutdown_budget;
    }

    /// Plans one campaign pass from a freshly polled snapshot and spawns
    /// when a task was selected — reached only after `pass` has re-checked
    /// the budget against the completed poll.
    async fn plan_and_spawn(
        &mut self,
        counters: &mut Counters,
        events: &mut Vec<LoopEvent>,
        elapsed: Duration,
        now: u64,
        mut snapshot: BoardSnapshot,
    ) -> Result<bool, CampaignError> {
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
            // Row first, then spawn: a durable insert failure must never
            // strand a running worker (a dropped handle detaches the task to
            // its 45m timeout, invisible to every sweep). The row itself is
            // the audit record; if the spawn then fails, the row is finished
            // below — no window exists where a live worker has no row.
            let row = self.runs.start(&StartRun {
                invocation_id: self.invocation_id.clone(),
                decision_id: stored.id,
                task_id: selected.task_id.clone(),
                repo: selected.repo.clone(),
                number: selected.number,
                started_at_secs: now,
            })?;
            // Spawn failures are per-task outcomes, not run-killers: the
            // production starter fails on ordinary per-task conditions
            // (missing fixture, unsupported provider, transport build) —
            // propagating would strand in-flight workers and re-select the
            // same dying task next invocation. Record a terminal `failed`
            // row, pace, and keep supervising. Store/clock errors still
            // propagate (durable-state failure is fatal); the row written
            // above is then swept terminal by the fatal-exit guard.
            let process = match self
                .starter
                .start(&selected.task_id, &self.routing, spend)
                .await
            {
                Ok(process) => process,
                Err(CampaignError::Spawn { message, .. }) => {
                    self.runs
                        .finish(row.id, RunRowStatus::Failed, now, 0.0, &message)?;
                    counters.failed += 1;
                    events.push(LoopEvent::WorkerFinished {
                        run_id: row.id,
                        task_id: selected.task_id.clone(),
                        completed: false,
                    });
                    self.pace_no_spawn(events, self.elapsed());
                    return Ok(false);
                }
                // Anything else from the starter is not a per-task spawn
                // condition — surface it.
                Err(error) => return Err(error),
            };
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
        Ok(false)
    }

    /// Advances the pass-time shutdown clone's seen version after the pass
    /// consumed a signal, keeping the clone's marker aligned with the main
    /// receiver (which the run loop advances separately).
    fn mark_signal_seen(&mut self) {
        if let Some(receiver) = self.shutdown.as_mut() {
            receiver.borrow_and_update();
        }
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
                    // A drain whose summary stop reason was never declared
                    // (a fatal-exit sweep): unattributed, by construction.
                    Some(StopIntent::Drained) => (
                        RunRowStatus::AbortedShutdown,
                        "unattributed-drain-abort".to_string(),
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
                RunRowStatus::Running => LoopEvent::WorkerFinished {
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
