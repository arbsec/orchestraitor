//! Adapter API data types.

use std::path::PathBuf;

use orchestraitor_events::EventCategory;
use orchestraitor_model::{AdapterId, AgentId, OperationId, Session, SessionId, Workspace};
use orchestraitor_provider_api::{ModelMessage, ProviderDescriptor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AdapterResult;

/// Boxed event stream returned by adapters.
pub type EventStream = Box<dyn Iterator<Item = AdapterResult<AdapterEvent>> + Send>;

/// Static adapter capabilities declared before a session starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterManifest {
    /// Stable adapter identifier.
    pub id: AdapterId,
    /// Human-readable adapter name.
    pub display_name: String,
    /// Adapter implementation version or upstream harness version constraint.
    pub version_compatibility: String,
    /// Supported target platform triples or OS labels.
    pub supported_platforms: Vec<String>,
    /// Transport mode used to drive the harness.
    pub transport_mode: TransportMode,
    /// Authentication capabilities required by the adapter.
    pub authentication: Vec<AuthRequirement>,
    /// Context-window control available to Orchestraitor.
    pub context_control: ContextControlLevel,
    /// Tool-call interception available to Orchestraitor.
    pub tool_interception: ToolInterceptionLevel,
    /// Permission-request interception available to Orchestraitor.
    pub permission_interception: PermissionInterceptionLevel,
    /// Whether this adapter can resume an existing session.
    pub supports_session_resume: bool,
    /// Token telemetry precision exposed by the adapter.
    pub token_telemetry: TokenTelemetryQuality,
    /// Filesystem paths the adapter requires inside the worker.
    pub required_filesystem_paths: Vec<PathBuf>,
    /// Network endpoints the adapter may require.
    pub required_network_endpoints: Vec<String>,
    /// Known flags that weaken isolation or bypass adapter controls.
    pub known_unsafe_flags: Vec<String>,
}

/// Runtime environment supplied to adapter probes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterEnvironment {
    /// Platform label observed by the trusted controller.
    pub platform: String,
    /// Workspace the adapter would run against.
    pub workspace: Workspace,
}

/// Probe result for an adapter in a concrete environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeResult {
    /// Whether the adapter is available.
    pub available: bool,
    /// Adapter or upstream harness version, when observable.
    pub detected_version: Option<String>,
    /// Non-secret diagnostics explaining availability or incompatibility.
    pub diagnostics: Vec<String>,
}

/// Request to start a new adapter session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartRequest {
    /// Session model allocated by the control plane.
    pub session: Session,
    /// Workspace exposed to the adapter.
    pub workspace: Workspace,
    /// Provider selected for model calls in this session.
    pub provider: ProviderDescriptor,
    /// Optional initial input sent after process startup.
    pub initial_input: Option<AgentInput>,
}

/// Request to resume an adapter session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeRequest {
    /// Session identifier to resume.
    pub session_id: SessionId,
    /// Adapter-specific opaque resume handle.
    pub resume_handle: String,
    /// Workspace exposed to the resumed adapter.
    pub workspace: Workspace,
}

/// Adapter-owned session handle returned after start or resume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// Control-plane session identifier.
    pub session_id: SessionId,
    /// Adapter that owns the session.
    pub adapter_id: AdapterId,
    /// Agent identity inside the adapter.
    pub agent_id: AgentId,
    /// Adapter-specific opaque runtime handle.
    pub runtime_handle: String,
    /// Adapter-specific opaque resume handle, when supported.
    pub resume_handle: Option<String>,
}

/// Input delivered from Orchestraitor to an adapter session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum AgentInput {
    /// Ordered model-style messages from the user or control plane.
    Messages {
        /// Messages to deliver.
        messages: Vec<ModelMessage>,
    },
    /// Result of a tool request previously emitted by the adapter.
    ToolResult {
        /// Adapter/provider tool-call id.
        call_id: String,
        /// Tool result payload after Orchestraitor normalization.
        result: Value,
    },
    /// Control-plane resume/correction signal.
    Control {
        /// Stable control signal name.
        signal: String,
        /// Signal-specific payload.
        payload: Value,
    },
}

/// Normalized adapter event before audit-store sequencing and hash chaining.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterEvent {
    /// Wall-clock timestamp captured at source.
    pub wall_clock_ts: String,
    /// Correlation operation identifier.
    pub correlation_id: OperationId,
    /// Optional parent operation identifier.
    pub parent_op_id: Option<OperationId>,
    /// Normalized event category.
    pub category: EventCategory,
    /// Category-specific redacted payload.
    pub payload: Value,
}

/// Adapter transport mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportMode {
    /// Native Rust SDK or in-process adapter.
    Native,
    /// Supervised subprocess protocol.
    Subprocess,
    /// Agent Client Protocol.
    Acp,
    /// Local JSON-RPC endpoint.
    JsonRpc,
    /// Authenticated local HTTP socket.
    LocalHttp,
    /// Wasmtime component plugin.
    WasmtimeComponent,
    /// Remote worker protocol.
    RemoteWorker,
}

/// Adapter authentication requirement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "type")]
pub enum AuthRequirement {
    /// No adapter-specific authentication requirement.
    None,
    /// Secret URI resolved by the trusted controller.
    SecretUri {
        /// Logical purpose for the secret.
        purpose: String,
    },
    /// Existing local CLI login state.
    LocalCliState {
        /// CLI or harness name that owns the login state.
        name: String,
    },
}

/// Context-window control level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContextControlLevel {
    /// Adapter accepts fully compiled context from Orchestraitor.
    Full,
    /// Adapter accepts bounded context but may add its own summaries.
    Bounded,
    /// Adapter controls context internally.
    AdapterOwned,
}

/// Tool-call interception level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolInterceptionLevel {
    /// No tool interception is available.
    None,
    /// Adapter reports tool requests before execution.
    ReportOnly,
    /// Adapter routes tool execution through Orchestraitor.
    Mediated,
}

/// Permission interception level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionInterceptionLevel {
    /// Permission prompts are not visible to Orchestraitor.
    None,
    /// Permission prompts are visible but not controllable.
    Observe,
    /// Permission prompts are delegated to the trusted controller.
    Delegated,
}

/// Token telemetry quality declared by an adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenTelemetryQuality {
    /// No token telemetry is available.
    None,
    /// Adapter reports approximate token counts.
    Estimated,
    /// Adapter reports provider-returned token counts.
    ProviderReported,
    /// Adapter reports exact prompt and completion accounting.
    Exact,
}
