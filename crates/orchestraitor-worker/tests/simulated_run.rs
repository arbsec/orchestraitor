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
    /// Yields during dispatch so the supervisor can observe the preceding beat.
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

/// Checks increasing progress beats at each model turn and before tool dispatch.
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

/// Checks that omitting the optional progress channel preserves worker completion.
#[tokio::test]
async fn no_progress_channel_still_completes() {
    // Default config emits nothing; the beat path must be inert.
    let script = vec![finish_action(true)];
    let (run, sim, worktree, _bash) = drive(script, BashMode::Ok).await;
    assert_eq!(run.status, RunStatus::Completed);
    drop(sim);
    drop(worktree);
}

#[tokio::test]
async fn cost_entries_recorded_per_model_call_with_attribution() {
    // One write turn + one finish turn: two model calls, two distinct rows.
    let sim = serve(vec![
        PlannedResponse::NonStreaming {
            content: action_text(&json!({
                "tool": "write_file",
                "path": "src/cost.txt",
                "content": "cost probe\n"
            })),
        },
        finish_action(true),
    ])
    .await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    let bash = FixtureBash {
        mode: BashMode::Ok,
        calls: std::sync::Mutex::new(0),
    };
    let task = fixture_task("t-cost");
    let captured: std::sync::Arc<std::sync::Mutex<Vec<orchestraitor_cost_ledger::CostEntry>>> =
        std::sync::Arc::default();
    let sink: std::sync::Arc<dyn orchestraitor_provider_neuralwatt::CostSink> =
        std::sync::Arc::new(SharedLedgerSink {
            captured: std::sync::Arc::clone(&captured),
        });
    let config = WorkerConfig::new(
        ProviderId::from_string("neuralwatt".to_string()),
        ModelId::from_string("glm-5.2".to_string()),
        fast_budgets(),
    )
    .with_cost_tracking(
        orchestraitor_provider_neuralwatt::cost::CostAttribution {
            agent_domain_id: orchestraitor_model::AgentId::from_string("t-cost".to_string()),
            role: "implement".to_string(),
            project: "test".to_string(),
            session: orchestraitor_model::SessionId::from_string("sess-t-cost".to_string()),
            repository: orchestraitor_model::RepositoryId::from_string("repo".to_string()),
        },
        sink,
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

    // One cost row per model call, each with a unique request_id (the
    // ledger primary key would silently drop duplicates).
    assert!(run.usage.input_tokens + run.usage.output_tokens > 0);
    let entries = captured.lock().unwrap().clone();
    assert_eq!(entries.len(), 2, "one cost row per model call");
    assert_eq!(entries[0].agent_domain_id.as_str(), "t-cost");
    assert_eq!(entries[0].role, "implement");
    assert_eq!(entries[0].session.as_str(), "sess-t-cost");
    assert_eq!(
        entries
            .iter()
            .map(|e| e.request_id.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2,
        "request ids must be unique per call"
    );
    let total_in: u64 = entries.iter().map(|e| e.input_tokens).sum();
    let total_out: u64 = entries.iter().map(|e| e.output_tokens).sum();
    assert_eq!(total_in, run.usage.input_tokens);
    assert_eq!(total_out, run.usage.output_tokens);
    assert!(entries.iter().all(|e| e.request_count == 1));
}

/// Shared in-memory sink for attribution assertions: records entries and
/// hands them back through the same Arc the config holds.
struct SharedLedgerSink {
    captured: std::sync::Arc<std::sync::Mutex<Vec<orchestraitor_cost_ledger::CostEntry>>>,
}

impl orchestraitor_provider_neuralwatt::CostSink for SharedLedgerSink {
    fn record(&self, entry: &orchestraitor_cost_ledger::CostEntry) -> Result<(), String> {
        self.captured.lock().unwrap().push(entry.clone());
        Ok(())
    }
}

fn guardrail_config(
    budgets: WorkerBudgets,
    guardrails: orchestraitor_worker::GuardrailsConfig,
) -> WorkerConfig {
    let mut config = WorkerConfig::new(
        ProviderId::from_string("neuralwatt".to_string()),
        ModelId::from_string("glm-5.2".to_string()),
        budgets,
    );
    config.guardrails = guardrails;
    config
}

/// The mktemp incident shape: K repeated poll-free bash calls with an
/// interleaved second command. Fires `FailureClass::ToolLoopChurn`.
#[tokio::test]
async fn tool_loop_churn_kills_the_attempt_with_the_typed_class() {
    let mut script = Vec::new();
    // 4x mktemp + 3x printf interleaved (7 turns); K=4, W=8 fires on turn 7.
    for _ in 0..3 {
        script.push(PlannedResponse::NonStreaming {
            content: action_text(&json!({"tool": "bash", "script": "mktemp -d"})),
        });
        script.push(PlannedResponse::NonStreaming {
            content: action_text(&json!({"tool": "bash", "script": "printf x > /dev/null"})),
        });
    }
    script.push(PlannedResponse::NonStreaming {
        content: action_text(&json!({"tool": "bash", "script": "mktemp -d"})),
    });
    script.push(finish_action(true));
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    let task = fixture_task("t-churn");
    let mut guardrails = orchestraitor_worker::GuardrailsConfig::bootstrap_defaults();
    guardrails.no_progress_turns = 0; // isolate the churn guard
    let config = guardrail_config(fast_budgets(), guardrails);
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
    assert_eq!(run.status, RunStatus::Failed);
    let failure = run.failure.expect("typed failure");
    assert_eq!(failure.class, FailureClass::ToolLoopChurn);
    assert_eq!(failure.reason, "tool-loop-churn");
    // The re-plan loop must NOT rescue a churn kill: it is attempt-fatal.
    assert_eq!(run.attempts, 1);
}

/// Distinct-but-real work never churn-fires: 10 distinct bash commands and
/// a `write_file` rewrite pattern complete normally.
#[tokio::test]
async fn normal_tool_diversity_completes_without_churn() {
    let mut script = Vec::new();
    for i in 0..10 {
        script.push(PlannedResponse::NonStreaming {
            content: action_text(&json!({"tool": "bash", "script": format!("cmd{i} --flag")})),
        });
    }
    script.push(finish_action(true));
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    let task = fixture_task("t-diverse");
    let mut guardrails = orchestraitor_worker::GuardrailsConfig::bootstrap_defaults();
    guardrails.no_progress_turns = 0;
    let config = guardrail_config(fast_budgets(), guardrails);
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
    assert_eq!(run.status, RunStatus::Completed);
}

/// A no-progress run (repeated no-op reads over an unchanged worktree)
/// fires `FailureClass::NoProgress` at the configured streak.
#[tokio::test]
async fn no_progress_fingerprint_streak_fails_the_attempt() {
    let mut script = Vec::new();
    // 4 read_file turns over the same file — the worktree never changes, so
    // the 3rd consecutive identical fingerprint fires (threshold 3).
    for _ in 0..4 {
        script.push(PlannedResponse::NonStreaming {
            content: action_text(&json!({"tool": "read_file", "path": "README.md"})),
        });
    }
    script.push(finish_action(true));
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    std::fs::write(worktree.path().join("README.md"), "fixture").unwrap();
    let task = fixture_task("t-noprogress");
    let guardrails = orchestraitor_worker::GuardrailsConfig {
        no_progress_turns: 3,
        tool_repeat_count: 0, // isolate the no-progress guard
        tool_repeat_window: 0,
        ci_poll_budget: orchestraitor_worker::GuardrailsConfig::bootstrap_defaults().ci_poll_budget,
    };
    let config = guardrail_config(fast_budgets(), guardrails);
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
    assert_eq!(run.status, RunStatus::Failed);
    let failure = run.failure.expect("typed failure");
    assert_eq!(failure.class, FailureClass::NoProgress);
    assert_eq!(failure.reason, "no-progress");
}

/// Real file writes reset the fingerprint: a write-then-read-then-finish
/// run of the same length never fires no-progress.
#[tokio::test]
async fn real_progress_resets_the_fingerprint_streak() {
    // Threshold 3: write(fp A) -> read(fp A, streak 2) -> write(fp B, reset)
    // -> finish. A no-op turn keeps the streak, real work resets it.
    let script = vec![
        PlannedResponse::NonStreaming {
            content: action_text(&json!({
                "tool": "write_file",
                "path": "out.txt",
                "content": "step one"
            })),
        },
        PlannedResponse::NonStreaming {
            content: action_text(&json!({"tool": "read_file", "path": "out.txt"})),
        },
        PlannedResponse::NonStreaming {
            content: action_text(&json!({
                "tool": "write_file",
                "path": "out.txt",
                "content": "step two"
            })),
        },
        finish_action(true),
    ];
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    let task = fixture_task("t-progress");
    let guardrails = orchestraitor_worker::GuardrailsConfig {
        no_progress_turns: 3,
        tool_repeat_count: 0,
        tool_repeat_window: 0,
        ci_poll_budget: orchestraitor_worker::GuardrailsConfig::bootstrap_defaults().ci_poll_budget,
    };
    let config = guardrail_config(fast_budgets(), guardrails);
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
    assert_eq!(
        run.status,
        RunStatus::Completed,
        "failure: {:?}",
        run.failure
    );
}

/// Fixture mediator that actually sleeps the script's first `sleep N`
/// argument (a bounded stand-in for the mediated interpreter).
struct FixtureSleepingBash;

#[async_trait]
impl BashMediator for FixtureSleepingBash {
    async fn run_bash(&self, script: &str) -> Result<MediatedRun, MediationError> {
        // Parse `sleep <secs>` and honor it (capped at 50ms in tests).
        let duration = script
            .split_whitespace()
            .nth(1)
            .and_then(|secs| secs.parse::<f64>().ok())
            .unwrap_or(0.0)
            .min(0.05);
        tokio::time::sleep(std::time::Duration::from_secs_f64(duration)).await;
        Ok(MediatedRun {
            exit_code: Some(0),
            stdout: b"fixture-bash-ok\n".to_vec(),
            stderr: Vec::new(),
        })
    }
}

/// Poll-shaped bash (sleep + `gh pr checks`) accumulates wall-clock; once
/// the cumulative budget is exceeded the attempt fails
/// `PollBudgetExhausted`. Deterministic: the fixture's wall-clock is real,
/// so the test uses a 1ms budget and sleep scripts the model cannot shorten.
#[tokio::test]
async fn poll_budget_exhaustion_fails_the_attempt_typed() {
    // 6 poll-shaped turns: each dispatch sleeps ~3ms; the 10ms budget
    // fires on one of the middle charges.
    let mut script = Vec::new();
    for _ in 0..6 {
        script.push(PlannedResponse::NonStreaming {
            content: action_text(&json!({
                "tool": "bash",
                "script": "sleep 0.003 && gh pr checks 42"
            })),
        });
    }
    script.push(finish_action(true));
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    let task = fixture_task("t-poll");
    let guardrails = orchestraitor_worker::GuardrailsConfig {
        no_progress_turns: 0,
        tool_repeat_count: 0,
        tool_repeat_window: 0,
        ci_poll_budget: std::time::Duration::from_millis(10),
    };
    let config = guardrail_config(fast_budgets(), guardrails);
    let run = run_worker(
        &task,
        worktree.path(),
        &transport,
        &FixtureSleepingBash,
        &FixtureDelivery,
        &config,
    )
    .await
    .unwrap();
    assert_eq!(run.status, RunStatus::Failed, "failure: {:?}", run.failure);
    let failure = run.failure.expect("typed failure");
    assert_eq!(failure.class, FailureClass::PollBudgetExhausted);
    assert_eq!(failure.reason, "ci-poll-budget-exhausted");
}

/// Non-poll bash (plain command work) never charges the budget even when
/// it runs long: the heuristic is bounded by design.
#[tokio::test]
async fn non_poll_bash_never_charges_the_poll_budget() {
    let mut script = Vec::new();
    for _ in 0..5 {
        script.push(PlannedResponse::NonStreaming {
            content: action_text(&json!({
                "tool": "bash",
                "script": "echo working"
            })),
        });
    }
    script.push(finish_action(true));
    let sim = serve(script).await;
    let transport = test_transport(sim.base_url());
    let worktree = tempfile::tempdir().unwrap();
    let task = fixture_task("t-nopoll");
    let guardrails = orchestraitor_worker::GuardrailsConfig {
        no_progress_turns: 0,
        tool_repeat_count: 0,
        tool_repeat_window: 0,
        ci_poll_budget: std::time::Duration::from_millis(1),
    };
    let config = guardrail_config(fast_budgets(), guardrails);
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
    assert_eq!(
        run.status,
        RunStatus::Completed,
        "failure: {:?}",
        run.failure
    );
}
