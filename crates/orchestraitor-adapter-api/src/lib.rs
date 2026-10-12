//! Agent adapter interface for native and wrapped coding-agent harnesses.
//!
//! This crate owns the spec §10.6 [`AgentAdapter`] boundary. It contains no
//! adapter implementations and no security decision logic; adapter hosts must
//! call Arbitraitor before invoking side-effecting adapter operations.

#![forbid(unsafe_code)]

mod error;
#[cfg(test)]
mod tests;
mod types;

use async_trait::async_trait;

pub use error::{AdapterError, AdapterResult};
pub use types::{
    AdapterEnvironment, AdapterEvent, AdapterManifest, AgentInput, AgentSession, AuthRequirement,
    ContextControlLevel, EventStream, PermissionInterceptionLevel, ProbeResult, ResumeRequest,
    StartRequest, TokenTelemetryQuality, ToolInterceptionLevel, TransportMode,
};

/// Project-owned interface implemented by native and wrapped agent adapters.
#[async_trait]
pub trait AgentAdapter: Send + Sync {
    /// Returns this adapter's static manifest.
    fn manifest(&self) -> &AdapterManifest;

    /// Probes whether this adapter can run in the supplied environment.
    ///
    /// # Errors
    /// Returns [`AdapterError`] when probing fails.
    async fn probe(&self, environment: &AdapterEnvironment) -> AdapterResult<ProbeResult>;

    /// Starts a new adapter-owned agent session.
    ///
    /// # Errors
    /// Returns [`AdapterError`] when the adapter cannot start the session.
    async fn start(&self, request: StartRequest) -> AdapterResult<AgentSession>;

    /// Resumes an existing adapter-owned agent session.
    ///
    /// # Errors
    /// Returns [`AdapterError`] when the adapter cannot resume the session.
    async fn resume(&self, request: ResumeRequest) -> AdapterResult<AgentSession>;

    /// Sends input to a running session.
    ///
    /// # Errors
    /// Returns [`AdapterError`] when the adapter rejects or loses the input.
    async fn send(&self, session: &AgentSession, input: AgentInput) -> AdapterResult<()>;

    /// Cancels in-flight work for a running session.
    ///
    /// # Errors
    /// Returns [`AdapterError`] when cancellation cannot be delivered.
    async fn cancel(&self, session: &AgentSession) -> AdapterResult<()>;

    /// Returns the adapter event stream for a running session.
    ///
    /// # Errors
    /// Returns [`AdapterError`] when the event stream cannot be opened.
    async fn events(&self, session: &AgentSession) -> AdapterResult<EventStream>;

    /// Shuts down adapter resources for a session.
    ///
    /// # Errors
    /// Returns [`AdapterError`] when shutdown fails.
    async fn shutdown(&self, session: AgentSession) -> AdapterResult<()>;
}
