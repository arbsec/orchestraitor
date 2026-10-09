//! Implementations for `orc` subcommands.

pub mod board;
pub mod campaign;
pub mod config;
pub mod github;
pub mod loop_runner;
pub mod models;
pub mod routing;
pub mod worker;

use orchestraitor_agent_catalog::{DecisionEndpointConfig, SYSTEMONE_DECISION_PROVIDER};

/// Builds the decision provider named by the `routing.provider` config flag
/// (spec `30-model-routing.md` §9.45). The `systemone` name selects the
/// protocol-level `SystemOneDecisionProvider` in `orchestraitor-provider-api`
/// — provider- and model-agnostic, pointed at whatever System
/// One-compatible endpoint the PROJECT's `[routing]` block names:
///
/// - `routing.base_url` — REQUIRED for `systemone` (the Neuralwatt cloud,
///   a local self-hosted Clef engine over tailscale, anything else serving
///   the protocol). Its absence is a typed build failure, never a silent
///   default URL: the endpoint is per-project operator configuration.
/// - `routing.model` — any model id the endpoint serves (default
///   `clef-flash`).
/// - `routing.api_key` — optional `secret://…` URI; absent or `none` sends
///   no `Authorization` header. The resolved credential never enters an
///   error or log line.
///
/// Names this factory does not own return `Ok(None)` (the agent-catalog
/// resolver reports the typed unknown-provider error). Configuration
/// failures surface as a typed build failure — the operator asked for a
/// decision provider, so silently degrading to the heuristic table would
/// mask a broken configuration.
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
        SYSTEMONE_DECISION_PROVIDER => {
            let base_url = endpoint.base_url.clone().ok_or_else(|| {
                "routing.base_url is required for routing.provider = \"systemone\": \
                 the System One protocol has no default endpoint — point it at \
                 your decision endpoint in the project's orchestraitor.toml"
                    .to_string()
            })?;
            let resolved = orchestraitor_provider_api::resolve_endpoint(
                base_url,
                endpoint.model.clone(),
                endpoint.api_key_uri.as_deref(),
            )
            .map_err(|error| error.to_string())?;
            let provider = orchestraitor_provider_api::SystemOneDecisionProvider::new(resolved)
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
