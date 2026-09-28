//! Decision-store tests: append-only semantics across reopen (the durable
//! local state the issue's rollback story relies on).

use std::path::PathBuf;

use orchestraitor_agent_catalog::RoleRoutingDecision;
use orchestraitor_board::ItemFacts;
use orchestraitor_campaign::{
    BoardSnapshot, CampaignDecision, CampaignDecisionStore, DecisionKind, NoOpReason,
    WorkerSpawner, run_once,
};
use orchestraitor_worker::{RunStatus, WorkerBudgets, WorkerRun};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct NoopSpawner;

impl WorkerSpawner for NoopSpawner {
    fn spawn(
        &self,
        task_id: &str,
        _routing: &RoleRoutingDecision,
    ) -> Result<WorkerRun, orchestraitor_campaign::CampaignError> {
        Ok(WorkerRun {
            task_id: task_id.to_string(),
            status: RunStatus::Completed,
            exit_code: 0,
            summary: None,
            failure: None,
            delivery: None,
            attempts: 1,
            replans: 0,
            turns: 1,
            model_calls: 1,
            usage: orchestraitor_worker::UsageTotals {
                input_tokens: 1,
                output_tokens: 1,
            },
            spend_soft_cap_exceeded: false,
            untrusted_writes: Vec::new(),
            receipts: Vec::new(),
            budgets: WorkerBudgets::bootstrap_defaults().echo(),
        })
    }
}

fn routing() -> RoleRoutingDecision {
    RoleRoutingDecision {
        role: "implement".to_string(),
        provider: "neuralwatt".to_string(),
        model: "glm-5.2".to_string(),
        precedence_path: "bootstrap-default".to_string(),
        fallback_reason: None,
    }
}

fn ready_snapshot(number: u64) -> BoardSnapshot {
    BoardSnapshot {
        open: vec![ItemFacts {
            item_node_id: format!("PVTI_{number}"),
            repo: "arbsec/orchestraitor".to_string(),
            number,
            title: format!("task {number}"),
            url: format!("https://github.com/arbsec/orchestraitor/issues/{number}"),
            issue_type: None,
            labels: Vec::new(),
            open_blockers: 0,
            target: Some("MVP".to_string()),
            status: Some("Ready".to_string()),
            priority: None,
        }],
        blocked_candidates: Vec::new(),
        ready: vec![orchestraitor_board::ReadyItem {
            number,
            title: format!("task {number}"),
            url: format!("https://github.com/arbsec/orchestraitor/issues/{number}"),
            repo: "arbsec/orchestraitor".to_string(),
            item_id: format!("PVTI_{number}"),
        }],
        warnings: Vec::new(),
    }
}

fn noop_decision() -> CampaignDecision {
    CampaignDecision {
        kind: DecisionKind::NoOp,
        no_op_reason: Some(NoOpReason::EmptyQueue),
        selected: None,
        role: "implement".to_string(),
        provider: "neuralwatt".to_string(),
        model: "glm-5.2".to_string(),
        precedence_path: "bootstrap-default".to_string(),
        fallback_reason: None,
        worker_args: vec!["worker".to_string()],
        rationale: "empty board".to_string(),
        alternatives: Vec::new(),
        blocked_graph: Vec::new(),
        skipped: Vec::new(),
    }
}

#[test]
fn records_survive_reopen_and_append_in_order() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path: PathBuf = dir.path().join("state").join("campaign.db");

    let store = CampaignDecisionStore::open(&path)?;
    store.record(&noop_decision())?;
    let outcome = run_once(&ready_snapshot(42), &routing(), &store, &NoopSpawner)?;
    drop(store);

    let reopened = CampaignDecisionStore::open(&path)?;
    let records = reopened.list()?;
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].decision.kind, DecisionKind::NoOp);
    assert_eq!(
        records[0].decision.no_op_reason,
        Some(NoOpReason::EmptyQueue)
    );
    assert_eq!(records[1].decision.kind, DecisionKind::Selected);
    let selected = records[1]
        .decision
        .selected
        .as_ref()
        .ok_or("selected row must carry a task")?;
    assert_eq!(selected.number, 42);
    assert_eq!(selected.task_id, "board-arbsec_orchestraitor-42");
    assert!(records[0].id < records[1].id, "append-only ids");
    assert_eq!(
        reopened.by_id(records[1].id)?.decision,
        outcome.decision.decision
    );
    Ok(())
}
