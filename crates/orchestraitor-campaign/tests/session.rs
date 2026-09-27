//! Hermetic campaign-pass tests (spec `50-contracts-data.md` §21.3): no
//! network, no provider, no daemon — the spawner is a recording fake.

use std::sync::{Arc, Mutex};

use orchestraitor_agent_catalog::RoleRoutingDecision;
use orchestraitor_board::{ItemFacts, ReadyItem, SkipWarning, WarningKind};
use orchestraitor_campaign::{
    BoardSnapshot, CampaignDecisionStore, CampaignError, DecisionKind, NoOpReason, WorkerSpawner,
    compute_selection, run_once, task_id_for,
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
    ItemFacts {
        item_node_id: format!("PVTI_{repo}_{number}"),
        repo: repo.to_string(),
        number,
        title: format!("task {repo}#{number}"),
        url: format!("https://github.com/{repo}/issues/{number}"),
        issue_type: None,
        labels: labels.iter().map(|label| (*label).to_string()).collect(),
        open_blockers,
        target: Some("MVP".to_string()),
        status: Some("Ready".to_string()),
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
        vec![item("arbsec/orchestraitor", 42, &[], 0)],
        vec![ready("arbsec/orchestraitor", 42)],
        Vec::new(),
    );
    let outcome = run_once(&snap, &routing(), &store, &spawner)?;

    assert_eq!(outcome.decision.decision.kind, DecisionKind::Selected);
    let (number, task_id) = selected(&outcome)?;
    assert_eq!(number, 42);
    assert_eq!(task_id, task_id_for("arbsec/orchestraitor", 42));
    let expected_task_id = task_id.clone();
    assert_eq!(spawner.recorded(), vec![expected_task_id]);
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
fn same_board_produces_identical_selection_records() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let make = || {
        snapshot(
            vec![
                item("arbsec/orchestraitor", 7, &[], 0),
                item("arbsec/orchestraitor", 3, &["P0"], 0),
            ],
            vec![
                ready("arbsec/orchestraitor", 3),
                ready("arbsec/orchestraitor", 7),
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
        3,
        "P0 first"
    );
    assert_eq!(store.list()?.len(), 2);
    Ok(())
}

#[test]
fn p0_items_order_before_higher_numbers() {
    let open = vec![
        item("arbsec/orchestraitor", 9, &[], 0),
        item("arbsec/orchestraitor", 4, &["P0"], 0),
        item("arbsec/orchestraitor", 2, &[], 0),
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
    assert!(spawner.recorded().is_empty());
    assert_eq!(store.list()?.len(), 1);
    Ok(())
}

#[test]
fn only_blocked_items_no_op_with_blocked_graph_and_no_spawn() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let snap = snapshot(
        vec![
            item("arbsec/orchestraitor", 5, &[], 2),
            item("arbsec/orchestraitor", 6, &[], 1),
        ],
        Vec::new(),
        Vec::new(),
    );
    let outcome = run_once(&snap, &routing(), &store, &spawner)?;

    let decision = &outcome.decision.decision;
    assert_eq!(decision.kind, DecisionKind::NoOp);
    assert_eq!(decision.no_op_reason, Some(NoOpReason::AllBlocked));
    assert!(outcome.worker.is_none());
    assert!(spawner.recorded().is_empty());
    let graph = &decision.blocked_graph;
    assert_eq!(graph.len(), 2);
    assert_eq!(graph[0].number, 5);
    assert_eq!(graph[0].open_blockers, 2);
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
fn alternatives_record_ready_items_after_the_selection() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::completed();
    let open = vec![
        item("arbsec/orchestraitor", 2, &[], 0),
        item("arbsec/orchestraitor", 3, &[], 0),
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
    Ok(())
}

#[test]
fn failed_worker_run_is_still_exactly_one_decision() -> TestResult {
    let store = CampaignDecisionStore::open_in_memory()?;
    let (spawner, _calls) = FakeSpawner::with_status(RunStatus::Failed);
    let snap = snapshot(
        vec![item("arbsec/orchestraitor", 42, &[], 0)],
        vec![ready("arbsec/orchestraitor", 42)],
        Vec::new(),
    );
    let outcome = run_once(&snap, &routing(), &store, &spawner)?;

    let worker = outcome.worker.as_ref().ok_or("spawned")?;
    assert_eq!(worker.status, RunStatus::Failed);
    assert_eq!(store.list()?.len(), 1);
    Ok(())
}
