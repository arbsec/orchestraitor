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

fn routing() -> RoleRoutingDecision {
    RoleRoutingDecision {
        role: "implement".to_string(),
        provider: "neuralwatt".to_string(),
        model: "glm-5.2".to_string(),
        precedence_path: "test".to_string(),
        fallback_reason: None,
    }
}

fn ready_item(number: u64) -> ReadyItem {
    ReadyItem {
        number,
        title: format!("fixture task {number}"),
        url: format!("https://github.com/{REPO}/issues/{number}"),
        repo: REPO.to_string(),
        item_id: format!("item-{number}"),
    }
}

fn empty_snapshot() -> BoardSnapshot {
    BoardSnapshot {
        open: Vec::new(),
        ready: Vec::new(),
        blocked_candidates: Vec::new(),
        warnings: Vec::new(),
    }
}

fn snapshot_with(numbers: &[u64]) -> BoardSnapshot {
    BoardSnapshot {
        ready: numbers.iter().copied().map(ready_item).collect(),
        ..empty_snapshot()
    }
}

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
    behavior: Behavior,
    /// Captured `(task_id, prior_daily_spend_usd)` per spawn — the spend
    /// feed-through surface. Shared through an `Arc` so the test can read
    /// it after the runner consumes a clone.
    spawns: Arc<std::sync::Mutex<Vec<(String, f64)>>>,
    /// Increments once per beat — the forbidden-effect counter: after a
    /// kill, it must never advance again.
    beats_observed: Arc<AtomicUsize>,
}

impl FakeStarter {
    fn new(behavior: Behavior) -> Self {
        Self {
            behavior,
            spawns: Arc::new(std::sync::Mutex::new(Vec::new())),
            beats_observed: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn spend_feed(&self) -> Vec<(String, f64)> {
        self.spawns.lock().unwrap().clone()
    }
}

#[async_trait]
impl LoopWorkerStarter for FakeStarter {
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
        let behavior = self.behavior.clone();
        let id = task_id.to_string();
        let run = tokio::spawn(async move {
            match behavior {
                Behavior::Complete { turns, tokens } => fixture_run(&id, turns, tokens),
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
    async fn poll(&self) -> Result<BoardSnapshot, CampaignError> {
        Ok(self.snapshot.clone())
    }
}

async fn run_loop(
    config: LoopConfig,
    snapshot: BoardSnapshot,
    starter: FakeStarter,
    invocation: &str,
    shutdown: impl std::future::Future<Output = ()> + Send + Unpin,
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

fn never() -> impl std::future::Future<Output = ()> + Send + Unpin {
    std::future::pending()
}

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
    assert!((feed[1].1 - 6.0).abs() < 1e-9, "prior spend is fed through");
}

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

#[tokio::test(start_paused = true)]
async fn shutdown_stops_cleanly_within_the_daemon_budget() {
    let starter = FakeStarter::new(Behavior::BeatEvery(Duration::from_mins(1)));
    let budgets = WorkerBudgets {
        worker_timeout: Duration::from_hours(2),
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), None).unwrap();
    // Synthetic SIGTERM 2s in; grace window 5s; abort at 7s.
    let sigterm = Box::pin(tokio::time::sleep(Duration::from_secs(2)));
    let (summary, _decisions, runs) =
        run_loop(config, snapshot_with(&[1]), starter, "inv", sigterm).await;

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

// Crash reconciliation at the runner level: with the concurrency cap at 1,
// an unreconciled stale `running` row would consume the only slot and
// block the respawn — so a successful spawn PROVES the sweep ran.
#[tokio::test(start_paused = true)]
async fn stale_running_rows_sweep_and_the_task_reruns() {
    let decisions = CampaignDecisionStore::open_in_memory().unwrap();
    let runs = orchestraitor_campaign::LoopRunStore::open_in_memory().unwrap();
    // A previous invocation died with the task still marked running.
    runs.start(&orchestraitor_campaign::StartRun {
        invocation_id: "inv-dead".to_string(),
        decision_id: 1,
        task_id: "board-arbsec_orchestraitor-1".to_string(),
        repo: REPO.to_string(),
        number: 1,
        started_at_secs: START_UNIX,
    })
    .unwrap();

    let starter = FakeStarter::new(Behavior::Complete {
        turns: 1,
        tokens: 0,
    });
    let budgets = WorkerBudgets {
        max_concurrent_workers: 1,
        ..WorkerBudgets::bootstrap_defaults()
    };
    let config = LoopConfig::new(budgets, Duration::from_secs(5), Some(2)).unwrap();
    let runner = LoopRunner::new(
        config,
        FakePoller {
            snapshot: snapshot_with(&[1]),
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

    assert_eq!(
        summary.spawns, 1,
        "the task reruns: the stale row was swept, freeing the single slot"
    );
    assert!(summary.events.iter().any(|event| {
        matches!(
            event,
            orchestraitor_campaign::LoopEvent::WorkerSpawned { .. }
        )
    }));
}
