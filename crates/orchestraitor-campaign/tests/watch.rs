//! Restart recovery and reconcile tests (issue #503, spec §9.24.2,
//! §9.36, §9.43): virtual-clock-free store tests plus the cadence gate.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
// Test-only allowances mirror `loop_run.rs`: a failed assertion must fail
// the test loudly.

use std::time::Duration;

use orchestraitor_board::ReadyItem;
use orchestraitor_campaign::{
    BoardSnapshot, LoopConfig, LoopRunStore, RunRow, RunRowStatus, StartRun, reconcile,
};

const REPO: &str = "arbsec/orchestraitor";
const START_UNIX: u64 = 1_000_000;

/// A ready item fixture for the reconcile promotion test.
fn ready_item(number: u64) -> ReadyItem {
    ReadyItem {
        number,
        title: format!("fixture task {number}"),
        url: format!("https://github.com/{REPO}/issues/{number}"),
        repo: REPO.to_string(),
        item_id: format!("item-{number}"),
    }
}

/// Seeds a `paused` row the way a future pause control would: a direct
/// status update through the same schema (test-only; no production
/// mutation path for non-terminal rows exists by contract).
fn rusqlite_touch_paused(path: std::path::PathBuf, task_id: &str) {
    let conn = rusqlite::Connection::open(path).expect("open");
    conn.execute(
        "UPDATE loop_worker_runs SET status = 'paused' WHERE task_id = ?1",
        [task_id],
    )
    .expect("pause update");
}

/// Inserts a running row through the store's own start path.
fn start_row(runs: &LoopRunStore, task_id: &str, number: u64) -> RunRow {
    runs.start(&StartRun {
        invocation_id: "watch-test".to_string(),
        decision_id: 1,
        task_id: task_id.to_string(),
        repo: REPO.to_string(),
        number,
        started_at_secs: START_UNIX,
    })
    .expect("row insert")
}

/// §9.24.2 restart recovery: a `running` row left behind by a killed
/// supervisor transitions to `orphaned` — never directly `failed`.
#[test]
fn restart_recovery_orphans_running_rows() {
    let runs = LoopRunStore::open_in_memory().expect("store");
    let row = start_row(&runs, "board-arbsec_orchestraitor-42", 42);

    let recovered = runs
        .recover_running_rows(START_UNIX + 60)
        .expect("recovery scan");
    assert_eq!(recovered.len(), 1, "exactly the one running row");
    assert_eq!(recovered[0].id, row.id);
    assert_eq!(recovered[0].status, RunRowStatus::Orphaned);
    assert_eq!(
        recovered[0].detail,
        "restart-recovery: supervisor did not reach a terminal status"
    );
    assert_eq!(recovered[0].finished_at_secs, Some(START_UNIX + 60));

    // The durable row reads back orphaned from a fresh decode.
    let durable = runs.by_id(row.id).expect("row");
    assert_eq!(durable.status, RunRowStatus::Orphaned);
}

/// §9.24.2: recovery is idempotent — a second scan over recovered rows is
/// a no-op, and terminal rows are never touched.
#[test]
fn restart_recovery_is_idempotent_and_leaves_terminal_rows() {
    let runs = LoopRunStore::open_in_memory().expect("store");
    let orphan_target = start_row(&runs, "board-arbsec_orchestraitor-42", 42);
    let completed = start_row(&runs, "board-arbsec_orchestraitor-43", 43);
    runs.finish(
        completed.id,
        RunRowStatus::Completed,
        START_UNIX,
        0.0,
        "natural",
    )
    .expect("finish");

    let first = runs.recover_running_rows(START_UNIX + 10).expect("first");
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].id, orphan_target.id);

    let second = runs.recover_running_rows(START_UNIX + 20).expect("second");
    assert!(second.is_empty(), "idempotent: nothing left running");

    // The completed row keeps its own terminal status and finish stamp.
    let durable = runs.by_id(completed.id).expect("row");
    assert_eq!(durable.status, RunRowStatus::Completed);
    assert_eq!(durable.finished_at_secs, Some(START_UNIX));
}

/// §9.24.2: a `paused` row stays paused across restart recovery. The
/// pause control itself is a later slice, so this pins the schema-level
/// contract: `paused` decodes as a non-terminal status the recovery scan
/// never transitions (the scan selects `status = 'running'` only).
#[test]
fn paused_status_is_non_terminal_and_untouched_by_recovery() {
    let paused: RunRowStatus = serde_json::from_str("\"paused\"").expect("paused status");
    assert_eq!(paused.as_str(), "paused");
    assert!(!paused.is_terminal(), "paused is a non-terminal state");

    // Through the real store: recovery over a database holding ONLY a
    // paused row (seeded via the same SQL the store owns) returns empty
    // and leaves the row decodable.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = LoopRunStore::open(&dir.path().join("loop.db")).expect("store");
    start_row(&store, "board-arbsec_orchestraitor-44", 44);
    // Simulate a future pause control's durable write: the only sanctioned
    // non-running non-terminal spelling.
    rusqlite_touch_paused(dir.path().join("loop.db"), "board-arbsec_orchestraitor-44");
    let recovered = store
        .recover_running_rows(START_UNIX + 60)
        .expect("recovery");
    assert!(recovered.is_empty(), "a paused row is never recovered");
}

/// §9.36 reconcile: a running row whose task vanished from the board's
/// open set records a `board-diverged` event (board wins, §9.43).
#[test]
fn reconcile_records_board_diverged_for_vanished_running_task() {
    let runs = LoopRunStore::open_in_memory().expect("store");
    start_row(&runs, "board-arbsec_orchestraitor-42", 42);

    // Fresh snapshot: task 42 is GONE from the open set (moved/cancelled
    // on the board).
    let snapshot = BoardSnapshot {
        open: Vec::new(),
        ready: Vec::new(),
        blocked_candidates: Vec::new(),
        warnings: Vec::new(),
    };
    let outcome = reconcile(&snapshot, &[], &[], &runs).expect("reconcile");
    assert_eq!(outcome.events.len(), 1, "one divergence recorded");
    match &outcome.events[0] {
        orchestraitor_campaign::ReconcileEvent::BoardDiverged {
            task_id,
            local_status,
        } => {
            assert_eq!(task_id, "board-arbsec_orchestraitor-42");
            assert_eq!(*local_status, RunRowStatus::Running);
        }
        other @ orchestraitor_campaign::ReconcileEvent::UnblockedTaskPromoted { .. } => {
            panic!("expected BoardDiverged, got {other:?}")
        }
    }
}

/// §9.36 reconcile: a running row whose task is still open on the board is
/// normal supervision, not a divergence.
#[test]
fn reconcile_ignores_running_rows_still_open_on_the_board() {
    let runs = LoopRunStore::open_in_memory().expect("store");
    start_row(&runs, "board-arbsec_orchestraitor-42", 42);

    let snapshot = BoardSnapshot {
        open: vec![orchestraitor_board::ItemFacts {
            item_node_id: "item-42".to_string(),
            repo: REPO.to_string(),
            number: 42,
            title: "fixture".to_string(),
            url: format!("https://github.com/{REPO}/issues/42"),
            issue_type: Some("Task".to_string()),
            priority: None,
            target: None,
            status: Some("In Progress".to_string()),
            labels: Vec::new(),
            open_blockers: 0,
        }],
        ready: Vec::new(),
        blocked_candidates: Vec::new(),
        warnings: Vec::new(),
    };
    let outcome = reconcile(&snapshot, &[], &[], &runs).expect("reconcile");
    assert!(
        outcome.events.is_empty(),
        "an open running task is not a divergence, got {:?}",
        outcome.events
    );
}

/// §9.40 promotion: a previously blocked candidate now on the ready queue
/// records an unblocked-task promotion event.
#[test]
fn reconcile_records_unblocked_task_promotion() {
    let runs = LoopRunStore::open_in_memory().expect("store");
    let snapshot = BoardSnapshot {
        open: Vec::new(),
        ready: vec![ready_item(42)],
        blocked_candidates: Vec::new(),
        warnings: Vec::new(),
    };
    // Previous pass: task 42 was a blocked candidate.
    let outcome = reconcile(&snapshot, &[ready_item(42)], &[], &runs).expect("reconcile");
    assert_eq!(outcome.events.len(), 1, "one promotion recorded");
    match &outcome.events[0] {
        orchestraitor_campaign::ReconcileEvent::UnblockedTaskPromoted {
            task_id,
            number,
            repo,
        } => {
            assert_eq!(task_id, "board-arbsec_orchestraitor-42");
            assert_eq!(*number, 42);
            assert_eq!(repo, REPO);
        }
        other @ orchestraitor_campaign::ReconcileEvent::BoardDiverged { .. } => {
            panic!("expected UnblockedTaskPromoted, got {other:?}")
        }
    }
    assert_eq!(
        outcome.ready_task_ids,
        vec!["board-arbsec_orchestraitor-42"]
    );
}

/// §9.40: a candidate still blocked is NOT a promotion.
#[test]
fn reconcile_ignores_still_blocked_candidates() {
    let runs = LoopRunStore::open_in_memory().expect("store");
    let snapshot = BoardSnapshot {
        open: Vec::new(),
        ready: Vec::new(),
        blocked_candidates: vec![ready_item(42)],
        warnings: Vec::new(),
    };
    let outcome = reconcile(&snapshot, &[ready_item(42)], &[], &runs).expect("reconcile");
    assert!(
        outcome.events.is_empty(),
        "still-blocked is not a promotion"
    );
}

/// §9.36 cadence: `LoopConfig::with_cadence` spaces board polls at the
/// configured interval; a zero cadence is rejected fail-closed.
#[test]
fn cadence_config_validation_rejects_zero() {
    let budgets = orchestraitor_worker::WorkerBudgets::bootstrap_defaults();
    let config =
        LoopConfig::with_cadence(budgets, Duration::from_secs(5), None, Some(Duration::ZERO));
    assert!(config.is_err(), "a zero cadence is a runaway poll loop");
    let config = LoopConfig::with_cadence(
        orchestraitor_worker::WorkerBudgets::bootstrap_defaults(),
        Duration::from_secs(5),
        None,
        Some(Duration::from_mins(1)),
    );
    assert!(config.is_ok(), "a positive cadence is accepted");
}
