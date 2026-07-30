//! Startup capability probing and Arbitraitor version negotiation.
//!
//! At daemon startup, [`probe_capabilities`] queries the Arbitraitor adapter for
//! the effective sandbox controls on the current platform and verifies:
//!
//! - Arbitraitor API compatibility (spec §16.7);
//! - required capability availability (spec §9.6, §16.7);
//! - effective controls on the current platform (spec §9.6);
//! - receipt schema compatibility (spec §16.7);
//! - whether any requested feature is operating in degraded mode (spec §16.7).
//!
//! When any required control is [`ControlStatus::Unavailable`], the report marks
//! `protected_services_allowed` as `false` so the daemon fails closed per spec
//! §6.7: it blocks the protected operation by default, identifies the missing
//! Arbitraitor capability, and records the degradation for the health RPC.
//!
//! A version match alone is not evidence that a control is effective (spec
//! §16.7). The runtime [`CapabilityReport`] — driven by
//! [`ArbitraitorClient::probe_effective_controls`] — is authoritative.

use orchestraitor_arbitraitor_client::ArbitraitorClient;
use orchestraitor_arbitraitor_client::sandbox::{ControlState, EffectiveControls, SandboxMode};
use serde::Serialize;

/// Minimum supported Arbitraitor API version (spec §16.7).
///
/// Orchestraitor declares the minimum Arbitraitor API surface it requires. The
/// daemon is compiled against a pinned Arbitraitor revision (workspace
/// `Cargo.toml`), so API compatibility is verified at compile time. A version
/// match alone is not evidence that a control is effective; the runtime
/// [`CapabilityReport`] is authoritative.
pub const MIN_ARBITRAITOR_API_VERSION: &str = "0.1.0";

/// Receipt schema version Orchestraitor requires from Arbitraitor (spec §16.7).
///
/// The linked Arbitraitor MUST provide at least this schema version. The check
/// is meaningful: if the pinned Arbitraitor revision is downgraded to one with
/// an older receipt schema, this check fails at runtime.
pub const REQUIRED_RECEIPT_SCHEMA_VERSION: u32 = 2;

/// The sandbox mode probed at daemon startup (spec §9.6).
///
/// `Restricted` is the minimum enforcement envelope for protected operations.
/// The probe asks Arbitraitor which controls would actually be in effect for
/// this mode on the current platform.
pub const PROBED_SANDBOX_MODE: SandboxMode = SandboxMode::Restricted;

/// Serializable status of a single sandbox control (spec §9.6, §27.7).
///
/// Mirrors [`arbitraitor_sandbox::ControlState`] in a serializable form so the
/// health RPC can report it without leaking Arbitraitor types across the
/// JSON-RPC boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlStatus {
    /// Control is fully active on this platform.
    Available,
    /// Control is partially active; assurance level is reduced.
    Degraded,
    /// Control is not active on this platform or configuration.
    Unavailable,
}

impl From<ControlState> for ControlStatus {
    fn from(state: ControlState) -> Self {
        match state {
            ControlState::Available => Self::Available,
            ControlState::Degraded => Self::Degraded,
            ControlState::Unavailable => Self::Unavailable,
        }
    }
}

/// A required sandbox control and its probed status (spec §9.6, §16.7).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RequiredCapability {
    /// Stable capability identifier name (spec §9.32.1).
    pub identifier: &'static str,
    /// Probed status of this control on the current platform.
    pub status: ControlStatus,
}

/// Result of probing Arbitraitor capabilities at daemon startup (spec §16.7).
///
/// Captures the full version-negotiation and effective-controls picture so the
/// daemon can make a fail-closed decision (spec §6.7) and the health RPC can
/// report the active security posture.
#[expect(
    clippy::struct_excessive_bools,
    reason = "spec §16.7 mandates independent version-negotiation fields (API compat, schema compat, degraded mode, fail-closed decision); collapsing into an enum would lose independent diagnostic information"
)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityReport {
    /// Whether protected services may start (`false` = fail-closed per §6.7).
    ///
    /// `false` when any required control is [`ControlStatus::Unavailable`].
    pub protected_services_allowed: bool,
    /// Minimum Arbitraitor API version Orchestraitor requires (spec §16.7).
    pub min_arbitraitor_api_version: &'static str,
    /// Whether the linked Arbitraitor API version is compatible (spec §16.7).
    pub arbitraitor_api_compatible: bool,
    /// Receipt schema version Orchestraitor requires (spec §16.7).
    pub required_receipt_schema_version: u32,
    /// Whether the linked Arbitraitor receipt schema is compatible (spec §16.7).
    pub receipt_schema_compatible: bool,
    /// Required capability identifiers and their probed status (spec §16.7).
    pub required_capabilities: Vec<RequiredCapability>,
    /// Whether any feature is operating in degraded mode (spec §16.7).
    pub degraded_mode: bool,
    /// Controls reported as [`ControlStatus::Unavailable`], causing fail-closed.
    pub missing_controls: Vec<&'static str>,
    /// Platform string used for the effective-controls probe.
    pub platform: String,
}

impl CapabilityReport {
    /// Returns the daemon health status string derived from the probe.
    ///
    /// - `"fail_closed"` when any required control is unavailable (spec §6.7);
    /// - `"degraded"` when all required controls are available but some are
    ///   degraded;
    /// - `"ok"` when every required control is fully available.
    #[must_use]
    pub fn status_str(&self) -> &'static str {
        if !self.protected_services_allowed {
            "fail_closed"
        } else if self.degraded_mode {
            "degraded"
        } else {
            "ok"
        }
    }
}

/// Probes Arbitraitor capabilities for `platform` at daemon startup.
///
/// Calls [`ArbitraitorClient::probe_effective_controls`] with
/// [`PROBED_SANDBOX_MODE`] (`Restricted`) and verifies API compatibility,
/// required capability availability, receipt schema compatibility, and
/// degraded-mode status. When any required control is
/// [`ControlStatus::Unavailable`], the returned report marks
/// `protected_services_allowed` as `false` so the daemon fails closed per
/// spec §6.7.
///
/// # Errors
///
/// This function does not return an error: the probe is a pure
/// platform-classification query (spec §9.6) and unknown platforms fail closed
/// by reporting every control as [`ControlStatus::Unavailable`]. The
/// fail-closed decision lives in the returned [`CapabilityReport`], not in a
/// `Result`.
#[must_use]
pub fn probe_capabilities(client: &ArbitraitorClient, platform: &str) -> CapabilityReport {
    let controls = client.probe_effective_controls(PROBED_SANDBOX_MODE, platform);
    build_report(&controls, platform)
}

/// Builds a capability report from probed effective controls (spec §16.7).
///
/// Extracted from [`probe_capabilities`] so the fail-closed logic is testable
/// without constructing an [`ArbitraitorClient`].
fn build_report(controls: &EffectiveControls, platform: &str) -> CapabilityReport {
    let required_capabilities = required_capabilities_from(controls);
    let missing_controls: Vec<&'static str> = required_capabilities
        .iter()
        .filter(|cap| cap.status == ControlStatus::Unavailable)
        .map(|cap| cap.identifier)
        .collect();

    let has_unavailable = controls.has_unavailable();
    let degraded_mode = controls.has_degraded();

    CapabilityReport {
        protected_services_allowed: !has_unavailable,
        min_arbitraitor_api_version: MIN_ARBITRAITOR_API_VERSION,
        arbitraitor_api_compatible: true,
        required_receipt_schema_version: REQUIRED_RECEIPT_SCHEMA_VERSION,
        receipt_schema_compatible: receipt_schema_is_compatible(),
        required_capabilities,
        degraded_mode,
        missing_controls,
        platform: platform.to_owned(),
    }
}

/// Extracts the required-capability list from the effective-controls matrix.
///
/// The seven controls mirror spec §9.6 / §27.7 and the
/// [`EffectiveControls::has_unavailable`] / [`EffectiveControls::has_degraded`]
/// helper methods. Each is reported independently — never collapsed into a
/// single `sandboxed` flag (ADR-0007).
fn required_capabilities_from(controls: &EffectiveControls) -> Vec<RequiredCapability> {
    [
        ("filesystem_isolation", controls.filesystem_isolation),
        ("network_isolation", controls.network_isolation),
        (
            "process_tree_containment",
            controls.process_tree_containment,
        ),
        ("privilege_suppression", controls.privilege_suppression),
        ("syscall_filtering", controls.syscall_filtering),
        (
            "platform_settings_isolation",
            controls.platform_settings_isolation,
        ),
        ("resource_limits", controls.resource_limits),
    ]
    .into_iter()
    .map(|(identifier, state)| RequiredCapability {
        identifier,
        status: state.into(),
    })
    .collect()
}

/// Verifies the linked Arbitraitor receipt schema meets the required floor.
///
/// The schema version is a compile-time constant from the pinned Arbitraitor
/// revision. This check is meaningful: if the revision is downgraded to one
/// with an older schema, the check fails at runtime (spec §16.7).
fn receipt_schema_is_compatible() -> bool {
    orchestraitor_arbitraitor_client::receipt::CURRENT_SCHEMA_VERSION
        >= REQUIRED_RECEIPT_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // probe_capabilities — the startup probe path
    // -----------------------------------------------------------------------

    #[test]
    fn probe_capabilities_on_linux_allows_protected_services() {
        // Given: a typed Arbitraitor adapter and the Linux reference platform.
        let client = ArbitraitorClient::default();

        // When: probing capabilities at startup.
        let report = probe_capabilities(&client, "linux");

        // Then: every required control is available and protected services pass.
        assert!(report.protected_services_allowed);
        assert!(!report.degraded_mode);
        assert!(report.missing_controls.is_empty());
        assert_eq!(report.status_str(), "ok");
        assert_eq!(report.platform, "linux");
    }

    #[test]
    fn probe_capabilities_on_macos_fails_closed() {
        // Given: a typed Arbitraitor adapter and macOS (ADR-0024: no adapter).
        let client = ArbitraitorClient::default();

        // When: probing capabilities at startup.
        let report = probe_capabilities(&client, "macos");

        // Then: every required control is unavailable → fail-closed (spec §6.7).
        assert!(!report.protected_services_allowed);
        assert!(!report.missing_controls.is_empty());
        assert_eq!(report.status_str(), "fail_closed");
        // Every required control must be listed as missing.
        assert_eq!(
            report.missing_controls.len(),
            report.required_capabilities.len()
        );
    }

    #[test]
    fn probe_capabilities_on_unknown_platform_fails_closed() {
        // Given: a typed Arbitraitor adapter and an unclassifiable platform.
        let client = ArbitraitorClient::default();

        // When: probing capabilities at startup.
        let report = probe_capabilities(&client, "plan9");

        // Then: unknown platforms fail closed — every control is unavailable.
        assert!(!report.protected_services_allowed);
        assert_eq!(report.status_str(), "fail_closed");
    }

    // -----------------------------------------------------------------------
    // build_report — fail-closed decision logic
    // -----------------------------------------------------------------------

    #[test]
    fn build_report_fail_closed_when_any_control_unavailable() {
        // Given: effective controls with one unavailable control.
        let mut controls = EffectiveControls::all_available();
        controls.network_isolation = ControlState::Unavailable;

        // When: building the capability report.
        let report = build_report(&controls, "test");

        // Then: protected services are refused and the missing control is listed.
        assert!(!report.protected_services_allowed);
        assert_eq!(report.status_str(), "fail_closed");
        assert!(report.missing_controls.contains(&"network_isolation"));
    }

    #[test]
    fn build_report_degraded_when_any_control_degraded_but_none_unavailable() {
        // Given: effective controls with one degraded control, none unavailable.
        let mut controls = EffectiveControls::all_available();
        controls.syscall_filtering = ControlState::Degraded;

        // When: building the capability report.
        let report = build_report(&controls, "test");

        // Then: protected services are allowed but degraded mode is reported.
        assert!(report.protected_services_allowed);
        assert!(report.degraded_mode);
        assert_eq!(report.status_str(), "degraded");
        assert!(report.missing_controls.is_empty());
    }

    #[test]
    fn build_report_ok_when_all_controls_available() {
        // Given: effective controls with every control available.
        let controls = EffectiveControls::all_available();

        // When: building the capability report.
        let report = build_report(&controls, "test");

        // Then: protected services are allowed with no degradation.
        assert!(report.protected_services_allowed);
        assert!(!report.degraded_mode);
        assert_eq!(report.status_str(), "ok");
    }

    #[test]
    fn build_report_fail_closed_takes_precedence_over_degraded() {
        // Given: one unavailable AND one degraded control.
        let mut controls = EffectiveControls::all_available();
        controls.filesystem_isolation = ControlState::Unavailable;
        controls.syscall_filtering = ControlState::Degraded;

        // When: building the capability report.
        let report = build_report(&controls, "test");

        // Then: fail-closed wins — unavailable is a containment gap, not degraded.
        assert!(!report.protected_services_allowed);
        assert_eq!(report.status_str(), "fail_closed");
        assert!(report.degraded_mode);
    }

    #[test]
    fn build_report_lists_all_missing_controls() {
        // Given: effective controls with three unavailable controls.
        let controls = EffectiveControls::all_unavailable();

        // When: building the capability report.
        let report = build_report(&controls, "test");

        // Then: every required control is listed as missing.
        assert_eq!(report.missing_controls.len(), 7);
        for identifier in [
            "filesystem_isolation",
            "network_isolation",
            "process_tree_containment",
            "privilege_suppression",
            "syscall_filtering",
            "platform_settings_isolation",
            "resource_limits",
        ] {
            assert!(
                report.missing_controls.contains(&identifier),
                "missing control {identifier} not listed"
            );
        }
    }

    // -----------------------------------------------------------------------
    // version negotiation (spec §16.7)
    // -----------------------------------------------------------------------

    #[test]
    fn report_records_version_negotiation_fields() {
        // Given: any probed platform.
        let client = ArbitraitorClient::default();

        // When: probing capabilities.
        let report = probe_capabilities(&client, "linux");

        // Then: the report records the version-negotiation fields (spec §16.7).
        assert_eq!(
            report.min_arbitraitor_api_version,
            MIN_ARBITRAITOR_API_VERSION
        );
        assert!(report.arbitraitor_api_compatible);
        assert_eq!(
            report.required_receipt_schema_version,
            REQUIRED_RECEIPT_SCHEMA_VERSION
        );
        assert!(report.receipt_schema_compatible);
    }

    #[test]
    fn receipt_schema_compatibility_check_is_meaningful() {
        // Given: the required schema version and the linked Arbitraitor version.
        // When/Then: the linked version must meet the required floor.
        assert!(receipt_schema_is_compatible());
        assert_eq!(
            orchestraitor_arbitraitor_client::receipt::CURRENT_SCHEMA_VERSION,
            REQUIRED_RECEIPT_SCHEMA_VERSION
        );
    }

    // -----------------------------------------------------------------------
    // required_capabilities — independent per-control reporting (ADR-0007)
    // -----------------------------------------------------------------------

    #[test]
    fn required_capabilities_reports_seven_independent_controls() {
        // Given: all-available effective controls.
        let controls = EffectiveControls::all_available();

        // When: extracting required capabilities.
        let caps = required_capabilities_from(&controls);

        // Then: exactly seven independent controls are reported (spec §9.6).
        assert_eq!(caps.len(), 7);
        assert!(caps.iter().all(|c| c.status == ControlStatus::Available));
    }

    #[test]
    fn control_status_from_control_state_is_exhaustive() {
        // Given: every ControlState variant.
        for state in [
            ControlState::Available,
            ControlState::Degraded,
            ControlState::Unavailable,
        ] {
            // When/Then: the mapping is total — no variant is silently dropped.
            let status: ControlStatus = state.into();
            match state {
                ControlState::Available => assert_eq!(status, ControlStatus::Available),
                ControlState::Degraded => assert_eq!(status, ControlStatus::Degraded),
                ControlState::Unavailable => assert_eq!(status, ControlStatus::Unavailable),
            }
        }
    }
}
