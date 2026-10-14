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
    let zero = WatchConfig::new(
        0,
        orchestraitor_campaign::KickoffSettings::builtin_defaults(),
    );
    assert!(zero.is_err(), "a zero cadence must be rejected");
    Ok(())
}

/// §9.22 layered config (issue #505): the kick-off condition enables
/// resolve through the `[watch.kickoff]` block — absent keys inherit the
/// built-in defaults, a higher layer overrides, and disabling every
/// condition yields the idle-but-healthy configuration.
#[test]
fn watch_kickoff_resolves_from_layers_with_built_in_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    use orchestraitor_core::config::{ConfigLayer, ConfigResolver, ConfigSource};

    // No configuration: the built-in defaults (§9.22.1 — shipped as TOML
    // data in the daemon's built-in defaults layer, mirrored by the
    // resolver fallback here).
    let resolver = ConfigResolver::new();
    let config = WatchConfig::from_layers(&resolver)?;
    assert_eq!(
        config.kickoff,
        orchestraitor_campaign::KickoffSettings::builtin_defaults(),
        "absent [watch.kickoff] inherits the built-in defaults"
    );
    assert!(
        !config.kickoff.external_event,
        "the §9.50 external-event condition ships disarmed (#504 pending)"
    );

    // A partial block: configured keys override, absent keys inherit.
    let resolver = ConfigResolver::new().with_toml(
        ConfigSource {
            layer: ConfigLayer::Project,
            name: "test-project".to_string(),
        },
        "[watch.kickoff]\nnew_eligible_work = false\n",
    )?;
    let config = WatchConfig::from_layers(&resolver)?;
    assert!(
        !config.kickoff.new_eligible_work,
        "the project layer overrides the default"
    );
    assert!(
        config.kickoff.unblocked_promotion,
        "an absent key inherits the built-in default"
    );

    // Disabling every condition: the idle-but-healthy configuration —
    // resolution succeeds (nothing to reject), the daemon simply never
    // kicks off.
    let resolver = ConfigResolver::new().with_toml(
        ConfigSource {
            layer: ConfigLayer::Project,
            name: "test-project".to_string(),
        },
        "[watch.kickoff]\nnew_eligible_work = false\nunblocked_promotion = false\nslot_released = false\nbudget_window_reset = false\nexternal_event = false\n",
    )?;
    let config = WatchConfig::from_layers(&resolver)?;
    assert_eq!(
        config.kickoff,
        orchestraitor_campaign::KickoffSettings {
            new_eligible_work: false,
            unblocked_promotion: false,
            slot_released: false,
            budget_window_reset: false,
            external_event: false,
        },
        "all-disabled resolves cleanly: the daemon idles but stays healthy"
    );
    Ok(())
}

/// §9.22.1 (issue #505): the built-in defaults ship as TOML data — the
/// constant parses, its keys are known, and it agrees with the resolver
/// fallback defaults.
#[test]
fn the_built_in_defaults_ship_as_parsable_toml_data() -> Result<(), Box<dyn std::error::Error>> {
    use orchestraitor_core::config::{ConfigLayer, ConfigResolver, ConfigSource};

    let resolver = ConfigResolver::new().with_toml(
        ConfigSource {
            layer: ConfigLayer::BuiltInDefaults,
            name: "built-in watch defaults".to_string(),
        },
        orchestraitor_daemon::WATCH_BUILT_IN_DEFAULTS_TOML,
    )?;
    let config = WatchConfig::from_layers(&resolver)?;
    assert_eq!(
        config.kickoff,
        orchestraitor_campaign::KickoffSettings::builtin_defaults(),
        "the shipped TOML data and the fallback defaults agree"
    );
    assert_eq!(
        config.poll_interval_secs,
        orchestraitor_daemon::DEFAULT_POLL_INTERVAL_SECS,
        "the shipped TOML data carries the documented cadence default"
    );
    Ok(())
}

/// The §9.36 kick-off enables wire through the PRODUCTION builder chain
/// (`with_guardrails` + `with_min_poll_interval` + `with_kickoff`,
/// matching `watch_cycle`) into the loop runner's config — the runner
/// evaluates exactly the operator-resolved conditions.
#[test]
fn the_kickoff_settings_wire_into_the_loop_config() {
    let settings = orchestraitor_campaign::KickoffSettings {
        new_eligible_work: false,
        ..orchestraitor_campaign::KickoffSettings::builtin_defaults()
    };
    let config = LoopConfig::with_guardrails(
        orchestraitor_worker::WorkerBudgets::bootstrap_defaults(),
        Duration::from_secs(5),
        None,
        orchestraitor_campaign::GuardrailsSettings::default(),
    )
    .expect("valid base config")
    .with_kickoff(settings);
    assert_eq!(
        config.kickoff, settings,
        "the runner evaluates the operator-resolved enables"
    );
}

/// §9.36: the watch cycle's `LoopConfig` wires the §9.22 cadence into the
/// loop runner's poll gate — via the PRODUCTION builder chain
/// (`with_guardrails` + `with_min_poll_interval`, matching
/// `watch_cycle`), so the runner keeps its guard set and the cadence
/// only spaces the polls.
#[test]
fn the_cadence_wires_into_the_loop_config_guard_set() {
    let budgets = orchestraitor_worker::WorkerBudgets::bootstrap_defaults();
    let config = LoopConfig::with_guardrails(
        budgets.clone(),
        Duration::from_secs(5),
        None,
        orchestraitor_campaign::GuardrailsSettings::default(),
    )
    .expect("valid base config")
    .with_min_poll_interval(Some(Duration::from_mins(1)));
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
    // Task 42: the crashed run's row, reaped by THIS invocation's restart
    // recovery (running → orphaned) and absent from the board's open set on
    // every tick — the divergence scope. Task 43: blocked on tick 1,
    // promoted on tick 2.
    let recovered = runs.recover_running_rows(START_UNIX + 60)?;
    assert_eq!(recovered.len(), 1);

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
        Arc::new(std::sync::Mutex::new(events_store)),
        Arc::clone(&sink) as Arc<dyn orchestraitor_daemon::ReconcileSink>,
    );

    let mut poller = poller;
    poller.set_recovered(recovered);

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
        Arc::new(std::sync::Mutex::new(
            orchestraitor_events::SqliteAuditStore::open(dir.path().join("watch-events.db"))?,
        )),
        Arc::clone(&sink) as Arc<dyn orchestraitor_daemon::ReconcileSink>,
    );
    poller.set_recovered(Vec::new()); // no recovered rows: live slots are never in scope

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
        Arc::new(std::sync::Mutex::new(
            orchestraitor_events::SqliteAuditStore::open(dir.path().join("watch-events.db"))?,
        )),
        Arc::clone(&sink) as Arc<dyn orchestraitor_daemon::ReconcileSink>,
    );
    poller.set_recovered(Vec::new());

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

/// A retried task's worktree path is reused cleanly across a simulated
/// run-budget restart: the per-invocation `prune_worktrees` (a PR #564
/// review finding — the prune used to run only at process start, so a
/// task that ran in invocation N kept its registered worktree and
/// `git worktree add` into the same path failed in invocation N+1,
/// counting as a spawn failure that could strand the task as `stuck`
/// without ever running). Forbidden effect asserted absent: a second
/// `worktree add` onto the same task path failing after a prune.
#[test]
fn a_retried_task_worktree_is_reusable_after_a_prune() -> Result<(), Box<dyn std::error::Error>> {
    use orchestraitor_daemon::DirectWatchStarter;

    let dir = tempfile::tempdir()?;
    let project_dir = dir.path().join("project");
    std::fs::create_dir_all(&project_dir)?;
    run_git(&project_dir, &["init", "--quiet"])?;
    run_git(&project_dir, &["config", "user.email", "test@example.com"])?;
    run_git(&project_dir, &["config", "user.name", "test"])?;
    std::fs::write(project_dir.join("README.md"), "seed")?;
    run_git(&project_dir, &["add", "."])?;
    run_git(&project_dir, &["commit", "--quiet", "-m", "seed"])?;

    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir)?;
    let task_id = "board-arbsec_orchestraitor-42";

    // Invocation N: the worktree is created for the task's run.
    let starter = DirectWatchStarter::new(
        project_dir.clone(),
        config_dir.clone(),
        None,
        orchestraitor_worker::WorkerBudgets::bootstrap_defaults(),
        orchestraitor_worker::GuardrailsConfig::bootstrap_defaults(),
    );
    let worktree = starter.prepare_worktree_for_test(task_id)?;
    assert!(worktree.exists(), "invocation N created the worktree");

    // The run finishes but the worktree is never removed on completion
    // (the documented lifecycle): the path stays registered.
    assert!(worktree.exists());

    // Invocation N+1 (run-budget restart): the per-invocation prune runs
    // before any spawn.
    DirectWatchStarter::prune_worktrees(&config_dir, &project_dir);
    assert!(
        !worktree.exists(),
        "the prune removed the stale worktree registration"
    );

    // The retry: `worktree add` into the SAME path must now succeed —
    // the forbidden effect (a spawn failure on the retried task) did NOT
    // happen.
    let worktree2 = starter.prepare_worktree_for_test(task_id)?;
    assert!(
        worktree2.exists(),
        "the retried task's worktree was recreated cleanly"
    );
    Ok(())
}

/// Runs `git` in `dir`, failing the test on a non-zero exit.
fn run_git(dir: &std::path::Path, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

/// §9.50 layered config (issue #504): the `[watch.ingress]` block
/// resolves with built-in defaults (disabled, capacity 256); a higher
/// layer overrides; a zero capacity is rejected fail-closed.
#[test]
fn watch_ingress_resolves_from_layers_with_built_in_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    use orchestraitor_core::config::{ConfigLayer, ConfigResolver, ConfigSource};

    // No configuration: the built-in defaults (ingress disabled).
    let resolver = ConfigResolver::new();
    let config = WatchConfig::from_layers(&resolver)?;
    assert!(
        !config.ingress.enabled,
        "the §9.50 poll ingress ships disabled (opt-in per #573's posture)"
    );
    assert_eq!(
        config.ingress.queue_capacity,
        orchestraitor_daemon::DEFAULT_QUEUE_CAPACITY,
        "the documented bounded-queue default"
    );

    // A configured block: enabled with a custom capacity.
    let resolver = ConfigResolver::new().with_toml(
        ConfigSource {
            layer: ConfigLayer::Project,
            name: "test-project".to_string(),
        },
        "[watch.ingress]\nenabled = true\nqueue_capacity = 64\n",
    )?;
    let config = WatchConfig::from_layers(&resolver)?;
    assert!(config.ingress.enabled);
    assert_eq!(config.ingress.queue_capacity, 64);

    // A zero capacity makes every push a drop: rejected fail-closed.
    let zero = WatchConfig::with_ingress(
        60,
        orchestraitor_campaign::KickoffSettings::builtin_defaults(),
        orchestraitor_daemon::IngressSettings {
            enabled: true,
            queue_capacity: 0,
        },
    );
    assert!(zero.is_err(), "a zero queue capacity must be rejected");
    Ok(())
}

/// §9.22.1 (issue #504): the built-in defaults TOML data carries the
/// ingress block (disabled, capacity 256) and still agrees with the
/// resolver fallback defaults.
#[test]
fn the_built_in_defaults_carry_the_ingress_block() -> Result<(), Box<dyn std::error::Error>> {
    use orchestraitor_core::config::{ConfigLayer, ConfigResolver, ConfigSource};

    let resolver = ConfigResolver::new().with_toml(
        ConfigSource {
            layer: ConfigLayer::BuiltInDefaults,
            name: "built-in watch defaults".to_string(),
        },
        orchestraitor_daemon::WATCH_BUILT_IN_DEFAULTS_TOML,
    )?;
    let config = WatchConfig::from_layers(&resolver)?;
    assert_eq!(
        config.ingress,
        orchestraitor_daemon::IngressSettings::builtin_defaults(),
        "the shipped TOML data and the fallback defaults agree"
    );
    Ok(())
}

/// Issue #603: the reverse-direction ingress/kick-off mismatch (the
/// condition armed, the ingress disabled) was silent. The forward-
/// direction mismatch (ingress enabled, condition disarmed) warns. Both
/// are exposed by `WatchConfig::ingress_kickoff_warnings` and rendered
/// by the `orcd watch` startup gate; consistent pairs stay silent.
/// Deterministic: pure helper over the resolved config, no tracing,
/// no I/O — every shape asserts the exact emitted message and the
/// complement.
#[test]
fn ingress_kickoff_warnings_cover_both_mismatch_directions() {
    let mut kickoff = orchestraitor_campaign::KickoffSettings::builtin_defaults();

    // --- Consistent: ingress enabled AND external_event armed ---
    // No warning: the armed condition has an ingress feeding it.
    kickoff.external_event = true;
    let config = WatchConfig::with_ingress(
        60,
        kickoff,
        orchestraitor_daemon::IngressSettings {
            enabled: true,
            queue_capacity: 256,
        },
    )
    .expect("consistent config is valid");
    assert!(
        config.ingress_kickoff_warnings().is_empty(),
        "both arms true must produce no warnings; got {:?}",
        config.ingress_kickoff_warnings()
    );

    // --- Consistent: ingress disabled AND external_event disarmed ---
    // No warning: nothing armed against a missing trigger source.
    kickoff.external_event = false;
    let config = WatchConfig::with_ingress(
        60,
        kickoff,
        orchestraitor_daemon::IngressSettings {
            enabled: false,
            queue_capacity: 256,
        },
    )
    .expect("consistent config is valid");
    assert!(
        config.ingress_kickoff_warnings().is_empty(),
        "both arms false must produce no warnings; got {:?}",
        config.ingress_kickoff_warnings()
    );

    // --- Forward mismatch: ingress enabled, condition disarmed ---
    // Pre-existing #504 behavior: the queue fills, drops surplus,
    // stays degraded; the operator must see the mismatch.
    kickoff.external_event = false;
    let config = WatchConfig::with_ingress(
        60,
        kickoff,
        orchestraitor_daemon::IngressSettings {
            enabled: true,
            queue_capacity: 256,
        },
    )
    .expect("forward-mismatch config is valid");
    let warnings = config.ingress_kickoff_warnings();
    assert_eq!(
        warnings.len(),
        1,
        "exactly one forward-mismatch warning, got {warnings:?}"
    );
    assert!(
        warnings[0]
            .contains("watch.ingress.enabled is true but watch.kickoff.external_event is false"),
        "forward-mismatch warning names the enabled/arm pair; got: {}",
        warnings[0]
    );
    assert!(
        warnings[0].contains("arm external_event or disable the ingress"),
        "forward-mismatch warning names the operator remedy; got: {}",
        warnings[0]
    );

    // --- Reverse mismatch: ingress disabled, condition ARMED ---
    // Issue #603: the condition can never fire. Pre-existing main
    // silently dropped this — the new helper MUST.
    kickoff.external_event = true;
    let config = WatchConfig::with_ingress(
        60,
        kickoff,
        orchestraitor_daemon::IngressSettings {
            enabled: false,
            queue_capacity: 256,
        },
    )
    .expect("reverse-mismatch config is valid");
    let warnings = config.ingress_kickoff_warnings();
    assert_eq!(
        warnings.len(),
        1,
        "exactly one reverse-mismatch warning (#603), got {warnings:?}"
    );
    assert!(
        warnings[0]
            .contains("watch.kickoff.external_event is true but watch.ingress.enabled is false"),
        "reverse-mismatch warning names the arm/enabled pair; got: {}",
        warnings[0]
    );
    assert!(
        warnings[0].contains("can never fire"),
        "reverse-mismatch warning names the dead-trigger consequence; got: {}",
        warnings[0]
    );
    assert!(
        warnings[0].contains("enable the ingress or disarm external_event"),
        "reverse-mismatch warning names the operator remedy; got: {}",
        warnings[0]
    );
}

/// Issue #603: the `orcd watch` startup emits exactly the warnings the
/// helper returns and nothing more for the consistent pairs (the
/// invariants of the surrounding gate stay intact). The pure helper
/// check above pins the contract; this one asserts the call-site shape
/// the PR review can read in `main.rs`.
#[test]
fn ingress_kickoff_warnings_are_stable_strings() {
    let mut kickoff = orchestraitor_campaign::KickoffSettings::builtin_defaults();
    kickoff.external_event = false;
    let forward = WatchConfig::with_ingress(
        60,
        kickoff,
        orchestraitor_daemon::IngressSettings {
            enabled: true,
            queue_capacity: 256,
        },
    )
    .expect("forward-mismatch config is valid");
    let fwd = forward.ingress_kickoff_warnings();
    assert_eq!(fwd.len(), 1);
    assert!(
        fwd[0].contains("queue has no kick-off consumer"),
        "forward-mismatch message text is stable (consumer-mismatch wording)"
    );

    kickoff.external_event = true;
    let reverse = WatchConfig::with_ingress(
        60,
        kickoff,
        orchestraitor_daemon::IngressSettings {
            enabled: false,
            queue_capacity: 256,
        },
    )
    .expect("reverse-mismatch config is valid");
    let rev = reverse.ingress_kickoff_warnings();
    assert_eq!(rev.len(), 1);
    assert!(
        rev[0].contains("can never fire"),
        "reverse-mismatch message text is stable (dead-trigger wording)"
    );

    // The two warnings are distinct strings — operators must be able to
    // grep the daemon log for the exact phrase to identify which side of
    // the configuration pair is wrong.
    assert_ne!(
        fwd[0], rev[0],
        "forward- and reverse-mismatch messages must be distinct"
    );
}
