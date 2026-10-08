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
use crate::mediator::BashMediator;
use crate::model::{
    RunState, SubsessionEvent, SubsessionEventOutcome, SubsessionEventRefusal, call_model,
};
use crate::prompt::task_prompt;
use crate::result::{FailureClass, RunStatus, TypedFailure, UsageTotals, WorkerConfig, WorkerRun};
use crate::subsession::{SubsessionOutcome, SubsessionParent, run_subsession};
use crate::task::WorkerTask;
use crate::tooldef::ToolMechanism;
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
    let policy = crate::tooldef::ToolPolicy {
        declared: config.tools.clone(),
        allowed_internal: config.subsession_allowed_internal.clone(),
        subsession_depth: config.subsession_depth,
    };
    let mut executor = ToolExecutor::new(&root, bash).with_policy(policy);
    let budgets = &config.budgets;
    let deadline = budgets.run_deadline();
    let started = Instant::now();
    let mut state = RunState {
        turns: 0,
        model_calls: 0,
        usage: UsageTotals::default(),
        spend_soft_cap_exceeded: false,
        progress_beat: 0,
        subsession_events: Vec::new(),
        started,
        worktree_root: root.clone(),
        project: "local".to_string(),
        repository: root.display().to_string(),
        session_id: format!("run-{}", task.id),
        transport,
        progress: config.progress.clone(),
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
    reason = "the attempt loop's budget + declared-tool branches are the audited surface; splitting them would scatter the exhaustion semantics the reviewer must see together"
)]
async fn run_attempt(
    task: &WorkerTask,
    transport: &dyn ProviderTransport,
    delivery: &dyn DeliverySink,
    config: &WorkerConfig,
    executor: &mut ToolExecutor<'_>,
    state: &mut RunState<'_>,
    replan_note: Option<&'static str>,
    started: Instant,
) -> AttemptOutcome {
    let budgets = &config.budgets;
    let mut messages = vec![
        ModelMessage {
            role: MessageRole::System,
            content: crate::prompt::system_prompt(config),
        },
        ModelMessage {
            role: MessageRole::User,
            content: task_prompt(task, replan_note),
        },
    ];
    let mut format_errors = 0_u32;
    let mut last_progress = Instant::now();
    let mut turns_this_attempt = 0_u32;

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
                // Declared-tool subagent dispatch (issue #535, T3): handled
                // HERE, not in the executor — the child await needs the
                // transport and the run context, and the beats below must
                // cover the child window (the supervision-gap fix).
                if let WorkerAction::DeclaredTool { tool_id, question } = &action {
                    let tool = config.tools.iter().find(|tool| &tool.id == tool_id);
                    let turn = match tool.map(|tool| &tool.mechanism) {
                        Some(ToolMechanism::Subagent { .. }) => {
                            let outcome = dispatch_subagent(
                                config,
                                state,
                                executor,
                                tool_id,
                                question.as_deref(),
                            )
                            .await;
                            crate::tools::ToolTurn {
                                observation: outcome,
                                mediation_failure: None,
                            }
                        }
                        _ => executor.dispatch(&action).await,
                    };
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
                    continue;
                }
                let turn = executor.dispatch(&action).await;
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
            }
        }
    }
}

fn attempt_failure(class: FailureClass, reason: &'static str) -> AttemptOutcome {
    AttemptOutcome::Failed(TypedFailure { class, reason })
}

/// Runs one declared subagent tool as a sub-session (issue #535, T3) and
/// renders the model-facing observation. Beats around the child await are
/// emitted by [`run_subsession`](crate::subsession::run_subsession) so the
/// supervisor's staleness detection covers the child window.
///
/// Refusals (role unrouted, budget carve impossible) are observation text
/// fed back to the model, never run-fatal: the parent can re-plan around a
/// failed auxiliary question. The child's typed failure rides the outcome
/// into the observation. A parent-side receipt records the dispatch so the
/// run's receipt stream is complete (the child's own receipts ride the
/// outcome/decision record).
async fn dispatch_subagent(
    config: &WorkerConfig,
    state: &mut RunState<'_>,
    executor: &mut ToolExecutor<'_>,
    tool_id: &str,
    question: Option<&str>,
) -> String {
    let Some(tool) = config.tools.iter().find(|tool| tool.id == tool_id) else {
        state
            .subsession_events
            .push(SubsessionEvent::Refusal(SubsessionEventRefusal {
                tool_id: tool_id.to_string(),
                reason: "tool-not-allowed",
            }));
        executor.record_subagent_refusal(tool_id, "tool-not-allowed");
        return "tool call refused: tool-not-allowed".to_string();
    };
    let ToolMechanism::Subagent { role, .. } = &tool.mechanism else {
        state
            .subsession_events
            .push(SubsessionEvent::Refusal(SubsessionEventRefusal {
                tool_id: tool_id.to_string(),
                reason: "not-a-subagent-tool",
            }));
        executor.record_subagent_refusal(tool_id, "not-a-subagent-tool");
        return "tool call refused: not-a-subagent-tool".to_string();
    };
    let Some((provider, model)) = config.subsession_routing.get(role) else {
        state
            .subsession_events
            .push(SubsessionEvent::Refusal(SubsessionEventRefusal {
                tool_id: tool_id.to_string(),
                reason: "subsession-role-unrouted",
            }));
        executor.record_subagent_refusal(tool_id, "subsession-role-unrouted");
        return format!("tool call refused: subsession-role-unrouted ({role})");
    };
    // The parent context: the run's ACTUAL remaining wall clock (deadline
    // minus elapsed; the never-incremented `subsession_wall_secs` counter
    // cannot see the time burned inside the child await — CR finding #1),
    // depth, and attribution labels. The deadline carve computes from the
    // tool budget inside run_subsession.
    let remaining = config
        .budgets
        .run_deadline()
        .saturating_sub(state.started.elapsed());
    let parent = SubsessionParent {
        worktree_root: state.worktree_root.clone(),
        remaining,
        depth: config.subsession_depth,
        prior_daily_spend_usd: config.prior_daily_spend_usd,
        role: role.clone(),
        project: state.project.clone(),
        repository: state.repository.clone(),
        session_id: state.session_id.clone(),
        attribution: config.attribution.clone(),
        cost_sink: config.cost_sink.clone(),
    };
    match Box::pin(run_subsession(
        &parent,
        tool,
        question,
        state.transport,
        (provider.as_str(), model.as_str()),
        state.progress.as_ref(),
    ))
    .await
    {
        Ok(outcome) => {
            executor.record_subagent_completion(tool_id, outcome.status == RunStatus::Completed);
            state
                .subsession_events
                .push(SubsessionEvent::Outcome(SubsessionEventOutcome {
                    tool_id: tool_id.to_string(),
                    role: role.clone(),
                    provider: provider.clone(),
                    model: model.clone(),
                    status: outcome.status,
                    usage: outcome.usage,
                    routing: outcome.routing.clone(),
                }));
            // The child's spend belongs to the parent's day (CR finding #3):
            // usage aggregates into the run totals and re-evaluates the soft
            // cap, so a sub-session can push the parent over the cap.
            aggregate_child_usage(config, state, outcome.usage);
            // Child writes surface on the parent's receipt trail via the
            // outcome (CR finding #2): the child's own writes ride
            // `outcome.untrusted_writes` into the parent's untrusted-output
            // list so the delivery seam sees every worktree mutation.
            for path in &outcome.untrusted_writes {
                executor.record_child_write(path);
            }
            render_subsession_summary(tool_id, tool.budget.max_result_bytes, &outcome)
        }
        Err(error) => {
            executor.record_subagent_refusal(tool_id, "subsession-spawn-failed");
            state
                .subsession_events
                .push(SubsessionEvent::Refusal(SubsessionEventRefusal {
                    tool_id: tool_id.to_string(),
                    reason: "subsession-spawn-failed",
                }));
            format!("subsession spawn failed closed: {error}")
        }
    }
}

/// Aggregates one child run's token usage into the parent state and
/// re-evaluates the daily spend soft cap (CR finding #3): a sub-session's
/// spend is the parent's spend — it must be able to push the parent over
/// the cap.
fn aggregate_child_usage(config: &WorkerConfig, state: &mut RunState, child: UsageTotals) {
    state.usage.input_tokens += child.input_tokens;
    state.usage.output_tokens += child.output_tokens;
    let total = state.usage.input_tokens + state.usage.output_tokens;
    #[expect(
        clippy::cast_precision_loss,
        reason = "token counts are far below 2^53; the estimate only feeds a soft-cap comparison"
    )]
    let estimated = total as f64 * config.budgets.usd_per_token_estimate;
    let spent = config.prior_daily_spend_usd + estimated;
    if spent > config.budgets.daily_spend_soft_cap_usd {
        state.spend_soft_cap_exceeded = true;
    }
}

/// Renders the model-facing observation for a completed sub-session spawn.
/// The COMPLETE parent-visible wrapper (completion prefix + summary +
/// truncation markers) is capped at `max_result_bytes` bytes on a char
/// boundary: marker/prefix headroom is reserved before the child summary is
/// truncated, and the assembled wrapper is re-capped defensively.
pub(crate) fn render_subsession_summary(
    tool_id: &str,
    max_result_bytes: u64,
    outcome: &SubsessionOutcome,
) -> String {
    const MARKER: &str = "\n[truncated]";
    let cap = usize::try_from(max_result_bytes).unwrap_or(usize::MAX);
    let mut rendered = if let Some(summary) = &outcome.summary {
        let prefix = format!("[subsession '{tool_id}' completed]\n");
        // Reserve headroom for the prefix and the truncation marker so
        // the child summary cut leaves room for everything around it.
        let body_budget = cap.saturating_sub(prefix.len() + MARKER.len());
        let body = crate::search::truncate_bytes(summary, body_budget);
        format!("{prefix}{body}")
    } else {
        // The failure branch caps identically to the success branch (CR
        // finding: the failure reason is child-controlled text too): the
        // prefix, reason, and marker must all fit within `max_result_bytes`.
        let prefix = format!("[subsession '{tool_id}' failed: ");
        let reason = outcome.failure.as_ref().map_or("none", |f| f.reason);
        // Reserve headroom for the prefix and the closing `"]"` before
        // cutting the reason, so the assembled wrapper stays inside the cap.
        let reason_budget = cap.saturating_sub(prefix.len() + 1);
        let body = crate::search::truncate_bytes(reason, reason_budget);
        format!("{prefix}{body}]")
    };
    if rendered.len() > cap {
        // Reserve room for the marker: the cut leaves space so the
        // re-appended marker keeps the whole wrapper inside the cap.
        let mut cut = cap.saturating_sub(MARKER.len());
        while cut > 0 && !rendered.is_char_boundary(cut) {
            cut -= 1;
        }
        rendered.truncate(cut);
        rendered.push_str(MARKER);
    }
    rendered
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
