//! Static heuristic role routing for the bootstrap (spec `30-model-routing.md`
//! §9.45): the six built-in orchestration roles resolve to `(provider, model)`
//! through the layered configuration (spec §9.22.2) under `roles.<id>.routing.*`,
//! which carries the same layer semantics as `agents.domains.<id>.routing.*`
//! (spec §9.19.2). No decision model and no subscription awareness live here;
//! those deepen behind the `DecisionProvider` trait in the E2 milestone.

use std::collections::BTreeSet;

use orchestraitor_core::{ConfigLayer, ConfigResolver, OrchestraitorConfig, ResolvedValue};

use crate::AgentCatalogError;
use crate::registry::BUILT_IN_ORCHESTRATION_ROLES;

/// Bootstrap fallback provider id, the single-provider default from spec §10.3.
pub const BOOTSTRAP_PROVIDER: &str = "neuralwatt";

/// Bootstrap fallback model id, the single-provider default from spec §10.3.
pub const BOOTSTRAP_MODEL: &str = "glm-5.2";

/// Max accepted length of a routing model identifier.
const MAX_MODEL_ID_LEN: usize = 256;

/// Max accepted length of a configured provider identifier.
const MAX_PROVIDER_ID_LEN: usize = 64;

/// One persisted unit of routing evidence for a resolved role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRoutingDecision {
    /// Orchestration role id that was resolved.
    pub role: String,
    /// Resolved provider identifier.
    pub provider: String,
    /// Resolved model identifier.
    pub model: String,
    /// Precedence path that produced the resolution.
    pub precedence_path: String,
    /// Documented fallback reason; `None` when a table entry matched directly.
    pub fallback_reason: Option<String>,
}

/// Deterministic role router over the layered configuration resolver.
#[derive(Debug, Clone, Copy)]
pub struct RoleRouter<'a> {
    resolver: &'a ConfigResolver,
}

impl<'a> RoleRouter<'a> {
    /// Creates a role router over a layered configuration resolver.
    #[must_use]
    pub const fn new(resolver: &'a ConfigResolver) -> Self {
        Self { resolver }
    }

    /// Resolves `(provider, model)` for one built-in orchestration role.
    ///
    /// A role with a complete `roles.<role>.routing.*` entry resolves to it,
    /// recording the layer that supplied each value. A role with no entry in
    /// any layer resolves via the documented single-provider bootstrap default
    /// from spec §10.3, recording the fallback reason. A partial entry (only
    /// one of `provider`/`model` set in the effective configuration) is a typed
    /// error naming the missing key — never a silent default-to-first-model.
    ///
    /// # Errors
    ///
    /// - [`AgentCatalogError::UnknownRole`] when `role` is not one of the six
    ///   built-in orchestration roles (custom roles are an E2 non-goal here).
    /// - [`AgentCatalogError::MissingRoutingKey`] when the entry is partial;
    ///   the error names the missing configuration key.
    /// - [`AgentCatalogError::InvalidRoutingValue`] when a value has an invalid
    ///   identifier shape or names a provider outside the allowed set.
    /// - [`AgentCatalogError::Config`] when layered resolution itself fails
    ///   (for example an ambiguous same-layer conflict naming the key).
    pub fn resolve(self, role: &str) -> Result<RoleRoutingDecision, AgentCatalogError> {
        let known = known_role_ids();
        if !BUILT_IN_ORCHESTRATION_ROLES
            .iter()
            .any(|definition| definition.id == role)
        {
            return Err(AgentCatalogError::UnknownRole {
                role: role.to_string(),
                known,
            });
        }
        let provider_key = format!("roles.{role}.routing.provider");
        let model_key = format!("roles.{role}.routing.model");
        let provider = self.resolver.resolve_value(&provider_key, |config| {
            role_entry(config, role)?.provider.clone()
        })?;
        let model = self
            .resolver
            .resolve_value(&model_key, |config| role_entry(config, role)?.model.clone())?;
        match (provider, model) {
            (Some(provider), Some(model)) => {
                self.validate_pair(&provider_key, &model_key, &provider, &model)?;
                let precedence_path = format!(
                    "roles.{role}.routing (provider: {}, model: {})",
                    layer_tag(provider.source.layer),
                    layer_tag(model.source.layer),
                );
                Ok(RoleRoutingDecision {
                    role: role.to_string(),
                    provider: provider.value,
                    model: model.value,
                    precedence_path,
                    fallback_reason: None,
                })
            }
            (Some(_), None) => Err(AgentCatalogError::MissingRoutingKey { key: model_key }),
            (None, Some(_)) => Err(AgentCatalogError::MissingRoutingKey { key: provider_key }),
            (None, None) => Ok(RoleRoutingDecision {
                role: role.to_string(),
                provider: BOOTSTRAP_PROVIDER.to_string(),
                model: BOOTSTRAP_MODEL.to_string(),
                precedence_path: "bootstrap-default".to_string(),
                fallback_reason: Some(format!(
                    "role '{role}' has no `roles.{role}.routing.*` entry in any \
                     configuration layer; applied the documented bootstrap default \
                     `{BOOTSTRAP_PROVIDER}/{BOOTSTRAP_MODEL}` (spec §10.3)"
                )),
            }),
        }
    }

    fn validate_pair(
        self,
        provider_key: &str,
        model_key: &str,
        provider: &ResolvedValue<String>,
        model: &ResolvedValue<String>,
    ) -> Result<(), AgentCatalogError> {
        if !is_valid_provider_id(&provider.value) {
            return Err(AgentCatalogError::InvalidRoutingValue {
                key: provider_key.to_string(),
                reason: format!(
                    "provider id '{}' must be 1..={MAX_PROVIDER_ID_LEN} lowercase ASCII \
                     letters, digits, `-` or `_`, starting with a letter or digit",
                    provider.value
                ),
            });
        }
        if !is_valid_model_id(&model.value) {
            return Err(AgentCatalogError::InvalidRoutingValue {
                key: model_key.to_string(),
                reason: format!(
                    "model id '{}' must be 1..={MAX_MODEL_ID_LEN} ASCII letters, digits, \
                     or `-`, `_`, `.`, `/`, `:`, starting with a letter or digit",
                    model.value
                ),
            });
        }
        let allowed = self.allowed_providers()?;
        if !allowed.contains(provider.value.as_str()) {
            return Err(AgentCatalogError::InvalidRoutingValue {
                key: provider_key.to_string(),
                reason: format!(
                    "provider '{}' is not a configured provider id; allowed: {}",
                    provider.value,
                    allowed.into_iter().collect::<Vec<_>>().join(", ")
                ),
            });
        }
        Ok(())
    }

    fn allowed_providers(self) -> Result<BTreeSet<String>, AgentCatalogError> {
        let config = self.resolver.resolve_config()?;
        let mut allowed = BTreeSet::new();
        if let Some(providers) = config.providers.as_ref() {
            allowed.extend(providers.keys().cloned());
        }
        // The bootstrap target is always routable: built-in defaults may route
        // to it before any `providers.*` block exists (spec §10.3).
        allowed.insert(BOOTSTRAP_PROVIDER.to_string());
        Ok(allowed)
    }
}

/// Builds the decision record payload for one resolution.
impl RoleRoutingDecision {
    /// Comma-separated list of the six built-in orchestration role ids.
    #[must_use]
    pub fn built_in_role_ids() -> String {
        known_role_ids()
    }
}

fn role_entry<'a>(
    config: &'a OrchestraitorConfig,
    role: &str,
) -> Option<&'a orchestraitor_core::config::RoutingConfig> {
    config.roles.as_ref()?.get(role)?.routing.as_ref()
}

fn known_role_ids() -> String {
    BUILT_IN_ORCHESTRATION_ROLES
        .iter()
        .map(|definition| definition.id)
        .collect::<Vec<_>>()
        .join(", ")
}

fn is_valid_provider_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PROVIDER_ID_LEN
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn is_valid_model_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MODEL_ID_LEN
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':'))
}

fn layer_tag(layer: ConfigLayer) -> &'static str {
    match layer {
        ConfigLayer::BuiltInDefaults => "built-in-defaults",
        ConfigLayer::PluginDefaults => "plugin-defaults",
        ConfigLayer::GlobalUser => "user",
        ConfigLayer::OrganizationTeam => "org",
        ConfigLayer::Project => "project",
        ConfigLayer::DirectoryDomain => "dir",
        ConfigLayer::TaskAgent => "task-agent",
        ConfigLayer::CliFlag => "cli-flag",
    }
}
