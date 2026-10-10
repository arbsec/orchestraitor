//! Budget cap evaluation for provider spawn admission.

use crate::error::LedgerResult;
use crate::storage::CostLedger;
use serde::{Deserialize, Serialize};

/// Configurable budget scope from spec §9.19.6.
///
/// `Organization` and `User` scopes are intentionally **not** part of this enum: the
/// `cost_entries` schema (spec §9.19.4) does not yet carry per-org or per-user
/// attribution columns. Including them now would require a no-op filter that counts
/// every ledger row against every org/user budget, which silently violates isolation.
/// They will be added back once the corresponding attribution columns ship (tracked
/// as a follow-up to this MEDIUM finding).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BudgetScope {
    /// Project budget scope.
    Project,
    /// Session budget scope.
    Session,
    /// Domain budget scope.
    Domain,
    /// Agent budget scope.
    Agent,
}

impl BudgetScope {
    /// Returns the stable `SQLite` spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Session => "session",
            Self::Domain => "domain",
            Self::Agent => "agent",
        }
    }
}

/// Budget cap strictness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapKind {
    /// Crossing this cap emits a warning but allows spawn.
    Soft,
    /// Crossing this cap blocks spawn and requests fallback routing.
    Hard,
}

impl CapKind {
    /// Returns the stable `SQLite` spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Soft => "soft",
            Self::Hard => "hard",
        }
    }
}

/// Budget cap metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapMetric {
    /// Token budget metric.
    Tokens,
    /// Monetary cost budget metric.
    Cost,
}

impl CapMetric {
    /// Returns the stable `SQLite` spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tokens => "tokens",
            Self::Cost => "cost",
        }
    }
}

/// Fallback route selected when a hard cap blocks spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingFallback {
    /// Provider id to try next.
    pub provider: String,
    /// Model id to try next.
    pub model: String,
    /// Human-readable fallback reason.
    pub reason: String,
}

/// Request evaluated before spawning a provider call.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetRequest {
    /// Scope being evaluated.
    pub scope: BudgetScope,
    /// Scope identifier being evaluated.
    pub scope_id: String,
    /// Estimated tokens for the next call.
    pub token_estimate: u64,
    /// Estimated monetary cost for the next call.
    pub cost_estimate: Option<f64>,
    /// Fallback route to surface if a hard cap is crossed.
    pub fallback_route: Option<RoutingFallback>,
}

/// Warning event emitted for soft and hard budget caps.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetWarningEvent {
    /// Scope that crossed a cap.
    pub scope: BudgetScope,
    /// Scope identifier that crossed a cap.
    pub scope_id: String,
    /// Metric that crossed a cap.
    pub metric: CapMetric,
    /// Cap kind that was crossed.
    pub cap_kind: CapKind,
    /// Existing usage plus the next-call estimate.
    pub projected: f64,
    /// Configured cap amount.
    pub cap: f64,
}

/// Pre-spawn budget admission decision.
#[derive(Debug, Clone, PartialEq)]
pub enum PreSpawnDecision {
    /// Spawn is allowed without warnings.
    Allow,
    /// Spawn is allowed, but a warning event must be surfaced.
    SoftWarn(BudgetWarningEvent),
    /// Spawn is blocked and fallback routing must be surfaced.
    HardCap {
        /// Warning event explaining the hard cap.
        event: BudgetWarningEvent,
        /// Configured fallback route, when one exists.
        fallback_route: Option<RoutingFallback>,
    },
}

impl PreSpawnDecision {
    /// Returns whether the decision blocks provider spawn.
    #[must_use]
    pub const fn blocks_spawn(&self) -> bool {
        match self {
            Self::Allow | Self::SoftWarn(_) => false,
            Self::HardCap {
                event: _,
                fallback_route: _,
            } => true,
        }
    }
}

/// Budget enforcer for control-plane provider spawn checks.
pub struct BudgetEnforcer<'a> {
    ledger: &'a CostLedger,
}

impl<'a> BudgetEnforcer<'a> {
    /// Creates a budget enforcer backed by a cost ledger.
    #[must_use]
    pub const fn new(ledger: &'a CostLedger) -> Self {
        Self { ledger }
    }

    /// Evaluates budget caps before provider invocation.
    ///
    /// # Errors
    /// Returns [`LedgerError`](crate::LedgerError) when stored usage or caps cannot be read.
    pub fn pre_spawn_check(&self, request: &BudgetRequest) -> LedgerResult<PreSpawnDecision> {
        let token_usage = self
            .ledger
            .token_usage_for_scope(request.scope, &request.scope_id)?;
        let cost_usage = self
            .ledger
            .cost_usage_for_scope(request.scope, &request.scope_id)?;
        let token_projected = u64_to_f64(token_usage.saturating_add(request.token_estimate))?;
        let cost_projected = cost_usage + request.cost_estimate.unwrap_or(0.0);
        let caps = self
            .ledger
            .caps_for_scope(request.scope, &request.scope_id)?;
        let mut soft_warning = None;
        for cap in caps {
            let projected = match cap.metric {
                CapMetric::Tokens => token_projected,
                CapMetric::Cost => cost_projected,
            };
            if projected > cap.amount {
                let event = BudgetWarningEvent {
                    scope: request.scope,
                    scope_id: request.scope_id.clone(),
                    metric: cap.metric,
                    cap_kind: cap.kind,
                    projected,
                    cap: cap.amount,
                };
                match cap.kind {
                    CapKind::Soft => soft_warning = Some(event),
                    CapKind::Hard => {
                        return Ok(PreSpawnDecision::HardCap {
                            event,
                            fallback_route: request.fallback_route.clone(),
                        });
                    }
                }
            }
        }
        Ok(match soft_warning {
            Some(event) => PreSpawnDecision::SoftWarn(event),
            None => PreSpawnDecision::Allow,
        })
    }
}

/// Stored budget cap row used by the enforcer.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredCap {
    /// Cap metric.
    pub metric: CapMetric,
    /// Cap kind.
    pub kind: CapKind,
    /// Cap amount.
    pub amount: f64,
}

fn u64_to_f64(value: u64) -> LedgerResult<f64> {
    value.to_string().parse::<f64>().map_err(Into::into)
}
