//! `LoopRunStore` roundtrip, reconciliation, and daily-spend tests
//! (issue #314). All clocks are explicit `now_secs` parameters — no test
//! reads a real clock.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    clippy::float_cmp
)]
// Test-only allowances mirror the session test harness: a failed assertion
// must fail the test loudly, and every float below is an exactly
// representable binary fraction summed in a fixed order.

use orchestraitor_campaign::{LoopRunStore, RunRowStatus, StartRun};

fn start_run(task: &str, at: u64) -> StartRun {
    StartRun {
        invocation_id: "inv-1".to_string(),
        decision_id: 42,
        task_id: task.to_string(),
        repo: "arbsec/orchestraitor".to_string(),
        number: 7,
        started_at_secs: at,
    }
}

#[test]
fn start_heartbeat_finish_roundtrip() {
    let store = LoopRunStore::open_in_memory().unwrap();
    let row = store.start(&start_run("board-x-1", 1_000)).unwrap();

    assert_eq!(row.status, RunRowStatus::Running);
    assert_eq!(row.heartbeat_turn, 0);
    assert_eq!(row.started_at_secs, 1_000);
    assert_eq!(row.finished_at_secs, None);

    store.heartbeat(row.id, 3, 1_500).unwrap();
    let beaten = store.by_id(row.id).unwrap();
    assert_eq!(beaten.heartbeat_turn, 3);
    assert_eq!(beaten.heartbeat_at_secs, 1_500);

    store
        .finish(
            row.id,
            RunRowStatus::Completed,
            2_000,
            0.25,
            "natural-completion",
        )
        .unwrap();
    let done = store.by_id(row.id).unwrap();
    assert_eq!(done.status, RunRowStatus::Completed);
    assert_eq!(done.finished_at_secs, Some(2_000));
    assert_eq!(done.spend_usd, 0.25);
    assert_eq!(done.detail, "natural-completion");
    assert!(done.status.is_terminal());
}

#[test]
fn heartbeat_and_finish_reject_terminal_rows() {
    let store = LoopRunStore::open_in_memory().unwrap();
    let row = store.start(&start_run("board-x-2", 1_000)).unwrap();
    store
        .finish(row.id, RunRowStatus::Stalled, 1_100, 0.0, "stall-timeout")
        .unwrap();

    let heartbeat = store.heartbeat(row.id, 5, 1_200);
    assert!(heartbeat.is_err(), "heartbeat after termination must fail");
    let refinish = store.finish(row.id, RunRowStatus::Failed, 1_300, 0.0, "again");
    assert!(refinish.is_err(), "double termination must fail");

    // The row is unchanged by the rejected mutations.
    let done = store.by_id(row.id).unwrap();
    assert_eq!(done.status, RunRowStatus::Stalled);
    assert_eq!(done.heartbeat_turn, 0);
}

#[test]
fn finish_rejects_the_running_status() {
    let store = LoopRunStore::open_in_memory().unwrap();
    let row = store.start(&start_run("board-x-3", 1_000)).unwrap();
    let result = store.finish(row.id, RunRowStatus::Running, 1_100, 0.0, "nope");
    assert!(result.is_err(), "running is not a terminal status");
}

#[test]
fn active_lists_only_running_rows() {
    let store = LoopRunStore::open_in_memory().unwrap();
    let first = store.start(&start_run("board-x-4", 1_000)).unwrap();
    let second = store.start(&start_run("board-x-5", 1_000)).unwrap();
    let third = store.start(&start_run("board-x-6", 1_000)).unwrap();
    store
        .finish(second.id, RunRowStatus::Failed, 1_050, 0.0, "typed-failure")
        .unwrap();

    let active = store.active().unwrap();
    assert_eq!(
        active.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![first.id, third.id],
        "only running rows hold concurrency slots"
    );
}

#[test]
fn runs_for_task_is_the_exclusion_surface() {
    let store = LoopRunStore::open_in_memory().unwrap();
    let row = store.start(&start_run("board-x-7", 1_000)).unwrap();
    store
        .finish(row.id, RunRowStatus::TimedOut, 9_000, 0.0, "worker-timeout")
        .unwrap();

    let runs = store.runs_for_task("board-x-7").unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunRowStatus::TimedOut);
    assert!(store.runs_for_task("board-x-unknown").unwrap().is_empty());
}

#[test]
fn daily_spend_sums_the_utc_day_of_the_explicit_clock() {
    let store = LoopRunStore::open_in_memory().unwrap();
    let day_one = 86_400; // 1970-01-02T00:00:00Z; day window [86_400, 172_800)
    let day_two = 3 * 86_400;

    let a = store.start(&start_run("board-x-8", day_one + 60)).unwrap();
    let b = store.start(&start_run("board-x-9", day_one + 120)).unwrap();
    let c = store.start(&start_run("board-x-10", day_two + 60)).unwrap();
    store
        .finish(a.id, RunRowStatus::Completed, day_one + 90, 1.5, "ok")
        .unwrap();
    store
        .finish(b.id, RunRowStatus::Completed, day_one + 150, 2.5, "ok")
        .unwrap();
    store
        .finish(c.id, RunRowStatus::Completed, day_two + 90, 9.0, "ok")
        .unwrap();

    assert_eq!(store.daily_spend(day_one + 200).unwrap(), 4.0);
    assert_eq!(store.daily_spend(day_two + 200).unwrap(), 9.0);
    // A run still in flight accrues nothing until it terminates.
    let in_flight = store
        .start(&start_run("board-x-11", day_two + 300))
        .unwrap();
    assert_eq!(store.daily_spend(day_two + 400).unwrap(), 9.0);
    drop(in_flight);
}

#[test]
fn reconcile_stale_sweeps_only_other_invocations() {
    let store = LoopRunStore::open_in_memory().unwrap();
    let stale = store.start(&start_run("board-x-12", 1_000)).unwrap();
    let fresh = store.start(&start_run("board-x-13", 1_000)).unwrap();

    // The fresh invocation reconciles at startup (same invocation id as the
    // rows it is about to create; the stale one predates it).
    let swept = store.reconcile_stale("inv-2", 2_000).unwrap();
    assert_eq!(swept, 2, "both rows belong to a previous invocation");

    let stale_row = store.by_id(stale.id).unwrap();
    assert_eq!(stale_row.status, RunRowStatus::AbortedCrash);
    assert_eq!(stale_row.finished_at_secs, Some(2_000));
    let fresh_row = store.by_id(fresh.id).unwrap();
    assert_eq!(fresh_row.status, RunRowStatus::AbortedCrash);

    // Rows created under the reconciled invocation survive a re-run.
    let mine = store
        .start(&StartRun {
            invocation_id: "inv-2".to_string(),
            ..start_run("board-x-14", 3_000)
        })
        .unwrap();
    let swept_again = store.reconcile_stale("inv-2", 3_100).unwrap();
    assert_eq!(swept_again, 0);
    assert_eq!(store.by_id(mine.id).unwrap().status, RunRowStatus::Running);
}
