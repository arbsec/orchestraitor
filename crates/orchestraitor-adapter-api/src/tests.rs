use std::path::PathBuf;

use async_trait::async_trait;
use orchestraitor_events::EventCategory;
use orchestraitor_model::{
    AdapterId, AgentId, Digest, GitAccess, ObjectId, OperationId, ProviderId, RepositoryId,
    SecurityMode, Session, SessionId, SessionState, Workspace, WorkspaceId, WorkspaceMode,
    WorkspaceTrustState,
};
use orchestraitor_provider_api::{CapabilitySupport, ProviderCapabilities, ProviderProtocol};

use crate::{
    AdapterEnvironment, AdapterError, AdapterManifest, AdapterResult, AgentAdapter, AgentInput,
    AgentSession, AuthRequirement, ContextControlLevel, EventStream, PermissionInterceptionLevel,
    ProbeResult, ResumeRequest, StartRequest, TokenTelemetryQuality, ToolInterceptionLevel,
    TransportMode,
};

struct StubAdapter {
    manifest: AdapterManifest,
}

#[async_trait]
impl AgentAdapter for StubAdapter {
    fn manifest(&self) -> &AdapterManifest {
        &self.manifest
    }

    async fn probe(&self, _environment: &AdapterEnvironment) -> AdapterResult<ProbeResult> {
        Ok(ProbeResult {
            available: true,
            detected_version: Some("stub-1".to_string()),
            diagnostics: Vec::new(),
        })
    }

    async fn start(&self, request: StartRequest) -> AdapterResult<AgentSession> {
        Ok(AgentSession {
            session_id: request.session.id,
            adapter_id: self.manifest.id.clone(),
            agent_id: AgentId::from_string("agent_stub".to_string()),
            runtime_handle: "runtime".to_string(),
            resume_handle: Some("resume".to_string()),
        })
    }

    async fn resume(&self, request: ResumeRequest) -> AdapterResult<AgentSession> {
        Ok(AgentSession {
            session_id: request.session_id,
            adapter_id: self.manifest.id.clone(),
            agent_id: AgentId::from_string("agent_stub".to_string()),
            runtime_handle: request.resume_handle,
            resume_handle: Some("resume".to_string()),
        })
    }

    async fn send(&self, _session: &AgentSession, _input: AgentInput) -> AdapterResult<()> {
        Ok(())
    }

    async fn cancel(&self, _session: &AgentSession) -> AdapterResult<()> {
        Ok(())
    }

    async fn events(&self, _session: &AgentSession) -> AdapterResult<EventStream> {
        let event = crate::AdapterEvent {
            wall_clock_ts: "2026-07-30T00:00:00Z".to_string(),
            correlation_id: OperationId::from_string("op_stub".to_string()),
            parent_op_id: None,
            category: EventCategory::AdapterLifecycle,
            payload: serde_json::json!({ "state": "started" }),
        };
        Ok(Box::new(vec![Ok(event)].into_iter()))
    }

    async fn shutdown(&self, _session: AgentSession) -> AdapterResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn agent_adapter_trait_object_accepts_stub_impl() -> AdapterResult<()> {
    // Given: a stub adapter behind the spec §10.6 trait object.
    let adapter = StubAdapter {
        manifest: stub_manifest(),
    };
    let adapter_object: &dyn AgentAdapter = &adapter;

    // When: probing and starting through the trait object.
    let probe = adapter_object.probe(&stub_environment()).await?;
    let session = adapter_object.start(stub_start_request()).await?;

    // Then: the adapter reports availability and returns an owned session.
    assert!(probe.available);
    assert_eq!(session.runtime_handle, "runtime");
    Ok(())
}

#[test]
fn adapter_manifest_round_trips_without_security_authority_types() -> AdapterResult<()> {
    // Given: a declared adapter manifest.
    let manifest = stub_manifest();

    // When: serializing through the public API boundary.
    let json = serde_json::to_string(&manifest).map_err(|error| AdapterError::OperationFailed {
        operation: "serialize_manifest",
        message: error.to_string(),
    })?;
    let round_trip: AdapterManifest =
        serde_json::from_str(&json).map_err(|error| AdapterError::OperationFailed {
            operation: "deserialize_manifest",
            message: error.to_string(),
        })?;

    // Then: no information is lost.
    assert_eq!(manifest, round_trip);
    Ok(())
}

fn stub_manifest() -> AdapterManifest {
    AdapterManifest {
        id: AdapterId::from_string("adapter_stub".to_string()),
        display_name: "Stub".to_string(),
        version_compatibility: "0.0.0".to_string(),
        supported_platforms: vec!["linux".to_string()],
        transport_mode: TransportMode::Native,
        authentication: vec![AuthRequirement::None],
        context_control: ContextControlLevel::Full,
        tool_interception: ToolInterceptionLevel::Mediated,
        permission_interception: PermissionInterceptionLevel::Delegated,
        supports_session_resume: true,
        token_telemetry: TokenTelemetryQuality::ProviderReported,
        required_filesystem_paths: Vec::new(),
        required_network_endpoints: Vec::new(),
        known_unsafe_flags: Vec::new(),
    }
}

fn stub_environment() -> AdapterEnvironment {
    AdapterEnvironment {
        platform: "linux".to_string(),
        workspace: stub_workspace(),
    }
}

fn stub_start_request() -> StartRequest {
    StartRequest {
        session: Session {
            id: SessionId::from_string("sess_stub".to_string()),
            repository_id: RepositoryId::from_string("repo_stub".to_string()),
            adapter_id: AdapterId::from_string("adapter_stub".to_string()),
            workspace_id: WorkspaceId::from_string("ws_stub".to_string()),
            security_mode: SecurityMode::Standard,
            policy_digest: Digest::new("a".repeat(64)),
            state: SessionState::Queued,
            created_at: chrono::Utc::now(),
        },
        workspace: stub_workspace(),
        provider: orchestraitor_provider_api::ProviderDescriptor {
            id: ProviderId::from_string("provider_stub".to_string()),
            display_name: "Stub Provider".to_string(),
            protocol: ProviderProtocol::CustomPlugin,
            capabilities: ProviderCapabilities {
                reasoning_effort: CapabilitySupport::Unsupported,
                prompt_caching: CapabilitySupport::Unsupported,
                tool_choice: CapabilitySupport::Unsupported,
                structured_outputs: CapabilitySupport::Unsupported,
                multimodal_inputs: CapabilitySupport::Unsupported,
                provider_hosted_tools: CapabilitySupport::Unsupported,
                server_side_conversation_state: CapabilitySupport::Unsupported,
                token_counting: CapabilitySupport::Unsupported,
            },
        },
        initial_input: None,
    }
}

fn stub_workspace() -> Workspace {
    Workspace {
        id: WorkspaceId::from_string("ws_stub".to_string()),
        mode: WorkspaceMode::Snapshot,
        base_commit: ObjectId("abc123".to_string()),
        path: PathBuf::from("/tmp/ws"),
        trust_state: WorkspaceTrustState::UntrustedExposed,
        git_access: GitAccess::NoGitAccess,
    }
}
