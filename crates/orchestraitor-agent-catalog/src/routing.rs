//! Model routing resolver for domain and role invocations.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::AgentCatalogError;

/// Concrete provider and model selected for an invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    /// Provider identifier selected by the control plane.
    pub provider: String,
    /// Model identifier selected by the control plane.
    pub model: String,
}

/// Routing rules keyed by the six spec §9.19.2 precedence steps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingTable {
    /// Per-agent explicit override keyed by agent id.
    pub explicit_agent_overrides: BTreeMap<String, Route>,
    /// Domain plus role overrides keyed as `domain.role`.
    pub domain_role_overrides: BTreeMap<String, Route>,
    /// Domain default routes keyed by domain id.
    pub domain_defaults: BTreeMap<String, Route>,
    /// Role default routes keyed by role id.
    pub role_defaults: BTreeMap<String, Route>,
    /// Project default route.
    pub project_default: Option<Route>,
    /// Global default route.
    pub global_default: Option<Route>,
}

/// Input tuple for resolving one agent invocation route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingRequest<'a> {
    /// Optional instantiated agent id supplied by the caller.
    pub agent_id: Option<&'a str>,
    /// Domain id for the invocation.
    pub domain: &'a str,
    /// Role id for the invocation.
    pub role: &'a str,
}

/// Precedence step that supplied the resolved route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchedStep {
    /// Explicit agent override matched.
    ExplicitAgentOverride,
    /// Domain plus role override matched.
    DomainRoleOverride,
    /// Domain default matched.
    DomainDefault,
    /// Role default matched.
    RoleDefault,
    /// Project default matched.
    ProjectDefault,
    /// Global default matched.
    GlobalDefault,
}

/// Resolved model route plus the precedence step that matched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRoute {
    /// Provider identifier selected by the resolver.
    pub provider: String,
    /// Model identifier selected by the resolver.
    pub model: String,
    /// Precedence step that supplied this route.
    pub matched_step: MatchedStep,
}

/// Deterministic model route resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingResolver {
    table: RoutingTable,
}

impl RoutingTable {
    /// Builds a stable key for `domain + role` overrides.
    #[must_use]
    pub fn domain_role_key(domain: &str, role: &str) -> String {
        format!("{domain}.{role}")
    }
}

impl RoutingResolver {
    /// Creates a resolver from a routing table.
    #[must_use]
    pub const fn new(table: RoutingTable) -> Self {
        Self { table }
    }

    /// Resolves `(provider, model)` using the six-step spec §9.19.2 chain.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCatalogError::MissingRoute`] when no route exists.
    pub fn resolve(&self, request: RoutingRequest<'_>) -> Result<ResolvedRoute, AgentCatalogError> {
        if let Some(agent_id) = request.agent_id
            && let Some(route) = self.table.explicit_agent_overrides.get(agent_id)
        {
            return Ok(resolved(route, MatchedStep::ExplicitAgentOverride));
        }
        let key = RoutingTable::domain_role_key(request.domain, request.role);
        if let Some(route) = self.table.domain_role_overrides.get(&key) {
            return Ok(resolved(route, MatchedStep::DomainRoleOverride));
        }
        if let Some(route) = self.table.domain_defaults.get(request.domain) {
            return Ok(resolved(route, MatchedStep::DomainDefault));
        }
        if let Some(route) = self.table.role_defaults.get(request.role) {
            return Ok(resolved(route, MatchedStep::RoleDefault));
        }
        if let Some(route) = self.table.project_default.as_ref() {
            return Ok(resolved(route, MatchedStep::ProjectDefault));
        }
        if let Some(route) = self.table.global_default.as_ref() {
            return Ok(resolved(route, MatchedStep::GlobalDefault));
        }
        Err(AgentCatalogError::MissingRoute {
            domain: request.domain.to_string(),
            role: request.role.to_string(),
        })
    }
}

fn resolved(route: &Route, matched_step: MatchedStep) -> ResolvedRoute {
    ResolvedRoute {
        provider: route.provider.clone(),
        model: route.model.clone(),
        matched_step,
    }
}
