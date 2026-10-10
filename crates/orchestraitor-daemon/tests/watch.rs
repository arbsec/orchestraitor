//! `orcd watch` integration tests (issue #503, spec §9.24.2, §9.36,
//! §21.4): crash-safe restart recovery, single-flight lock ownership, and
//! the cadence/config contract. The adversarial paths assert the forbidden
//! effect did NOT happen — never merely that an error occurred.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
// Test-only allowances mirror the cli/daemon integration tests: a failed
// assertion must fail the test loudly.

use std::sync::Arc;
use std::time::Duration;

use orchestraitor_campaign::{
    BoardPoller, LoopConfig, LoopRunStore, RunRow, RunRowStatus, StartRun,
};
use orchestraitor_daemon::{WatchConfig, acquire_instance_lock};

const START_UNIX: u64 = 1_000_000;

/// A real on-disk store for crash-recovery tests.
fn store(dir: &std::path::Path) -> LoopRunStore {
    LoopRunStore::open(&dir.join("loop.db")).expect("store open")
}

/// Inserts a `running` row the way a killed supervisor would leave it.
fn seed_running(runs: &LoopRunStore, task_id: &str, number: u64) -> RunRow {
    runs.start(&StartRun {
        invocation_id: "watch-crashed-invocation".to_string(),
        decision_id: 1,
        task_id: task_id.to_string(),
        repo: "arbsec/orchestraitor".to_string(),
        number,
        started_at_secs: START_UNIX,
    })
    .expect("row insert")
}

/// §9.24.2 crash recovery, adversarial (§21.4): after a kill -9 mid-run,
/// the restarted daemon's recovery pass transitions the in-flight row to
/// `orphaned`. The FORBIDDEN effect asserted against: a `failed` row (a
/// lease expiry or crash must never directly fail a run — the operator
/// gets an extendable orphan, not a closed failure), and a row still
/// `running` after recovery (a stranded liveness claim).
#[test]
fn kill9_restart_transitions_running_to_orphaned_never_failed()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let runs = store(dir.path());
    let row = seed_running(&runs, "board-arbsec_orchestraitor-42", 42);
    drop(runs);

    // --- the crash boundary: the process dies; the file persists. The
    // restarted daemon opens the store fresh and runs recovery.
    let runs = store(dir.path());
    let recovered = runs.recover_running_rows(START_UNIX + 60)?;

    assert_eq!(recovered.len(), 1, "exactly the crashed row recovered");
    assert_eq!(recovered[0].id, row.id);
    assert_eq!(
        recovered[0].status,
        RunRowStatus::Orphaned,
        "the crashed run is orphaned"
    );

    // Forbidden effect #1: the row must NOT read back `failed`.
    let durable = runs.by_id(row.id)?;
    assert_ne!(
        durable.status,
        RunRowStatus::Failed,
        "a crash-recovered row must never be failed directly (§9.24.2, §9.36)"
    );
    // Forbidden effect #2: no row may still claim `running` after the
    // recovery pass — a stranded liveness claim would let a later
    // invocation skip the work as if someone else held it.
    let still_running = runs.running_rows()?;
    assert!(
        still_running.is_empty(),
        "no row survives recovery still running, got {still_running:?}"
    );
    Ok(())
}

/// §9.24.2: a `paused` row survives the kill -9/restart cycle exactly
/// where it is — recovery never touches it, so the paused work is neither
/// orphaned nor resumed behind the operator's back.
#[test]
fn kill9_restart_keeps_paused_rows_paused() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let runs = store(dir.path());
    let paused_row = seed_running(&runs, "board-arbsec_orchestraitor-43", 43);
    // Seed the paused state the way the future pause control will: the
    // same schema column, the sanctioned non-terminal spelling.
    {
        let conn = rusqlite::Connection::open(dir.path().join("loop.db"))?;
        conn.execute(
            "UPDATE loop_worker_runs SET status = 'paused' WHERE id = ?1",
            [paused_row.id],
        )?;
    }
    drop(runs);

    // --- restart boundary.
    let runs = store(dir.path());
    let recovered = runs.recover_running_rows(START_UNIX + 60)?;

    assert!(
        recovered.is_empty(),
        "the paused row must not be recovered, got {recovered:?}"
    );
    let durable = runs.by_id(paused_row.id)?;
    assert_eq!(
        durable.status,
        RunRowStatus::Paused,
        "paused stays paused across a restart (§9.24.2)"
    );
    assert!(
        durable.finished_at_secs.is_none(),
        "a paused row is not terminal — the operator's pause survives"
    );
    Ok(())
}

/// §9.36 single-flight: a second daemon instance is refused with the
/// typed rejection — the daemon takes over the loop's instance lock, so
/// the foreground loop and the daemon (and a second daemon) can never run
/// concurrently on one config dir. Forbidden effect asserted: the second
/// acquirer must NOT receive a lock, and the first holder must still hold
/// a usable one.
#[test]
fn a_second_watch_instance_is_refused_the_instance_lock() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempfile::tempdir()?;
    let config_dir = dir.path().join("config");

    // First daemon takes the lock (the loop's lock file).
    let first = acquire_instance_lock(&config_dir)?;
    let lock_path = config_dir.join("loop.lock");
    assert!(lock_path.exists(), "the lock file is the loop's loop.lock");

    // A second daemon (or a foreground `orc loop`) is refused.
    let second = acquire_instance_lock(&config_dir);
    assert!(
        second.is_err(),
        "the second instance must be refused, not granted a parallel lock"
    );
    let error = second.unwrap_err().to_string();
    assert!(
        error.contains("already holds the lock"),
        "the typed single-flight rejection names the holder, got: {error}"
    );

    // The first holder is unaffected: its lock file still exists and the
    // handle is live.
    assert!(lock_path.exists(), "the first holder keeps the lock");
    drop(first);

    // After the holder exits (drop), the OS releases the lock: a fresh
    // daemon can take over — a crashed daemon can never wedge the next.
    let third = acquire_instance_lock(&config_dir);
    assert!(
        third.is_ok(),
        "a released lock must be acquirable (crash-safe single-flight)"
    );
    Ok(())
}

/// §9.22 layered config: the poll cadence resolves `watch.poll_interval_secs`
/// through the layered resolver; an absent key falls back to the
/// documented 60s default and a zero value is rejected fail-closed.
#[test]
fn watch_cadence_resolves_from_layers_and_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    use orchestraitor_core::config::{ConfigLayer, ConfigResolver};

    // No configuration: the documented default.
    let resolver = ConfigResolver::new();
    let config = WatchConfig::from_layers(&resolver)?;
    assert_eq!(
        config.poll_interval_secs,
        orchestraitor_daemon::DEFAULT_POLL_INTERVAL_SECS,
        "the absent key resolves to the documented default cadence"
    );

    // A configured cadence resolves through the project layer.
    let resolver = ConfigResolver::new().with_toml(
        orchestraitor_core::config::ConfigSource {
            layer: ConfigLayer::Project,
            name: "test-project".to_string(),
        },
        "[watch]\npoll_interval_secs = 300\n",
    )?;
    let config = WatchConfig::from_layers(&resolver)?;
    assert_eq!(config.poll_interval_secs, 300);

    // A zero cadence is a runaway poll loop: rejected fail-closed.
    let zero = WatchConfig::new(0);
    assert!(zero.is_err(), "a zero cadence must be rejected");
    Ok(())
}

/// §9.36: the watch cycle's `LoopConfig` wires the §9.22 cadence into the
/// loop runner's poll gate — the runner keeps its guard set and the
/// cadence only spaces the polls.
#[test]
fn the_cadence_wires_into_the_loop_config_guard_set() {
    let budgets = orchestraitor_worker::WorkerBudgets::bootstrap_defaults();
    let config = LoopConfig::with_cadence(
        budgets.clone(),
        Duration::from_secs(5),
        None,
        Some(Duration::from_mins(1)),
    )
    .expect("valid cadence config");
    assert_eq!(
        config.min_poll_interval,
        Some(Duration::from_mins(1)),
        "the cadence rides the runner's poll gate"
    );
    // The pinned guard set is untouched by the cadence wiring.
    assert_eq!(
        config.budgets.max_concurrent_workers,
        budgets.max_concurrent_workers
    );
    assert_eq!(config.budgets.stall_timeout, budgets.stall_timeout);
}

/// A scripted poller: returns the queued snapshots in order (then repeats the
/// last one), simulating board evolution across ticks.
struct ScriptedPoller {
    snapshots: std::sync::Mutex<Vec<orchestraitor_campaign::BoardSnapshot>>,
}

#[async_trait::async_trait]
impl orchestraitor_campaign::BoardPoller for ScriptedPoller {
    async fn poll(
        &self,
    ) -> Result<orchestraitor_campaign::BoardSnapshot, orchestraitor_campaign::CampaignError> {
        let mut queue = self.snapshots.lock().expect("queue lock");
        if queue.len() > 1 {
            Ok(queue.remove(0))
        } else {
            Ok(queue[0].clone())
        }
    }
}

/// A capture sink recording every reconcile outcome.
#[derive(Default)]
struct CaptureSink {
    outcomes: std::sync::Mutex<Vec<orchestraitor_campaign::ReconcileOutcome>>,
}

impl orchestraitor_daemon::ReconcileSink for CaptureSink {
    fn record(&self, outcome: &orchestraitor_campaign::ReconcileOutcome) {
        self.outcomes
            .lock()
            .expect("sink lock")
            .push(outcome.clone());
    }
}

fn ready_item(number: u64) -> orchestraitor_board::ReadyItem {
    orchestraitor_board::ReadyItem {
        number,
        title: format!("fixture task {number}"),
        url: format!("https://github.com/arbsec/orchestraitor/issues/{number}"),
        repo: "arbsec/orchestraitor".to_string(),
        item_id: format!("item-{number}"),
    }
}

/// §9.40 promotion + §9.43 dedup, behavior-level: across three ticks the
/// poller (1) records `unblocked-task-promoted` when a previously blocked
/// candidate reaches the ready queue, and (2) records `board-diverged` for a
/// vanished running task ONCE — not once per tick. Forbidden effects asserted
/// absent: a missing promotion event, and duplicate divergence envelopes.
#[tokio::test(flavor = "current_thread")]
async fn poller_promotes_unblocked_tasks_and_deduplicates_divergences()
-> Result<(), Box<dyn std::error::Error>> {
    use orchestraitor_campaign::{BoardSnapshot, LoopRunStore, ReconcileEvent, StartRun};
    use orchestraitor_daemon::ReconcilePoller;

    let dir = tempfile::tempdir()?;
    let runs = LoopRunStore::open(&dir.path().join("loop.db"))?;
    runs.start(&StartRun {
        invocation_id: "watch-poller-test".to_string(),
        decision_id: 1,
        task_id: "board-arbsec_orchestraitor-42".to_string(),
        repo: "arbsec/orchestraitor".to_string(),
        number: 42,
        started_at_secs: START_UNIX,
    })?;
    // Task 42: running locally but absent from the board's open set on every
    // tick (the divergence). Task 43: blocked on tick 1, promoted on tick 2.

    let tick1 = BoardSnapshot {
        open: Vec::new(),
        ready: Vec::new(),
        blocked_candidates: vec![ready_item(43)],
        warnings: Vec::new(),
    };
    let tick2 = BoardSnapshot {
        open: Vec::new(),
        ready: vec![ready_item(43)],
        blocked_candidates: Vec::new(),
        warnings: Vec::new(),
    };
    let tick3 = tick2.clone();

    let sink = Arc::new(CaptureSink::default());
    let events_store =
        orchestraitor_events::SqliteAuditStore::open(dir.path().join("watch-events.db"))?;
    let poller = ReconcilePoller::new(
        ScriptedPoller {
            snapshots: std::sync::Mutex::new(vec![tick1, tick2, tick3]),
        },
        Arc::new(std::sync::Mutex::new(runs)),
        Arc::new(std::sync::Mutex::new(events_store)),
        Arc::clone(&sink) as Arc<dyn orchestraitor_daemon::ReconcileSink>,
    );

    // Three ticks: blocked → promoted → steady state.
    poller.poll().await?;
    poller.poll().await?;
    poller.poll().await?;

    let recorded = sink.outcomes.lock().expect("sink lock");
    let mut promotions = 0_usize;
    let mut divergences = Vec::new();
    for outcome in recorded.iter() {
        for event in &outcome.events {
            match event {
                ReconcileEvent::UnblockedTaskPromoted { task_id, .. } => {
                    promotions += 1;
                    assert_eq!(task_id, "board-arbsec_orchestraitor-43");
                }
                ReconcileEvent::BoardDiverged { task_id, .. } => {
                    divergences.push(task_id.clone());
                }
            }
        }
    }
    assert_eq!(
        promotions, 1,
        "exactly one promotion when the blocked candidate reaches the ready queue"
    );
    assert_eq!(
        divergences.len(),
        1,
        "the vanished running task records board-diverged ONCE, not once per tick \
         (forbidden effect: a duplicate envelope every cadence tick); got {divergences:?}"
    );
    Ok(())
}

/// A running row of the CURRENT invocation whose task leaves the board's
/// open set is NORMAL supervision (the worker moved/closed the item while
/// finishing), never a divergence — the runner's live slots are excluded
/// from the divergence scan. Forbidden effect asserted absent: a
/// `board-diverged` record for ordinary in-flight work.
#[tokio::test(flavor = "current_thread")]
async fn live_slots_of_the_current_invocation_are_not_divergences()
-> Result<(), Box<dyn std::error::Error>> {
    use orchestraitor_campaign::{BoardSnapshot, LoopRunStore, ReconcileEvent, StartRun};
    use orchestraitor_daemon::ReconcilePoller;

    let dir = tempfile::tempdir()?;
    let runs = LoopRunStore::open(&dir.path().join("loop.db"))?;
    runs.start(&StartRun {
        invocation_id: "watch-current".to_string(),
        decision_id: 1,
        task_id: "board-arbsec_orchestraitor-42".to_string(),
        repo: "arbsec/orchestraitor".to_string(),
        number: 42,
        started_at_secs: START_UNIX,
    })?;

    // Every tick: task 42 (currently supervised) absent from the open set.
    let tick = BoardSnapshot {
        open: Vec::new(),
        ready: Vec::new(),
        blocked_candidates: Vec::new(),
        warnings: Vec::new(),
    };
    let sink = Arc::new(CaptureSink::default());
    let mut poller = ReconcilePoller::new(
        ScriptedPoller {
            snapshots: std::sync::Mutex::new(vec![tick.clone(), tick]),
        },
        Arc::new(std::sync::Mutex::new(runs)),
        Arc::new(std::sync::Mutex::new(
            orchestraitor_events::SqliteAuditStore::open(dir.path().join("watch-events.db"))?,
        )),
        Arc::clone(&sink) as Arc<dyn orchestraitor_daemon::ReconcileSink>,
    );
    poller.set_invocation("watch-current");

    poller.poll().await?;
    poller.poll().await?;

    let recorded = sink.outcomes.lock().expect("sink lock");
    let divergences: Vec<_> = recorded
        .iter()
        .flat_map(|outcome| outcome.events.iter())
        .filter_map(|event| match event {
            ReconcileEvent::BoardDiverged { task_id, .. } => Some(task_id.clone()),
            ReconcileEvent::UnblockedTaskPromoted { .. } => None,
        })
        .collect();
    assert!(
        divergences.is_empty(),
        "a live slot is normal supervision, never a divergence; got {divergences:?}"
    );
    Ok(())
}

/// §9.40 promotion fires even when the earlier tick produced NO reconcile
/// events (the common case). Forbidden effect asserted absent: a promotion
/// that only ever fires when some unrelated event happened first.
#[tokio::test(flavor = "current_thread")]
async fn promotion_fires_without_a_preceding_event() -> Result<(), Box<dyn std::error::Error>> {
    use orchestraitor_campaign::{BoardSnapshot, LoopRunStore, ReconcileEvent, StartRun};
    use orchestraitor_daemon::ReconcilePoller;

    let dir = tempfile::tempdir()?;
    let runs = LoopRunStore::open(&dir.path().join("loop.db"))?;
    runs.start(&StartRun {
        invocation_id: "watch-promo-test".to_string(),
        decision_id: 1,
        task_id: "board-arbsec_orchestraitor-99".to_string(),
        repo: "arbsec/orchestraitor".to_string(),
        number: 99,
        started_at_secs: START_UNIX,
    })?;
    // Task 99 stays on the board's open set (no divergence ever), while
    // task 43 goes blocked (tick 1) → ready (tick 2).
    let facts_99 = orchestraitor_board::ItemFacts {
        item_node_id: "item-99".to_string(),
        repo: "arbsec/orchestraitor".to_string(),
        number: 99,
        title: "fixture 99".to_string(),
        url: "https://github.com/arbsec/orchestraitor/issues/99".to_string(),
        issue_type: Some("Task".to_string()),
        priority: None,
        target: None,
        status: Some("In Progress".to_string()),
        labels: Vec::new(),
        open_blockers: 0,
    };
    let tick1 = BoardSnapshot {
        open: vec![facts_99.clone()],
        ready: Vec::new(),
        blocked_candidates: vec![ready_item(43)],
        warnings: Vec::new(),
    };
    let tick2 = BoardSnapshot {
        open: vec![facts_99],
        ready: vec![ready_item(43)],
        blocked_candidates: Vec::new(),
        warnings: Vec::new(),
    };

    let sink = Arc::new(CaptureSink::default());
    let mut poller = ReconcilePoller::new(
        ScriptedPoller {
            snapshots: std::sync::Mutex::new(vec![tick1, tick2]),
        },
        Arc::new(std::sync::Mutex::new(runs)),
        Arc::new(std::sync::Mutex::new(
            orchestraitor_events::SqliteAuditStore::open(dir.path().join("watch-events.db"))?,
        )),
        Arc::clone(&sink) as Arc<dyn orchestraitor_daemon::ReconcileSink>,
    );
    poller.set_invocation("watch-promo-test");

    poller.poll().await?;
    poller.poll().await?;

    let recorded = sink.outcomes.lock().expect("sink lock");
    let promotions: Vec<_> = recorded
        .iter()
        .flat_map(|outcome| outcome.events.iter())
        .filter_map(|event| match event {
            ReconcileEvent::UnblockedTaskPromoted { task_id, .. } => Some(task_id.clone()),
            ReconcileEvent::BoardDiverged { .. } => None,
        })
        .collect();
    assert_eq!(
        promotions,
        vec!["board-arbsec_orchestraitor-43"],
        "the promotion must fire on the quiet path (no divergence on the earlier tick)"
    );
    Ok(())
}
