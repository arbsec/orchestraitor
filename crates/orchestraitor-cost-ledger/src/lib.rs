//! Cost, usage, subscription, and budget ledger for Orchestraitor.
//!
//! This crate implements spec §9.19.4-§9.19.6. It records provider usage,
//! keeps metered API spend separate from subscription utilization, and enforces
//! non-security budget caps before provider invocation.

#![forbid(unsafe_code)]

pub mod budget;
pub mod error;
pub mod model;
pub mod storage;

pub use budget::{
    BudgetEnforcer, BudgetRequest, BudgetScope, BudgetWarningEvent, CapKind, CapMetric,
    PreSpawnDecision, RoutingFallback,
};
pub use error::{LedgerError, LedgerResult};
pub use model::{
    ApiSpendRecord, CostEntry, DomainCostRollup, MonetaryCostBasis, Subscription, SubscriptionId,
    SubscriptionUtilizationEntry, UtilizationLabel,
};
pub use storage::{ApiSpendTable, CostLedger, SubscriptionUtilizationTable};
