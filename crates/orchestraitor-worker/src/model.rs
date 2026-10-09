//! One model call against the `ProviderTransport` boundary: bounded retries
//! (backoff per issue #310), event folding, usage accrual, and the daily spend
//! soft-cap check.

use orchestraitor_cost_ledger::{CostEntry, MonetaryCostBasis};
use orchestraitor_provider_api::ProviderTransportError;
use orchestraitor_provider_api::transport::{
    ModelEvent, ModelEventStream, ModelMessage, ModelRequest, ProviderTransport, TokenCount,
};
use serde::Serialize;
use std::sync::atomic::Ordering;
use tracing::debug;

use crate::budget::backoff_delay;
use crate::result::{FailureClass, RunStatus, TypedFailure, UsageTotals, WorkerConfig};

/// Mutable run state threaded through model calls (shared with the loop).
pub(super) struct RunState<'a> {
    /// Model-call turns consumed across all attempts.
    pub(super) turns: u32,
    /// Model calls issued (including retried calls).
    pub(super) model_calls: u32,
    /// Token usage totals.
    pub(super) usage: UsageTotals,
    /// Whether the daily spend soft cap was exceeded.
    pub(super) spend_soft_cap_exceeded: bool,
    /// Monotonic progress-beat counter sent on the optional supervisor
    /// channel (issue #314); opaque sequence number, one increment per emit.
    pub(super) progress_beat: u64,
    /// Sub-session decision events (issue #535, T3): one per subagent tool
    /// invocation, carried on the run result for decision records and cost
    /// attribution.
    pub(super) subsession_events: Vec<SubsessionEvent>,
    /// When THIS run started: the parent's remaining wall clock for the
    /// sub-session deadline carve is `run_deadline − elapsed`, measured from
    /// here (CR finding #1: a counter that never increments cannot bound the
    /// total run).
    pub(super) started: std::time::Instant,
    /// Run-context labels the sub-session parent context needs.
    pub(super) worktree_root: std::path::PathBuf,
    pub(super) project: String,
    pub(super) repository: String,
    pub(super) session_id: String,
    /// The transport the sub-session runs on (the same provider transport
    /// the parent loop holds; the child's model comes from the routing
    /// map).
    pub(super) transport: &'a dyn ProviderTransport,
    /// The parent's beat channel, forwarded so child-window beats land on
    /// the same supervisor stream.
    pub(super) progress: Option<tokio::sync::watch::Sender<u64>>,
}

/// One sub-session outcome event (decision-record shape, issue #535 T3).
/// Carried on the run state AND the run result: the parent's decision
/// records carry the routing and effort evidence, and the outcome events
/// surface on the `WorkerRun` JSON.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubsessionEvent {
    /// A spawn completed (or failed as a typed child run).
    Outcome(Box<SubsessionEventOutcome>),
    /// A spawn was refused before any run existed.
    Refusal(SubsessionEventRefusal),
}

#[allow(dead_code, reason = "consumed by the T4 decision-record wiring")]
impl SubsessionEvent {
    /// The tool id the event belongs to (the decision-record key).
    pub(crate) fn tool_id(&self) -> &str {
        match self {
            Self::Outcome(outcome) => &outcome.tool_id,
            Self::Refusal(refusal) => &refusal.tool_id,
        }
    }

    /// Whether the spawn completed a run (`false` = refused pre-run).
    #[must_use]
    pub(crate) const fn ran(&self) -> bool {
        matches!(self, Self::Outcome(_))
    }
}

/// A completed/failed sub-session's decision-record payload.
#[derive(Clone, Debug, Serialize)]
pub struct SubsessionEventOutcome {
    pub tool_id: String,
    pub role: String,
    pub provider: String,
    pub model: String,
    pub status: RunStatus,
    pub usage: UsageTotals,
    pub routing: crate::subsession::RoleRoutingEvidence,
    /// The reasoning-effort tier the spawn ran with (`None` = routing
    /// default; issue #535 §9.45).
    pub effort: Option<crate::tooldef::ReasoningEffort>,
    /// The summary cap the spawn ran with (`None` = budget default).
    pub max_summary_bytes: Option<u64>,
    /// Whether the spawn demanded a structured-only finish.
    pub structured_summary: Option<bool>,
}

/// A refused sub-session spawn's decision-record payload.
#[derive(Clone, Debug, Serialize)]
pub struct SubsessionEventRefusal {
    pub tool_id: String,
    pub reason: &'static str,
}

/// Builds the per-call request: bounded output, no streaming, and the
/// tool-declared reasoning-effort tier (issue #535 §9.45) on the wire.
fn build_request(config: &WorkerConfig, messages: &[ModelMessage]) -> ModelRequest {
    let mut extensions = serde_json::Map::new();
    extensions.insert("stream".to_string(), serde_json::Value::Bool(false));
    ModelRequest {
        provider_id: config.provider_id.clone(),
        model_id: config.model_id.clone(),
        messages: messages.to_vec(),
        // Bounded per-call output: the cap guards context and
        // wall-clock burn (see a44af33); cost tracking must not change it.
        max_output_tokens: Some(8_192),
        temperature: None,
        // The tool-declared effort tier rides the wire — never a silent
        // drop back to the routing default.
        reasoning: config.effort.map(|effort| {
            orchestraitor_provider_api::transport::ReasoningConfig {
                effort,
                budget_tokens: None,
            }
        }),
        structured_output: None,
        tool_choice: None,
        extensions,
    }
}

/// One model call with bounded provider retries (`10s·2^n` capped, issue #310).
pub(super) async fn call_model(
    transport: &dyn ProviderTransport,
    config: &WorkerConfig,
    messages: &[ModelMessage],
    state: &mut RunState<'_>,
) -> Result<String, TypedFailure> {
    let budgets = &config.budgets;
    let request = build_request(config, messages);
    let mut retries = 0_u32;
    loop {
        state.model_calls += 1;
        let started = std::time::Instant::now();
        // Wall-clock start of THIS attempt: the ledger's `started_at` must
        // reflect when the call began, not when the (later) record write
        // happens — `completed_at` + `wall_ms` otherwise contradict each
        // other (zero-span timestamps vs. real elapsed time).
        let started_at = chrono::Utc::now();
        match transport.stream(request.clone()).await {
            Ok(events) => {
                match collect_events(events) {
                    Ok((text, usage)) => {
                        accumulate_usage(state, config, usage);
                        record_call_cost(
                            config,
                            &request,
                            usage,
                            CallOutcome::Completed,
                            started,
                            started_at,
                        );
                        return Ok(text);
                    }
                    Err((failure, usage)) => {
                        // The transport attempt happened and may have
                        // produced usage before the stream went invalid:
                        // record the failed call with any partial usage so
                        // the ledger reflects the spend (spec §9.26.4).
                        record_call_cost(
                            config,
                            &request,
                            usage,
                            CallOutcome::Failed,
                            started,
                            started_at,
                        );
                        return Err(failure);
                    }
                }
            }
            Err(ProviderTransportError::RequestFailed { .. }) => {
                record_call_cost(
                    config,
                    &request,
                    None,
                    CallOutcome::Failed,
                    started,
                    started_at,
                );
                if retries >= budgets.max_provider_retries {
                    return Err(TypedFailure {
                        class: FailureClass::ProviderError,
                        reason: "provider-request-failed",
                    });
                }
                let delay = backoff_delay(budgets, retries);
                retries += 1;
                debug!(?delay, retry = retries, "provider call failed; backing off");
                tokio::time::sleep(delay).await;
            }
            Err(ProviderTransportError::InvalidEvent) => {
                record_call_cost(
                    config,
                    &request,
                    None,
                    CallOutcome::Failed,
                    started,
                    started_at,
                );
                return Err(TypedFailure {
                    class: FailureClass::ProviderError,
                    reason: "provider-invalid-event",
                });
            }
            Err(ProviderTransportError::CapabilityUnavailable { .. }) => {
                record_call_cost(
                    config,
                    &request,
                    None,
                    CallOutcome::Failed,
                    started,
                    started_at,
                );
                return Err(TypedFailure {
                    class: FailureClass::ProviderError,
                    reason: "provider-capability-unavailable",
                });
            }
        }
    }
}

/// Folds one event stream into response text and usage. A stream without a
/// `Completed` terminator — or carrying native tool calls the text protocol
/// never requests — is an invalid stream (fail closed, not a silent trim).
/// The `Err` payload carries any usage observed before the failure so the
/// failed call still records its partial spend.
fn collect_events(
    events: ModelEventStream,
) -> Result<(String, Option<TokenCount>), (TypedFailure, Option<TokenCount>)> {
    let mut text = String::new();
    let mut usage = None;
    let mut completed = false;
    for event in events {
        let Ok(event) = event else {
            return Err((
                TypedFailure {
                    class: FailureClass::ProviderError,
                    reason: "provider-invalid-event",
                },
                usage,
            ));
        };
        match event {
            ModelEvent::Started => {}
            ModelEvent::Completed => completed = true,
            ModelEvent::TextDelta { text: delta } => text.push_str(&delta),
            ModelEvent::Usage { token_count } => usage = Some(token_count),
            ModelEvent::ToolCall { .. } => {
                return Err((
                    TypedFailure {
                        class: FailureClass::ProviderError,
                        reason: "provider-unexpected-tool-call",
                    },
                    usage,
                ));
            }
        }
    }
    if !completed {
        return Err((
            TypedFailure {
                class: FailureClass::ProviderError,
                reason: "provider-invalid-event",
            },
            usage,
        ));
    }
    Ok((text, usage))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// A stream item error after a Usage event must carry the partial usage
    /// out of `collect_events` (spec §9.26.4: failed calls still record spend).
    #[test]
    fn stream_error_after_usage_carries_partial_usage() {
        let partial = TokenCount {
            input_tokens: 11,
            output_tokens: 7,
            cached_tokens: 0,
            reasoning_tokens: 0,
        };
        let events: ModelEventStream = Box::new(
            vec![
                Ok(ModelEvent::Started),
                Ok(ModelEvent::Usage {
                    token_count: partial,
                }),
                Err(ProviderTransportError::InvalidEvent),
            ]
            .into_iter(),
        );
        let Err((_, usage)) = collect_events(events) else {
            panic!("stream error must fail collect_events");
        };
        assert_eq!(usage, Some(partial));
    }

    /// A missing `Completed` terminator is an invalid stream, and any usage
    /// observed before the truncation must still be carried out.
    #[test]
    fn missing_completed_terminator_carries_partial_usage() {
        let partial = TokenCount {
            input_tokens: 3,
            output_tokens: 4,
            cached_tokens: 0,
            reasoning_tokens: 0,
        };
        let events: ModelEventStream = Box::new(
            vec![
                Ok(ModelEvent::Started),
                Ok(ModelEvent::TextDelta {
                    text: "partial".to_string(),
                }),
                Ok(ModelEvent::Usage {
                    token_count: partial,
                }),
            ]
            .into_iter(),
        );
        let Err((failure, usage)) = collect_events(events) else {
            panic!("truncated stream must fail collect_events");
        };
        assert_eq!(failure.reason, "provider-invalid-event");
        assert_eq!(usage, Some(partial));
    }
}

/// Accumulates usage and evaluates the daily spend soft cap (soft: recorded,
/// never a hard stop; the hard bounds are the worker timeout and run budget).
fn accumulate_usage(state: &mut RunState<'_>, config: &WorkerConfig, usage: Option<TokenCount>) {
    if let Some(usage) = usage {
        state.usage.input_tokens += usage.input_tokens;
        state.usage.output_tokens += usage.output_tokens;
    }
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

/// Whether a model call completed or failed. Failed calls still record a
/// cost row (spec §9.26.4: usage records are mandatory even on failure) —
/// undercounting spend for flaky providers defeats the ledger's purpose.
#[derive(Clone, Copy)]
enum CallOutcome {
    Completed,
    Failed,
}

impl CallOutcome {
    fn label(self) -> &'static str {
        match self {
            Self::Completed => "worker-bootstrap",
            Self::Failed => "worker-bootstrap-failed",
        }
    }
}

/// Records one cost entry for a completed model call (spec §9.19.4 per-call
/// attribution). Best-effort: a sink write failure is logged and dropped —
/// cost bookkeeping must never fail a delivery run. Errors carry no request
/// content (§9.23.4). `usage = None` still records the request (row shows a
/// provider call with zero reported tokens).
///
/// The row key mints a per-call sequence number: `request_id` is the ledger
/// primary key, so a constant id would silently swallow every entry after
/// the first.
fn record_call_cost(
    config: &WorkerConfig,
    request: &ModelRequest,
    usage: Option<TokenCount>,
    outcome: CallOutcome,
    started: std::time::Instant,
    started_at: chrono::DateTime<chrono::Utc>,
) {
    let (Some(attribution), Some(sink)) = (&config.attribution, &config.cost_sink) else {
        return;
    };
    let call_number = config.model_call_sequence.fetch_add(1, Ordering::Relaxed) + 1;
    let (input_tokens, output_tokens, reasoning_tokens, cache_read_tokens) =
        usage.map_or((0, 0, 0, 0), |u| {
            (
                u.input_tokens,
                u.output_tokens,
                u.reasoning_tokens,
                u.cached_tokens,
            )
        });
    let completed_at = chrono::Utc::now();
    let entry = CostEntry {
        model: request.model_id.clone(),
        provider: request.provider_id.clone(),
        agent_domain_id: attribution.agent_domain_id.clone(),
        role: attribution.role.clone(),
        project: attribution.project.clone(),
        session: attribution.session.clone(),
        repository: attribution.repository.clone(),
        input_tokens,
        output_tokens,
        reasoning_tokens,
        cache_read_tokens,
        cache_write_tokens: 0,
        request_count: 1,
        // The transport does not surface a provider request id through the
        // event stream yet; run-scoped session id + per-call sequence
        // number gives the row a stable, unique, dedupable key.
        request_id: format!("{}/call-{call_number}", attribution.session.as_str()),
        parent_request_id: None,
        started_at,
        completed_at,
        wall_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        monetary_cost_measured: None,
        monetary_cost_estimated: None,
        monetary_cost_basis: MonetaryCostBasis::UtilizationOnly,
        subscription_attribution_id: None,
        routing_decision: outcome.label().to_owned(),
    };
    if let Err(error) = orchestraitor_provider_neuralwatt::CostSink::record(sink.as_ref(), &entry) {
        debug!(%error, "cost sink write failed; entry dropped");
    }
}
