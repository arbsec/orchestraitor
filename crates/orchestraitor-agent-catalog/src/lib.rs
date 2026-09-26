//! Domain and role catalog, model routing, and project-domain detection.
//!
//! This crate contains registry data and deterministic resolver logic only. It
//! instantiates no agents and performs no security enforcement; the `security`
//! domain is analysis-only, and all enforcement remains Arbitraitor-owned.

#![forbid(unsafe_code)]

pub mod decision_store;
pub mod detection;
pub mod error;
pub mod registry;
pub mod role_routing;
pub mod routing;

pub use decision_store::{
    LATEST_SCHEMA_VERSION as DECISION_STORE_SCHEMA_VERSION, RoleRoutingDecisionStore,
    StoredRoleRoutingDecision,
};
pub use detection::{DetectedDomain, DetectionArtifact, DetectionRuleSet, Detector};
pub use error::{AgentCatalogError, AgentCatalogResult};
pub use registry::{
    BUILT_IN_DOMAINS, BUILT_IN_ORCHESTRATION_ROLES, BUILT_IN_ROLES, DomainDefinition,
    RoleDefinition,
};
pub use role_routing::{BOOTSTRAP_MODEL, BOOTSTRAP_PROVIDER, RoleRouter, RoleRoutingDecision};
pub use routing::{
    MatchedStep, ResolvedRoute, Route, RoutingRequest, RoutingResolver, RoutingTable,
};

/// Re-exported core config source metadata for callers that attach route provenance.
pub use orchestraitor_core::ConfigSource;
/// Re-exported model data-sensitivity labels used by catalog consumers.
pub use orchestraitor_model::DataSensitivity;

#[cfg(test)]
mod tests;
