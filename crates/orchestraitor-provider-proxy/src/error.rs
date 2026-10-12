//! Error types for the provider proxy.

use orchestraitor_provider_api::{AuthError, ProviderTransportError};
use thiserror::Error;

/// Convenience result type for provider proxy operations.
pub type ProxyResult<T> = Result<T, ProxyError>;

/// Provider proxy failures. Secret material is never included in variants.
#[derive(Debug, Error)]
pub enum ProxyError {
    /// Local bearer token was missing, expired, or unknown.
    #[error("local proxy authentication failed")]
    LocalAuthentication,
    /// No provider route exists for a requested model or surface.
    #[error("no provider route is configured for model `{model}` on `{surface}`")]
    RouteMissing {
        /// Requested model id text.
        model: String,
        /// Requested protocol surface.
        surface: &'static str,
    },
    /// Request path or method is unsupported by the proxy.
    #[error("unsupported provider proxy endpoint `{method} {path}`")]
    UnsupportedEndpoint {
        /// HTTP method.
        method: String,
        /// HTTP path.
        path: String,
    },
    /// Request body was invalid JSON or incompatible with the selected surface.
    #[error("invalid provider proxy request body: {message}")]
    InvalidRequest {
        /// Non-secret parse or schema message.
        message: String,
    },
    /// Upstream authentication failed without exposing credential material.
    #[error(transparent)]
    UpstreamAuth(#[from] AuthError),
    /// Provider transport failed.
    #[error(transparent)]
    Provider(#[from] ProviderTransportError),
    /// Cost ledger write failed.
    #[error(transparent)]
    Ledger(#[from] orchestraitor_cost_ledger::LedgerError),
    /// JSON serialization failed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// HTTP body collection failed.
    #[error("failed to read request body")]
    Body,
    /// HTTP response construction failed.
    #[error("failed to build HTTP response")]
    Http,
}
