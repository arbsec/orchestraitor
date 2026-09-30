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

/// Decision provider failures (spec `30-model-routing.md` §9.45).
///
/// A provider error means "unavailable for this decision": callers fall back
/// to the heuristic table and record the unavailability. No variant carries
/// secret material.
#[derive(Debug, Error)]
pub enum DecisionProviderError {
    /// The provider could not be reached or refused the single-shot request.
    #[error("decision provider `{provider_id}` is unavailable: {reason}")]
    Unavailable {
        /// Stable decision-provider id.
        provider_id: ProviderId,
        /// Log-safe, non-secret reason.
        reason: String,
    },
    /// A proposal or selection carried confidence outside `0.0..=1.0`
    /// (NaN included). Never clamped — the output is rejected.
    #[error("decision provider returned invalid confidence {value}: must be within 0.0..=1.0")]
    InvalidConfidence {
        /// The rejected raw confidence value.
        value: f64,
    },
    /// A decision provider id matched no available implementation.
    #[error("unknown decision provider `{name}`; available: {available}")]
    UnknownProvider {
        /// Configured provider name that matched nothing.
        name: String,
        /// Comma-separated list of available implementation names.
        available: String,
    },
    /// A structured output failed to deserialize with valid confidence.
    #[error("decision provider returned a malformed structured output: {0}")]
    MalformedOutput(String),
}
