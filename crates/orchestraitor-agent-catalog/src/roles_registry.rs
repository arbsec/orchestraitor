//! Queryable role registry for the orchestration loop (spec
//! `30-model-routing.md` §9.45, §9.22.4): the registry is configuration, not a
//! hardcoded taxonomy. The six built-in orchestration roles are always known;
//! every `roles.<id>` key in the effective layered configuration registers a
//! custom role that resolves through the same routing path as a built-in.

use std::collections::BTreeMap;

use orchestraitor_core::{ConfigResolver, OrchestraitorConfig};

use crate::error::AgentCatalogError;
use crate::registry::BUILT_IN_ORCHESTRATION_ROLES;

/// Whether a registry role is built-in or user-defined through configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RegistryRoleKind {
    /// One of the six built-in orchestration roles from spec §9.45.
    BuiltIn,
    /// A custom role registered by a `roles.<id>` key in a configuration layer.
    Custom,
}

/// One role in the role registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRole {
    /// Stable role identifier used in configuration and events.
    pub id: String,
    /// Whether the role is built-in or custom.
    pub kind: RegistryRoleKind,
    /// Human-readable description; `Some` for built-in roles, `None` for
    /// custom roles (the configuration schema carries no description field).
    pub description: Option<&'static str>,
}

/// The role registry: the built-in orchestration roles plus every custom role
/// defined by a `roles.<id>` key in the effective configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRegistry {
    kinds: BTreeMap<String, RegistryRoleKind>,
}

impl RoleRegistry {
    /// Builds the registry from a layered configuration resolver.
    ///
    /// # Errors
    ///
    /// Returns [`AgentCatalogError::Config`] when resolving the effective
    /// merged configuration fails (for example an ambiguous same-layer
    /// conflict naming the conflicting key).
    pub fn from_resolver(resolver: &ConfigResolver) -> Result<Self, AgentCatalogError> {
        let config = resolver.resolve_config()?;
        Ok(Self::from_config(&config))
    }

    /// Builds the registry from an effective merged configuration. A
    /// configured key that collides with a built-in role id stays built-in.
    fn from_config(config: &OrchestraitorConfig) -> Self {
        let mut kinds = BTreeMap::new();
        for definition in BUILT_IN_ORCHESTRATION_ROLES {
            kinds.insert(definition.id.to_string(), RegistryRoleKind::BuiltIn);
        }
        if let Some(configured) = config.roles.as_ref() {
            for id in configured.keys() {
                kinds.entry(id.clone()).or_insert(RegistryRoleKind::Custom);
            }
        }
        Self { kinds }
    }

    /// Whether `role` is a built-in orchestration role or a configured custom
    /// role.
    #[must_use]
    pub fn is_known(&self, role: &str) -> bool {
        self.kinds.contains_key(role)
    }

    /// All known roles: built-ins in spec order first, then custom roles by
    /// id.
    #[must_use]
    pub fn list(&self) -> Vec<RegistryRole> {
        let mut roles = Vec::with_capacity(self.kinds.len());
        for definition in BUILT_IN_ORCHESTRATION_ROLES {
            if self.kinds.get(definition.id) == Some(&RegistryRoleKind::BuiltIn) {
                roles.push(RegistryRole {
                    id: definition.id.to_string(),
                    kind: RegistryRoleKind::BuiltIn,
                    description: Some(definition.description),
                });
            }
        }
        for (id, kind) in &self.kinds {
            if *kind == RegistryRoleKind::Custom {
                roles.push(RegistryRole {
                    id: id.clone(),
                    kind: *kind,
                    description: None,
                });
            }
        }
        roles
    }

    /// Comma-separated known role ids, for typed error messages.
    #[must_use]
    pub fn known_ids(&self) -> String {
        self.list()
            .iter()
            .map(|role| role.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}
