//! Provider transport traits, authentication resolution, and redacted tracing.
//!
//! This crate owns Orchestraitor's provider-facing public API. Concrete HTTP
//! clients remain implementation details behind [`ProviderTransport`]; no
//! provider SDK or `reqwest` types cross this crate boundary.

#![forbid(unsafe_code)]

pub mod auth;
pub mod capabilities;
pub mod error;
pub mod trace;
pub mod transport;

pub use auth::{
    AuthError, AuthResolver, EnvAuthResolver, RedactedSecret, SecretReference, resolve_secret_uri,
};
pub use capabilities::{
    CapabilitySupport, DiscoveredModel, ModelMetadataSource, ProviderCapabilities,
};
pub use error::ProviderTransportError;
pub use trace::{RedactingLayer, is_sensitive_trace_field};
pub use transport::{
    MessageRole, ModelEvent, ModelEventStream, ModelMessage, ModelRequest, ProviderDescriptor,
    ProviderHealth, ProviderHealthStatus, ProviderProtocol, ProviderResult, ProviderTransport,
    ReasoningConfig, ReasoningEffort, TokenCount, TokenCountRequest, ToolChoice,
};
