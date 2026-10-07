//! Static heuristic role routing for the bootstrap (spec
//! `30-model-routing.md` §9.45): orchestration roles resolve to `(provider, model)` through the
//! layered configuration (spec §9.22.2) under `roles.<id>.routing.*`, which
//! carries the same layer semantics as `agents.domains.<id>.routing.*` (spec
//! §9.19.2). The role registry is configuration, not a hardcoded taxonomy
//! (spec §9.22.4): every `roles.<id>` key in the effective configuration is a
//! custom role resolving through the same path as the six built-ins. A
//! `DecisionProvider` may be consulted first behind the default-off
//! `routing.provider` config flag (spec §9.45 `DecisionProvider`); the
//! heuristic table stays the default and the fallback chain when the
//! provider errors or is unavailable — the unavailability is recorded in the
//! decision record's `fallback_reason` (`SCHEMA_V1` field; a typed
//! `provider_confidence` column lands as `SCHEMA_V2` in a later slice).

use std::collections::BTreeSet;

use orchestraitor_core::{ConfigLayer, ConfigResolver, OrchestraitorConfig, ResolvedValue};
use orchestraitor_provider_api::DecisionProvider;

use crate::error::AgentCatalogError;
use crate::roles_registry::RoleRegistry;

use thiserror::Error as ThisError;

/// Bootstrap fallback provider id, the single-provider default from spec §10.3.
pub const BOOTSTRAP_PROVIDER: &str = "neuralwatt";

/// Bootstrap fallback model id, the single-provider default from spec §10.3.
///
/// `glm-5.2` is deprecated at the Neuralwatt endpoint (absent from
/// `/v1/models`); the default tracks a currently-served id. The routing
/// config (`roles.<role>.routing.model`) overrides this per deployment —
/// bump or pin there rather than relying on the fallback.
pub const BOOTSTRAP_MODEL: &str = "glm-5.3-flash";

/// Max accepted length of a routing model identifier.
const MAX_MODEL_ID_LEN: usize = 256;

/// Max accepted length of a configured provider identifier.
const MAX_PROVIDER_ID_LEN: usize = 64;

/// Max accepted length of a configured custom role identifier.
const MAX_ROLE_ID_LEN: usize = 64;

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

    /// Resolves `(provider, model)` for one orchestration role.
    ///
    /// A role is resolvable when it is one of the six built-in orchestration
    /// roles or when a `roles.<id>` key exists in the effective configuration
    /// (custom role, spec §9.22.4). A role with a complete
    /// `roles.<role>.routing.*` entry resolves to it, recording the layer that
    /// supplied each value. A role with no entry in any layer resolves via the
    /// documented single-provider bootstrap default from spec §10.3, recording
    /// the fallback reason. A partial entry (only one of `provider`/`model`
    /// set in the effective configuration) is a typed error naming the missing
    /// key — never a silent default-to-first-model.
    ///
    /// # Errors
    ///
    /// - [`AgentCatalogError::InvalidRoleId`] when `role` is a configured
    ///   custom role whose id violates the identifier shape rules.
    /// - [`AgentCatalogError::UnknownRole`] when `role` is neither built-in
    ///   nor configured; the error lists the known roles.
    /// - [`AgentCatalogError::MissingRoutingKey`] when the entry is partial;
    ///   the error names the missing configuration key.
    /// - [`AgentCatalogError::InvalidRoutingValue`] when a value has an invalid
    ///   identifier shape or names a provider outside the allowed set.
    /// - [`AgentCatalogError::Config`] when layered resolution itself fails
    ///   (for example an ambiguous same-layer conflict naming the key).
    pub fn resolve(self, role: &str) -> Result<RoleRoutingDecision, AgentCatalogError> {
        let registry = RoleRegistry::from_resolver(self.resolver)?;
        if !registry.is_known(role) {
            return Err(AgentCatalogError::UnknownRole {
                role: role.to_string(),
                known: registry.known_ids(),
            });
        }
        validate_role_id(role)?;
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

    /// Resolves one role consulting a [`DecisionProvider`] first (spec
    /// §9.45 "DecisionProvider"): a well-formed proposal wins and the
    /// provider's confidence is recorded in `precedence_path`; any provider
    /// error or unavailability falls back to the heuristic table with the
    /// unavailability documented in `fallback_reason`. The heuristic table —
    /// not the proposal — is validated against the layered configuration's
    /// provider allowlist before the provider is consulted, and the
    /// resolution is recorded either way.
    ///
    /// `provider_id` is the fixture proposal's own id, surfaced in the
    /// precedence path for evidence.
    ///
    /// # Errors
    ///
    /// Same as [`RoleRouter::resolve`]; a failing `DecisionProvider` is NOT
    /// an error — it engages the documented fallback chain.
    pub async fn resolve_with_decision_provider(
        self,
        role: &str,
        decision_provider: &dyn DecisionProvider,
    ) -> Result<RoleRoutingDecision, AgentCatalogError> {
        let heuristic = self.resolve(role)?;
        // A proposal grants no authority: only well-formed, provider-id-safe
        // values are adoptable, and the target provider must be routable in
        // the effective configuration (spec §9.45: untrusted typed data).
        let proposal = decision_provider.propose_role_resolution(role).await;
        let rejection = match proposal {
            Ok(proposal) => {
                let shape_ok = is_valid_provider_id(proposal.provider.as_str())
                    && is_valid_model_id(&proposal.model);
                let routable = shape_ok && {
                    let allowed = self.allowed_providers()?;
                    allowed.contains(proposal.provider.as_str())
                };
                if routable {
                    return Ok(RoleRoutingDecision {
                        role: role.to_string(),
                        provider: proposal.provider.as_str().to_string(),
                        model: proposal.model.clone(),
                        precedence_path: format!(
                            "decision-provider:{} (confidence {:.2}, {} alternative(s))",
                            decision_provider.id().as_str(),
                            proposal.confidence,
                            proposal.alternatives.len(),
                        ),
                        fallback_reason: None,
                    });
                }
                // Explicit attribution: a shape-invalid proposal and a
                // shape-valid but non-routable proposal are distinct
                // fallback causes in the decision record.
                if shape_ok {
                    format!(
                        "proposed provider '{}' is not a configured provider id",
                        proposal.provider
                    )
                } else {
                    format!(
                        "proposed provider '{}' or model '{}' failed identifier shape validation",
                        proposal.provider, proposal.model
                    )
                }
            }
            Err(error) => error.to_string(),
        };
        Ok(heuristic_with_provider_fallback(
            heuristic,
            decision_provider.id().as_str(),
            rejection.as_str(),
        ))
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

fn role_entry<'a>(
    config: &'a OrchestraitorConfig,
    role: &str,
) -> Option<&'a orchestraitor_core::config::RoutingConfig> {
    config.roles.as_ref()?.get(role)?.routing.as_ref()
}

/// Validates a custom role identifier shape: 1..=64 characters, ASCII
/// lowercase letters, digits, `-` or `_`, starting with a letter or digit —
/// the same shape rules as provider ids, so dotted role keys stay unambiguous
/// (a role id can never contain a path or key separator).
fn validate_role_id(role: &str) -> Result<(), AgentCatalogError> {
    let valid_shape = !role.is_empty()
        && role.len() <= MAX_ROLE_ID_LEN
        && role
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && role
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !valid_shape {
        return Err(AgentCatalogError::InvalidRoleId {
            role: role.to_string(),
        });
    }
    Ok(())
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

/// Documents a decision-provider unavailability in the heuristic decision's
/// `fallback_reason` (`SCHEMA_V1` field). When the heuristic resolution itself
/// already carried a fallback reason (for example `bootstrap-default`), both
/// facts are joined so no evidence is lost.
fn heuristic_with_provider_fallback(
    heuristic: RoleRoutingDecision,
    provider_id: &str,
    detail: &str,
) -> RoleRoutingDecision {
    let unavailability = format!(
        "decision provider '{provider_id}' unavailable: {detail}; applied the heuristic table fallback (spec 30-model-routing.md §9.45)"
    );
    let fallback_reason = Some(match heuristic.fallback_reason {
        Some(existing) => format!("{existing}; {unavailability}"),
        None => unavailability,
    });
    RoleRoutingDecision {
        fallback_reason,
        ..heuristic
    }
}

/// Typed error for `routing.provider` decision-provider resolution.
#[derive(Debug, ThisError)]
pub enum DecisionProviderConfigError {
    /// The configured decision provider name matches no implementation.
    #[error("unknown decision provider `{name}`; available: {available}")]
    Unknown {
        /// Configured name that matched nothing.
        name: String,
        /// Comma-separated list of available implementation names.
        available: String,
    },
    /// Layered configuration resolution failed before the flag could be
    /// read (for example an ambiguous same-layer conflict naming the key).
    #[error("decision provider configuration resolution failed: {0}")]
    Config(#[from] orchestraitor_core::OrchestraitorError),
    /// A transport-backed decision provider recognized the configured name
    /// but could not be built (for example an unresolvable API key).
    #[error("decision provider `{name}` could not be built: {message}")]
    Build {
        /// Configured provider name.
        name: String,
        /// Log-safe build failure reason.
        message: String,
    },
}

/// The only shipped decision-provider implementation name (the deterministic
/// fixture; the Neuralwatt Clef Flash adapter is built by the CLI layer,
/// which owns the transport dependency).
pub const FIXTURE_DECISION_PROVIDER: &str = "fixture";

/// The System One decision-protocol provider's config name (spec §9.45):
/// `routing.provider = "systemone"` selects the protocol-level
/// `SystemOneDecisionProvider` in `orchestraitor-provider-api`, pointed at
/// any System One-compatible endpoint through the REQUIRED
/// `routing.base_url`. Provider- and model-agnostic: the endpoint and model
/// are per-project configuration, never vendor names in code.
pub const SYSTEMONE_DECISION_PROVIDER: &str = "systemone";

/// Available decision-provider implementation names, in stable order.
pub const AVAILABLE_DECISION_PROVIDERS: [&str; 2] =
    [FIXTURE_DECISION_PROVIDER, SYSTEMONE_DECISION_PROVIDER];

/// Builds the decision provider named by the effective `routing.provider`
/// config value (spec §9.45, default off): `None` when the flag is unset
/// (heuristic table only), the deterministic fixture behind `"fixture"`,
/// and a typed unknown-provider error for any other value. The
/// `systemone` name is recognized by the CLI layer (which resolves the
/// project-scoped endpoint configuration and builds
/// `orchestraitor_provider_api::SystemOneDecisionProvider`); pass that
/// implementation through [`resolve_decision_provider_with`] when it must
/// resolve here.
///
/// # Errors
///
/// Returns [`DecisionProviderConfigError::Unknown`] for a configured name
/// that matches no available implementation and
/// [`DecisionProviderConfigError::Config`] when layered configuration
/// resolution itself fails.
pub fn resolve_decision_provider(
    resolver: &ConfigResolver,
) -> Result<Option<Box<dyn DecisionProvider>>, DecisionProviderConfigError> {
    resolve_decision_provider_with(resolver, &|_name, _endpoint| Ok(None))
}

/// The decision-endpoint configuration the effective `routing.*` block
/// carries (spec `30-model-routing.md` §9.45): everything the
/// transport-backed factories need beyond the provider name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionEndpointConfig {
    /// Optional endpoint base URL override (for example a self-hosted
    /// decision engine over tailscale). Absent keeps the implementation
    /// default.
    pub base_url: Option<String>,
    /// Optional decision model id override. Absent keeps the
    /// implementation default.
    pub model: Option<String>,
    /// Optional non-secret credential reference (`secret://…` URI text).
    /// Absent means the endpoint takes no auth; the factory resolves the
    /// reference, and the resolved value never enters this struct, an
    /// error, or a log.
    pub api_key_uri: Option<String>,
}

/// A caller-supplied factory for transport-backed decision-provider names:
/// `Ok(None)` for names the factory does not own, `Err(message)` for a
/// recognized name that cannot be built. Receives the endpoint
/// configuration resolved from the `routing.*` block so self-hosted
/// endpoints flow through without the factory re-reading config. The
/// `api_key` carries the non-secret URI reference only — the factory
/// resolves it, and the value never enters this struct, an error, or a log.
pub type ExternalDecisionProviderFactory<'a> =
    &'a dyn Fn(&str, &DecisionEndpointConfig) -> Result<Option<Box<dyn DecisionProvider>>, String>;

/// [`resolve_decision_provider`] with a caller-supplied factory for names
/// this crate does not build itself (the transport-backed adapters). The
/// factory returns `Ok(None)` for names it does not own and
/// `Err(message)` when a provider it owns cannot be built (for example an
/// unresolvable API key) — the message surfaces as a typed config error so
/// a broken decision-provider configuration is never silently degraded to
/// the heuristic table.
///
/// # Errors
///
/// Returns [`DecisionProviderConfigError::Unknown`] for a configured name
/// that no implementation claims, [`DecisionProviderConfigError::Build`]
/// when the external factory fails, and
/// [`DecisionProviderConfigError::Config`] when layered configuration
/// resolution itself fails.
pub fn resolve_decision_provider_with(
    resolver: &ConfigResolver,
    external: ExternalDecisionProviderFactory<'_>,
) -> Result<Option<Box<dyn DecisionProvider>>, DecisionProviderConfigError> {
    let routing = resolver.resolve_config()?.routing;
    let configured = routing
        .as_ref()
        .and_then(|routing| routing.provider.clone());
    let endpoint = DecisionEndpointConfig {
        base_url: routing
            .as_ref()
            .and_then(|routing| routing.base_url.clone()),
        model: routing.as_ref().and_then(|routing| routing.model.clone()),
        api_key_uri: routing
            .as_ref()
            .and_then(|routing| routing.api_key.as_ref())
            .map(orchestraitor_core::SecretUri::as_uri),
    };
    match configured.as_deref() {
        None => Ok(None),
        Some(FIXTURE_DECISION_PROVIDER) => Ok(Some(Box::new(
            orchestraitor_provider_api::FixtureDecisionProvider::new(),
        ))),
        Some(name) => match external(name, &endpoint) {
            Ok(Some(provider)) => Ok(Some(provider)),
            Ok(None) => Err(DecisionProviderConfigError::Unknown {
                name: name.to_string(),
                available: AVAILABLE_DECISION_PROVIDERS.join(", "),
            }),
            Err(message) => Err(DecisionProviderConfigError::Build {
                name: name.to_string(),
                message,
            }),
        },
    }
}

#[cfg(test)]
mod decision_provider_tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

    use orchestraitor_core::ConfigSource;
    use orchestraitor_provider_api::{
        DecisionProposal, DecisionProvider, DecisionResult, FixtureDecisionProvider, FixtureMode,
        TaskSelection,
    };

    use super::*;

    fn resolver_from(layers: &[(ConfigLayer, &str, &str)]) -> ConfigResolver {
        let mut resolver = ConfigResolver::new();
        for (layer, name, toml) in layers {
            resolver = resolver
                .with_toml(
                    ConfigSource {
                        layer: *layer,
                        name: name.to_string(),
                    },
                    toml,
                )
                .unwrap();
        }
        resolver
    }

    #[tokio::test]
    async fn flag_unset_router_behavior_is_identical_to_resolve() {
        // Default-off: with no decision provider supplied, the caller path is
        // plain `resolve`; this asserts the fixture path with flag unset is
        // byte-identical to the heuristic resolution.
        let resolver = resolver_from(&[]);
        let plain = RoleRouter::new(&resolver).resolve("implement").unwrap();
        assert_eq!(plain.provider, BOOTSTRAP_PROVIDER);
        assert_eq!(plain.model, BOOTSTRAP_MODEL);
        assert_eq!(plain.precedence_path, "bootstrap-default");
        assert!(
            plain
                .fallback_reason
                .as_deref()
                .unwrap_or("")
                .contains("bootstrap default")
        );
    }

    #[tokio::test]
    async fn flag_on_fixture_proposal_wins_with_confidence_recorded() {
        let resolver = resolver_from(&[]);
        let provider = FixtureDecisionProvider::new();
        let decision = RoleRouter::new(&resolver)
            .resolve_with_decision_provider("implement", &provider)
            .await
            .unwrap();
        // The fixture table proposes the same pair the heuristic table ships,
        // but the precedence path must carry the provider + confidence so
        // decision records prove the proposal won (spec §9.45).
        assert_eq!(decision.provider, "neuralwatt");
        assert_eq!(decision.model, "glm-5.2");
        assert!(
            decision
                .precedence_path
                .starts_with("decision-provider:fixture (confidence 0.95"),
            "precedence_path must record provider and confidence: {}",
            decision.precedence_path
        );
        assert!(decision.fallback_reason.is_none());
    }

    #[tokio::test]
    async fn flag_on_unavailable_provider_falls_back_with_unavailability_recorded() {
        let resolver = resolver_from(&[]);
        let provider = FixtureDecisionProvider::with_mode(FixtureMode::Unavailable);
        let decision = RoleRouter::new(&resolver)
            .resolve_with_decision_provider("implement", &provider)
            .await
            .unwrap();
        // Heuristic fallback engages with identical resolution...
        assert_eq!(decision.provider, BOOTSTRAP_PROVIDER);
        assert_eq!(decision.model, BOOTSTRAP_MODEL);
        assert_eq!(decision.precedence_path, "bootstrap-default");
        // ...and the unavailability is recorded in the SCHEMA_V1 record.
        let reason = decision
            .fallback_reason
            .expect("unavailability must be recorded");
        assert!(
            reason.contains("decision provider 'fixture' unavailable"),
            "fallback_reason must document the unavailability: {reason}"
        );
        assert!(
            reason.contains("heuristic table fallback"),
            "fallback_reason must document the fallback: {reason}"
        );
    }

    #[tokio::test]
    async fn flag_on_provider_error_over_table_entry_falls_back_to_layer_value() {
        let toml = "[roles.implement.routing]\nprovider = \"neuralwatt\"\nmodel = \"glm-5.2\"\n";
        let resolver = resolver_from(&[(ConfigLayer::Project, "project", toml)]);
        let provider = FixtureDecisionProvider::with_mode(FixtureMode::Unavailable);
        let decision = RoleRouter::new(&resolver)
            .resolve_with_decision_provider("implement", &provider)
            .await
            .unwrap();
        assert_eq!(decision.provider, "neuralwatt");
        assert_eq!(decision.model, "glm-5.2");
        assert!(
            decision.precedence_path.contains("project"),
            "layer attribution must be preserved: {}",
            decision.precedence_path
        );
        assert!(decision.fallback_reason.is_some());
    }

    #[tokio::test]
    async fn unknown_role_is_typed_error_even_with_provider_configured() {
        let resolver = resolver_from(&[]);
        let provider = FixtureDecisionProvider::new();
        let error = RoleRouter::new(&resolver)
            .resolve_with_decision_provider("nonexistent", &provider)
            .await
            .unwrap_err();
        assert!(matches!(error, AgentCatalogError::UnknownRole { .. }));
    }

    #[test]
    fn resolve_decision_provider_none_when_flag_unset() {
        let resolver = resolver_from(&[]);
        let built = resolve_decision_provider(&resolver).unwrap();
        assert!(built.is_none(), "default must stay off");
    }

    #[test]
    fn resolve_decision_provider_builds_fixture_when_configured() {
        let resolver = resolver_from(&[(
            ConfigLayer::Project,
            "project",
            "[routing]\nprovider = \"fixture\"\n",
        )]);
        let built = resolve_decision_provider(&resolver).unwrap();
        assert!(built.is_some());
        assert_eq!(
            built.as_ref().map(|provider| provider.id().as_str()),
            Some("fixture")
        );
    }

    #[test]
    fn resolve_decision_provider_with_builds_external_named_provider() {
        let resolver = resolver_from(&[(
            ConfigLayer::Project,
            "project",
            "[routing]\nprovider = \"systemone\"\n",
        )]);
        let built = resolve_decision_provider_with(&resolver, &|name, _endpoint| match name {
            SYSTEMONE_DECISION_PROVIDER => Ok(Some(Box::new(
                orchestraitor_provider_api::FixtureDecisionProvider::new(),
            ))),
            _ => Ok(None),
        })
        .unwrap();
        assert!(built.is_some());
    }

    #[test]
    fn resolve_decision_provider_without_external_factory_rejects_transport_names() {
        let resolver = resolver_from(&[(
            ConfigLayer::Project,
            "project",
            "[routing]\nprovider = \"systemone\"\n",
        )]);
        // Without a factory that owns the transport-backed name, the base
        // resolver reports the typed unknown-provider error listing both
        // shipped names.
        let Err(error) = resolve_decision_provider(&resolver) else {
            panic!("transport-backed names need an owning factory");
        };
        assert!(
            error.to_string().contains("unknown decision provider"),
            "{error}"
        );
        assert!(
            error.to_string().contains("systemone"),
            "available list must name the shipped implementations: {error}"
        );
    }

    #[test]
    fn resolve_decision_provider_with_surfaces_build_failures() {
        let resolver = resolver_from(&[(
            ConfigLayer::Project,
            "project",
            "[routing]\nprovider = \"systemone\"\n",
        )]);
        let Err(error) = resolve_decision_provider_with(&resolver, &|name, _endpoint| match name {
            SYSTEMONE_DECISION_PROVIDER => {
                Err("neuralwatt decision auth resolution failed: key missing".to_string())
            }
            _ => Ok(None),
        }) else {
            panic!("a failed external build must surface as a typed error");
        };
        assert!(error.to_string().contains("could not be built"), "{error}");
        assert!(
            error.to_string().contains("key missing"),
            "build message must be carried: {error}"
        );
    }

    #[test]
    fn resolve_decision_provider_unknown_value_is_typed_error() {
        let resolver = resolver_from(&[(
            ConfigLayer::Project,
            "project",
            "[routing]\nprovider = \"typesafe\"\n",
        )]);
        let Err(error) = resolve_decision_provider(&resolver) else {
            panic!("unknown provider value must be a typed error");
        };
        assert!(
            error
                .to_string()
                .contains("unknown decision provider `typesafe`"),
            "error must be the typed unknown-provider error: {error}"
        );
        assert!(error.to_string().contains("fixture"));
    }

    #[test]
    fn resolve_decision_provider_config_failure_is_not_unknown_provider() {
        // Two same-layer shards defining the same key are an ambiguous
        // conflict: the error must be attributed to configuration
        // resolution, not misreported as an unknown provider.
        let toml = "[routing]\nprovider = \"fixture\"\n";
        let mut resolver = ConfigResolver::new();
        for name in ["shard-a", "shard-b"] {
            resolver = resolver
                .with_toml(
                    ConfigSource {
                        layer: ConfigLayer::Project,
                        name: name.to_string(),
                    },
                    toml,
                )
                .unwrap();
        }
        let Err(error) = resolve_decision_provider(&resolver) else {
            panic!("ambiguous conflict must be a typed error");
        };
        assert!(
            error
                .to_string()
                .contains("configuration resolution failed"),
            "config failure must carry its own attribution: {error}"
        );
        assert!(
            !error.to_string().contains("unknown decision provider"),
            "config failure must not surface as unknown-provider: {error}"
        );
    }

    #[tokio::test]
    async fn shape_invalid_proposal_fallback_names_provider_and_model() {
        struct Malformed;
        static MALFORMED_ID: std::sync::LazyLock<orchestraitor_model::ProviderId> =
            std::sync::LazyLock::new(|| {
                orchestraitor_model::ProviderId::from_string("malformed".to_string())
            });
        #[async_trait::async_trait]
        impl DecisionProvider for Malformed {
            fn id(&self) -> &orchestraitor_model::ProviderId {
                &MALFORMED_ID
            }

            async fn propose_role_resolution(
                &self,
                _role: &str,
            ) -> DecisionResult<DecisionProposal> {
                DecisionProposal::new(
                    "implement",
                    orchestraitor_model::ProviderId::from_string("BAD ID".to_string()),
                    "bad model id with spaces",
                    0.9,
                    Vec::new(),
                )
            }

            async fn propose_task_selection(
                &self,
                _ready_task_ids: &[String],
            ) -> DecisionResult<TaskSelection> {
                TaskSelection::new("task-a", 1.0)
            }
        }

        let resolver = resolver_from(&[]);
        let decision = RoleRouter::new(&resolver)
            .resolve_with_decision_provider("implement", &Malformed)
            .await
            .unwrap();
        assert_eq!(decision.provider, BOOTSTRAP_PROVIDER);
        let reason = decision.fallback_reason.expect("refusal must be recorded");
        assert!(
            reason.contains("'BAD ID'") && reason.contains("'bad model id with spaces'"),
            "fallback_reason must name the rejected provider and model: {reason}"
        );
        assert!(
            reason.contains("shape validation"),
            "shape failures must be attributed as such: {reason}"
        );
    }

    #[tokio::test]
    async fn proposal_naming_unroutable_provider_falls_back() {
        // A proposal is untrusted typed data naming a target; a provider
        // outside the effective allowlist must never be adopted.
        struct Rogue;
        static ROGUE_ID: std::sync::LazyLock<orchestraitor_model::ProviderId> =
            std::sync::LazyLock::new(|| {
                orchestraitor_model::ProviderId::from_string("rogue".to_string())
            });
        #[async_trait::async_trait]
        impl DecisionProvider for Rogue {
            fn id(&self) -> &orchestraitor_model::ProviderId {
                &ROGUE_ID
            }

            async fn propose_role_resolution(
                &self,
                _role: &str,
            ) -> DecisionResult<DecisionProposal> {
                DecisionProposal::new(
                    "implement",
                    orchestraitor_model::ProviderId::from_string("unroutable".to_string()),
                    "evil-model",
                    1.0,
                    Vec::new(),
                )
            }

            async fn propose_task_selection(
                &self,
                _ready_task_ids: &[String],
            ) -> DecisionResult<TaskSelection> {
                TaskSelection::new("task-a", 1.0)
            }
        }

        let resolver = resolver_from(&[]);
        let decision = RoleRouter::new(&resolver)
            .resolve_with_decision_provider("implement", &Rogue)
            .await
            .unwrap();
        assert_eq!(decision.provider, BOOTSTRAP_PROVIDER);
        assert_eq!(decision.model, BOOTSTRAP_MODEL);
        let reason = decision.fallback_reason.expect("refusal must be recorded");
        assert!(
            reason.contains("not a configured provider id"),
            "fallback_reason must name the refusal cause: {reason}"
        );
    }
}
