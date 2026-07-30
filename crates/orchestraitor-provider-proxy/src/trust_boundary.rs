//! Trust-boundary reporting for provider-proxy mode.

use serde::{Deserialize, Serialize};

/// Machine-readable Mode D guarantee status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustBoundaryStatus {
    /// Provider traffic is proxied, but external harness tool execution is outside containment.
    ProviderProxyOnly,
}

/// Displayable trust-boundary report required by spec §10.1 Mode D.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustBoundaryReport {
    /// Integration mode represented by this proxy.
    pub integration_mode: &'static str,
    /// Current guarantee status.
    pub status: TrustBoundaryStatus,
    /// Actions covered by this crate.
    pub protected_actions: &'static [&'static str],
    /// Actions explicitly outside this proxy's trust boundary.
    pub outside_trust_boundary: &'static [&'static str],
    /// Human-facing warning.
    pub display_warning: &'static str,
}

impl TrustBoundaryReport {
    /// Returns the fixed report for provider-compatible proxy mode.
    #[must_use]
    pub const fn provider_proxy_only() -> Self {
        Self {
            integration_mode: "provider-proxy",
            status: TrustBoundaryStatus::ProviderProxyOnly,
            protected_actions: &[
                "provider-routing",
                "local-authentication",
                "upstream-credential-isolation",
                "cost-attribution",
                "provider-telemetry",
            ],
            outside_trust_boundary: &[
                "external-harness-filesystem-tools",
                "external-harness-shell-commands",
                "external-harness-mcp-tools-not-routed-through-orchestraitor",
            ],
            display_warning: "Provider proxy mode does not contain filesystem or shell actions performed independently by an external harness. Use orc wrap or native mode for tool containment.",
        }
    }
}
