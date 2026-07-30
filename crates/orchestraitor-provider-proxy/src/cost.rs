//! Cost attribution for proxied provider calls.

use chrono::{DateTime, Utc};
use orchestraitor_cost_ledger::{CostEntry, CostLedger, MonetaryCostBasis};
use orchestraitor_model::{AgentId, RepositoryId, SessionId};
use orchestraitor_provider_api::TokenCount;

use crate::protocol::ProviderRoute;
use crate::{ProtocolSurface, ProxyResult};

/// Static attribution context attached to every proxied provider call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostAttribution {
    /// Domain-agent identifier attributed to proxied calls.
    pub agent_domain_id: AgentId,
    /// Agent role attributed to proxied calls.
    pub role: String,
    /// Project label attributed to proxied calls.
    pub project: String,
    /// Session identifier attributed to proxied calls.
    pub session: SessionId,
    /// Repository identifier attributed to proxied calls.
    pub repository: RepositoryId,
}

impl CostAttribution {
    /// Creates deterministic local attribution for proxy-only calls before a daemon session exists.
    #[must_use]
    pub fn local_proxy_defaults() -> Self {
        Self {
            agent_domain_id: AgentId::from_string("provider-proxy".to_owned()),
            role: "provider-proxy".to_owned(),
            project: "local".to_owned(),
            session: SessionId::from_string("provider-proxy-session".to_owned()),
            repository: RepositoryId::from_string("provider-proxy-repository".to_owned()),
        }
    }
}

/// Records per-call cost entries after provider completion.
pub struct CostRecorder<'a> {
    ledger: &'a CostLedger,
    attribution: CostAttribution,
}

impl<'a> CostRecorder<'a> {
    /// Creates a cost recorder over an existing ledger.
    #[must_use]
    pub const fn new(ledger: &'a CostLedger, attribution: CostAttribution) -> Self {
        Self {
            ledger,
            attribution,
        }
    }

    /// Inserts exactly one cost entry for a completed provider call.
    ///
    /// # Errors
    /// Returns a ledger error when the entry cannot be persisted.
    pub fn record_completion(&self, input: CompletionCostInput<'_>) -> ProxyResult<()> {
        let entry = CostEntry {
            model: input.route.model_id.clone(),
            provider: input.route.provider_id.clone(),
            agent_domain_id: self.attribution.agent_domain_id.clone(),
            role: self.attribution.role.clone(),
            project: self.attribution.project.clone(),
            session: self.attribution.session.clone(),
            repository: self.attribution.repository.clone(),
            input_tokens: input.usage.input_tokens,
            output_tokens: input.usage.output_tokens,
            reasoning_tokens: input.usage.reasoning_tokens,
            cache_read_tokens: input.usage.cached_tokens,
            cache_write_tokens: 0,
            request_count: 1,
            request_id: input.request_id.to_owned(),
            parent_request_id: None,
            started_at: input.started_at,
            completed_at: input.completed_at,
            wall_ms: u64::try_from((input.completed_at - input.started_at).num_milliseconds())
                .unwrap_or(0),
            monetary_cost_measured: None,
            monetary_cost_estimated: None,
            monetary_cost_basis: MonetaryCostBasis::UtilizationOnly,
            subscription_attribution_id: None,
            routing_decision: format!(
                "{}:{}",
                input.surface.as_str(),
                input.route.routing_decision
            ),
        };
        self.ledger.api_spend().insert_cost_entry(&entry)?;
        Ok(())
    }
}

/// Inputs needed to build one cost entry without parameter bloat.
#[derive(Debug, Clone, Copy)]
pub struct CompletionCostInput<'a> {
    /// Protocol surface handling the request.
    pub surface: ProtocolSurface,
    /// Provider route selected for this request.
    pub route: &'a ProviderRoute,
    /// Provider/controller request id.
    pub request_id: &'a str,
    /// Token usage reported or estimated for the call.
    pub usage: TokenCount,
    /// Request start timestamp.
    pub started_at: DateTime<Utc>,
    /// Request completion timestamp.
    pub completed_at: DateTime<Utc>,
}
