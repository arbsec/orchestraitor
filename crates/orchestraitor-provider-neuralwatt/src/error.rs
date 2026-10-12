//! Error types for the Neuralwatt provider adapter.

use thiserror::Error;

/// Neuralwatt provider adapter failures without secret material.
#[derive(Debug, Error)]
pub enum NeuralwattError {
    /// The base URL host is forbidden (spec §10.3).
    #[error(
        "base URL host `{host}` is forbidden; use neuralwatt.com or z.ai instead of bigmodel.cn"
    )]
    ForbiddenHost {
        /// The forbidden host name.
        host: String,
    },
    /// The base URL could not be parsed.
    #[error("invalid base URL: {source}")]
    InvalidBaseUrl {
        /// Underlying URL parse error.
        #[source]
        source: url::ParseError,
    },
    /// Authentication resolution failed.
    #[error("neuralwatt auth resolution failed: {0}")]
    Auth(String),
    /// The HTTP request failed.
    #[error("neuralwatt HTTP request failed: {source}")]
    HttpRequest {
        /// Underlying reqwest error.
        #[source]
        source: reqwest::Error,
    },
    /// The provider returned an error status.
    #[error("neuralwatt provider returned HTTP {status}")]
    ProviderStatus {
        /// HTTP status code.
        status: u16,
        /// Non-secret response body excerpt.
        body_excerpt: String,
    },
    /// The response body could not be parsed.
    #[error("neuralwatt response parse failed: {source}")]
    ResponseParse {
        /// Underlying `serde_json` error.
        #[source]
        source: serde_json::Error,
    },
    /// The SSE stream contained an invalid event.
    #[error("neuralwatt SSE stream emitted an invalid event: {0}")]
    InvalidStreamEvent(String),
    /// The cost ledger rejected the entry.
    #[error("neuralwatt cost ledger error: {source}")]
    CostLedger {
        /// Underlying ledger error.
        #[source]
        source: orchestraitor_cost_ledger::LedgerError,
    },
}
