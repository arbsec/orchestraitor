//! Virtual-clock guard tests for the loop runner (issue #314, spec §21.4):
//! every guard test asserts outcomes AND elapsed virtual time — a real
//! sleep would never advance under `start_paused`, so elapsed assertions
//! pin the no-real-sleeps requirement positively.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
// Test-only allowances mirror the session test harness: a failed assertion
// must fail the test loudly.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use orchestraitor_agent_catalog::RoleRoutingDecision;
use orchestraitor_board::ReadyItem;
use orchestraitor_campaign::{
    BoardPoller, BoardSnapshot, CampaignDecisionStore, CampaignError, LoopConfig, LoopRunner,
    LoopWorkerStarter, StopReason, WorkerProcess,
};
use orchestraitor_worker::{BudgetEcho, RunStatus, UsageTotals, WorkerBudgets, WorkerRun};

const REPO: &str = "arbsec/orchestraitor";
const START_UNIX: u64 = 1_000_000;

/// Builds a deterministic worker routing decision for loop fixtures.
fn routing() -> RoleRoutingDecision {
    RoleRoutingDecision {
        role: "implement".to_string(),
        provider: "neuralwatt".to_string(),
        model: "glm-5.2".to_string(),
        precedence_path: "test".to_string(),
        fallback_reason: None,
    }
}

/// Builds a ready board item for the fixture repository and issue number.
fn ready_item(number: u64) -> ReadyItem {
    ReadyItem {
        number,
        title: format!("fixture task {number}"),
        url: format!("https://github.com/{REPO}/issues/{number}"),
        repo: REPO.to_string(),
        item_id: format!("item-{number}"),
    }
}

/// Creates a board snapshot with no eligible or blocked work.
fn empty_snapshot() -> BoardSnapshot {
    BoardSnapshot {
        open: Vec::new(),
        ready: Vec::new(),
        blocked_candidates: Vec::new(),
        warnings: Vec::new(),
    }
}

/// Creates a board snapshot whose ready queue contains the supplied issues.
fn snapshot_with(numbers: &[u64]) -> BoardSnapshot {
    BoardSnapshot {
        ready: numbers.iter().copied().map(ready_item).collect(),
        ..empty_snapshot()
    }
}

/// Builds a completed worker result with explicit turn and token totals.
fn fixture_run(task_id: &str, turns: u32, tokens: u64) -> WorkerRun {
    WorkerRun {
        task_id: task_id.to_string(),
        status: RunStatus::Completed,
        exit_code: 0,
        summary: Some("fixture-completion".to_string()),
        failure: None,
        delivery: None,
        attempts: 1,
        replans: 0,
        turns,
        model_calls: turns,
        usage: UsageTotals {
            input_tokens: tokens / 2,
            output_tokens: tokens - tokens / 2,
        },
        spend_soft_cap_exceeded: false,
        untrusted_writes: Vec::new(),
        receipts: Vec::new(),
        budgets: BudgetEcho {
            max_attempts: 3,
            max_replans: 2,
            worker_timeout_secs: 45 * 60,
            max_concurrent_workers: 2,
            stall_timeout_secs: 10 * 60,
            retry_base_delay_ms: 10_000,
            retry_max_delay_secs: 300,
            max_provider_retries: 3,
            daily_spend_soft_cap_usd: 10.0,
            run_budget_secs: 4 * 3600,
            max_turns_per_attempt: 25,
            max_consecutive_format_errors: 3,
        },
    }
}

/// What the fake worker does once started.
#[derive(Clone)]
enum Behavior {
    /// Completes immediately with the given turn/token totals.
    Complete { turns: u32, tokens: u64 },
    /// Sleeps the duration, then completes.
    CompleteAfter(Duration),
    /// Beats every interval forever; only the worker timeout can kill it.
    BeatEvery(Duration),
    /// Beats n times, then hangs silently (stall-killed).
    BeatThenPark { beats: u32, interval: Duration },
    /// Hangs silently from the start (stall-killed at spawn + stall).
    SilentHang,
    /// Panics immediately (typed failed row, loop continues).
    Panic,
}

#[derive(Clone)]
struct FakeStarter {
    /// One behavior per spawn (cycled); a single-behavior starter repeats
    /// the same behavior.
    behaviors: Vec<Behavior>,
    /// Captured `(task_id, prior_daily_spend_usd)` per spawn — the spend
    /// feed-through surface. Shared through an `Arc` so the test can read
    /// it after the runner consumes a clone.
    spawns: Arc<std::sync::Mutex<Vec<(String, f64)>>>,
    /// Increments once per beat — the forbidden-effect counter: after a
    /// kill, it must never advance again.
    beats_observed: Arc<AtomicUsize>,
}

impl FakeStarter {
    /// Creates a starter that repeats one worker behavior for every spawn.
    fn new(behavior: Behavior) -> Self {
        Self::with_behaviors(vec![behavior])
    }

    /// Creates a starter that cycles through the supplied worker behaviors.
    fn with_behaviors(behaviors: Vec<Behavior>) -> Self {
        Self {
            behaviors,
            spawns: Arc::new(std::sync::Mutex::new(Vec::new())),
            beats_observed: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Returns the task ids and prior spend passed to each worker spawn.
    fn spend_feed(&self) -> Vec<(String, f64)> {
        self.spawns.lock().unwrap().clone()
    }
}

#[async_trait]
impl LoopWorkerStarter for FakeStarter {
    /// Records spawn inputs and runs the next scripted behavior with a beat channel.
    async fn start(
        &self,
        task_id: &str,
        _routing: &RoleRoutingDecision,
        prior_daily_spend_usd: f64,
    ) -> Result<WorkerProcess, CampaignError> {
        self.spawns
            .lock()
            .unwrap()
            .push((task_id.to_string(), prior_daily_spend_usd));
        let (tx, rx) = tokio::sync::watch::channel(0_u64);
        let beats = self.beats_observed.clone();
        let spawn_index = self.spawns.lock().unwrap().len() - 1;
        let behavior = self.behaviors[spawn_index % self.behaviors.len()].clone();
        let id = task_id.to_string();
        let run = tokio::spawn(async move {
            match behavior {
                Behavior::Complete { turns, tokens } => Ok(fixture_run(&id, turns, tokens)),
                Behavior::CompleteAfter(delay) => {
                    tokio::time::sleep(delay).await;
                    Ok(fixture_run(&id, 1, 0))
                }
                Behavior::BeatEvery(interval) => {
                    let mut tick = 0_u64;
                    loop {
                        tokio::time::sleep(interval).await;
                        tick += 1;
                        let _ = tx.send(tick);
                        beats.fetch_add(1, Ordering::SeqCst);
                    }
                }
                Behavior::BeatThenPark { beats: n, interval } => {
                    for i in 1..=n {
                        tokio::time::sleep(interval).await;
                        let _ = tx.send(u64::from(i));
                        beats.fetch_add(1, Ordering::SeqCst);
                    }
                    std::future::pending::<()>().await;
                    unreachable!("pending never resolves");
                }
                Behavior::SilentHang => {
                    std::future::pending::<()>().await;
                    unreachable!("pending never resolves");
                }
                Behavior::Panic => {
                    panic!("fixture-worker-panic");
                }
            }
        });
        Ok(WorkerProcess { beats: rx, run })
    }
}

struct FakePoller {
    snapshot: BoardSnapshot,
}

#[async_trait]
impl BoardPoller for FakePoller {
    /// Returns the same board snapshot on every poll.
    async fn poll(&self) -> Result<BoardSnapshot, CampaignError> {
        Ok(self.snapshot.clone())
    }
}

/// Runs a fixture invocation and returns its summary and stores for assertions.
async fn run_loop(
    config: LoopConfig,
    snapshot: BoardSnapshot,
    starter: FakeStarter,
    invocation: &str,
    shutdown: tokio::sync::watch::Receiver<u64>,
) -> (
    orchestraitor_campaign::LoopSummary,
    CampaignDecisionStore,
    orchestraitor_campaign::LoopRunStore,
) {
    let decisions = CampaignDecisionStore::open_in_memory().unwrap();
    let runs = orchestraitor_campaign::LoopRunStore::open_in_memory().unwrap();
    let runner = LoopRunner::new(
        config,
        FakePoller { snapshot },
        starter,
        &decisions,
        &runs,
        routing(),
        invocation.to_string(),
        START_UNIX,
    )
    .unwrap();
    let summary = runner.run(shutdown).await.unwrap();
    (summary, decisions, runs)
}

/// A shutdown channel that never signals: the sender is leaked (not
/// dropped) so `changed()` stays pending — the runner waits on timers.
fn never() -> tokio::sync::watch::Receiver<u64> {
    let (tx, rx) = tokio::sync::watch::channel(0_u64);
    std::mem::forget(tx);
    rx
}

/// Checks that a completed task is recorded once and excluded from later passes.
#[tokio::test(start_paused = true)]
async fn happy_path_spawns_completes_and_no_ops_the_next_cycle() {
    let starter = FakeStarter::new(Behavior::Complete {
        turns: 2,
        tokens: 100,
    });
    let config = LoopConfig::new(
        WorkerBudgets::bootstrap_defaults(),
        Duration::from_secs(5),
        Some(2),
    )
    .unwrap();
    let (summary, decisions, runs) =
        run_loop(config, snapshot_with(&[1]), starter, "inv", never()).await;

    assert_eq!(summary.stop_reason, StopReason::CycleBudget);
    assert_eq!(summary.spawns, 1);
    assert_eq!(summary.completed, 1);
    assert_eq!(summary.failed, 0);
    assert_eq!(summary.stalled, 0);
    // Cycle 1 selected the task; cycle 2 saw the exclusion and no-op'd.
    assert_eq!(summary.cycles, 2);
    let records = decisions.list().unwrap();
    assert_eq!(records.len(), 2, "exactly one decision record per pass");
    assert!(records[0].decision.selected.is_some());
    assert!(records[1].decision.selected.is_none());
    // The run row reached the terminal completed status.
    let rows = runs.runs_for_invocation("inv").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].status,
        orchestraitor_campaign::RunRowStatus::Completed
    );
    // Virtual clock: a real sleep would never reach a full backoff window.
    assert!(summary.elapsed_secs >= 1, "loop must have ticked");
    assert!(summary.elapsed_secs < 60, "happy path must not pace long");
}

/// Checks the stall deadline and proves the aborted worker emits no further beats.
#[tokio::test(start_paused = true)]
async fn stalled_worker_is_killed_within_one_stall_window() {
    let starter = FakeStarter::new(Behavior::BeatThenPark {
        beats: 2,
        interval: Duration::from_mins(1),
    });
    // Enough cycles that the no-op passes during the 12m stall window do
    // not exhaust the bound before the kill (backoffs 10+20+40+80+160+300
    // reach ~11m by pass 7).
    let config = LoopConfig::new(
        WorkerBudgets::bootstrap_defaults(),
        Duration::from_secs(5),
        Some(10),
    )
    .unwrap();
    let beats = starter.beats_observed.clone();
    let (summary, _decisions, runs) =
        run_loop(config, snapshot_with(&[1]), starter, "inv", never()).await;

    assert_eq!(summary.stalled, 1);
    assert_eq!(summary.completed, 0);
    let rows = runs.runs_for_invocation("inv").unwrap();
    assert_eq!(
        rows[0].status,
        orchestraitor_campaign::RunRowStatus::Stalled
    );
    // Beats at 1m and 2m, then silence: kill at 2m + 10m = 12m. The kill
    // moment is the row's own span (started -> finished), not the summary
    // elapsed (the loop legitimately continues to its cycle bound after
    // the kill).
    let span = rows[0].finished_at_secs.unwrap() - rows[0].started_at_secs;
    assert!(span >= 12 * 60, "kill must wait the full stall window");
    assert!(
        span <= 12 * 60 + 60,
        "kill must land within one window (+ tick slop)"
    );
    // Forbidden effect (spec §21.4): the killed worker performs NO further
    // beats — advance well past where a live worker would have beaten.
    let frozen = beats.load(Ordering::SeqCst);
    tokio::time::advance(Duration::from_mins(10)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        beats.load(Ordering::SeqCst),
        frozen,
        "killed worker must perform no further work"
    );
}

/// Checks that live worker rows never exceed the configured two-slot cap.
#[tokio::test(start_paused = true)]
async fn concurrency_cap_never_exceeds_two_slots() {
    let starter = FakeStarter::new(Behavior::SilentHang);
    // 30s run budget: shorter than the 10m stall window, so the drain (not
    // a stall kill) reaps the two slots.
    let budgets = WorkerBudgets {
        run_budget: Duration::from_secs(30),
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), None).unwrap();
    let (summary, _decisions, runs) =
        run_loop(config, snapshot_with(&[1, 2, 3]), starter, "inv", never()).await;

    assert_eq!(summary.stop_reason, StopReason::RunBudgetExhausted);
    assert_eq!(summary.spawns, 2, "the cap holds a third task back");
    assert_eq!(summary.aborted_on_stop, 2);
    let rows = runs.runs_for_invocation("inv").unwrap();
    assert_eq!(rows.len(), 2, "task 3 must never have started");
    assert!(summary.elapsed_secs >= 30, "drain waits for the run budget");
    assert!(summary.elapsed_secs < 60, "drain aborts within the budget");
}

/// Checks that heartbeats cannot extend a worker beyond its total timeout.
#[tokio::test(start_paused = true)]
async fn beating_worker_is_timeout_killed_not_stall_killed() {
    let starter = FakeStarter::new(Behavior::BeatEvery(Duration::from_mins(1)));
    // 2h run budget so the 45m worker timeout fires first; enough cycles
    // that the no-op passes during the wait do not exhaust the bound
    // before the kill (backoff schedule reaches 45m around pass 14).
    let budgets = WorkerBudgets {
        run_budget: Duration::from_hours(2),
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), Some(16)).unwrap();
    let (summary, _decisions, runs) =
        run_loop(config, snapshot_with(&[1]), starter, "inv", never()).await;

    assert_eq!(summary.timed_out, 1);
    assert_eq!(summary.stalled, 0, "a beating worker is never stall-killed");
    let rows = runs.runs_for_invocation("inv").unwrap();
    assert_eq!(
        rows[0].status,
        orchestraitor_campaign::RunRowStatus::TimedOut
    );
    // Kill moment = the row's own span: the full worker timeout.
    let span = rows[0].finished_at_secs.unwrap() - rows[0].started_at_secs;
    assert!(span >= 45 * 60, "kill waits the full timeout");
    assert!(
        span <= 45 * 60 + 60,
        "kill lands within one window (+ slop)"
    );
}

/// Checks virtual pass times against the exponential backoff and its ceiling.
#[tokio::test(start_paused = true)]
async fn backoff_between_no_op_passes_doubles_and_caps() {
    let starter = FakeStarter::new(Behavior::Complete {
        turns: 1,
        tokens: 0,
    });
    let config = LoopConfig::new(
        WorkerBudgets::bootstrap_defaults(),
        Duration::from_secs(5),
        Some(8),
    )
    .unwrap();
    let (summary, _decisions, _runs) =
        run_loop(config, empty_snapshot(), starter, "inv", never()).await;

    assert_eq!(summary.stop_reason, StopReason::CycleBudget);
    let delays: Vec<u64> = summary
        .events
        .iter()
        .filter_map(|event| match event {
            orchestraitor_campaign::LoopEvent::BackingOff { delay_secs, .. } => Some(*delay_secs),
            _ => None,
        })
        .collect();
    assert_eq!(
        delays,
        vec![10, 20, 40, 80, 160, 300, 300, 300],
        "10s·2^n capped at 5m"
    );
    // Every scheduled delay except the final one was actually waited out —
    // the loop exits on the cycle bound before pacing out the last delay.
    let scheduled: u64 = delays.iter().take(delays.len().saturating_sub(1)).sum();
    assert!(
        summary.elapsed_secs >= scheduled,
        "passes honor the schedule"
    );
}

/// Checks that the run budget ends intake and records aborted stragglers.
#[tokio::test(start_paused = true)]
async fn run_budget_stops_the_loop_and_records_aborts() {
    let starter = FakeStarter::new(Behavior::BeatEvery(Duration::from_mins(1)));
    let budgets = WorkerBudgets {
        run_budget: Duration::from_hours(1),
        worker_timeout: Duration::from_hours(2),
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), None).unwrap();
    let (summary, _decisions, runs) =
        run_loop(config, snapshot_with(&[1]), starter, "inv", never()).await;

    assert_eq!(summary.stop_reason, StopReason::RunBudgetExhausted);
    assert_eq!(summary.aborted_on_stop, 1);
    assert!(summary.elapsed_secs >= 3600);
    assert!(summary.elapsed_secs < 3600 + 15, "drain stays in budget");
    let rows = runs.runs_for_invocation("inv").unwrap();
    assert_eq!(
        rows[0].status,
        orchestraitor_campaign::RunRowStatus::AbortedShutdown
    );
}

/// Checks that recorded spend reaches the cap and prevents another spawn.
#[tokio::test(start_paused = true)]
async fn spend_soft_cap_seals_intake_and_drains() {
    // 3M tokens × $2/M = $6 per run; two runs cross the $10 cap.
    let starter = FakeStarter::new(Behavior::Complete {
        turns: 3,
        tokens: 3_000_000,
    });
    let budgets = WorkerBudgets {
        usd_per_token_estimate: 2e-6,
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), None).unwrap();
    let (summary, _decisions, _runs) = run_loop(
        config,
        snapshot_with(&[1, 2]),
        starter.clone(),
        "inv",
        never(),
    )
    .await;

    assert_eq!(summary.stop_reason, StopReason::SpendSoftCap);
    assert_eq!(summary.spawns, 2, "no spawn after the cap");
    assert_eq!(summary.completed, 2, "in-flight work drains naturally");
    assert_eq!(summary.aborted_on_stop, 0, "soft cap never aborts");
    // Spend feed-through: the first spawn saw $0 accrued, the second $6.
    let feed = starter.spend_feed();
    assert_eq!(feed.len(), 2);
    assert!(feed[0].1.abs() < 1e-9);
    assert!(
        (feed[1].1 - 6.0).abs() < 1e-9,
        "prior spend is fed through; feed: {feed:?}"
    );
}

/// Checks that configurations weakening any pinned guard are rejected.
#[tokio::test(start_paused = true)]
async fn guardrail_weakening_is_rejected_fail_closed() {
    let zero_concurrency = LoopConfig::new(
        WorkerBudgets {
            max_concurrent_workers: 0,
            ..WorkerBudgets::bootstrap_defaults()
        },
        Duration::from_secs(5),
        None,
    );
    assert!(zero_concurrency.is_err(), "concurrency 0 is a runaway loop");

    let zero_stall = LoopConfig::new(
        WorkerBudgets {
            stall_timeout: Duration::ZERO,
            ..WorkerBudgets::bootstrap_defaults()
        },
        Duration::from_secs(5),
        None,
    );
    assert!(
        zero_stall.is_err(),
        "stall 0 kills healthy workers instantly"
    );

    let zero_shutdown = LoopConfig::new(WorkerBudgets::bootstrap_defaults(), Duration::ZERO, None);
    assert!(zero_shutdown.is_err(), "shutdown 0 is an unkillable loop");

    let negative_spend = LoopConfig::new(
        WorkerBudgets {
            daily_spend_soft_cap_usd: -1.0,
            ..WorkerBudgets::bootstrap_defaults()
        },
        Duration::from_secs(5),
        None,
    );
    assert!(negative_spend.is_err(), "negative spend cap is no cap");

    let zero_deadline = LoopConfig::new(
        WorkerBudgets {
            run_budget: Duration::ZERO,
            worker_timeout: Duration::ZERO,
            ..WorkerBudgets::bootstrap_defaults()
        },
        Duration::from_secs(5),
        None,
    );
    assert!(
        zero_deadline.is_err(),
        "zero run deadline never bounds work"
    );
}

/// Checks that shutdown aborts hung workers within the five-second drain budget.
#[tokio::test(start_paused = true)]
async fn shutdown_stops_cleanly_within_the_daemon_budget() {
    let starter = FakeStarter::new(Behavior::BeatEvery(Duration::from_mins(1)));
    let budgets = WorkerBudgets {
        worker_timeout: Duration::from_hours(2),
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), None).unwrap();
    // Synthetic SIGTERM 2s in; grace window 5s; abort at 7s.
    let (signal_tx, signal_rx) = tokio::sync::watch::channel(0_u64);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ignore = signal_tx.send(1);
    });
    let (summary, _decisions, runs) =
        run_loop(config, snapshot_with(&[1]), starter, "inv", signal_rx).await;

    assert_eq!(summary.stop_reason, StopReason::Shutdown);
    assert_eq!(summary.aborted_on_stop, 1);
    assert!(
        summary.elapsed_secs <= 12,
        "clean stop must stay inside the 5s daemon budget (+ tick slop)"
    );
    let rows = runs.runs_for_invocation("inv").unwrap();
    assert_eq!(
        rows[0].status,
        orchestraitor_campaign::RunRowStatus::AbortedShutdown
    );
    assert_eq!(rows[0].detail, "shutdown-abort");
}

/// Checks that a worker panic records a failure while subsequent passes continue.
#[tokio::test(start_paused = true)]
async fn panicked_worker_is_a_typed_failure_and_the_loop_continues() {
    let starter = FakeStarter::new(Behavior::Panic);
    let config = LoopConfig::new(
        WorkerBudgets::bootstrap_defaults(),
        Duration::from_secs(5),
        Some(3),
    )
    .unwrap();
    let (summary, _decisions, runs) =
        run_loop(config, snapshot_with(&[1]), starter, "inv", never()).await;

    assert_eq!(summary.failed, 1);
    assert_eq!(summary.stalled, 0, "a panic is never a stall");
    let rows = runs.runs_for_invocation("inv").unwrap();
    assert_eq!(rows[0].status, orchestraitor_campaign::RunRowStatus::Failed);
    assert_eq!(rows[0].detail, "worker-panicked");
    // The loop continued: two more passes ran (no-op after exclusion).
    assert_eq!(summary.cycles, 3);
}

// Q5(b) race arbitration: a run that completes INSIDE the stall window is
// naturally completed — the kill never fires (Ok(run) wins; abort-after-
// completion would be a no-op anyway, but here no abort is even declared).
/// Checks that completion before the stall deadline retains its completed status.
#[tokio::test(start_paused = true)]
async fn natural_completion_inside_the_stall_window_wins_the_arbitration() {
    let starter = FakeStarter::new(Behavior::CompleteAfter(Duration::from_millis(
        9 * 60_000 + 59_500,
    )));
    // The bound must outlive the completion: no-op passes run at backoff
    // pace during the wait (0, 1, 11, 31, 71, 151, 311, 611 seconds), so a
    // tight bound would drain-abort the in-flight worker instead.
    let config = LoopConfig::new(
        WorkerBudgets::bootstrap_defaults(),
        Duration::from_secs(5),
        Some(8),
    )
    .unwrap();
    let (summary, _decisions, runs) =
        run_loop(config, snapshot_with(&[1]), starter, "inv", never()).await;

    assert_eq!(summary.completed, 1);
    assert_eq!(
        summary.stalled, 0,
        "an in-window completion is never killed"
    );
    let rows = runs.runs_for_invocation("inv").unwrap();
    assert_eq!(
        rows[0].status,
        orchestraitor_campaign::RunRowStatus::Completed
    );
    let span = rows[0].finished_at_secs.unwrap() - rows[0].started_at_secs;
    assert!(span >= 9 * 60 + 59, "the row reflects the fake's own delay");
}

// Q5(c) subscribe race: a beat that lands before the supervisor's first
// tick still counts as liveness — the stall clock runs from the observed
// beat, never from a silent zero baseline.
/// Checks that a beat observed at the first tick extends the stall deadline.
#[tokio::test(start_paused = true)]
async fn an_early_beat_before_the_first_tick_resets_the_stall_clock() {
    // Beat at 0s (before any tick can observe it fresh), then park. The
    // kill must fire at >= 10m from the OBSERVED beat, i.e. the span stays
    // below 11m despite the pre-tick beat.
    let starter = FakeStarter::new(Behavior::BeatThenPark {
        beats: 1,
        interval: Duration::from_secs(0),
    });
    let config = LoopConfig::new(
        WorkerBudgets::bootstrap_defaults(),
        Duration::from_secs(5),
        Some(10),
    )
    .unwrap();
    let (summary, _decisions, runs) =
        run_loop(config, snapshot_with(&[1]), starter, "inv", never()).await;

    assert_eq!(summary.stalled, 1);
    let rows = runs.runs_for_invocation("inv").unwrap();
    let span = rows[0].finished_at_secs.unwrap() - rows[0].started_at_secs;
    assert!(span >= 10 * 60, "the stall window is honored in full");
    assert!(
        span <= 11 * 60,
        "the early beat resets the clock (no double-count)"
    );
}

// Q5(k) interplay: the spend cap trips while a long run is still in
// flight — the intake is sealed and the in-flight run drains NATURALLY
// (no abort), then the loop exits. The cap crosses at run 1's finish
// (pre-seeded $5 + run 1's $6); run 2 is the busy slot.
/// Checks that reaching the spend cap lets a busy worker complete naturally.
#[tokio::test(start_paused = true)]
async fn spend_cap_with_a_busy_slot_drains_without_aborting() {
    let decisions = CampaignDecisionStore::open_in_memory().unwrap();
    let runs = orchestraitor_campaign::LoopRunStore::open_in_memory().unwrap();
    // Prior spend today from an earlier invocation: $5.
    let prior = runs
        .start(&orchestraitor_campaign::StartRun {
            invocation_id: "inv-dead".to_string(),
            decision_id: 1,
            task_id: "prior-task".to_string(),
            repo: REPO.to_string(),
            number: 0,
            started_at_secs: START_UNIX,
        })
        .unwrap();
    runs.finish(
        prior.id,
        orchestraitor_campaign::RunRowStatus::Completed,
        START_UNIX + 1,
        5.0,
        "prior",
    )
    .unwrap();

    let starter = FakeStarter::with_behaviors(vec![
        // Run 1 is the slow busy slot (in flight when the cap trips); run 2
        // is fast and crosses the cap at its finish ($5 prior + $6).
        Behavior::CompleteAfter(Duration::from_millis(59_500)),
        Behavior::Complete {
            turns: 1,
            tokens: 3_000_000,
        },
    ]);
    let budgets = WorkerBudgets {
        usd_per_token_estimate: 2e-6,
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), None).unwrap();
    let runner = LoopRunner::new(
        config,
        FakePoller {
            snapshot: snapshot_with(&[1, 2]),
        },
        starter,
        &decisions,
        &runs,
        routing(),
        "inv".to_string(),
        START_UNIX,
    )
    .unwrap();
    let summary = runner.run(never()).await.unwrap();

    assert_eq!(summary.stop_reason, StopReason::SpendSoftCap);
    assert_eq!(summary.spawns, 2);
    assert_eq!(summary.completed, 2, "the busy slot drains to completion");
    assert_eq!(summary.aborted_on_stop, 0, "the soft cap never aborts");
    assert!(
        runs.runs_for_invocation("inv")
            .unwrap()
            .iter()
            .all(|row| row.status.is_terminal())
    );
}

// Shutdown preempts the spend cap's natural drain: without a signal the
// busy slot drains to completion indefinitely; a signal during that drain
// bounds it to the shutdown budget, kills the straggler, records it as a
// shutdown-abort (the drain reason converged to Shutdown), and keeps the
// summary's stop reason as the budget cause.
/// Checks that shutdown bounds a spend-cap drain and preserves completed rows.
#[tokio::test(start_paused = true)]
async fn shutdown_preempts_the_spend_cap_drain_within_the_budget() {
    let decisions = CampaignDecisionStore::open_in_memory().unwrap();
    let runs = orchestraitor_campaign::LoopRunStore::open_in_memory().unwrap();
    // Prior spend today from an earlier invocation: $5 — the cap crosses at
    // run 1's finish, while run 2 (the busy slot) is still in flight.
    let prior = runs
        .start(&orchestraitor_campaign::StartRun {
            invocation_id: "inv-dead".to_string(),
            decision_id: 1,
            task_id: "prior-task".to_string(),
            repo: REPO.to_string(),
            number: 0,
            started_at_secs: START_UNIX,
        })
        .unwrap();
    runs.finish(
        prior.id,
        orchestraitor_campaign::RunRowStatus::Completed,
        START_UNIX + 1,
        5.0,
        "prior",
    )
    .unwrap();

    let starter = FakeStarter::with_behaviors(vec![
        // Run 1 is the busy slot: it beats forever, so without the shutdown
        // only its 45m worker timeout would end the natural drain.
        Behavior::BeatEvery(Duration::from_mins(1)),
        // Run 2 is fast and crosses the cap at its finish ($5 prior + $6)
        // while run 1 is still in flight.
        Behavior::Complete {
            turns: 1,
            tokens: 3_000_000,
        },
    ]);
    let budgets = WorkerBudgets {
        usd_per_token_estimate: 2e-6,
        worker_timeout: Duration::from_hours(2),
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), None).unwrap();
    // Synthetic SIGTERM 2s after the cap trips (the cap trips at ~0s in
    // this scenario — run 1 completes on the first pass).
    let (signal_tx, signal_rx) = tokio::sync::watch::channel(0_u64);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ignore = signal_tx.send(1);
    });
    let runner = LoopRunner::new(
        config,
        FakePoller {
            snapshot: snapshot_with(&[1, 2]),
        },
        starter,
        &decisions,
        &runs,
        routing(),
        "inv".to_string(),
        START_UNIX,
    )
    .unwrap();
    let summary = runner.run(signal_rx).await.unwrap();

    // The summary keeps the budget cause; the drain reason converged.
    assert_eq!(summary.stop_reason, StopReason::SpendSoftCap);
    assert_eq!(
        summary.aborted_on_stop, 1,
        "the shutdown killed the straggler"
    );
    // Cap tripped ~0s + signal at 2s + 5s grace + tick slop; WITHOUT the
    // preemption this would be 2 hours (the busy slot's worker timeout).
    assert!(
        summary.elapsed_secs <= 12,
        "shutdown must bound the spend-cap drain to the daemon budget: {}",
        summary.elapsed_secs
    );
    let rows = runs.runs_for_invocation("inv").unwrap();
    let aborted = rows
        .iter()
        .find(|row| row.status == orchestraitor_campaign::RunRowStatus::AbortedShutdown)
        .expect("the straggler is recorded");
    assert_eq!(aborted.detail, "shutdown-abort");
    // The completed run 1 is untouched.
    assert_eq!(
        rows.iter()
            .filter(|row| row.status == orchestraitor_campaign::RunRowStatus::Completed)
            .count(),
        1
    );
}

// Q5(m) poll race: a signal arriving while the board poll is in flight
// must interrupt the poll (the board client can block up to its 60s total
// timeout — far outside the 5s daemon budget) and end the loop promptly,
// never swallow the signal.
//
/// A poller whose poll never resolves (the hung-transport case).
struct HangingPoller;

#[async_trait]
impl BoardPoller for HangingPoller {
    /// Models a board request that never completes.
    async fn poll(&self) -> Result<BoardSnapshot, CampaignError> {
        std::future::pending().await
    }
}

/// Serves the snapshot once (the spawn pass), then hangs (the wedged
/// transport on the next cycle).
struct FirstPollSnapshot {
    snapshot: BoardSnapshot,
    polls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl BoardPoller for FirstPollSnapshot {
    /// Returns the initial snapshot once, then models a hung board request.
    async fn poll(&self) -> Result<BoardSnapshot, CampaignError> {
        if self.polls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(self.snapshot.clone())
        } else {
            std::future::pending().await
        }
    }
}

/// Checks that shutdown cancels a hung board poll within the daemon budget.
#[tokio::test(start_paused = true)]
async fn shutdown_interrupts_an_in_flight_board_poll() {
    let decisions = CampaignDecisionStore::open_in_memory().unwrap();
    let runs = orchestraitor_campaign::LoopRunStore::open_in_memory().unwrap();
    let config = LoopConfig::new(
        WorkerBudgets::bootstrap_defaults(),
        Duration::from_secs(5),
        None,
    )
    .unwrap();
    // Synthetic SIGTERM 2s in, while the poll of cycle 1 is still hung.
    let (signal_tx, signal_rx) = tokio::sync::watch::channel(0_u64);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ignore = signal_tx.send(1);
    });
    let runner = LoopRunner::new(
        config,
        HangingPoller,
        FakeStarter::new(Behavior::Complete {
            turns: 1,
            tokens: 0,
        }),
        &decisions,
        &runs,
        routing(),
        "inv".to_string(),
        START_UNIX,
    )
    .unwrap();
    let summary = runner.run(signal_rx).await.unwrap();

    assert_eq!(summary.stop_reason, StopReason::Shutdown);
    // Signal at 2s; a swallowed signal would hang the loop inside the poll
    // (60s board timeout) or wait it out — well past 5s.
    assert!(
        summary.elapsed_secs <= 5,
        "the in-flight poll must be interrupted by the shutdown: {}",
        summary.elapsed_secs
    );
    assert_eq!(summary.spawns, 0, "no spawn from the interrupted pass");
    assert!(runs.runs_for_invocation("inv").unwrap().is_empty());
}

// Q5(m2) poll race WITH an in-flight worker: the pass's interrupt arm
// consumes the send through the clone, but the MAIN receiver still has it
// pending — the run loop must advance its seen version, or wait_tick
// misreads the first signal as a second one and collapses the 5s grace to
// one tick. Asserts the full grace survives (straggler aborted no earlier
// than signal + budget) and still lands inside the budget.
/// Checks that cancelling a board poll still gives active workers their drain grace.
#[tokio::test(start_paused = true)]
async fn shutdown_during_a_hung_poll_keeps_the_grace_for_in_flight_work() {
    let decisions = CampaignDecisionStore::open_in_memory().unwrap();
    let runs = orchestraitor_campaign::LoopRunStore::open_in_memory().unwrap();
    let budgets = WorkerBudgets {
        worker_timeout: Duration::from_hours(2),
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), None).unwrap();

    let starter = FakeStarter::new(Behavior::BeatEvery(Duration::from_mins(1)));
    // Cycle 1's poll returns a task (the worker spawns); cycle 2's poll
    // hangs — the signal lands mid-hang with the run still in flight.
    let poller = FirstPollSnapshot {
        snapshot: snapshot_with(&[1]),
        polls: std::sync::atomic::AtomicUsize::new(0),
    };

    let (signal_tx, signal_rx) = tokio::sync::watch::channel(0_u64);
    // The first pass completes fast; the signal fires during cycle 2's
    // hang. Startup + first pass are sub-second; 2s is safely inside it.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ignore = signal_tx.send(1);
    });
    let runner = LoopRunner::new(
        config,
        poller,
        starter,
        &decisions,
        &runs,
        routing(),
        "inv".to_string(),
        START_UNIX,
    )
    .unwrap();
    let summary = runner.run(signal_rx).await.unwrap();

    assert_eq!(summary.stop_reason, StopReason::Shutdown);
    assert_eq!(summary.spawns, 1, "the in-flight worker existed");
    // The grace must NOT collapse to a tick (that abort would land ~3s);
    // the straggler is aborted at signal + budget + tick slop.
    assert!(
        summary.elapsed_secs >= 6,
        "the advertised grace must survive a pass-consumed signal: {}",
        summary.elapsed_secs
    );
    assert!(
        summary.elapsed_secs <= 12,
        "the straggler still aborts within the budget: {}",
        summary.elapsed_secs
    );
    assert_eq!(summary.aborted_on_stop, 1);
    let rows = runs.runs_for_invocation("inv").unwrap();
    assert_eq!(rows[0].detail, "shutdown-abort");
}

// Two signals: the second short-circuits the remaining grace window. The
// loop must abort at the SECOND signal (2s + 1s), not at the full grace
// budget (2s + 5s) — the docs and CHANGELOG advertise this short-circuit.
/// Checks that a second shutdown signal aborts workers before grace expires.
#[tokio::test(start_paused = true)]
async fn a_second_signal_short_circuits_the_remaining_grace() {
    let starter = FakeStarter::new(Behavior::BeatEvery(Duration::from_mins(1)));
    let budgets = WorkerBudgets {
        worker_timeout: Duration::from_hours(2),
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), None).unwrap();
    // First signal at 2s starts the 5s grace; the second at 3s must end it.
    let (signal_tx, signal_rx) = tokio::sync::watch::channel(0_u64);
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ignore = signal_tx.send(1);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let _ignore = signal_tx.send(2);
    });
    let (summary, _decisions, runs) =
        run_loop(config, snapshot_with(&[1]), starter, "inv", signal_rx).await;

    assert_eq!(summary.stop_reason, StopReason::Shutdown);
    assert_eq!(summary.aborted_on_stop, 1);
    // Short-circuit aborts at ~3s (+ tick slop); the full grace would end
    // no sooner than 7s.
    assert!(
        summary.elapsed_secs < 7,
        "the second signal must skip the remaining grace: {}",
        summary.elapsed_secs
    );
    let rows = runs.runs_for_invocation("inv").unwrap();
    assert_eq!(
        rows[0].status,
        orchestraitor_campaign::RunRowStatus::AbortedShutdown
    );
    assert_eq!(rows[0].detail, "shutdown-abort");
}
