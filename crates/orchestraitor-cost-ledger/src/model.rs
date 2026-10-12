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
    /// Named configuration profile the run's context settings resolved
    /// through (spec §9.22.5; the A/B grouping label for §13.5.1
    /// comparisons). `None` when the run carried no profile label.
    /// Backward compatibility: rows written before §13.5.1 lack this
    /// column; readers deserialize it with a default of `None`
    /// (`#[serde(default)]`).
    #[serde(default)]
    pub profile: Option<String>,
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
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
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

/// A context receipt recorded for efficiency rollups (spec §13.5.1). The
/// session id is the join key to `CostEntry` rows; the §18.4 receipt's
/// digest/provenance fields stay in the receipt producer — the ledger
/// stores only the counters the rollup math needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiptRecord {
    /// Context request that produced the receipt (ledger-unique key).
    pub request_id: String,
    /// Session the request belonged to (the rollup join key).
    pub session: SessionId,
    /// Task class of the request (spec §9.19.1).
    pub task_class: String,
    /// Token budget imposed for the request.
    pub budget_tokens: u64,
    /// Total tokens of all candidate items before selection.
    pub candidate_tokens: u64,
    /// Tokens actually selected for the prompt.
    pub selected_tokens: u64,
    /// Number of candidate items omitted from selection.
    pub omitted_count: u64,
    /// Tokens of raw tool output included in the build, before compaction.
    pub raw_tool_output_tokens: u64,
    /// Tokens of tool output after compaction (same build).
    pub compacted_tool_output_tokens: u64,
    /// Tokens avoided by dedup/cache reuse during the build.
    pub repeated_tokens_avoided: u64,
    /// Tokens in the compiled prompt eligible for prompt-cache reuse.
    pub prompt_cache_eligible_tokens: u64,
}

/// Token-efficiency summary for one grouping key (session, agent, or
/// profile; spec §13.5.1). Token deltas come from context receipts keyed
/// by the same session id the cost entries carry; a session without
/// receipts reports `None` savings — never an estimated number.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TokenEfficiencyRollup {
    /// Session id the rollup is keyed by (empty when grouped by profile).
    pub session: Option<String>,
    /// Domain-agent id the rollup is keyed by (empty when grouped by
    /// session or profile).
    pub agent_domain_id: Option<String>,
    /// Profile grouping label.
    pub profile: Option<String>,
    /// Summed provider-reported input tokens.
    pub input_tokens: u64,
    /// Summed provider-reported output tokens.
    pub output_tokens: u64,
    /// Summed cache-read tokens.
    pub cache_read_tokens: u64,
    /// Summed cache-write tokens.
    pub cache_write_tokens: u64,
    /// Summed candidate context tokens from receipts (`None` when the
    /// group produced no receipts).
    pub candidate_tokens: Option<u64>,
    /// Summed selected context tokens from receipts (`None` when the
    /// group produced no receipts).
    pub selected_tokens: Option<u64>,
    /// Summed raw tool-output tokens entering compiled contexts (spec
    /// §13.5; `None` when the group produced no receipts).
    pub raw_tool_output_tokens: Option<u64>,
    /// Summed tool-output tokens after compaction (spec §13.5; `None`
    /// when the group produced no receipts).
    pub compacted_tool_output_tokens: Option<u64>,
    /// Compiled-context savings ratio (spec §13.5.1):
    /// `1 - (selected + compacted_tool_output) / (candidate +
    /// raw_tool_output)` — the measured reduction against the
    /// compiler-candidate counterfactual baseline. `None` when no receipt
    /// exists or the denominator is zero (reported as "not measured",
    /// never as zero savings). Under profile grouping this is derived
    /// from group-summed counters and weights sessions by their baseline;
    /// use [`TokenEfficiencyRollup::median_session_savings_ratio`] for
    /// the spec-required median comparison.
    pub savings_ratio: Option<f64>,
    /// Per-session savings ratios inside the group (profile grouping
    /// only; `None` when the grouping does not carry sessions). Sessions
    /// without receipts (or with a zero baseline) are omitted — the
    /// median never fabricates a 0% for them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_savings_ratios: Option<Vec<(String, f64)>>,
    /// The serialized [`TokenEfficiencyRollup::median_session_savings_ratio`]
    /// value: the spec-required A/B comparison stat, directly readable by
    /// JSON consumers. `None` when no session in the group has a
    /// measurable ratio (never a fabricated 0).
    pub median_savings_ratio: Option<f64>,
}

impl TokenEfficiencyRollup {
    /// The median of the group's per-session savings ratios (spec
    /// §13.5.1: "the comparison is over medians across sessions"). `None`
    /// when no session in the group has a measured ratio. The median of
    /// an even count is the mean of the two middle values.
    #[must_use]
    pub fn median_session_savings_ratio(&self) -> Option<f64> {
        let mut ratios: Vec<f64> = self
            .session_savings_ratios
            .as_ref()?
            .iter()
            .map(|(_, ratio)| *ratio)
            .collect();
        if ratios.is_empty() {
            return None;
        }
        ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = ratios.len() / 2;
        Some(if ratios.len() % 2 == 1 {
            ratios[mid]
        } else {
            f64::midpoint(ratios[mid - 1], ratios[mid])
        })
    }

    /// Computes the savings ratio from summed receipt counters (spec
    /// §13.5.1). `None` when the denominator is zero: a zero candidate
    /// baseline carries no measurable savings, and reporting a value would
    /// fabricate one.
    #[must_use]
    pub fn savings_from_receipt(
        candidate_tokens: u64,
        selected_tokens: u64,
        raw_tool_output_tokens: u64,
        compacted_tool_output_tokens: u64,
    ) -> Option<f64> {
        let selected_total = selected_tokens.saturating_add(compacted_tool_output_tokens);
        let baseline = candidate_tokens.saturating_add(raw_tool_output_tokens);
        if baseline == 0 {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "token counts are far below 2^53; the ratio only feeds a percentage display"
        )]
        let ratio = 1.0 - (selected_total as f64) / (baseline as f64);
        Some(ratio)
    }
}

/// Queries the ledger's token-efficiency summaries.
#[derive(Debug, Clone, Copy)]
pub enum EfficiencyGrouping {
    /// One rollup per session id (spec §13.5.1 per-session table).
    Session,
    /// One rollup per profile label; entries without a label group under
    /// `None` (spec §13.5.1 A/B comparison). Session-level savings live
    /// on [`TokenEfficiencyRollup::session_savings_ratios`] so consumers
    /// compute the spec-required MEDIAN of per-session ratios (a ratio
    /// of group sums would weight sessions by their baseline).
    Profile,
}
