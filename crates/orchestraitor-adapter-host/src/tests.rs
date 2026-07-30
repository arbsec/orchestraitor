use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use orchestraitor_adapter_api::{
    AdapterEnvironment, AdapterManifest, AdapterResult, AgentAdapter, AgentInput, AgentSession,
    AuthRequirement, ContextControlLevel, EventStream, PermissionInterceptionLevel, ProbeResult,
    ResumeRequest, StartRequest, TokenTelemetryQuality, ToolInterceptionLevel, TransportMode,
};
use orchestraitor_arbitraitor_client::{EvalContext, Finding, PolicyError, Verdict};
use orchestraitor_events::{EventCategory, InMemoryAuditStore};
use orchestraitor_provider_api::{
    CapabilitySupport, MessageRole, ModelMessage, ProviderCapabilities, ProviderDescriptor,
    ProviderProtocol,
};
use orchestraitor_workspace::{
    AdapterId, AgentId, Digest, GitAccess, ObjectId, OperationId, ProviderId, RepositoryId,
    SecurityMode, Session, SessionId, SessionState, Workspace, WorkspaceId, WorkspaceMode,
    WorkspaceTrustState,
};

use crate::{
    AdapterHostError, AdapterSupervisor, ArbitraitorEvaluationRequest, ArbitraitorPolicyEvaluator,
    HostResult, SupervisedStartRequest,
};

#[derive(Clone)]
struct StubEvaluator {
    verdict: Verdict,
}

impl ArbitraitorPolicyEvaluator for StubEvaluator {
    fn evaluate_adapter_operation(
        &self,
        _policy_toml: &str,
        _findings: &[Finding],
        _context: &EvalContext,
    ) -> Result<Verdict, PolicyError> {
        Ok(self.verdict)
    }
}

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
            detected_version: None,
            diagnostics: Vec::new(),
        })
    }

    async fn start(&self, request: StartRequest) -> AdapterResult<AgentSession> {
        Ok(AgentSession {
            session_id: request.session.id,
            adapter_id: self.manifest.id.clone(),
            agent_id: AgentId::from_string("agent_stub".to_string()),
            runtime_handle: "runtime".to_string(),
            resume_handle: None,
        })
    }

    async fn resume(&self, request: ResumeRequest) -> AdapterResult<AgentSession> {
        Ok(AgentSession {
            session_id: request.session_id,
            adapter_id: self.manifest.id.clone(),
            agent_id: AgentId::from_string("agent_stub".to_string()),
            runtime_handle: request.resume_handle,
            resume_handle: None,
        })
    }

    async fn send(&self, _session: &AgentSession, _input: AgentInput) -> AdapterResult<()> {
        Ok(())
    }

    async fn cancel(&self, _session: &AgentSession) -> AdapterResult<()> {
        Ok(())
    }

    async fn events(&self, _session: &AgentSession) -> AdapterResult<EventStream> {
        let events = vec![Ok(orchestraitor_adapter_api::AdapterEvent {
            wall_clock_ts: "2026-07-30T00:00:00Z".to_string(),
            correlation_id: OperationId::from_string("op_adapter".to_string()),
            parent_op_id: None,
            category: EventCategory::AdapterLifecycle,
            payload: serde_json::json!({ "adapter_state": "started" }),
        })];
        Ok(Box::new(events.into_iter()))
    }

    async fn shutdown(&self, _session: AgentSession) -> AdapterResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn start_is_blocked_when_arbitraitor_returns_block() -> HostResult<()> {
    // Given: a registered stub adapter and an Arbitraitor Block verdict.
    let mut supervisor = supervisor_with(Verdict::Block);
    supervisor.register_adapter(Arc::new(StubAdapter {
        manifest: stub_manifest(),
    }));

    // When: starting the adapter through the supervisor.
    let result = supervisor.start(supervised_start_request()).await;

    // Then: startup fails before the adapter can run.
    assert!(matches!(
        result,
        Err(AdapterHostError::ArbitraitorVerdict {
            verdict: Verdict::Block
        })
    ));
    Ok(())
}

#[tokio::test]
async fn stub_adapter_event_stream_is_multiplexed_into_audit_store() -> HostResult<()> {
    // Given: a registered stub adapter and an Arbitraitor Pass verdict.
    let mut supervisor = supervisor_with(Verdict::Pass);
    supervisor.register_adapter(Arc::new(StubAdapter {
        manifest: stub_manifest(),
    }));
    let session = supervisor.start(supervised_start_request()).await?;

    // When: draining the adapter's mock event stream.
    let records = supervisor.multiplex_events(&session).await?;

    // Then: the unified audit store contains a sequenced adapter event.
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].envelope.monotonic_seq, 1);
    assert_eq!(
        records[0].envelope.category,
        EventCategory::AdapterLifecycle
    );
    assert_eq!(supervisor.audit_store().records().len(), 1);
    Ok(())
}

#[tokio::test]
async fn missing_adapter_fails_closed_without_fallback() -> HostResult<()> {
    // Given: no registered native adapter implementations in the MVP host.
    let supervisor = supervisor_with(Verdict::Pass);

    // When: starting a missing adapter.
    let result = supervisor.start(supervised_start_request()).await;

    // Then: there is no silent fallback adapter.
    assert!(matches!(
        result,
        Err(AdapterHostError::AdapterMissing { adapter_id }) if adapter_id.as_str() == "adapter_stub"
    ));
    Ok(())
}

fn supervisor_with(verdict: Verdict) -> AdapterSupervisor<InMemoryAuditStore, StubEvaluator> {
    AdapterSupervisor::new(StubEvaluator { verdict }, InMemoryAuditStore::default())
}

fn supervised_start_request() -> SupervisedStartRequest {
    SupervisedStartRequest {
        adapter_id: AdapterId::from_string("adapter_stub".to_string()),
        request: stub_start_request(),
        enforcement: ArbitraitorEvaluationRequest {
            policy_toml: String::new(),
            findings: Vec::new(),
            context: EvalContext::new(false),
        },
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
        provider: ProviderDescriptor {
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
        initial_input: Some(AgentInput::Messages {
            messages: vec![ModelMessage {
                role: MessageRole::User,
                content: "hello".to_string(),
            }],
        }),
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
