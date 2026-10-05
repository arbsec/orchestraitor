//! Implementations for `orc` subcommands.

pub mod board;
pub mod campaign;
pub mod config;
pub mod github;
pub mod loop_runner;
pub mod models;
pub mod routing;
pub mod worker;

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
