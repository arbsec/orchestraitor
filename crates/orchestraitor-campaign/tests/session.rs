//! Hermetic campaign-pass tests (spec `50-contracts-data.md` §21.3): no
//! network, no provider, no daemon — the spawner is a recording fake.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
// Test-only allowances mirror the CLI test harness: a failed assertion must
// fail the test loudly.

use std::sync::{Arc, Mutex};

use orchestraitor_agent_catalog::RoleRoutingDecision;
use orchestraitor_board::{ItemFacts, ReadyItem, SkipWarning, WarningKind};
use orchestraitor_campaign::{
    BlockedNode, BoardSnapshot, CampaignDecisionStore, CampaignError, DecisionKind, NoOpReason,
    WorkerSpawner, compute_selection, run_once, task_id_for,
};
use orchestraitor_worker::{RunStatus, WorkerBudgets, WorkerRun};

type BoxError = Box<dyn std::error::Error>;
type TestResult = Result<(), BoxError>;
type SpawnRecord = Arc<Mutex<Vec<String>>>;

struct FakeSpawner {
    calls: SpawnRecord,
    status: RunStatus,
}

impl FakeSpawner {
    fn completed() -> (Self, SpawnRecord) {
        Self::with_status(RunStatus::Completed)
    }

    fn with_status(status: RunStatus) -> (Self, SpawnRecord) {
        let calls: SpawnRecord = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                calls: Arc::clone(&calls),
                status,
            },
            calls,
        )
    }

    fn recorded(&self) -> Vec<String> {
        self.calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default()
    }
}

impl WorkerSpawner for FakeSpawner {
    fn spawn(
        &self,
        task_id: &str,
        _routing: &RoleRoutingDecision,
    ) -> Result<WorkerRun, CampaignError> {
        self.calls
            .lock()
            .map(|mut calls| calls.push(task_id.to_string()))
            .map_err(|error| CampaignError::Spawn {
                task_id: task_id.to_string(),
                message: format!("fake spawner mutex poisoned: {error}"),
            })?;
        Ok(fake_run(task_id, self.status))
    }
}

fn fake_run(task_id: &str, status: RunStatus) -> WorkerRun {
    WorkerRun {
        task_id: task_id.to_string(),
        status,
        exit_code: match status {
            RunStatus::Completed => 0,
            RunStatus::Failed => 1,
        },
        summary: Some("done".to_string()),
        failure: None,
        delivery: None,
        attempts: 1,
        replans: 0,
        turns: 2,
        model_calls: 2,
        usage: orchestraitor_worker::UsageTotals {
            input_tokens: 10,
            output_tokens: 5,
        },
        spend_soft_cap_exceeded: false,
        untrusted_writes: Vec::new(),
        receipts: Vec::new(),
        subsession_events: Vec::new(),
        budgets: WorkerBudgets::bootstrap_defaults().echo(),
    }
}

fn routing() -> RoleRoutingDecision {
    RoleRoutingDecision {
        role: "implement".to_string(),
        provider: "neuralwatt".to_string(),
        model: "glm-5.2".to_string(),
        precedence_path: "bootstrap-default".to_string(),
        fallback_reason: Some("no explicit entry".to_string()),
    }
}

fn item(repo: &str, number: u64, labels: &[&str], open_blockers: u64) -> ItemFacts {
    self_item(repo, number, labels, open_blockers, None)
}

fn self_item(
    repo: &str,
    number: u64,
    labels: &[&str],
    open_blockers: u64,
    priority: Option<&str>,
) -> ItemFacts {
    ItemFacts {
        item_node_id: format!("PVTI_{repo}_{number}"),
        repo: repo.to_string(),
        number,
        title: format!("task {repo}#{number}"),
        url: format!("https://github.com/{repo}/issues/{number}"),
        issue_type: Some("Task".to_string()),
        labels: labels.iter().map(|label| (*label).to_string()).collect(),
        open_blockers,
        target: Some("MVP".to_string()),
        status: Some("Ready".to_string()),
        priority: priority.map(std::string::ToString::to_string),
    }
}

fn triage_item(repo: &str, number: u64) -> ItemFacts {
    ItemFacts {
        status: Some("Triage".to_string()),
        ..item(repo, number, &[], 0)
    }
}

fn ready(repo: &str, number: u64) -> ReadyItem {
    ReadyItem {
        number,
        title: format!("task {repo}#{number}"),
        url: format!("https://github.com/{repo}/issues/{number}"),
        repo: repo.to_string(),
        item_id: format!("PVTI_{repo}_{number}"),
    }
}

fn snapshot(
    open: Vec<ItemFacts>,
    ready: Vec<ReadyItem>,
    warnings: Vec<SkipWarning>,
) -> BoardSnapshot {
    BoardSnapshot {
        open,
        ready,
        blocked_candidates: Vec::new(),
        warnings,
    }
}

fn selected(outcome: &orchestraitor_campaign::CampaignOutcome) -> Result<(u64, String), BoxError> {
    let task = outcome
        .decision
        .decision
        .selected
        .clone()
        .ok_or("Selected decision must carry a task")?;
    Ok((task.number, task.task_id))
}

#[test]
fn happy_pass_selects_first_ready_and_spawns_worker_once() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let snap = snapshot(
        vec![self_item("arbsec/orchestraitor", 42, &[], 0, None)],
        vec![ready("arbsec/orchestraitor", 42)],
        Vec::new(),
    );
    let outcome = run_once(&snap, &routing(), &store, &spawner)?;

    assert_eq!(outcome.decision.decision.kind, DecisionKind::Selected);
    let (number, task_id) = selected(&outcome)?;
    assert_eq!(number, 42);
    assert_eq!(task_id, task_id_for("arbsec/orchestraitor", 42));
    assert_eq!(spawner.recorded(), vec![task_id.clone()]);
    let worker = outcome
        .worker
        .as_ref()
        .ok_or("selected pass spawns the worker")?;
    assert_eq!(worker.status, RunStatus::Completed);
    assert_eq!(store.list()?.len(), 1);
    let stored = store.by_id(outcome.decision.id)?;
    assert_eq!(
        stored.decision.worker_args,
        vec![
            "worker".to_string(),
            "run".to_string(),
            "--task".to_string(),
            task_id.clone(),
            "--json".to_string(),
        ]
    );
    match stored.decision.selected {
        Some(task) => assert_eq!(task.task_id, task_id),
        None => return Err("read-back must carry the selected task".into()),
    }
    Ok(())
}

#[test]
fn p0_priority_field_orders_before_higher_numbers() {
    let open = vec![
        self_item("arbsec/orchestraitor", 9, &[], 0, None),
        self_item("arbsec/orchestraitor", 4, &[], 0, Some("P0")),
        self_item("arbsec/orchestraitor", 2, &[], 0, None),
    ];
    let ready = vec![
        ready("arbsec/orchestraitor", 2),
        ready("arbsec/orchestraitor", 4),
        ready("arbsec/orchestraitor", 9),
    ];
    let ordered = compute_selection(&ready, &open);
    assert_eq!(
        ordered.iter().map(|item| item.number).collect::<Vec<_>>(),
        vec![4, 2, 9]
    );
}

#[test]
fn p0_labels_are_not_authoritative_field_absent_degenerates_to_number_order() {
    // The board config declares labels advisory and fields authoritative; a
    // P0 label without a priority-field value must NOT promote the item.
    let open = vec![
        self_item("arbsec/orchestraitor", 3, &["P0"], 0, None),
        self_item("arbsec/orchestraitor", 2, &[], 0, None),
    ];
    let ready = vec![
        ready("arbsec/orchestraitor", 2),
        ready("arbsec/orchestraitor", 3),
    ];
    let ordered = compute_selection(&ready, &open);
    assert_eq!(
        ordered.iter().map(|item| item.number).collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[test]
fn same_board_produces_identical_selection_records() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let make = || {
        snapshot(
            vec![
                self_item("arbsec/orchestraitor", 2, &[], 0, None),
                self_item("arbsec/orchestraitor", 9, &[], 0, Some("P0")),
            ],
            vec![
                ready("arbsec/orchestraitor", 2),
                ready("arbsec/orchestraitor", 9),
            ],
            Vec::new(),
        )
    };
    let first = run_once(&make(), &routing(), &store, &spawner)?;
    let second = run_once(&make(), &routing(), &store, &spawner)?;

    assert_eq!(
        first.decision.decision, second.decision.decision,
        "same board must produce identical decisions"
    );
    assert_eq!(
        first
            .decision
            .decision
            .selected
            .as_ref()
            .map(|task| task.number)
            .unwrap_or_default(),
        9,
        "P0 first; the P0 item carries the HIGHER number, so plain number-order would \
         select 2 and this pin independently backstops the priority rule"
    );
    assert_eq!(store.list()?.len(), 2);
    Ok(())
}

#[test]
fn cross_repo_same_number_is_deterministic_and_does_not_collide() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let open = vec![
        self_item("arbsec/orchestraitor", 42, &[], 0, None),
        self_item("arbsec/arbitraitor", 42, &[], 0, None),
    ];
    let ready = vec![
        ready("arbsec/arbitraitor", 42),
        ready("arbsec/orchestraitor", 42),
    ];
    let outcome = run_once(
        &snapshot(open, ready, Vec::new()),
        &routing(),
        &store,
        &spawner,
    )?;

    // Deterministic: `arbsec/arbitraitor` < `arbsec/orchestraitor`.
    let (number, task_id) = selected(&outcome)?;
    assert_eq!(number, 42);
    assert_eq!(task_id, task_id_for("arbsec/arbitraitor", 42));
    // Repo-scoped ids: the two repos must never share a fixture file.
    assert_ne!(
        task_id_for("arbsec/arbitraitor", 42),
        task_id_for("arbsec/orchestraitor", 42)
    );
    assert_eq!(spawner.recorded(), vec![task_id]);
    Ok(())
}

#[test]
fn task_ids_respect_the_worker_charset_and_length_bound() {
    for repo in [
        "arbsec/orchestraitor",
        "a/b",
        "owner.with.dots/repo_name-x",
        "very-long-owner-name/very-long-repository-name-that-keeps-going",
    ] {
        let id = task_id_for(repo, 7);
        assert!(
            id.len() <= 64,
            "id must fit the worker file-name bound: {id}"
        );
        let mut chars = id.chars();
        let first = chars.next().expect("non-empty");
        assert!(first.is_ascii_alphanumeric(), "alphanumeric start: {id}");
        assert!(
            chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
            "charset violation: {id}"
        );
    }
}

#[test]
fn empty_board_is_empty_queue_no_op_without_spawn() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let outcome = run_once(
        &snapshot(Vec::new(), Vec::new(), Vec::new()),
        &routing(),
        &store,
        &spawner,
    )?;

    assert_eq!(outcome.decision.decision.kind, DecisionKind::NoOp);
    assert_eq!(
        outcome.decision.decision.no_op_reason,
        Some(NoOpReason::EmptyQueue)
    );
    assert!(outcome.worker.is_none());
    assert_eq!(spawner.recorded(), [] as [String; 0]);
    assert_eq!(store.list()?.len(), 1);
    assert_eq!(outcome.decision.decision.worker_args, [] as [String; 0]);
    Ok(())
}

#[test]
fn open_items_without_readiness_or_blockers_are_empty_queue_not_all_blocked() -> TestResult {
    // Regression: the live board's normal state (open items in Triage) must
    // not report all-blocked — spec §9.35 reserves that for eligible work
    // whose candidates are all blocked.
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let snap = snapshot(
        vec![
            triage_item("arbsec/orchestraitor", 5),
            triage_item("arbsec/orchestraitor", 6),
        ],
        Vec::new(),
        Vec::new(),
    );
    let outcome = run_once(&snap, &routing(), &store, &spawner)?;

    assert_eq!(
        outcome.decision.decision.no_op_reason,
        Some(NoOpReason::EmptyQueue)
    );
    assert_eq!(
        outcome.decision.decision.blocked_graph,
        [] as [BlockedNode; 0]
    );
    assert!(outcome.worker.is_none());
    Ok(())
}

#[test]
fn blocked_eligible_candidates_are_all_blocked_with_graph() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let open = vec![
        self_item("arbsec/orchestraitor", 5, &[], 2, None),
        self_item("arbsec/orchestraitor", 6, &[], 1, None),
    ];
    let mut snap = snapshot(open, Vec::new(), Vec::new());
    snap.blocked_candidates = vec![
        ready("arbsec/orchestraitor", 5),
        ready("arbsec/orchestraitor", 6),
    ];
    let outcome = run_once(&snap, &routing(), &store, &spawner)?;

    let decision = &outcome.decision.decision;
    assert_eq!(decision.kind, DecisionKind::NoOp);
    assert_eq!(decision.no_op_reason, Some(NoOpReason::AllBlocked));
    assert!(outcome.worker.is_none());
    assert_eq!(spawner.recorded(), [] as [String; 0]);
    let graph = &decision.blocked_graph;
    assert_eq!(graph.len(), 2);
    assert_eq!(graph[0].number, 5);
    assert_eq!(graph[0].open_blockers, 2);
    assert_eq!(graph[0].target.as_deref(), Some("MVP"));
    assert_eq!(graph[1].number, 6);
    assert_eq!(store.list()?.len(), 1);
    Ok(())
}

#[test]
fn all_items_closed_is_epic_exhausted_no_op() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let warnings = vec![SkipWarning {
        number: Some(11),
        reason: "closed".to_string(),
        kind: WarningKind::NotOpen,
    }];
    let outcome = run_once(
        &snapshot(Vec::new(), Vec::new(), warnings),
        &routing(),
        &store,
        &spawner,
    )?;

    assert_eq!(outcome.decision.decision.kind, DecisionKind::NoOp);
    assert_eq!(
        outcome.decision.decision.no_op_reason,
        Some(NoOpReason::EpicExhausted)
    );
    assert!(outcome.worker.is_none());
    Ok(())
}

#[test]
fn mixed_closed_and_unevaluable_items_are_not_epic_exhausted() -> TestResult {
    // Regression: a malformed item's open-state is unknown, so closed+malformed
    // must conservatively report empty-queue (with disclosure), never
    // epic-exhausted — spec §9.35 + fail-closed warning channel.
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let warnings = vec![
        SkipWarning {
            number: Some(11),
            reason: "closed".to_string(),
            kind: WarningKind::NotOpen,
        },
        SkipWarning {
            number: Some(12),
            reason: "malformed item: missing content".to_string(),
            kind: WarningKind::Malformed,
        },
    ];
    let outcome = run_once(
        &snapshot(Vec::new(), Vec::new(), warnings),
        &routing(),
        &store,
        &spawner,
    )?;

    assert_eq!(outcome.decision.decision.kind, DecisionKind::NoOp);
    assert_eq!(
        outcome.decision.decision.no_op_reason,
        Some(NoOpReason::EmptyQueue),
        "unknown-state items must not allow an epic-exhausted claim"
    );
    assert!(outcome.worker.is_none());
    Ok(())
}

#[test]
fn long_repos_truncating_to_the_same_slug_do_not_collide() {
    let long_a = "arbsec/some-very-long-repository-name-that-exceeds-the-budget-A";
    let long_b = "arbsec/some-very-long-repository-name-that-exceeds-the-budget-B";
    assert_ne!(
        task_id_for(long_a, 42),
        task_id_for(long_b, 42),
        "truncated slugs must stay collision-free via the digest suffix"
    );
    // Short repos keep the plain readable form.
    assert_eq!(
        task_id_for("arbsec/orchestraitor", 42),
        "board-arbsec_orchestraitor-42"
    );
}

#[test]
fn dash_position_repo_variants_do_not_fold_to_the_same_task_id() {
    assert_ne!(
        task_id_for("foo/bar-baz", 42),
        task_id_for("foo-bar/baz", 42),
        "the owner/repo separator must stay distinguishable from a literal '-'"
    );
    assert_ne!(task_id_for("x-y/z", 7), task_id_for("x/y-z", 7));
    assert_ne!(
        task_id_for("arbsec/some-repo", 3),
        task_id_for("arbsec-some/repo", 3)
    );
    assert_eq!(
        task_id_for("foo/bar-baz", 42),
        "board-foo_bar-baz-42",
        "hyphenated names stay readable with '_' as the separator"
    );
    assert_eq!(task_id_for("foo-bar/baz", 42), "board-foo-bar_baz-42");
}

#[test]
fn readable_form_is_injective_over_a_small_alphabet() {
    // Owner/name over {a, b, -} up to length 3: every pair must yield a
    // distinct task id. Catches any fold that merges the '/' with a legal
    // owner/name character (the gen-3/gen-4 collision class).
    fn all_parts(alphabet: [&str; 3], max_len: usize) -> Vec<String> {
        let mut parts = vec![String::new()];
        let mut frontier = vec![String::new()];
        for _ in 0..max_len {
            let mut next = Vec::new();
            for base in &frontier {
                for piece in alphabet {
                    next.push(format!("{base}{piece}"));
                }
            }
            parts.extend(next.iter().cloned());
            frontier = next;
        }
        parts.retain(|part| !part.is_empty());
        parts
    }

    let parts = all_parts(["a", "b", "-"], 3);
    let mut ids = std::collections::HashSet::new();
    for owner in &parts {
        for name in &parts {
            let repo = format!("{owner}/{name}");
            assert!(
                ids.insert(task_id_for(&repo, 42)),
                "task-id collision for repo {repo}"
            );
        }
    }
}

#[test]
fn punctuation_variant_repos_do_not_fold_to_the_same_task_id() {
    let dotted = task_id_for("arbsec/foo.bar", 7);
    let dashed = task_id_for("arbsec/foo-bar", 7);
    let underscored = task_id_for("arbsec/foo_bar", 7);
    assert_ne!(dotted, dashed, "fold must not merge punctuation variants");
    assert_ne!(dotted, underscored);
    assert_ne!(dashed, underscored);
    assert_eq!(
        task_id_for("arbsec/orchestraitor", 7),
        "board-arbsec_orchestraitor-7",
        "charset-clean folds keep the readable short form"
    );
}

#[test]
fn unevaluable_items_are_carried_on_the_record() -> TestResult {
    // Fail-closed data-quality signals (truncated windows, malformed items)
    // must survive onto the durable record, not vanish.
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let warnings = vec![
        SkipWarning {
            number: Some(12),
            reason: "truncated `blockedBy` window; failing closed".to_string(),
            kind: WarningKind::Truncated,
        },
        SkipWarning {
            number: None,
            reason: "malformed item: missing content".to_string(),
            kind: WarningKind::Malformed,
        },
    ];
    let outcome = run_once(
        &snapshot(Vec::new(), Vec::new(), warnings),
        &routing(),
        &store,
        &spawner,
    )?;

    assert_eq!(
        outcome.decision.decision.no_op_reason,
        Some(NoOpReason::EmptyQueue)
    );
    assert!(
        outcome.decision.decision.rationale.contains("unevaluable"),
        "rationale must disclose the skipped items"
    );
    let skipped = &outcome.decision.decision.skipped;
    assert_eq!(skipped.len(), 2);
    assert_eq!(skipped[0].kind, "malformed");
    assert_eq!(skipped[1].kind, "truncated");
    Ok(())
}

#[test]
fn alternatives_record_ready_items_after_the_selection() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let open = vec![
        self_item("arbsec/orchestraitor", 2, &[], 0, None),
        self_item("arbsec/orchestraitor", 3, &[], 0, None),
    ];
    let ready = vec![
        ready("arbsec/orchestraitor", 2),
        ready("arbsec/orchestraitor", 3),
    ];
    let outcome = run_once(
        &snapshot(open, ready, Vec::new()),
        &routing(),
        &store,
        &spawner,
    )?;

    let alternatives = &outcome.decision.decision.alternatives;
    assert_eq!(alternatives.len(), 1);
    assert_eq!(alternatives[0].number, 3);
    assert_eq!(alternatives[0].target.as_deref(), Some("MVP"));
    assert_eq!(alternatives[0].status.as_deref(), Some("Ready"));
    Ok(())
}

#[test]
fn failed_worker_run_is_still_exactly_one_decision() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::with_status(RunStatus::Failed);
    let snap = snapshot(
        vec![self_item("arbsec/orchestraitor", 42, &[], 0, None)],
        vec![ready("arbsec/orchestraitor", 42)],
        Vec::new(),
    );
    let outcome = run_once(&snap, &routing(), &store, &spawner)?;

    let worker = outcome.worker.as_ref().ok_or("spawned")?;
    assert_eq!(worker.status, RunStatus::Failed);
    assert_eq!(store.list()?.len(), 1);
    Ok(())
}
