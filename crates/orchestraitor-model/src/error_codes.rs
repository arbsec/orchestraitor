//! Structured error code registry (spec §9.34).
//!
//! Error codes follow the pattern `ORC-<COMPONENT>-<NNN>` where `<COMPONENT>`
//! is the crate short-name and `<NNN>` is a zero-padded number.

use strum::{Display, EnumString};

/// All Orchestraitor error code components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, Display)]
pub enum ErrorComponent {
    /// Workspace controller / lifecycle errors.
    #[strum(serialize = "WORKSPACE")]
    Workspace,
    /// Provider / harness adapter errors.
    #[strum(serialize = "PROVIDER")]
    Provider,
    /// MCP server / tool gateway errors.
    #[strum(serialize = "MCP")]
    Mcp,
    /// Local daemon / IPC errors.
    #[strum(serialize = "DAEMON")]
    Daemon,
    /// Configuration parsing and validation errors.
    #[strum(serialize = "CONFIG")]
    Config,
    /// Artifact delivery / publish errors.
    #[strum(serialize = "DELIVERY")]
    Delivery,
    /// Sandbox / containment errors (proxied through Arbitraitor).
    #[strum(serialize = "SANDBOX")]
    Sandbox,
    /// Context compiler / selection errors.
    #[strum(serialize = "CONTEXT")]
    Context,
    /// Event store / event log errors.
    #[strum(serialize = "EVENTS")]
    Events,
}

impl ErrorComponent {
    /// Formats a structured error code of the form `ORC-<COMPONENT>-<NNN>`.
    #[must_use]
    pub fn code(&self, number: u32) -> String {
        format!("ORC-{self}-{number:03}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_format() {
        assert_eq!(ErrorComponent::Workspace.code(1), "ORC-WORKSPACE-001");
        assert_eq!(ErrorComponent::Provider.code(42), "ORC-PROVIDER-042");
    }
}
