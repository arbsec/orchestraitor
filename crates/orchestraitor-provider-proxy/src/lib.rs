//! Provider-compatible local proxy for `OpenAI` and Anthropic protocol surfaces.
//!
//! The proxy implements spec §10.1 Mode D: model traffic routing, local short-lived
//! authentication, credential isolation, telemetry, and cost attribution. It does
//! not claim filesystem or shell containment for tools executed by external harnesses.

#![forbid(unsafe_code)]

mod auth;
mod body;
mod cost;
mod error;
mod protocol;
mod service;
mod trust_boundary;

pub use auth::{ChildEnvironment, LocalAuthToken, LocalTokenIssuer, UpstreamCredentialBroker};
pub use cost::{CostAttribution, CostRecorder};
pub use error::{ProxyError, ProxyResult};
pub use protocol::{ProtocolSurface, ProviderRegistry, ProviderRoute};
pub use service::{ProxyHttpService, ProxyRequestBody, ProxyResponseBody, TRUST_BOUNDARY_HEADER};
pub use trust_boundary::{TrustBoundaryReport, TrustBoundaryStatus};
