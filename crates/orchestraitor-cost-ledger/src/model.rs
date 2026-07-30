//! Domain records stored by the cost ledger.

use chrono::{DateTime, Utc};
use orchestraitor_model::{AgentId, ModelId, ProviderId, RepositoryId, SessionId};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Stable subscription attribution identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubscriptionId(
    /// Underlying subscription identifier, preserved verbatim.
    pub String,
);

impl SubscriptionId {
    /// Creates a subscription identifier from existing configuration text.
    #[must_use]
    pub fn from_string(value: String) -> Self {
        Self(value)
    }

    /// Returns the underlying identifier text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Explains how a monetary cost field was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MonetaryCostBasis {
    /// Provider supplied a reliable per-call charge.
    ProviderMeasured,
    /// Provider pricing and usage were available, but the value was computed locally.
    PriceSheetEstimated,
    /// User supplied a flat-rate price; utilization may be displayed against it.
    UserConfiguredSubscriptionPrice,
    /// No reliable monetary value is known; display utilization only.
    UtilizationOnly,
}

/// Per-call cost entry required by spec §9.19.4.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostEntry {
    /// Model used for the call.
    pub model: ModelId,
    /// Provider used for the call.
    pub provider: ProviderId,
    /// Domain-agent identifier attributed to the call.
    pub agent_domain_id: AgentId,
    /// Agent role for the call.
    pub role: String,
    /// Project identifier or path label attributed to the call.
    pub project: String,
    /// Session identifier attributed to the call.
    pub session: SessionId,
    /// Repository identifier attributed to the call.
    pub repository: RepositoryId,
    /// Input token count reported or estimated for the call.
    pub input_tokens: u64,
    /// Output token count reported or estimated for the call.
    pub output_tokens: u64,
    /// Reasoning token count, when reported by the provider.
    pub reasoning_tokens: u64,
    /// Cache-read token count.
    pub cache_read_tokens: u64,
    /// Cache-write token count.
    pub cache_write_tokens: u64,
    /// Number of provider requests represented by this entry.
    pub request_count: u64,
    /// Provider or controller request identifier.
    pub request_id: String,
    /// Parent request identifier for retries, fallbacks, or shadow calls.
    pub parent_request_id: Option<String>,
    /// Timestamp when the call started.
    pub started_at: DateTime<Utc>,
    /// Timestamp when the call completed.
    pub completed_at: DateTime<Utc>,
    /// Wall-clock duration in milliseconds.
    pub wall_ms: u64,
    /// Provider-measured monetary cost, when reliable.
    pub monetary_cost_measured: Option<f64>,
    /// Locally estimated monetary cost, when reliable pricing data exists.
    pub monetary_cost_estimated: Option<f64>,
    /// Basis explaining monetary cost fields.
    pub monetary_cost_basis: MonetaryCostBasis,
    /// Subscription utilization attribution, when applicable.
    pub subscription_attribution_id: Option<SubscriptionId>,
    /// Routing precedence decision that selected the provider and model.
    pub routing_decision: String,
}

impl CostEntry {
    /// Returns all token counters summed for budget enforcement.
    #[must_use]
    pub const fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.reasoning_tokens
            + self.cache_read_tokens
            + self.cache_write_tokens
    }
}

/// API spend row; separate from subscription utilization rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiSpendRecord {
    /// Cost entry associated with metered API spend.
    pub cost_entry: CostEntry,
}

/// Subscription utilization confidence label from spec §9.19.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UtilizationLabel {
    /// Provider exposes exact quota usage telemetry.
    Measured,
    /// Partial telemetry exists and the remainder is inferred.
    Estimated,
    /// User supplied quota metadata and Orchestraitor tracks against it.
    UserConfigured,
}

impl UtilizationLabel {
    /// Returns the stable storage spelling for this label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Measured => "measured",
            Self::Estimated => "estimated",
            Self::UserConfigured => "user-configured",
        }
    }
}

/// Optional flat-rate subscription metadata from spec §9.19.5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    /// Stable subscription identifier.
    pub id: SubscriptionId,
    /// Provider the subscription belongs to.
    pub provider: ProviderId,
    /// Billing period label such as `daily`, `weekly`, `monthly`, `annual`, or `custom`.
    pub billing_period: String,
    /// Optional user-supplied monthly price in USD.
    pub monthly_price_usd: Option<f64>,
    /// Optional included token quota.
    pub included_tokens: Option<u64>,
    /// Optional soft token cap.
    pub soft_cap_tokens: Option<u64>,
    /// Optional hard token cap.
    pub hard_cap_tokens: Option<u64>,
    /// Optional active-time cap per day.
    pub active_time_cap_minutes_per_day: Option<u64>,
    /// Reset rule text from configuration.
    pub reset_at: String,
}

/// Subscription utilization row; separate from metered API spend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionUtilizationEntry {
    /// Subscription attributed to this utilization row.
    pub subscription_id: SubscriptionId,
    /// Cost-entry request id that consumed utilization.
    pub request_id: String,
    /// Utilization confidence label.
    pub label: UtilizationLabel,
    /// Tokens consumed from the subscription quota.
    pub consumed_tokens: u64,
    /// Optional quota denominator used for display.
    pub quota_tokens: Option<u64>,
    /// Optional user-supplied monthly price in USD.
    pub monthly_price_usd: Option<f64>,
}

impl SubscriptionUtilizationEntry {
    /// Returns utilization-derived USD only when the user supplied monthly price and quota.
    #[must_use]
    pub fn user_configured_cost_usd(&self) -> Option<f64> {
        let quota = self.quota_tokens?;
        if quota == 0 {
            return None;
        }
        let price = self.monthly_price_usd?;
        let consumed = self.consumed_tokens.to_string().parse::<f64>().ok()?;
        let quota = quota.to_string().parse::<f64>().ok()?;
        Some(price * (consumed / quota))
    }
}

/// Rollup totals for a domain-agent id.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainCostRollup {
    /// Domain-agent identifier for the rollup.
    pub agent_domain_id: AgentId,
    /// Summed input tokens.
    pub input_tokens: u64,
    /// Summed output tokens.
    pub output_tokens: u64,
    /// Summed reasoning tokens.
    pub reasoning_tokens: u64,
    /// Summed cache-read tokens.
    pub cache_read_tokens: u64,
    /// Summed cache-write tokens.
    pub cache_write_tokens: u64,
    /// Summed provider request count.
    pub request_count: u64,
    /// Summed measured monetary cost.
    pub monetary_cost_measured: f64,
    /// Summed estimated monetary cost.
    pub monetary_cost_estimated: f64,
}

impl DomainCostRollup {
    /// Returns all token counters in the rollup.
    #[must_use]
    pub const fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.output_tokens
            + self.reasoning_tokens
            + self.cache_read_tokens
            + self.cache_write_tokens
    }
}
