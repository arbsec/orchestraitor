//! Cost entry emission per spec §9.19.4.
//!
//! The cost ledger keeps metered API spend separate from subscription
//! utilization. This module provides a [`CostSink`] trait that the transport
//! calls after each provider completion, plus an in-memory implementation
//! for tests and a ledger-backed implementation for production.

use std::sync::Mutex;

use orchestraitor_cost_ledger::{CostEntry, CostLedger, LedgerResult, MonetaryCostBasis};
use orchestraitor_model::{AgentId, RepositoryId, SessionId};
use orchestraitor_provider_api::TokenCount;

/// Attribution context attached to every Neuralwatt provider call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostAttribution {
    /// Domain-agent identifier attributed to the call.
    pub agent_domain_id: AgentId,
    /// Agent role for the call.
    pub role: String,
    /// Project label attributed to the call.
    pub project: String,
    /// Session identifier attributed to the call.
    pub session: SessionId,
    /// Repository identifier attributed to the call.
    pub repository: RepositoryId,
}

impl CostAttribution {
    /// Creates deterministic local attribution for provider-only calls.
    #[must_use]
    pub fn local_defaults() -> Self {
        Self {
            agent_domain_id: AgentId::from_string("neuralwatt".to_owned()),
            role: "neuralwatt".to_owned(),
            project: "local".to_owned(),
            session: SessionId::from_string("neuralwatt-session".to_owned()),
            repository: RepositoryId::from_string("neuralwatt-repository".to_owned()),
        }
    }
}

/// Inputs needed to build one cost entry without parameter bloat.
#[derive(Debug, Clone)]
pub struct CompletionCostInput<'a> {
    /// Provider/controller request id.
    pub request_id: &'a str,
    /// Token usage reported by the provider.
    pub usage: TokenCount,
    /// Routing decision label.
    pub routing_decision: &'a str,
}

/// Sink for recording per-call cost entries (spec §9.19.4).
pub trait CostSink: Send + Sync {
    /// Records one cost entry for a completed provider call.
    ///
    /// # Errors
    ///
    /// Returns an error when the sink cannot persist the entry.
    fn record(&self, entry: &CostEntry) -> Result<(), String>;
}

/// Ledger-backed cost sink for production use.
///
/// Note: `CostLedger` is `Send` but not `Sync` (it wraps a `rusqlite::Connection`).
/// This type does not implement [`CostSink`] directly; callers use
/// [`record_completion`](Self::record_completion) or wrap entries via
/// [`build_entry`] for the [`CostSink`] trait.
pub struct LedgerCostSink<'a> {
    ledger: &'a CostLedger,
}

impl<'a> LedgerCostSink<'a> {
    /// Creates a cost sink backed by a [`CostLedger`].
    #[must_use]
    pub const fn new(ledger: &'a CostLedger) -> Self {
        Self { ledger }
    }

    /// Builds and records a cost entry from the completion input.
    ///
    /// # Errors
    ///
    /// Returns a ledger error when the entry cannot be persisted.
    pub fn record_completion(
        &self,
        attribution: &CostAttribution,
        input: &CompletionCostInput<'_>,
        model: &str,
        provider: &str,
    ) -> LedgerResult<()> {
        let entry = build_entry(attribution, input, model, provider);
        self.ledger.api_spend().insert_cost_entry(&entry)?;
        Ok(())
    }
}

/// In-memory cost sink for tests and short-lived callers.
#[derive(Debug, Default)]
pub struct InMemoryCostSink {
    entries: Mutex<Vec<CostEntry>>,
}

impl InMemoryCostSink {
    /// Creates an empty in-memory cost sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of all recorded entries.
    ///
    ///
    #[must_use]
    pub fn entries(&self) -> Vec<CostEntry> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Returns the number of recorded entries.
    ///
    ///
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Returns whether no entries have been recorded.
    ///
    ///
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl CostSink for InMemoryCostSink {
    fn record(&self, entry: &CostEntry) -> Result<(), String> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry.clone());
        Ok(())
    }
}

/// Builds a [`CostEntry`] from attribution and completion input.
fn build_entry(
    attribution: &CostAttribution,
    input: &CompletionCostInput<'_>,
    model: &str,
    provider: &str,
) -> CostEntry {
    use chrono::Utc;
    let now = Utc::now();
    CostEntry {
        model: orchestraitor_model::ModelId::from_string(model.to_owned()),
        provider: orchestraitor_model::ProviderId::from_string(provider.to_owned()),
        agent_domain_id: attribution.agent_domain_id.clone(),
        role: attribution.role.clone(),
        project: attribution.project.clone(),
        session: attribution.session.clone(),
        repository: attribution.repository.clone(),
        input_tokens: input.usage.input_tokens,
        output_tokens: input.usage.output_tokens,
        reasoning_tokens: input.usage.reasoning_tokens,
        cache_read_tokens: input.usage.cached_tokens,
        cache_write_tokens: 0,
        request_count: 1,
        request_id: input.request_id.to_owned(),
        parent_request_id: None,
        started_at: now,
        completed_at: now,
        wall_ms: 0,
        monetary_cost_measured: None,
        monetary_cost_estimated: None,
        monetary_cost_basis: MonetaryCostBasis::UtilizationOnly,
        subscription_attribution_id: None,
        routing_decision: input.routing_decision.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn in_memory_sink_records_entry() {
        let sink = InMemoryCostSink::new();
        let attribution = CostAttribution::local_defaults();
        let usage = TokenCount {
            input_tokens: 10,
            output_tokens: 5,
            cached_tokens: 0,
            reasoning_tokens: 0,
        };
        let input = CompletionCostInput {
            request_id: "req-1",
            usage,
            routing_decision: "openai-chat-completions:neuralwatt",
        };
        let entry = build_entry(&attribution, &input, "glm-5.2", "neuralwatt");
        sink.record(&entry).unwrap();
        assert_eq!(sink.len(), 1);
        let recorded = &sink.entries()[0];
        assert_eq!(recorded.input_tokens, 10);
        assert_eq!(recorded.output_tokens, 5);
        assert_eq!(recorded.model.as_str(), "glm-5.2");
        assert_eq!(recorded.provider.as_str(), "neuralwatt");
    }

    #[test]
    fn ledger_sink_records_entry() {
        let ledger = CostLedger::open_in_memory().unwrap();
        let sink = LedgerCostSink::new(&ledger);
        let attribution = CostAttribution::local_defaults();
        let usage = TokenCount {
            input_tokens: 100,
            output_tokens: 50,
            cached_tokens: 0,
            reasoning_tokens: 0,
        };
        let input = CompletionCostInput {
            request_id: "req-2",
            usage,
            routing_decision: "openai-chat-completions:neuralwatt",
        };
        sink.record_completion(&attribution, &input, "glm-5.2", "neuralwatt")
            .unwrap();
        let rollup = ledger
            .api_spend()
            .domain_rollup(&attribution.agent_domain_id)
            .unwrap();
        let rollup = rollup.unwrap();
        assert_eq!(rollup.input_tokens, 100);
        assert_eq!(rollup.output_tokens, 50);
    }
}
