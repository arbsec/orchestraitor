//! One model call against the `ProviderTransport` boundary: bounded retries
//! (backoff per issue #310), event folding, usage accrual, and the daily spend
//! soft-cap check.

use orchestraitor_cost_ledger::{CostEntry, MonetaryCostBasis};
use orchestraitor_provider_api::ProviderTransportError;
use orchestraitor_provider_api::transport::{
    ModelEvent, ModelEventStream, ModelMessage, ModelRequest, ProviderTransport, TokenCount,
};
use std::sync::atomic::Ordering;
use tracing::debug;

use crate::budget::backoff_delay;
use crate::result::{FailureClass, TypedFailure, UsageTotals, WorkerConfig};

/// Mutable run state threaded through model calls (shared with the loop).
pub(super) struct RunState {
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
}

/// One model call with bounded provider retries (`10s·2^n` capped, issue #310).
pub(super) async fn call_model(
    transport: &dyn ProviderTransport,
    config: &WorkerConfig,
    messages: &[ModelMessage],
    state: &mut RunState,
) -> Result<String, TypedFailure> {
    let budgets = &config.budgets;
    let mut extensions = serde_json::Map::new();
    extensions.insert("stream".to_string(), serde_json::Value::Bool(false));
    let request = ModelRequest {
        provider_id: config.provider_id.clone(),
        model_id: config.model_id.clone(),
        messages: messages.to_vec(),
        // Bounded per-call output: the cap guards context and
        // wall-clock burn (see a44af33); cost tracking must not change it.
        max_output_tokens: Some(8_192),
        temperature: None,
        reasoning: None,
        structured_output: None,
        tool_choice: None,
        extensions,
    };
    let mut retries = 0_u32;
    loop {
        state.model_calls += 1;
        let started = std::time::Instant::now();
        match transport.stream(request.clone()).await {
            Ok(events) => {
                let (text, usage) = collect_events(events)?;
                accumulate_usage(state, config, usage);
                record_call_cost(config, &request, usage, CallOutcome::Completed, started);
                return Ok(text);
            }
            Err(ProviderTransportError::RequestFailed { .. }) => {
                record_call_cost(config, &request, None, CallOutcome::Failed, started);
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
                record_call_cost(config, &request, None, CallOutcome::Failed, started);
                return Err(TypedFailure {
                    class: FailureClass::ProviderError,
                    reason: "provider-invalid-event",
                });
            }
            Err(ProviderTransportError::CapabilityUnavailable { .. }) => {
                record_call_cost(config, &request, None, CallOutcome::Failed, started);
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
fn collect_events(events: ModelEventStream) -> Result<(String, Option<TokenCount>), TypedFailure> {
    let mut text = String::new();
    let mut usage = None;
    let mut completed = false;
    for event in events {
        let event = event.map_err(|_| TypedFailure {
            class: FailureClass::ProviderError,
            reason: "provider-invalid-event",
        })?;
        match event {
            ModelEvent::Started => {}
            ModelEvent::Completed => completed = true,
            ModelEvent::TextDelta { text: delta } => text.push_str(&delta),
            ModelEvent::Usage { token_count } => usage = Some(token_count),
            ModelEvent::ToolCall { .. } => {
                return Err(TypedFailure {
                    class: FailureClass::ProviderError,
                    reason: "provider-unexpected-tool-call",
                });
            }
        }
    }
    if !completed {
        return Err(TypedFailure {
            class: FailureClass::ProviderError,
            reason: "provider-invalid-event",
        });
    }
    Ok((text, usage))
}

/// Accumulates usage and evaluates the daily spend soft cap (soft: recorded,
/// never a hard stop; the hard bounds are the worker timeout and run budget).
fn accumulate_usage(state: &mut RunState, config: &WorkerConfig, usage: Option<TokenCount>) {
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
    let now = chrono::Utc::now();
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
        started_at: now,
        completed_at: now,
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
