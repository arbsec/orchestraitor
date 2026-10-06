//! One model call against the `ProviderTransport` boundary: bounded retries
//! (backoff per issue #310), event folding, usage accrual, and the daily spend
//! soft-cap check.

use orchestraitor_provider_api::ProviderTransportError;
use orchestraitor_provider_api::transport::{
    ModelEvent, ModelEventStream, ModelMessage, ModelRequest, ProviderTransport, TokenCount,
};
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
        // Bounded per call: an uncapped completion lets a verbose model
        // (glm-5.x multi-block mode) burn context and wall clock in one
        // turn. 8k output tokens is ample for one fenced action block plus
        // reasoning; the guard is a bound, not a target.
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
        match transport.stream(request.clone()).await {
            Ok(events) => {
                let (text, usage) = collect_events(events)?;
                accumulate_usage(state, config, usage);
                return Ok(text);
            }
            Err(ProviderTransportError::RequestFailed { .. }) => {
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
                return Err(TypedFailure {
                    class: FailureClass::ProviderError,
                    reason: "provider-invalid-event",
                });
            }
            Err(ProviderTransportError::CapabilityUnavailable { .. }) => {
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
