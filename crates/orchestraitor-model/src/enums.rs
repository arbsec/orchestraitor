//! Domain enums used across the workspace.
//!
//! These map directly to the spec's state machines and mode definitions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use strum::{Display, EnumString};

/// Security mode (spec §14).
///
/// Controls the workspace isolation and enforcement level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum SecurityMode {
    /// Disposable workspace, no host Git metadata, strictest sandbox (spec §14.1).
    Strict,
    /// Snapshot workspace, typed Git RPC, standard sandbox (spec §14.2).
    Standard,
    /// Brokered worktree, broader access inside sandbox (spec §14.3).
    Compatible,
    /// Agent runs in user's current checkout — explicit override only (spec §9.4 mode 4).
    Host,
}

/// Workspace mode (spec §9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum WorkspaceMode {
    /// Controller exports a commit tree; worker has no `.git` (spec §9.4 mode 1).
    Snapshot,
    /// Controller creates a worktree; worker sees files but not shared metadata (spec §9.4 mode 2).
    BrokeredWorktree,
    /// Worker can access Git metadata — explicit weakened-policy selection (spec §9.4 mode 3).
    FullWorktree,
    /// Agent runs in the user's current checkout (spec §9.4 mode 4).
    Host,
}

/// Workspace trust state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum WorkspaceTrustState {
    /// Files exposed to worker are untrusted; promotion required (spec §9.14).
    UntrustedExposed,
    /// Files promoted through the output quarantine pipeline.
    Promoted,
    /// Worker has host-level access — explicit override (spec §9.4 mode 4).
    HostTrusted,
}

/// Git access level for a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum GitAccess {
    /// No `.git` directory exposed (snapshot mode).
    NoGitAccess,
    /// Typed RPC access to Git operations (brokered worktree mode).
    TypedRpc,
    /// Full Git metadata access (full worktree / host mode).
    FullGitAccess,
}

/// Task and session lifecycle state (spec §9.24.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum SessionState {
    /// Task admitted to the controller; not yet scheduled.
    Queued,
    /// Worker picked up the task and is processing.
    Running,
    /// Worker paused awaiting user input (typed, not security approval).
    InputRequired,
    /// Worker paused awaiting Arbitraitor-originated approval (§9.9).
    ApprovalRequired,
    /// Worker paused awaiting user-side credential resolution.
    AuthenticationRequired,
    /// User/admin explicit pause; resumable.
    Paused,
    /// Task finished successfully; receipts written.
    Completed,
    /// Task terminated abnormally; partial results may exist.
    Failed,
    /// Cancellation propagated; resources released.
    Cancelled,
    /// Arbitraitor refused the plan; never ran; receipt emitted.
    Rejected,
    /// Process exit detected with task state still in `running`; recovery pending.
    Orphaned,
}

/// Timestamp type used across the workspace.
pub type Timestamp = DateTime<Utc>;

/// Shell access mode (spec §9.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ShellMode {
    /// Shell unavailable except curated task adapters.
    Strict,
    /// Shell sandboxed, statically planned, observed, and reconciled.
    Standard,
    /// Broad shell access inside the outer sandbox.
    Compatible,
    /// Harness-native behavior with explicit loss-of-containment warning.
    Host,
}

/// Integration mode (spec §10.1, MVP-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum IntegrationMode {
    /// Direct provider mode — control plane owns the agent loop.
    Native,
    /// Wrapped CLI mode — existing CLI in isolated session.
    Wrapped,
    /// MCP tool gateway mode.
    McpGateway,
    /// Provider-compatible proxy (spec §10.1 Mode D).
    ProviderProxy,
    /// Observation mode — non-protective (spec MVP-2).
    Observe,
}

/// Data sensitivity classification (spec §9.28.1).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    Display,
    EnumString,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum DataSensitivity {
    /// Any provider.
    Public,
    /// Default — per-policy providers.
    Internal,
    /// Approved providers only, region-constrained.
    Confidential,
    /// Local-only providers only, or redaction required.
    Restricted,
}

/// Context item provenance origin (spec §9.15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum ContextOrigin {
    /// Direct user instruction typed into the UI.
    UserInstruction,
    /// Trusted configuration source (policy, settings, manifests).
    TrustedConfig,
    /// File content from the repository under work.
    RepositoryContent,
    /// Response returned by an MCP server.
    McpResponse,
    /// Output produced by an Orchestraitor / Arbitraitor tool.
    ToolOutput,
    /// Output produced by the model itself (often untrusted, spec §9.15.1).
    ModelOutput,
    /// Content fetched from the public web.
    WebContent,
    /// Summary produced by the context compiler.
    GeneratedSummary,
    /// File attached to the session outside the repository root.
    SessionAttachment,
    /// Content originating from external logs or telemetry.
    ExternalLog,
}

/// Trust class for context items (spec §9.15.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum TrustClass {
    /// Origin is trusted; content may be acted on without re-checks.
    Trusted,
    /// Origin is untrusted; content must be treated as data, never as instructions.
    Untrusted,
    /// Production verified by Arbitraitor (treated as trusted for output use).
    ArbitraitorVerified,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn session_state_round_trips() {
        let state = SessionState::ApprovalRequired;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"approval-required\"");
        let back: SessionState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }

    #[test]
    fn security_mode_round_trips() {
        let mode = SecurityMode::Strict;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"strict\"");
        let back: SecurityMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, back);
    }

    #[test]
    fn workspace_mode_round_trips() {
        let mode = WorkspaceMode::BrokeredWorktree;
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, "\"brokered_worktree\"");
        let back: WorkspaceMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, back);
    }

    #[test]
    fn data_sensitivity_ord() {
        assert!(DataSensitivity::Restricted > DataSensitivity::Public);
    }
}
