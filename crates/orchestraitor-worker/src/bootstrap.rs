//! Production wiring for the bootstrap worker (spec §10.3 single provider).
//!
//! The loop consumes `&dyn ProviderTransport`; this module builds the
//! bootstrap transport — the Neuralwatt GLM-5.2 BYOK adapter — from process
//! configuration. The hidden endpoint override exists for simulator-backed
//! tests, mirroring the CLI's other hidden test endpoints.

use orchestraitor_provider_neuralwatt::{NeuralwattConfig, NeuralwattTransport};

use crate::error::WorkerError;

/// Builds the bootstrap `ProviderTransport` (Neuralwatt, spec §10.3).
///
/// The API key resolves through the adapter's configured secret URI
/// (`secret://keyring/neuralwatt`, falling back to `NEURALWATT_API_KEY`) —
/// never from an ambient credential sniff.
///
/// # Errors
///
/// Returns [`WorkerError::Transport`] with a static reason code when the
/// configuration or key resolution fails.
pub fn build_bootstrap_transport(
    endpoint_override: Option<String>,
) -> Result<NeuralwattTransport, WorkerError> {
    let mut config = NeuralwattConfig::new();
    if let Some(endpoint) = endpoint_override {
        config = config.with_base_url(endpoint).map_err(|error| {
            let reason = match &error {
                orchestraitor_provider_neuralwatt::NeuralwattError::ForbiddenHost { .. } => {
                    "forbidden-host"
                }
                _ => "invalid-base-url",
            };
            WorkerError::Transport { reason }
        })?;
    }
    NeuralwattTransport::from_config(config).map_err(|error| {
        let reason = match &error {
            orchestraitor_provider_neuralwatt::NeuralwattError::Auth(_) => "auth-resolution-failed",
            _ => "transport-construction",
        };
        WorkerError::Transport { reason }
    })
}
