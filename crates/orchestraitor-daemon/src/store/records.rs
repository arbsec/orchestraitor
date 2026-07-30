//! Typed record inputs for daemon-store tables.

use orchestraitor_events::EventCategory;
use orchestraitor_model::{Digest, ModelId, OperationId, ProviderId, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Cost-ledger row persisted by the daemon metadata store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostLedgerRecord {
    /// Provider or controller request id.
    pub request_id: String,
    /// Session attributed to the provider call.
    pub session_id: SessionId,
    /// Provider selected by routing.
    pub provider_id: ProviderId,
    /// Model selected by routing.
    pub model_id: ModelId,
    /// Versioned cost payload owned by the cost-ledger domain.
    pub payload: Value,
}

/// Durable backlog state row for recovery after daemon restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacklogStateRecord {
    /// Stable backlog item id.
    pub item_id: String,
    /// Session that owns the item, when already admitted.
    pub session_id: Option<SessionId>,
    /// State-machine state label.
    pub state: String,
    /// Versioned backlog payload.
    pub payload: Value,
    /// Stable timestamp string captured by the caller.
    pub updated_at: String,
}

/// Arbitraitor receipt row retained by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiptRecord {
    /// Stable receipt id.
    pub receipt_id: String,
    /// Session that emitted the receipt, if session-scoped.
    pub session_id: Option<SessionId>,
    /// Receipt kind label from Arbitraitor or the integration boundary.
    pub receipt_kind: String,
    /// Optional digest of the canonical receipt bytes or referenced artifact.
    pub digest: Option<Digest>,
    /// Receipt payload as produced by Arbitraitor-owned code.
    pub payload: Value,
}

/// Parent-child delegation edge for multi-agent reconstruction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegationRecord {
    /// Stable delegation-chain id.
    pub chain_id: String,
    /// Session that owns the delegation.
    pub session_id: Option<SessionId>,
    /// Parent operation id.
    pub parent_op_id: OperationId,
    /// Child operation id.
    pub child_op_id: OperationId,
    /// Versioned delegation payload.
    pub payload: Value,
}

pub(crate) const fn category_text(category: EventCategory) -> &'static str {
    match category {
        EventCategory::SessionLifecycle => "session_lifecycle",
        EventCategory::AdapterLifecycle => "adapter_lifecycle",
        EventCategory::ModelRequest => "model_request",
        EventCategory::ModelResponseMetadata => "model_response_metadata",
        EventCategory::ContextSelection => "context_selection",
        EventCategory::ToolRequest => "tool_request",
        EventCategory::ActionPlan => "action_plan",
        EventCategory::PolicyDecision => "policy_decision",
        EventCategory::Approval => "approval",
        EventCategory::ProcessExecution => "process_execution",
        EventCategory::NetworkRequest => "network_request",
        EventCategory::SecretUse => "secret_use",
        EventCategory::FileObservation => "file_observation",
        EventCategory::GitOperation => "git_operation",
        EventCategory::OutputPromotion => "output_promotion",
        EventCategory::SandboxCapability => "sandbox_capability",
        EventCategory::ResourceUsage => "resource_usage",
        EventCategory::Error => "error",
        EventCategory::SecurityFinding => "security_finding",
    }
}
