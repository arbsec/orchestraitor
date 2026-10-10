//! Cost, routing, and subscription panel data (spec §9.19.7).
//!
//! The TUI renders:
//! - active agents per session (domain, role, provider, model, last cost)
//! - per-agent / per-domain / per-repo / per-provider cost rollups
//! - subscription utilization meters with `measured` / `estimated` /
//!   `user-configured` labels
//! - live per-call model-routing events (which precedence step matched)
//! - soft-cap and hard-cap warnings

use orchestraitor_cost_ledger::{
    BudgetWarningEvent, CapKind, CostEntry, DomainCostRollup, MonetaryCostBasis,
    SubscriptionUtilizationEntry, UtilizationLabel,
};

/// A single row in the cost panel, carrying the confidence label.
#[derive(Debug, Clone, PartialEq)]
pub struct CostPanelRow {
    /// Label for the row (e.g. agent name, domain, provider).
    pub label: String,
    /// Monetary cost, if known.
    pub monetary_cost: Option<f64>,
    /// Basis explaining the monetary cost field.
    pub cost_basis: MonetaryCostBasis,
    /// Token count for this row.
    pub tokens: u64,
    /// Optional subscription attribution.
    pub subscription_label: Option<String>,
}

impl CostPanelRow {
    /// Creates a row from a cost entry, preserving the cost basis label.
    #[must_use]
    pub fn from_entry(entry: &CostEntry) -> Self {
        let (monetary_cost, subscription_label) = match entry.monetary_cost_basis {
            MonetaryCostBasis::ProviderMeasured => (
                entry.monetary_cost_measured,
                entry
                    .subscription_attribution_id
                    .as_ref()
                    .map(ToString::to_string),
            ),
            MonetaryCostBasis::PriceSheetEstimated
            | MonetaryCostBasis::UserConfiguredSubscriptionPrice => (
                entry.monetary_cost_estimated,
                entry
                    .subscription_attribution_id
                    .as_ref()
                    .map(ToString::to_string),
            ),
            MonetaryCostBasis::UtilizationOnly => (None, None),
        };
        Self {
            label: format!(
                "{} / {} / {}",
                entry.agent_domain_id, entry.role, entry.provider
            ),
            monetary_cost,
            cost_basis: entry.monetary_cost_basis,
            tokens: entry.total_tokens(),
            subscription_label,
        }
    }

    /// Creates a row from a domain rollup.
    #[must_use]
    pub fn from_rollup(rollup: &DomainCostRollup) -> Self {
        let monetary_cost = if rollup.monetary_cost_measured > 0.0 {
            Some(rollup.monetary_cost_measured)
        } else if rollup.monetary_cost_estimated > 0.0 {
            Some(rollup.monetary_cost_estimated)
        } else {
            None
        };
        let cost_basis = if rollup.monetary_cost_measured > 0.0 {
            MonetaryCostBasis::ProviderMeasured
        } else if rollup.monetary_cost_estimated > 0.0 {
            MonetaryCostBasis::PriceSheetEstimated
        } else {
            MonetaryCostBasis::UtilizationOnly
        };
        Self {
            label: rollup.agent_domain_id.to_string(),
            monetary_cost,
            cost_basis,
            tokens: rollup.total_tokens(),
            subscription_label: None,
        }
    }

    /// Returns the display label for the cost basis.
    #[must_use]
    pub const fn basis_label(&self) -> &'static str {
        match self.cost_basis {
            MonetaryCostBasis::ProviderMeasured => "measured",
            MonetaryCostBasis::PriceSheetEstimated => "estimated",
            MonetaryCostBasis::UserConfiguredSubscriptionPrice => "user-configured",
            MonetaryCostBasis::UtilizationOnly => "utilization-only",
        }
    }
}

/// A subscription utilization meter row.
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionPanelRow {
    /// Subscription identifier.
    pub subscription_id: String,
    /// Utilization confidence label.
    pub label: UtilizationLabel,
    /// Tokens consumed.
    pub consumed_tokens: u64,
    /// Optional quota denominator.
    pub quota_tokens: Option<u64>,
    /// Optional monthly price in USD.
    pub monthly_price_usd: Option<f64>,
}

impl SubscriptionPanelRow {
    /// Creates a row from a subscription utilization entry.
    #[must_use]
    pub fn from_entry(entry: &SubscriptionUtilizationEntry) -> Self {
        Self {
            subscription_id: entry.subscription_id.to_string(),
            label: entry.label,
            consumed_tokens: entry.consumed_tokens,
            quota_tokens: entry.quota_tokens,
            monthly_price_usd: entry.monthly_price_usd,
        }
    }

    /// Returns the utilization percentage, if quota is known.
    #[must_use]
    pub fn utilization_percent(&self) -> Option<f64> {
        let quota = self.quota_tokens?;
        if quota == 0 {
            return None;
        }
        let consumed = f64::from(u32::try_from(self.consumed_tokens).unwrap_or(u32::MAX));
        let quota = f64::from(u32::try_from(quota).unwrap_or(u32::MAX));
        Some((consumed / quota) * 100.0)
    }

    /// Returns the display label for the utilization confidence.
    #[must_use]
    pub fn label_text(&self) -> &'static str {
        self.label.as_str()
    }
}

/// Kind of routing panel entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingPanelEntryKind {
    /// A live routing event showing which precedence step matched.
    RoutingEvent,
    /// A soft-cap warning.
    SoftCapWarning,
    /// A hard-cap warning.
    HardCapWarning,
}

/// A single entry in the routing panel.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutingPanelEntry {
    /// Kind of entry.
    pub kind: RoutingPanelEntryKind,
    /// Human-readable text for the entry.
    pub text: String,
}

impl RoutingPanelEntry {
    /// Creates a routing event entry from a matched step label.
    #[must_use]
    pub fn routing_event(matched_step: &str, provider: &str, model: &str) -> Self {
        Self {
            kind: RoutingPanelEntryKind::RoutingEvent,
            text: format!("{matched_step} → {provider}/{model}"),
        }
    }

    /// Creates a budget warning entry.
    #[must_use]
    pub fn budget_warning(warning: &BudgetWarningEvent) -> Self {
        let (kind, kind_label) = match warning.cap_kind {
            CapKind::Soft => (RoutingPanelEntryKind::SoftCapWarning, "soft"),
            CapKind::Hard => (RoutingPanelEntryKind::HardCapWarning, "hard"),
        };
        Self {
            kind,
            text: format!(
                "{} {} {} cap: {:.0} / {:.0} ({})",
                warning.scope.as_str(),
                kind_label,
                warning.metric.as_str(),
                warning.projected,
                warning.cap,
                warning.scope_id
            ),
        }
    }
}

/// Aggregated cost panel data carried in the app snapshot.
#[derive(Debug, Clone, Default)]
pub struct CostPanelData {
    /// Cost rows for the active view.
    pub rows: Vec<CostPanelRow>,
    /// Subscription utilization meters.
    pub subscriptions: Vec<SubscriptionPanelRow>,
    /// Routing events and cap warnings.
    pub routing_entries: Vec<RoutingPanelEntry>,
}
