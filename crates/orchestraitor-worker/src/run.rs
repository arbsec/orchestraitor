//! The attempt-bounded one-shot worker loop (mini-swe-agent pattern).
//!
//! One run: task description in → bounded loop of model calls and tool
//! dispatches → structured [`WorkerRun`] out. The loop consumes a
//! [`ProviderTransport`] over the provider-api trait boundary, mediates every
//! tool call through the four-tool executor, and terminates with a typed
//! failure when any budget is exhausted — exhaustion is a result, never an
//! infinite retry (issue #310; spec `10-orchestrator.md` §9.24).

use std::path::Path;
use std::time::Instant;

use orchestraitor_provider_api::transport::{MessageRole, ModelMessage, ProviderTransport};
use tracing::{debug, warn};

use crate::action::{ActionRejection, WorkerAction, parse_action};
use crate::delivery::{DeliveryOutcome, DeliveryRequest, DeliverySink};
use crate::error::WorkerError;
use crate::guardrails::{
    ChurnWindow, NoProgressGuard, PollBudget, poll_shaped, progress_fingerprint, tool_call_shape,
};
use crate::mediator::BashMediator;
use crate::model::{RunState, call_model};
use crate::prompt::{SYSTEM_PROMPT, task_prompt};
use crate::result::{FailureClass, RunStatus, TypedFailure, UsageTotals, WorkerConfig, WorkerRun};
use crate::task::WorkerTask;
use crate::tools::ToolExecutor;

/// Internal outcome of one attempt.
enum AttemptOutcome {
    Completed {
        summary: String,
        delivery: DeliveryOutcome,
    },
    Failed(TypedFailure),
}

/// Runs one leaf task through the attempt-bounded worker loop.
///
/// `worktree` is the task worktree the file tools confine to. `bash` crosses
/// the Arbitraitor boundary via the mediator on every call; `delivery` is the
/// seam the completed task is handed to.
///
/// # Errors
///
/// Returns [`WorkerError`] only when no run can be produced at all (e.g. an
/// unusable worktree root). Classifiable failures — budget exhaustion,
/// mediation refusal, provider errors — are typed failures inside the
/// returned [`WorkerRun`], never a retry loop.
#[expect(
    clippy::too_many_lines,
    reason = "the attempt loop's budget branches are the audited surface of this slice; splitting them would scatter the exhaustion semantics the reviewer must see together"
)]
pub async fn run_worker(
    task: &WorkerTask,
    worktree: &Path,
    transport: &dyn ProviderTransport,
    bash: &dyn BashMediator,
    delivery: &dyn DeliverySink,
    config: &WorkerConfig,
) -> Result<WorkerRun, WorkerError> {
    let root = canonical_root(worktree)?;
    let mut executor = ToolExecutor::new(&root, bash);
    let budgets = &config.budgets;
    let deadline = budgets.run_deadline();
    let started = Instant::now();
    let mut state = RunState {
        turns: 0,
        model_calls: 0,
        usage: UsageTotals::default(),
        spend_soft_cap_exceeded: false,
        progress_beat: 0,
    };

    let mut attempts = 0_u32;
    let mut replans = 0_u32;
    let mut replan_note: Option<&'static str> = None;
    let outcome = loop {
        attempts += 1;
        debug!(task = %task.id, attempt = attempts, "worker attempt started");
        let attempt = run_attempt(
            task,
            transport,
            delivery,
            config,
            &mut executor,
            &mut state,
            replan_note,
            started,
        )
        .await;
        match attempt {
            AttemptOutcome::Completed { .. } => break attempt,
            AttemptOutcome::Failed(failure) => {
                // Mediation refusal is run-fatal: re-planning cannot restore a
                // missing sandbox control (fail closed, spec §6.7).
                if failure.class == FailureClass::MediationRefused {
                    break AttemptOutcome::Failed(failure);
                }
                // Anti-stuck guard kills are re-plan-fatal: a fresh re-plan
                // note cannot fix a loop the model is mechanistically stuck
                // in (churn, no progress, or an unbounded external poll).
                // They surface as their own typed class, not as an attempt
                // budget reclassification.
                if matches!(
                    failure.class,
                    FailureClass::ToolLoopChurn
                        | FailureClass::NoProgress
                        | FailureClass::PollBudgetExhausted
                ) {
                    break AttemptOutcome::Failed(failure);
                }
                if attempts >= budgets.max_attempts {
                    break AttemptOutcome::Failed(TypedFailure {
                        class: FailureClass::AttemptBudgetExhausted,
                        reason: failure.reason,
                    });
                }
                if replans >= budgets.max_replans {
                    break AttemptOutcome::Failed(TypedFailure {
                        class: FailureClass::ReplanBudgetExhausted,
                        reason: failure.reason,
                    });
                }
                replans += 1;
                replan_note = Some(failure.reason);
                if started.elapsed() > deadline {
                    break AttemptOutcome::Failed(TypedFailure {
                        class: FailureClass::WorkerTimeout,
                        reason: "worker-timeout",
                    });
                }
            }
        }
    };

    let (receipts, untrusted_writes) = executor.into_records();
    let run = match outcome {
        AttemptOutcome::Completed { summary, delivery } => WorkerRun {
            task_id: task.id.clone(),
            status: RunStatus::Completed,
            exit_code: 0,
            summary: Some(summary),
            failure: None,
            delivery: Some(delivery),
            attempts,
            replans,
            turns: state.turns,
            model_calls: state.model_calls,
            usage: state.usage,
            spend_soft_cap_exceeded: state.spend_soft_cap_exceeded,
            untrusted_writes,
            receipts,
            budgets: budgets.echo(),
        },
        AttemptOutcome::Failed(failure) => {
            warn!(task = %task.id, class = ?failure.class, reason = failure.reason, "worker run failed");
            WorkerRun {
                task_id: task.id.clone(),
                status: RunStatus::Failed,
                exit_code: 1,
                summary: None,
                failure: Some(failure),
                delivery: None,
                attempts,
                replans,
                turns: state.turns,
                model_calls: state.model_calls,
                usage: state.usage,
                spend_soft_cap_exceeded: state.spend_soft_cap_exceeded,
                untrusted_writes,
                receipts,
                budgets: budgets.echo(),
            }
        }
    };
    Ok(run)
}

/// One bounded conversation attempt.
#[expect(
    clippy::too_many_arguments,
    reason = "attempt state is threaded explicitly; grouping into a context struct would hide the data flow this thin slice needs to audit"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the turn body is the guard state machine (stall, turn bound, format errors, poll budget, churn window, no-progress fingerprint) read top-to-bottom in dispatch order; extracting arms would scatter the exhaustion semantics the reviewer must see together"
)]
async fn run_attempt(
    task: &WorkerTask,
    transport: &dyn ProviderTransport,
    delivery: &dyn DeliverySink,
    config: &WorkerConfig,
    executor: &mut ToolExecutor<'_>,
    state: &mut RunState,
    replan_note: Option<&'static str>,
    started: Instant,
) -> AttemptOutcome {
    let budgets = &config.budgets;
    let mut messages = vec![
        ModelMessage {
            role: MessageRole::System,
            content: SYSTEM_PROMPT.to_string(),
        },
        ModelMessage {
            role: MessageRole::User,
            content: task_prompt(task, replan_note),
        },
    ];
    let mut format_errors = 0_u32;
    let mut last_progress = Instant::now();
    let mut turns_this_attempt = 0_u32;

    // Anti-stuck guardrail state (spec `10-orchestrator.md` §9.36 detection): churn window,
    // no-progress fingerprint streak, and the CI-poll wall-clock budget.
    // All three are per-attempt; the wall-clock stall check above is
    // untouched and still owns the hang case.
    let guardrails = &config.guardrails;
    let mut churn = ChurnWindow::new(guardrails);
    let mut no_progress = NoProgressGuard::new(guardrails);
    let mut poll_budget = PollBudget::new(guardrails);
    let churn_enabled = guardrails.churn_enabled();
    let no_progress_enabled = guardrails.no_progress_enabled();

    loop {
        if started.elapsed() > budgets.run_deadline() {
            return attempt_failure(FailureClass::WorkerTimeout, "worker-timeout");
        }
        if last_progress.elapsed() > budgets.stall_timeout {
            return attempt_failure(FailureClass::Stalled, "stall-timeout");
        }
        if turns_this_attempt >= budgets.max_turns_per_attempt {
            return attempt_failure(FailureClass::TurnBudgetExhausted, "turn-budget");
        }
        turns_this_attempt += 1;
        state.turns += 1;
        emit_beat(config, state);
        let text = match call_model(transport, config, &messages, state).await {
            Ok(text) => text,
            Err(failure) => return AttemptOutcome::Failed(failure),
        };
        messages.push(ModelMessage {
            role: MessageRole::Assistant,
            content: text.clone(),
        });

        match parse_action(&text) {
            Err(ActionRejection::Malformed { reason }) => {
                format_errors += 1;
                if format_errors > budgets.max_consecutive_format_errors {
                    return attempt_failure(FailureClass::FormatErrorsExhausted, reason);
                }
                messages.push(ModelMessage {
                    role: MessageRole::User,
                    content: format!("response rejected: {reason}; respond with exactly one fenced json action block"),
                });
            }
            Err(ActionRejection::UnknownTool { name }) => {
                let turn = executor.record_refusal(&name, "unknown-tool");
                messages.push(ModelMessage {
                    role: MessageRole::User,
                    content: turn.observation,
                });
            }
            Ok(WorkerAction::Finish { summary, success }) => {
                if !success {
                    return attempt_failure(FailureClass::TaskNotCompleted, "task-not-completed");
                }
                let request = DeliveryRequest {
                    task_id: task.id.clone(),
                    summary: summary.clone(),
                    untrusted_writes: executor.untrusted_writes().to_vec(),
                };
                return match delivery.deliver(&request).await {
                    Ok(outcome) => AttemptOutcome::Completed {
                        summary,
                        delivery: outcome,
                    },
                    Err(error) => attempt_failure(FailureClass::DeliveryFailed, error.reason),
                };
            }
            Ok(action) => {
                emit_beat(config, state);
                // Poll-shaped bash (sleep + CI wait) is charged and bounded
                // BEFORE the dispatch it requested: the wait's wall-clock
                // burns inside the dispatch, so exceeding the budget stops
                // the attempt at the turn that asked for it (spec §21.10:
                // a bounded wait budget + parked task, never an unbounded
                // poll loop).
                let is_poll = match &action {
                    WorkerAction::Bash { script } => poll_shaped(script),
                    _ => false,
                };
                let dispatch_started = Instant::now();
                // The budget BOUNDS the dispatch, not just the accounting:
                // a poll-shaped Bash call is cancelled at the remaining
                // budget (the mediated path's kill-on-drop guard reaps the
                // process group), so the wait's wall-clock can never
                // overrun the budget — the charge below then observes the
                // elapse and fails the attempt typed (spec §21.10).
                let turn = if is_poll {
                    match poll_budget.remaining() {
                        Some(remaining) => {
                            match tokio::time::timeout(remaining, executor.dispatch(&action)).await
                            {
                                Ok(turn) => turn,
                                Err(_elapsed) => {
                                    // The dispatch consumed the remaining
                                    // budget: record the elapse and fail the
                                    // attempt typed. `charge` can report
                                    // false on the exact-boundary case (spent
                                    // == budget, not >), but the timeout
                                    // proves the budget is gone either way —
                                    // report the exhaustion unconditionally.
                                    let _fired = poll_budget.charge(remaining);
                                    return attempt_failure(
                                        FailureClass::PollBudgetExhausted,
                                        "ci-poll-budget-exhausted",
                                    );
                                }
                            }
                        }
                        None => executor.dispatch(&action).await,
                    }
                } else {
                    executor.dispatch(&action).await
                };
                if guardrails.poll_budget_enabled()
                    && is_poll
                    && poll_budget.charge(dispatch_started.elapsed())
                {
                    return attempt_failure(
                        FailureClass::PollBudgetExhausted,
                        "ci-poll-budget-exhausted",
                    );
                }
                let mediation_failure = turn.mediation_failure;
                messages.push(ModelMessage {
                    role: MessageRole::User,
                    content: turn.observation,
                });
                last_progress = Instant::now();
                format_errors = 0;
                if let Some(reason) = mediation_failure {
                    return attempt_failure(FailureClass::MediationRefused, reason);
                }
                // Post-dispatch churn + no-progress guards: the newest
                // receipt drives the churn window; the worktree fingerprint
                // drives no-progress. Both run before the next model call so
                // a stuck loop stops instead of burning the turn budget.
                if let Some(receipt) = executor.last_receipt() {
                    if churn_enabled && churn.observe(tool_call_shape(&action, receipt)) {
                        return attempt_failure(FailureClass::ToolLoopChurn, "tool-loop-churn");
                    }
                    if no_progress_enabled
                        && no_progress.observe(progress_fingerprint(
                            executor.root_path(),
                            executor.untrusted_writes(),
                        ))
                    {
                        return attempt_failure(FailureClass::NoProgress, "no-progress");
                    }
                }
            }
        }
    }
}

fn attempt_failure(class: FailureClass, reason: &'static str) -> AttemptOutcome {
    AttemptOutcome::Failed(TypedFailure { class, reason })
}

/// Sends one progress beat on the optional supervisor channel.
///
/// Two emit sites exist (`run.rs` turn boundary and pre-dispatch) so that
/// supervisor-side stall detection (issue #314) matches the worker-internal
/// stall definition exactly: the internal check resets on tool dispatch and
/// cannot fire while a `call_model` await hangs or a tool call runs long, so
/// the beats must cover both windows. A send error means every receiver was
/// dropped — the supervisor stopped watching (shutdown/abort) — and is never
/// a reason for the worker to stop.
fn emit_beat(config: &WorkerConfig, state: &mut RunState) {
    state.progress_beat = state.progress_beat.wrapping_add(1);
    if let Some(sender) = &config.progress {
        let _ = sender.send(state.progress_beat);
    }
}

/// Canonicalizes the worktree root, failing closed when it cannot be used.
fn canonical_root(worktree: &Path) -> Result<std::path::PathBuf, WorkerError> {
    if !worktree.exists() {
        return Err(WorkerError::WorktreeRoot { reason: "missing" });
    }
    if !worktree.is_dir() {
        return Err(WorkerError::WorktreeRoot {
            reason: "not-a-directory",
        });
    }
    worktree
        .canonicalize()
        .map_err(|_| WorkerError::WorktreeRoot {
            reason: "canonicalize-io",
        })
}
