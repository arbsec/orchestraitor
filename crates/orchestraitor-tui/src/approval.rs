//! Approval rendering types for the TUI (spec §9.9).
//!
//! Approval plans, binding, validation, and authorization are owned by
//! Arbitraitor. Orchestraitor renders trusted client views from
//! Arbitraitor-provided structured data. These types are **view models** —
//! they carry only what the TUI needs to display. They never construct,
//! validate, or hold approval tokens.
//!
//! Per spec §9.9, the UI must show:
//! - operation
//! - executable identity
//! - arguments
//! - paths
//! - network destinations
//! - secret use **without** secret value
//! - sandbox controls
//! - expected outputs
//! - static findings
//! - policy rule
//! - scope and expiry
//! - whether the action affects host-trusted state
//!
//! The agent cannot approve its own request. Agent prose is shown separately
//! and marked untrusted.

use std::collections::BTreeSet;

/// The kind of action being requested (spec §9.9 approval types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalAction {
    /// One action.
    OneAction,
    /// Repeated identical action.
    RepeatedAction,
    /// Capability for current turn.
    TurnCapability,
    /// Capability for the session.
    SessionCapability,
    /// Capability for repository policy.
    RepositoryPolicyCapability,
    /// Time-limited capability.
    TimeLimitedCapability,
    /// Destination-specific network capability.
    NetworkCapability,
    /// Read-only or write-scoped Git capability.
    GitCapability,
}

impl ApprovalAction {
    /// Returns the human-readable label for this action kind.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::OneAction => "one action",
            Self::RepeatedAction => "repeated action",
            Self::TurnCapability => "turn capability",
            Self::SessionCapability => "session capability",
            Self::RepositoryPolicyCapability => "repository policy capability",
            Self::TimeLimitedCapability => "time-limited capability",
            Self::NetworkCapability => "network capability",
            Self::GitCapability => "git capability",
        }
    }
}

/// A network destination in an approval request, without credentials.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetworkDestination {
    /// Hostname or IP address.
    pub host: String,
    /// Port number, if specified.
    pub port: Option<u16>,
    /// URI scheme (`https`, `tcp`, etc.).
    pub scheme: String,
    /// Purpose label for this destination.
    pub purpose: String,
}

/// Indicates that a secret is used, without revealing its value (spec §9.9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretUseIndicator {
    /// Human-readable secret name or URI (never the value).
    pub name: String,
    /// Secret URI scheme, e.g. `secret://env/API_KEY`.
    pub uri: String,
    /// What the secret is used for.
    pub purpose: String,
}

/// Sandbox controls in effect for the approval (spec §9.6, §9.9).
///
/// These are **display labels** sourced from Arbitraitor's effective-controls
/// report. Orchestraitor never computes or asserts these independently.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SandboxControl {
    /// Control name (e.g. `landlock`, `seccomp`, `network_isolated`).
    pub name: String,
    /// Whether the control is reported as effective.
    pub effective: bool,
}

/// Scope and expiry for a capability or approval (spec §9.9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApprovalScope {
    /// Human-readable scope description.
    pub description: String,
    /// Expiry timestamp in RFC 3339 format, or `None` for session-scoped.
    pub expiry: Option<String>,
}

/// Complete approval data rendered in the TUI (spec §9.9).
///
/// All fields are display-only. The TUI never holds token material.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalData {
    /// The operation being requested.
    pub operation: String,
    /// The kind of approval action.
    pub action: ApprovalAction,
    /// Executable identity (e.g. binary path or agent id).
    pub executable: String,
    /// Command-line arguments.
    pub arguments: Vec<String>,
    /// Filesystem paths the operation touches.
    pub paths: Vec<String>,
    /// Network destinations, without credentials.
    pub network_destinations: Vec<NetworkDestination>,
    /// Secrets used, without values.
    pub secret_use: Vec<SecretUseIndicator>,
    /// Sandbox controls in effect.
    pub sandbox_controls: Vec<SandboxControl>,
    /// Expected outputs from the operation.
    pub expected_outputs: Vec<String>,
    /// Static analysis findings from Arbitraitor.
    pub static_findings: Vec<String>,
    /// Policy rule that governs this request.
    pub policy_rule: String,
    /// Scope and expiry of the approval.
    pub scope: ApprovalScope,
    /// Whether the action affects host-trusted state.
    pub affects_host_trusted_state: bool,
    /// Agent-provided prose, shown separately and marked untrusted (spec §9.9).
    pub agent_prose: Option<String>,
}

impl ApprovalData {
    /// Returns the set of unique path roots for a compact summary.
    #[must_use]
    pub fn path_roots(&self) -> BTreeSet<String> {
        self.paths
            .iter()
            .map(|p| {
                p.split(std::path::MAIN_SEPARATOR)
                    .next()
                    .unwrap_or(p)
                    .to_string()
            })
            .collect()
    }

    /// Returns whether any sandbox control is reported as effective.
    #[must_use]
    pub fn has_effective_sandbox(&self) -> bool {
        self.sandbox_controls.iter().any(|c| c.effective)
    }
}
