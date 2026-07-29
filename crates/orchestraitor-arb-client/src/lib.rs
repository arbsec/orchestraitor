//! Typed adapter over Arbitraitor-owned security APIs.
//!
//! This crate translates Orchestraitor orchestration requests into calls to
//! Arbitraitor crates and returns Arbitraitor-owned types. It intentionally does
//! not implement sandboxing, policy decisions, approvals, receipts, or workspace
//! projection primitives.

pub use arbitraitor_core as core;
pub use arbitraitor_exec as exec;
pub use arbitraitor_mcp as mcp;
pub use arbitraitor_model as model;
pub use arbitraitor_plugin_api as plugin_api;
pub use arbitraitor_policy as policy;
pub use arbitraitor_receipt as receipt;
pub use arbitraitor_sandbox as sandbox;

pub use arbitraitor_mcp::{ApprovalTokenIssuer, PlanContext};
pub use arbitraitor_model::finding::Finding;
pub use arbitraitor_model::verdict::Verdict;
pub use arbitraitor_policy::{EvalContext, PolicyEngine, PolicyError};
pub use arbitraitor_receipt::{ReceiptBuilder, ReceiptTimestamps, VerdictInfo};
pub use arbitraitor_sandbox::{EffectiveControls, SandboxMode};
pub use orchestraitor_model::digest::Digest;

/// Configuration for constructing an [`ArbitraitorClient`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArbitraitorClientConfig {
    /// Preferred workspace projection backend requested by Orchestraitor UX.
    ///
    /// Backend availability and enforcement are still Arbitraitor-owned. When
    /// the pinned Arbitraitor revision has no projection probe API, the client
    /// reports a degraded materialized workspace instead of claiming stronger
    /// semantics.
    pub preferred_projection_backend: Option<WorkspaceProjectionBackend>,
}

/// Typed adapter over Arbitraitor crates.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ArbitraitorClient {
    config: ArbitraitorClientConfig,
}

impl ArbitraitorClient {
    /// Constructs a client from adapter configuration.
    #[must_use]
    pub const fn new(config: ArbitraitorClientConfig) -> Self {
        Self { config }
    }

    /// Returns the adapter configuration used to construct this client.
    #[must_use]
    pub const fn config(&self) -> &ArbitraitorClientConfig {
        &self.config
    }

    /// Probes Arbitraitor sandbox effective controls for `mode` on `platform`.
    #[must_use]
    pub fn probe_effective_controls(&self, mode: SandboxMode, platform: &str) -> EffectiveControls {
        let _ = self;
        arbitraitor_sandbox::compute_effective_controls(mode, platform)
    }

    /// Builds an Arbitraitor plan context for mediated Bash execution.
    #[must_use]
    pub fn build_plan_context_for_bash(
        &self,
        network_isolated: bool,
        policy_snapshot_digest: &Digest,
    ) -> PlanContext {
        let _ = self;
        PlanContext::for_bash(network_isolated, policy_snapshot_digest.as_str())
    }

    /// Builds a fresh Arbitraitor approval-token issuer.
    #[must_use]
    pub fn build_approval_token_issuer(&self) -> ApprovalTokenIssuer {
        let _ = self;
        ApprovalTokenIssuer::new()
    }

    /// Evaluates findings and runtime context with Arbitraitor policy.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] when Arbitraitor rejects the policy TOML during
    /// [`PolicyEngine::load`].
    pub fn evaluate_policy(
        &self,
        policy_toml: &str,
        findings: &[Finding],
        context: &EvalContext,
    ) -> Result<Verdict, PolicyError> {
        let _ = self;
        PolicyEngine::load(policy_toml).map(|engine| engine.evaluate(findings, context))
    }

    /// Builds an Arbitraitor receipt builder from caller-supplied receipt inputs.
    #[must_use]
    pub fn build_receipt(&self, input: ReceiptBuilderInput) -> ReceiptBuilder {
        let _ = self;
        ReceiptBuilder::new(
            input.arbitraitor_version,
            input.artifact_sha256,
            input.artifact_size,
            input.verdict,
            input.timestamps,
        )
    }

    /// Selects the workspace projection backend through Arbitraitor when available.
    ///
    /// The pinned Arbitraitor revision does not expose a projection-specific
    /// capability probe. Per spec §9.4.2 and §16.2, this returns an explicitly
    /// degraded materialized-workspace result and does not claim live mediation.
    #[must_use]
    pub const fn select_workspace_projection_backend(&self) -> WorkspaceProjectionResult {
        let _ = self;
        WorkspaceProjectionResult::degraded_materialized()
    }
}

/// Inputs required by [`ArbitraitorClient::build_receipt`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceiptBuilderInput {
    /// Arbitraitor version string recorded in the receipt.
    pub arbitraitor_version: String,
    /// SHA-256 digest of the artifact being receipted.
    pub artifact_sha256: String,
    /// Artifact size in bytes.
    pub artifact_size: u64,
    /// Arbitraitor policy verdict information for the receipt.
    pub verdict: VerdictInfo,
    /// Receipt timestamps supplied by Arbitraitor-facing caller code.
    pub timestamps: ReceiptTimestamps,
}

/// Workspace projection backend names reported by the adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceProjectionBackend {
    /// Arbitraitor projected VFS backend.
    ProjectedVfs,
    /// Arbitraitor native overlay backend.
    NativeOverlay,
    /// Materialized disposable workspace fallback.
    Materialized,
}

/// Workspace projection selection report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceProjectionResult {
    /// Backend selected for this session.
    pub selected_backend: WorkspaceProjectionBackend,
    /// Semantics unavailable with the selected backend.
    pub unsupported_semantics: &'static [&'static str],
    /// Human-readable enforcement level for presentation.
    pub enforcement_level: &'static str,
}

impl WorkspaceProjectionResult {
    /// Returns the degraded materialized-workspace projection report.
    #[must_use]
    pub const fn degraded_materialized() -> Self {
        Self {
            selected_backend: WorkspaceProjectionBackend::Materialized,
            unsupported_semantics: &[
                "per-operation-mediation",
                "live-attribution",
                "per-principal-scanges",
            ],
            enforcement_level: "transactional-only",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::{Any, TypeId};

    #[test]
    fn probe_effective_controls_returns_arbitraitor_type() {
        // Given: a typed adapter over Arbitraitor sandbox probes.
        let client = ArbitraitorClient::default();

        // When: probing effective controls.
        let controls = client.probe_effective_controls(SandboxMode::Observe, "linux");

        // Then: the returned value is the Arbitraitor sandbox type, not an
        // Orchestraitor-defined surrogate.
        assert_eq!(
            TypeId::of::<arbitraitor_sandbox::EffectiveControls>(),
            (&controls as &dyn Any).type_id()
        );
    }

    #[test]
    fn plan_context_for_bash_returns_arbitraitor_type() {
        // Given: a policy snapshot digest from the Orchestraitor model crate.
        let client = ArbitraitorClient::default();
        let digest = Digest::new("a".repeat(64));

        // When: building an approval plan context for Bash.
        let context = client.build_plan_context_for_bash(true, &digest);

        // Then: the returned value is Arbitraitor MCP's PlanContext.
        assert_eq!(
            TypeId::of::<arbitraitor_mcp::PlanContext>(),
            (&context as &dyn Any).type_id()
        );
    }

    #[test]
    fn public_api_has_no_local_security_decision_helpers() {
        // Given: this crate's public API source.
        let source = include_str!("lib.rs");

        // When/Then: no Orchestraitor-local allow/deny/safety helper exists.
        for forbidden in [
            concat!("pub fn is_", "safe_to_run"),
            concat!("pub fn is_", "allowed"),
            concat!("pub fn ", "allow_"),
            concat!("pub fn ", "deny_"),
            concat!("pub enum ", "Verdict"),
            concat!("pub struct ", "Verdict"),
        ] {
            assert!(
                !source.contains(forbidden),
                "forbidden local security API found: {forbidden}"
            );
        }
    }

    #[test]
    fn projection_selection_reports_degraded_materialized_backend() {
        // Given: the pinned Arbitraitor revision lacks projection probing.
        let client = ArbitraitorClient::default();

        // When: selecting a workspace projection backend.
        let result = client.select_workspace_projection_backend();

        // Then: the adapter reports only materialized transactional semantics.
        assert_eq!(
            result.selected_backend,
            WorkspaceProjectionBackend::Materialized
        );
        assert_eq!(result.enforcement_level, "transactional-only");
        assert!(
            result
                .unsupported_semantics
                .contains(&"per-operation-mediation")
        );
    }
}
