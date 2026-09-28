//! Simulator-backed worker-loop scenarios (issue #310 QA; spec §21.3 — the
//! deterministic `orchestraitor-testkit` simulator, never a live provider).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use async_trait::async_trait;
use orchestraitor_provider_neuralwatt::{NeuralwattConfig, NeuralwattTransport};
use orchestraitor_testkit::{OpenAiMockServer, PlannedResponse};
use orchestraitor_worker::delivery::DeliveryError;
use orchestraitor_worker::{
    BashMediator, DeliveryOutcome, DeliveryRequest, DeliverySink, FailureClass, FixtureTaskSource,
    MediatedRun, MediationError, ModelId, PendingDeliverySink, ProviderId, RunStatus, TaskSource,
    WorkerBudgets, WorkerConfig, WorkerRun, run_worker,
};
use secrecy::SecretString;
use serde_json::json;

/// Builds the real Neuralwatt transport pointed at the simulator.
fn test_transport(base_url: &str) -> NeuralwattTransport {
    let config =
        NeuralwattConfig::with_endpoint(base_url.to_string(), "secret://env/UNUSED".to_string())
            .unwrap();
    NeuralwattTransport::with_key(config, SecretString::from("simulator-test-key")).unwrap()
}

/// Fast, deterministic budgets: count bounds dominate; wall-clock bounds are
/// generous so tests never flake on timing.
fn fast_budgets() -> WorkerBudgets {
    WorkerBudgets {
        retry_base_delay: Duration::from_millis(1),
        retry_max_delay: Duration::from_millis(2),
        max_consecutive_format_errors: 2,
        max_turns_per_attempt: 10,
        ..WorkerBudgets::bootstrap_defaults()
    }
}

/// Fixture mediator (deterministic; `MediationError` is not `Clone`).
enum BashMode {
    Ok,
    Refused,
}

struct FixtureBash {
    mode: BashMode,
    calls: std::sync::Mutex<u32>,
}

#[async_trait]
impl BashMediator for FixtureBash {
    async fn run_bash(&self, _script: &str) -> Result<MediatedRun, MediationError> {
        *self.calls.lock().unwrap() += 1;
        match self.mode {
            BashMode::Ok => Ok(MediatedRun {
                exit_code: Some(0),
                stdout: b"fixture-bash-ok\n".to_vec(),
                stderr: Vec::new(),
            }),
            BashMode::Refused => Err(MediationError::Context {
                reason: "test-fixture-mediation-unavailable",
            }),
        }
    }
}

/// Fixture mediator that yields inside the dispatch, mirroring the real
/// mediated path (a subprocess boundary always suspends the worker task).
struct FixtureSlowBash;

#[async_trait]
impl BashMediator for FixtureSlowBash {
    async fn run_bash(&self, _script: &str) -> Result<MediatedRun, MediationError> {
        tokio::task::yield_now().await;
        Ok(MediatedRun {
            exit_code: Some(0),
            stdout: b"fixture-bash-ok\n".to_vec(),
            stderr: Vec::new(),
        })
    }
}

/// Fixture delivery sink: completes with a deterministic PR reference.
struct FixtureDelivery;

#[async_trait]
impl DeliverySink for FixtureDelivery {
    async fn deliver(&self, request: &DeliveryRequest) -> Result<DeliveryOutcome, DeliveryError> {
        Ok(DeliveryOutcome::PrOpened {
            reference: format!("pr://fixture/{}", request.task_id),
        })
    }
}

fn action_text(value: &serde_json::Value) -> String {
    format!("```json\n{value}\n```")
}

fn finish_action(success: bool) -> PlannedResponse {
    PlannedResponse::NonStreaming {
        content: action_text(&json!({
            "tool": "finish",
            "summary": "task finished",
            "success": success,
        })),
    }
}

async fn serve(script: Vec<PlannedResponse>) -> OpenAiMockServer {
    OpenAiMockServer::serve(script).await.unwrap()
}

fn fixture_task(id: &str) -> orchestraitor_worker::WorkerTask {
    orchestraitor_worker::WorkerTask {
        id: id.to_string(),
        slug: format!("{id}-slug"),
        description: "write the fixture file".to_string(),
    }
}

async fn drive(
    script: Vec<PlannedResponse>,
    bash_mode: BashMode,
) -> (WorkerRun, OpenAiMockServer, tempfile::TempDir, FixtureBash) {
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    let bash = FixtureBash {
        mode: bash_mode,
        calls: std::sync::Mutex::new(0),
    };
    let task = fixture_task("t-7");
    let config = WorkerConfig::new(
        ProviderId::from_string("neuralwatt".to_string()),
        ModelId::from_string("glm-5.2".to_string()),
        fast_budgets(),
    );
    let run = run_worker(
        &task,
        worktree.path(),
        &transport,
        &bash,
        &FixtureDelivery,
        &config,
    )
    .await
    .unwrap();
    (run, sim, worktree, bash)
}

#[tokio::test]
async fn happy_path_completes_with_pr_ref_via_the_delivery_seam() {
    let script = vec![
        PlannedResponse::NonStreaming {
            content: action_text(&json!({
                "tool": "write_file",
                "path": "src/answer.txt",
                "content": "42\n"
            })),
        },
        finish_action(true),
    ];
    let (run, sim, worktree, _bash) = drive(script, BashMode::Ok).await;

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.exit_code, 0);
    assert_eq!(run.task_id, "t-7");
    assert_eq!(run.attempts, 1);
    assert_eq!(
        run.delivery,
        Some(DeliveryOutcome::PrOpened {
            reference: "pr://fixture/t-7".to_string()
        })
    );
    assert_eq!(run.untrusted_writes, ["src/answer.txt"]);
    assert_eq!(
        std::fs::read_to_string(worktree.path().join("src/answer.txt")).unwrap(),
        "42\n"
    );
    // Every tool call produced a receipt.
    assert_eq!(run.receipts.len(), 1);
    assert_eq!(run.receipts[0].tool, "write_file");
    assert_eq!(run.receipts[0].outcome, "completed");
    assert!(run.receipts[0].admitted);
    // The observation round-tripped: the second request carries more messages.
    let captured = sim.captured_requests();
    assert_eq!(captured.len(), 2);
    assert!(captured[1].message_count > captured[0].message_count);

    // The result serializes to the structured JSON shape the CLI prints.
    let json = serde_json::to_value(&run).unwrap();
    assert_eq!(json["task_id"], "t-7");
    assert_eq!(json["exit_code"], 0);
    assert_eq!(json["status"], "completed");
    assert_eq!(json["delivery"]["kind"], "pr-opened");
}

#[tokio::test]
async fn attempt_budget_exhaustion_is_typed_and_never_retries_forever() {
    // The model never produces a parseable action: every attempt burns its
    // format-error budget, then the attempt/re-plan budgets stop the run.
    let script = vec![PlannedResponse::NonStreaming {
        content: "no action block here".to_string(),
    }];
    let (run, sim, _worktree, _bash) = drive(script, BashMode::Ok).await;

    assert_eq!(run.status, RunStatus::Failed);
    assert_eq!(run.exit_code, 1);
    let failure = run.failure.unwrap();
    assert_eq!(failure.class, FailureClass::AttemptBudgetExhausted);
    assert_eq!(failure.reason, "no-action-block");
    assert_eq!(run.attempts, 3);
    assert_eq!(run.replans, 2);
    // 3 attempts x (2 tolerated format errors + 1 failing call) = 9 model
    // calls exactly; the 10th never happens (no infinite retry).
    assert_eq!(run.model_calls, 9);
    assert_eq!(sim.captured_requests().len(), 9);
}

#[tokio::test]
async fn tool_outside_the_four_tool_set_is_refused_recorded_and_the_loop_continues() {
    let script = vec![
        PlannedResponse::NonStreaming {
            content: action_text(&json!({
                "tool": "delete_everything",
                "path": "/"
            })),
        },
        finish_action(true),
    ];
    let (run, sim, _worktree, _bash) = drive(script, BashMode::Ok).await;

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.receipts.len(), 1);
    let receipt = &run.receipts[0];
    assert_eq!(receipt.tool, "delete_everything");
    assert!(!receipt.admitted);
    assert_eq!(receipt.outcome, "refused");
    assert_eq!(receipt.reason, Some("unknown-tool"));
    // The refusal was fed back and the model recovered on the next turn.
    assert_eq!(sim.captured_requests().len(), 2);
}

#[tokio::test]
async fn mediated_bash_unavailable_fails_closed_per_the_preflight_contract() {
    let script = vec![PlannedResponse::NonStreaming {
        content: action_text(&json!({
            "tool": "bash",
            "script": "echo hi"
        })),
    }];
    let (run, _sim, _worktree, bash) = drive(script, BashMode::Refused).await;

    assert_eq!(run.status, RunStatus::Failed);
    let failure = run.failure.unwrap();
    assert_eq!(failure.class, FailureClass::MediationRefused);
    assert_eq!(failure.reason, "mediation-context");
    // Fail closed and run-fatal: exactly one bash attempt, no re-plan loop.
    assert_eq!(run.attempts, 1);
    assert_eq!(*bash.calls.lock().unwrap(), 1);
    assert_eq!(run.receipts.len(), 1);
    assert_eq!(run.receipts[0].tool, "bash");
    assert_eq!(run.receipts[0].outcome, "failed");
}

#[tokio::test]
async fn provider_failure_retries_with_backoff_then_recovers() {
    let script = vec![
        PlannedResponse::Failure {
            status: 429,
            body: json!({"error": {"message": "rate limited", "type": "rate_limit"}}),
        },
        finish_action(true),
    ];
    let (run, sim, _worktree, _bash) = drive(script, BashMode::Ok).await;

    assert_eq!(run.status, RunStatus::Completed);
    // One failed call + one retried call; the retry used the configured
    // (test-fast) backoff schedule.
    assert_eq!(run.model_calls, 2);
    assert_eq!(sim.captured_requests().len(), 2);
}

#[tokio::test]
async fn model_declared_incompletion_replans_then_exhausts() {
    let script = vec![finish_action(false)];
    let (run, _sim, _worktree, _bash) = drive(script, BashMode::Ok).await;

    assert_eq!(run.status, RunStatus::Failed);
    let failure = run.failure.unwrap();
    assert_eq!(failure.class, FailureClass::AttemptBudgetExhausted);
    assert_eq!(failure.reason, "task-not-completed");
    assert_eq!(run.attempts, 3);
    assert_eq!(run.replans, 2);
    assert!(run.delivery.is_none(), "failed runs never reach delivery");
}

#[tokio::test]
async fn pending_delivery_sink_reports_pending_for_the_bootstrap() {
    // The production sink never fabricates a PR ref in this slice.
    let sink = PendingDeliverySink;
    let outcome = sink
        .deliver(&DeliveryRequest {
            task_id: "t-1".to_string(),
            summary: "s".to_string(),
            untrusted_writes: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(
        outcome,
        DeliveryOutcome::Pending {
            reason: "delivery-path-not-wired"
        }
    );
}

#[test]
fn fixture_task_source_loads_the_cli_shape() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("leaf-1.json"),
        r#"{"id": "leaf-1", "slug": "leaf-1", "description": "do it"}"#,
    )
    .unwrap();
    let task = FixtureTaskSource::new(temp.path().to_path_buf())
        .load("leaf-1")
        .unwrap();
    assert_eq!(task.id, "leaf-1");
}

#[tokio::test]
async fn progress_beats_fire_per_turn_and_per_dispatch() {
    // Script: turn 1 dispatches bash (through the yielding fixture), turn 2
    // finishes. Expected beats: turn-1 boundary, pre-dispatch, turn-2
    // boundary (finish dispatches nothing) — three beats, increasing.
    let script = vec![
        PlannedResponse::NonStreaming {
            content: action_text(&json!({
                "tool": "bash",
                "script": "echo fixture-beat"
            })),
        },
        finish_action(true),
    ];
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    let task = fixture_task("t-beat");
    let (tx, mut rx) = tokio::sync::watch::channel(0_u64);
    let counter = tokio::spawn(async move {
        let mut beats = Vec::new();
        // The channel closes when the test drops the sender after the run;
        // each beat lands between worker awaits, so none is overwritten.
        while rx.changed().await.is_ok() {
            beats.push(*rx.borrow_and_update());
        }
        beats
    });
    let config = WorkerConfig::new(
        ProviderId::from_string("neuralwatt".to_string()),
        ModelId::from_string("glm-5.2".to_string()),
        fast_budgets(),
    )
    .with_progress(tx);
    let run = run_worker(
        &task,
        worktree.path(),
        &transport,
        &FixtureSlowBash,
        &FixtureDelivery,
        &config,
    )
    .await
    .unwrap();
    drop(config);

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!(run.turns, 2);
    assert_eq!(
        counter.await.unwrap(),
        vec![1, 2, 3],
        "one beat per turn + one per dispatch"
    );
}

#[tokio::test]
async fn no_progress_channel_still_completes() {
    // Default config emits nothing; the beat path must be inert.
    let script = vec![finish_action(true)];
    let (run, sim, worktree, _bash) = drive(script, BashMode::Ok).await;
    assert_eq!(run.status, RunStatus::Completed);
    drop(sim);
    drop(worktree);
}
