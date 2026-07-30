//! Provider transport error types.

use orchestraitor_model::ProviderId;
use thiserror::Error;

/// Provider transport failures without secret material.
#[derive(Debug, Error)]
pub enum ProviderTransportError {
    /// Provider rejected or could not satisfy the request.
    #[error("provider request failed for `{provider_id}`")]
    RequestFailed {
        /// Explicit provider id.
        provider_id: ProviderId,
    },
    /// Provider streamed an invalid event.
    #[error("provider stream emitted an invalid event")]
    InvalidEvent,
    /// Provider capability was not available.
    #[error("provider capability `{capability}` is unavailable for `{provider_id}`")]
    CapabilityUnavailable {
        /// Explicit provider id.
        provider_id: ProviderId,
        /// Missing capability name.
        capability: &'static str,
    },
}
