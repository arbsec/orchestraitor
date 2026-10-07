//! Provider transport traits, authentication resolution, and redacted tracing.
//!
//! This crate owns Orchestraitor's provider-facing public API. Concrete HTTP
//! clients remain implementation details behind [`ProviderTransport`]; no
//! provider SDK or `reqwest` types cross this crate boundary.
//!
//! Decision-model-backed selection (spec `30-model-routing.md` §9.45) lives
//! behind the separate [`DecisionProvider`] trait: a single-shot
//! structured-output request/response class with calibrated confidence — no
//! message streams. The [`FixtureDecisionProvider`] is the deterministic
//! conformance target; the Neuralwatt-hosted Clef Flash decision model
//! (Apache-2.0, open source) is the reference decision model behind the
//! `NeuralwattDecisionProvider` in `orchestraitor-provider-neuralwatt`.

#![forbid(unsafe_code)]

pub mod auth;
pub mod capabilities;
pub mod decision;
pub mod decision_fixture;
pub mod error;
pub mod trace;
pub mod transport;

pub use auth::{
    AuthError, AuthResolver, EnvAuthResolver, RedactedSecret, SecretReference, resolve_secret_uri,
};
pub use capabilities::{
    CapabilitySupport, DiscoveredModel, ModelMetadataSource, ProviderCapabilities,
};
pub use decision::{
    DecisionAlternative, DecisionProposal, DecisionProvider, DecisionResult, TaskSelection,
    TaskSplitProposal, TaskSplitSubtask, TaskSummary, ToolDescriptor, ToolQueryContext,
    ToolSelection,
};
pub use decision_fixture::{FixtureDecisionProvider, FixtureMode};
pub use error::{DecisionProviderError, ProviderTransportError};
pub use trace::{RedactingLayer, is_sensitive_trace_field};
pub use transport::{
    MessageRole, ModelEvent, ModelEventStream, ModelMessage, ModelRequest, ProviderDescriptor,
    ProviderHealth, ProviderHealthStatus, ProviderProtocol, ProviderResult, ProviderTransport,
    ReasoningConfig, ReasoningEffort, TokenCount, TokenCountRequest, ToolChoice,
};
