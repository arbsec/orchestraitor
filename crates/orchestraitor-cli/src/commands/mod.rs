//! Implementations for `orc` subcommands.

pub mod board;
pub mod campaign;
pub mod config;
pub mod github;
pub mod loop_runner;
pub mod models;
pub mod routing;
pub mod worker;

use orchestraitor_agent_catalog::{
    DecisionEndpointConfig, NEURALWATT_CLEF_FLASH_DECISION_PROVIDER,
};

/// Builds the transport-backed decision provider named by the
/// `routing.provider` config flag (spec `30-model-routing.md` §9.45). This
/// crate owns the transport dependency, so the `neuralwatt-clef-flash`
/// adapter is constructed here; names this factory does not own return
/// `Ok(None)` (the agent-catalog resolver reports the typed
/// unknown-provider error for those). The `routing.base_url` /
/// `routing.model` / `routing.api_key` keys flow through
/// [`DecisionEndpointConfig`]: a self-hosted decision engine (for example a
/// local Metal-native Clef server over tailscale) sets `routing.base_url`
/// and leaves `routing.api_key` unset (no auth); the hosted Neuralwatt
/// deployment sets `routing.api_key = "secret://env/NEURALWATT_API_KEY"`.
/// Configuration failures (unresolvable credential, invalid base URL)
/// surface as a diagnostic — the operator asked for a decision provider, so
/// silently degrading to the heuristic table would mask a broken
/// configuration. The credential value never enters an error or log line.
///
/// # Errors
///
/// Returns the typed build failure message when the named provider cannot
/// be built.
pub(crate) fn build_named_decision_provider(
    name: &str,
    endpoint: &DecisionEndpointConfig,
) -> std::result::Result<Option<Box<dyn orchestraitor_provider_api::DecisionProvider>>, String> {
    match name {
        NEURALWATT_CLEF_FLASH_DECISION_PROVIDER => {
            let config = match endpoint.base_url.as_deref() {
                Some(base_url) => {
                    orchestraitor_provider_neuralwatt::NeuralwattConfig::with_endpoint(
                        base_url.to_string(),
                        // The override below replaces this placeholder before
                        // resolution; a no-auth endpoint never reads it.
                        "secret://none".to_string(),
                    )
                    .map_err(|error| error.to_string())?
                }
                None => orchestraitor_provider_neuralwatt::NeuralwattConfig::new(),
            };
            let provider =
                orchestraitor_provider_neuralwatt::NeuralwattDecisionProvider::with_endpoint(
                    config,
                    None,
                    endpoint.api_key_uri.as_deref(),
                    endpoint.model.clone().unwrap_or_else(|| {
                        orchestraitor_provider_neuralwatt::DEFAULT_DECISION_MODEL.to_string()
                    }),
                )
                .map_err(|error| error.to_string())?;
            Ok(Some(Box::new(provider)))
        }
        _ => Ok(None),
    }
}

/// The role the dispatched workers run as.
pub(crate) const WORKER_ROLE: &str = "implement";

/// Fails closed when the resolved routing does not target the bootstrap
/// provider: the daemon-less direct path wires a bootstrap transport that
/// speaks only `neuralwatt` (spec §10.3). The settings layer already
/// rejects a non-configured provider id (`RoleRouter::validate_pair`
/// checks the provider allowlist); this gate covers the one provider id
/// that remains routable in configuration but unrunnable on the direct
/// path. Shared verbatim by `orc campaign run`, `orc worker run`, and
/// `orc loop` — the three entry points that spawn the direct-path worker.
///
/// # Errors
///
/// Returns the bootstrap-only-provider diagnostic carrying the resolved
/// provider id.
pub(crate) fn require_bootstrap_provider(
    provider: &str,
) -> std::result::Result<(), miette::Report> {
    use orchestraitor_agent_catalog::BOOTSTRAP_PROVIDER;

    if provider != BOOTSTRAP_PROVIDER {
        return Err(miette::miette!(
            "bootstrap worker supports only the `{BOOTSTRAP_PROVIDER}` provider (spec §10.3); \
             roles.{WORKER_ROLE}.routing.provider resolved to `{provider}`"
        ));
    }
    Ok(())
}
