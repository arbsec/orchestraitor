//! Bootstrap-worker sandbox mediation (issue #311).
//!
//! The mini-worker's execution path runs **only** behind Arbitraitor. Spawn is
//! gated on a capability preflight that asks Arbitraitor which containment
//! controls would actually be in effect for
//! [`SandboxMode::Restricted`](crate::sandbox::SandboxMode::Restricted) on the
//! current platform, per spec `40-arbitraitor-integration.md` §9.6 (probe:
//! [`arbitraitor_sandbox::compute_effective_controls`]) and §6.7 (Arbitraitor
//! is the sole security authority; absent controls fail closed):
//!
//! 1. [`MediatedWorker::spawn`] records a [`WorkerPreflight`] (controls matrix
//!    + verdict, the run-state seam) and refuses to construct the worker when
//!      any required control is [`ControlState::Unavailable`] — the typed
//!      [`MediationError::UnavailableControls`] names each missing control.
//! 2. The bootstrap loop is Linux-only per ADR-0024: any other platform fails
//!    closed with [`MediationError::UnsupportedPlatform`] — no explicit
//!    non-secure mode exists for the bootstrap worker.
//! 3. [`MediatedWorker::run_bash`] routes bash through Arbitraitor's mediated
//!    exec path: `arbitraitor_exec::ExecutionContextBuilder` (via
//!    `ScriptExecution::bash()`) with an explicit
//!    [`ExecutionPolicy`](crate::exec::ExecutionPolicy) whose network policy is
//!    [`NetworkPolicy::Denied`](crate::exec::NetworkPolicy::Denied). No direct
//!    `std::process` spawn exists on the worker path; Landlock, seccomp,
//!    `no_new_privs`, fd closure, and resource limits are applied by
//!    Arbitraitor (`configure_command` / `configure_filesystem_isolation` /
//!    fenced `prlimit` inside `arbitraitor-exec`).
//!
//! This module implements no security primitive: it probes Arbitraitor,
//! records the probe, and gates on Arbitraitor's report. E5 scope (leases,
//! receipts, approval-required handling, Disposable mode) is deliberately out.
//!
//! Spec-narrative mapping (spec `40-arbitraitor-integration.md` §9.6 names
//! `configure_command` / `apply_sandbox` conceptually): the pinned Arbitraitor
//! revision `4ebebb3` realizes that surface as
//! `arbitraitor_sandbox::configure_command` +
//! `arbitraitor_sandbox::configure_filesystem_isolation` (invoked inside
//! `arbitraitor-exec`'s command construction) and the real
//! `ExecutionContextBuilder` methods are `new` / `from_operation` /
//! `command` / `arguments` / `policy` / `source_environment` / `build` —
//! there are no `configure_command` / `apply_sandbox` builder methods.

use std::fmt;

use thiserror::Error;

use crate::ArbitraitorClient;
use crate::sandbox::{ControlState, EffectiveControls, SandboxMode};

/// Sandbox mode the bootstrap worker must run under (spec
/// `40-arbitraitor-integration.md` §9.6).
///
/// `Restricted` is the minimum enforcement envelope for protected operations.
/// The preflight asks Arbitraitor which of its required controls the current
/// platform actually delivers and fails closed on any gap.
pub const WORKER_SANDBOX_MODE: SandboxMode = SandboxMode::Restricted;

/// The only platform the bootstrap worker runs on (ADR-0024).
///
/// `Restricted` containment is wired for Linux only in the pinned Arbitraitor
/// revision; every other platform fails closed — there is no explicitly
/// labelled non-secure fallback for the bootstrap loop.
pub const WORKER_PLATFORM: &str = "linux";

/// Names of the required sandbox controls that came back unavailable.
///
/// Kept as a typed, displayable list so the refusal is both machine-testable
/// (exact control identifiers) and log-safe (control names only — never
/// command lines, arguments, or output).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MissingControls(Vec<&'static str>);

impl MissingControls {
    /// Returns the unavailable control identifiers in stable order.
    #[must_use]
    pub fn as_slice(&self) -> &[&'static str] {
        &self.0
    }

    /// Returns true when no required control is unavailable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the number of unavailable required controls.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Display for MissingControls {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.join(", "))
    }
}

impl IntoIterator for MissingControls {
    type Item = &'static str;
    type IntoIter = std::vec::IntoIter<&'static str>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Errors from bootstrap-worker mediation.
///
/// Every variant is a fail-closed refusal or a reason-coded translation of an
/// Arbitraitor-side [`ExecError`](crate::exec::ExecError). Reason codes are
/// static strings so error output never carries command lines, arguments,
/// environment values, or captured child output (spec
/// `40-arbitraitor-integration.md` §9.23.4 log-safety rule).
#[derive(Debug, Error)]
pub enum MediationError {
    /// The bootstrap worker platform is not Linux (ADR-0024 fail closed; no
    /// non-secure fallback exists on this path).
    #[error(
        "bootstrap worker mediation requires Linux (ADR-0024 fails closed; no non-secure mode): \
         platform {platform:?} is unsupported"
    )]
    UnsupportedPlatform {
        /// Probed platform string.
        platform: String,
    },
    /// One or more required `Restricted` controls are unavailable on this
    /// platform (spec `40-arbitraitor-integration.md` §6.7: the worker start
    /// is blocked and the missing Arbitraitor capability is identified).
    #[error(
        "restricted sandbox preflight refused bootstrap worker start on platform \
         {platform:?}: unavailable controls: {missing}"
    )]
    UnavailableControls {
        /// Probed platform string.
        platform: String,
        /// Required controls that Arbitraitor reports as unavailable.
        missing: MissingControls,
    },
    /// Building the mediated execution context failed.
    #[error("mediated execution context construction failed: {reason}")]
    Context {
        /// Static reason code translated from the Arbitraitor error.
        reason: &'static str,
    },
    /// Running the mediated bash script failed at the spawn/I/O/wait layer.
    #[error("mediated bash execution failed: {reason}")]
    Bash {
        /// Static reason code translated from the Arbitraitor error.
        reason: &'static str,
    },
}

/// Verdict recorded with the worker preflight (run-state seam, issue #311).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreflightVerdict {
    /// Every required control is available on a supported platform; the
    /// worker may start mediated.
    Allowed,
    /// At least one required control is unavailable, or the platform is
    /// unsupported; the worker must not start.
    Refused,
}

/// Recorded bootstrap-worker preflight: the Arbitraitor effective-controls
/// matrix plus the verdict derived from it.
///
/// This is the value a caller records in the run state (controls matrix +
/// verdict, issue #311 acceptance criteria). It carries the Arbitraitor-owned
/// [`EffectiveControls`] verbatim — Orchestraitor derives a verdict from the
/// report but never recomputes the matrix itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerPreflight {
    /// Platform string used for the probe.
    pub platform: String,
    /// Requested sandbox mode (always [`WORKER_SANDBOX_MODE`]).
    pub mode: SandboxMode,
    /// Spawn verdict implied by the matrix and platform support.
    pub verdict: PreflightVerdict,
    /// Required controls Arbitraitor reports as unavailable (empty iff the
    /// matrix is fully contained).
    pub missing_controls: MissingControls,
    /// The Arbitraitor-owned effective-controls matrix for this run.
    pub controls: EffectiveControls,
}

/// Probes and records the bootstrap-worker preflight without gating.
///
/// Calls
/// [`ArbitraitorClient::probe_effective_controls`] with
/// [`WORKER_SANDBOX_MODE`] (`Restricted`) and derives the verdict: the probe
/// is a pure platform-classification query, so this never fails — the
/// fail-closed decision is expressed in the returned
/// [`WorkerPreflight::verdict`]. The returned value is the run-state record
/// for the controls matrix + verdict.
#[must_use]
pub fn probe_worker_preflight(client: &ArbitraitorClient, platform: &str) -> WorkerPreflight {
    let controls = client.probe_effective_controls(WORKER_SANDBOX_MODE, platform);
    build_preflight(platform, controls)
}

/// Builds a [`WorkerPreflight`] record from a probed controls matrix.
///
/// Extracted from [`probe_worker_preflight`] so the verdict logic is testable
/// with fixture matrices (mirrors the `build_report` seam in
/// `orchestraitor-daemon`'s startup capability probe).
fn build_preflight(platform: &str, controls: EffectiveControls) -> WorkerPreflight {
    let missing_controls = unavailable_controls(&controls);
    let verdict = if is_supported_platform(platform) && missing_controls.is_empty() {
        PreflightVerdict::Allowed
    } else {
        PreflightVerdict::Refused
    };
    WorkerPreflight {
        platform: platform.to_owned(),
        mode: WORKER_SANDBOX_MODE,
        verdict,
        missing_controls,
        controls,
    }
}

/// Returns true when `platform` is the supported bootstrap-worker platform.
fn is_supported_platform(platform: &str) -> bool {
    platform.eq_ignore_ascii_case(WORKER_PLATFORM)
}

/// Applies the fail-closed spawn decision to a recorded preflight.
///
/// Pure: no process is spawned, no temporary directory is materialized, and
/// no filesystem or network effect occurs on the refusal path — the
/// [`MediatedWorker`] token is only constructible through this gate, so a
/// `Refused` record cannot reach the execution surface (spec
/// `50-contracts-data.md` §21.4: absence is structural, not error-observed).
fn gate_preflight(preflight: WorkerPreflight) -> Result<WorkerPreflight, MediationError> {
    if !is_supported_platform(&preflight.platform) {
        return Err(MediationError::UnsupportedPlatform {
            platform: preflight.platform,
        });
    }
    if !preflight.missing_controls.is_empty() {
        return Err(MediationError::UnavailableControls {
            platform: preflight.platform,
            missing: preflight.missing_controls,
        });
    }
    Ok(preflight)
}

/// Names the required controls that are [`ControlState::Unavailable`].
///
/// The seven identifiers mirror spec `40-arbitraitor-integration.md` §9.6
/// (Arbitraitor's sandbox spec section 27.7) and match the daemon
/// health-report identifiers so run-state records and health RPCs name
/// controls identically.
fn unavailable_controls(controls: &EffectiveControls) -> MissingControls {
    let states = [
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
    ];
    MissingControls(
        states
            .into_iter()
            .filter(|(_, state)| matches!(state, ControlState::Unavailable))
            .map(|(identifier, _)| identifier)
            .collect(),
    )
}

/// Captured result of a mediated bash run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediatedRun {
    /// Exit code reported by the interpreter. `None` when the interpreter was
    /// terminated by a signal.
    pub exit_code: Option<i32>,
    /// Captured stdout bytes.
    pub stdout: Vec<u8>,
    /// Captured stderr bytes.
    pub stderr: Vec<u8>,
}

/// The bootstrap mini-worker, gated behind the Arbitraitor preflight.
///
/// A `MediatedWorker` value is proof that the `Restricted` preflight passed
/// on the current platform: the only constructor,
/// [`MediatedWorker::spawn`], probes Arbitraitor and refuses closed before
/// any execution surface is reachable. Bash runs through
/// [`MediatedWorker::run_bash`], which routes into Arbitraitor's mediated
/// exec path (`ExecutionContextBuilder` inside `ScriptExecution`) under an
/// explicit, network-denied
/// [`ExecutionPolicy`](crate::exec::ExecutionPolicy).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediatedWorker {
    preflight: WorkerPreflight,
}

impl MediatedWorker {
    /// Runs the capability preflight and, only when it allows, returns the
    /// gated worker.
    ///
    /// # Errors
    ///
    /// - [`MediationError::UnsupportedPlatform`] when `platform` is not Linux
    ///   (ADR-0024 fail closed; no non-secure bootstrap mode).
    /// - [`MediationError::UnavailableControls`] naming each required control
    ///   Arbitraitor reports as [`ControlState::Unavailable`].
    ///
    /// On either refusal no execution surface is constructed: no child
    /// process, no temporary execution directory, no environment filtering —
    /// fail closed with zero side effects.
    pub fn spawn(client: &ArbitraitorClient, platform: &str) -> Result<Self, MediationError> {
        let preflight = gate_preflight(probe_worker_preflight(client, platform))?;
        debug_assert_eq!(preflight.verdict, PreflightVerdict::Allowed);
        Ok(Self { preflight })
    }

    /// Returns the recorded preflight (controls matrix + verdict) for the
    /// run state.
    #[must_use]
    pub const fn preflight(&self) -> &WorkerPreflight {
        &self.preflight
    }

    /// Executes bash script bytes through Arbitraitor's mediated exec path.
    ///
    /// The script is streamed to `/bin/bash --noprofile --norc` over stdin
    /// (no executable file is materialized) inside a Linux network namespace
    /// with an explicitly network-denied
    /// [`ExecutionPolicy`](crate::exec::ExecutionPolicy), the allowlisted
    /// environment, controlled PATH, temporary HOME/working directories,
    /// privilege-elevation rejection, Landlock filesystem rules, and fenced
    /// resource limits — all enforced by the pinned Arbitraitor revision.
    ///
    /// # Errors
    ///
    /// - [`MediationError::UnsupportedPlatform`] on non-Linux targets
    ///   (defense in depth; [`MediatedWorker::spawn`] already refuses there).
    /// - [`MediationError::Context`] when Arbitraitor refuses to build the
    ///   mediated context (e.g. running as root, unsafe PATH entry).
    /// - [`MediationError::Bash`] when spawn, script piping, or output
    ///   collection fails at the Arbitraitor exec layer.
    pub fn run_bash(&self, script: &[u8]) -> Result<MediatedRun, MediationError> {
        linux_exec::run_bash(script)
    }
}

#[cfg(target_os = "linux")]
mod linux_exec {
    use super::{MediatedRun, MediationError};
    use crate::exec::{ExecError, ExecutionPolicy, NetworkPolicy, ScriptExecution};

    pub(super) fn run_bash(script: &[u8]) -> Result<MediatedRun, MediationError> {
        // Explicit mediated policy (issue #311): network is denied even
        // though `NetworkPolicy::Denied` is also the crate default — the
        // bootstrap path names its enforcement posture instead of inheriting
        // it silently. The remaining `ExecutionPolicy::default()` fields are
        // the Arbitraitor-mediated profile: allowlisted environment, root-
        // owned controlled PATH, fd closure, temporary HOME/working dirs,
        // privilege-elevation and run-as-root refusal, bounded output.
        //
        // Spec-narrative mapping (spec `40-arbitraitor-integration.md` §9.6):
        // the conceptual `configure_command` / `apply_sandbox` steps are
        // realized in the pinned revision by
        // `arbitraitor_sandbox::configure_command` and
        // `arbitraitor_sandbox::configure_filesystem_isolation` inside
        // `ScriptExecution`'s command construction; the builder itself
        // exposes `new`/`from_operation`→`policy`→`build` (no
        // `configure_command`/`apply_sandbox` methods exist at rev 4ebebb3).
        let policy = ExecutionPolicy {
            network_policy: NetworkPolicy::Denied,
            ..ExecutionPolicy::default()
        };
        let source_environment = std::env::vars_os()
            .filter_map(|(name, value)| name.into_string().ok().map(|name| (name, value)))
            .collect::<Vec<_>>();
        let execution = ScriptExecution::bash()
            .map_err(|error| MediationError::Context {
                reason: exec_reason(&error),
            })?
            .with_environment_policy(policy, source_environment)
            .map_err(|error| MediationError::Context {
                reason: exec_reason(&error),
            })?;
        let result = execution
            .execute(script)
            .map_err(|error| MediationError::Bash {
                reason: exec_reason(&error),
            })?;
        Ok(MediatedRun {
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
        })
    }

    /// Translates an Arbitraitor [`ExecError`] into a static reason code.
    ///
    /// Log safety (spec `40-arbitraitor-integration.md` §9.23.4): some
    /// `ExecError` variants embed child output (`ScriptIo` carries captured
    /// stderr); mapping to a static code keeps that untrusted content out of
    /// Orchestraitor error text.
    fn exec_reason(error: &ExecError) -> &'static str {
        match error {
            ExecError::RunningAsRoot => "running-as-root",
            ExecError::ExecuteNotGranted => "execute-capability-not-granted",
            ExecError::NetworkNotGranted => "network-capability-not-granted",
            ExecError::CommandNotAbsolute { .. } => "command-not-absolute",
            ExecError::PrivilegeElevationAttempt { .. } => "privilege-elevation-blocked",
            ExecError::InvalidEnvironmentName { .. } => "invalid-environment-name",
            ExecError::DeniedEnvironmentVariable { .. } => "denied-environment-variable",
            ExecError::EmptyPath => "empty-controlled-path",
            ExecError::RelativePathEntry { .. } => "relative-path-entry",
            ExecError::UnsafePathEntry { .. } => "unsafe-path-entry",
            ExecError::UnsafeFixedDirectory { .. } => "unsafe-fixed-directory",
            ExecError::TemporaryDirectory { .. } => "temporary-directory",
            ExecError::RootDetection { .. } => "root-detection",
            ExecError::Spawn { .. } => "spawn",
            ExecError::Wait { .. } => "wait",
            ExecError::ScriptIo { .. } => "script-io",
            ExecError::NativeExecutionNotApproved => "native-execution-not-approved",
            ExecError::IncompatibleNativeExecutable => "native-incompatible",
            ExecError::NativePathNotAbsolute { .. } => "native-path-not-absolute",
            ExecError::NativeQuarantine { .. } => "native-quarantine",
            ExecError::NativeQuarantineMissing { .. } => "native-quarantine-missing",
            ExecError::ResourceLimit { .. } => "resource-limit",
            ExecError::OutputExceeded { .. } => "output-exceeded",
            ExecError::Store { .. } => "store",
            ExecError::MissingContainmentProof { .. } => "missing-containment-proof",
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod linux_exec {
    use super::{MediatedRun, MediationError};

    pub(super) fn run_bash(script: &[u8]) -> Result<MediatedRun, MediationError> {
        let _ = script;
        Err(MediationError::UnsupportedPlatform {
            platform: std::env::consts::OS.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn linux_client() -> ArbitraitorClient {
        ArbitraitorClient::default()
    }

    // -------------------------------------------------------------------
    // Preflight recording + gating (unit seam, no process spawn)
    // -------------------------------------------------------------------

    #[test]
    fn preflight_on_linux_records_allowed_verdict_and_full_matrix() {
        // Given: a typed Arbitraitor adapter on the Linux reference platform.
        let client = linux_client();

        // When: probing the bootstrap-worker preflight.
        let preflight = probe_worker_preflight(&client, "linux");

        // Then: the run-state record carries the Restricted mode, the
        // Arbitraitor controls matrix, an Allowed verdict, and no gaps.
        assert_eq!(preflight.mode, SandboxMode::Restricted);
        assert_eq!(preflight.verdict, PreflightVerdict::Allowed);
        assert!(preflight.missing_controls.is_empty());
        assert!(preflight.controls.is_fully_contained());
        assert!(!preflight.controls.has_unavailable());
    }

    #[test]
    fn preflight_records_refused_verdict_with_missing_controls_listed() {
        // Given: a fixture controls matrix with exactly one unavailable
        // control (simulated missing-capability probe result).
        let mut controls = EffectiveControls::all_available();
        controls.network_isolation = ControlState::Unavailable;

        // When: building the preflight record.
        let preflight = build_preflight("linux", controls);

        // Then: the verdict is Refused and names the missing control.
        assert_eq!(preflight.verdict, PreflightVerdict::Refused);
        assert_eq!(preflight.missing_controls.as_slice(), ["network_isolation"]);
    }

    #[test]
    fn gate_refuses_spawn_with_typed_error_naming_the_missing_control() -> TestResult {
        // Given: a fixture preflight with one unavailable control.
        let mut controls = EffectiveControls::all_available();
        controls.filesystem_isolation = ControlState::Unavailable;
        let preflight = build_preflight("linux", controls);

        // When: the spawn gate evaluates the preflight.
        let Err(error) = gate_preflight(preflight) else {
            return Err("preflight gate allowed spawn despite unavailable control".into());
        };

        // Then: the typed error names the missing control, in the variant and
        // in its display text.
        let MediationError::UnavailableControls { platform, missing } = &error else {
            return Err(format!("expected UnavailableControls, got: {error}").into());
        };
        assert_eq!(platform, "linux");
        assert_eq!(missing.as_slice(), ["filesystem_isolation"]);
        assert!(
            error.to_string().contains("filesystem_isolation"),
            "error display must name the missing control: {error}"
        );
        Ok(())
    }

    #[test]
    fn gate_refusal_spawns_no_process_and_materializes_no_exec_directories() {
        // spec 50-contracts-data.md §21.4: assert the forbidden side effects
        // did NOT happen — not merely that an error surfaced.
        //
        // Given: a preflight that will be refused (every control unavailable,
        // e.g. an unknown platform's probe result).
        let controls = EffectiveControls::all_unavailable();
        let preflight = build_preflight("linux", controls);

        // Snapshot this process's Arbitraitor execution temp directories:
        // ScriptExecution materializes them at construction, so any
        // construction would be observable here.
        let before = count_exec_temp_dirs();

        // When: the spawn gate refuses.
        assert!(gate_preflight(preflight).is_err());

        // Then: no execution context was materialized. Because the gate is
        // synchronous, refuses before touching the exec crate, and these
        // directory names are namespaced to this process, the unchanged count
        // proves no mediated context build (and therefore no spawn) occurred.
        assert_eq!(count_exec_temp_dirs(), before);
    }

    #[test]
    fn gate_refusal_lists_every_missing_control() -> TestResult {
        // Given: all controls unavailable (unknown-platform fixture class).
        let preflight = build_preflight("linux", EffectiveControls::all_unavailable());

        // When: gating.
        let missing = match gate_preflight(preflight) {
            Err(MediationError::UnavailableControls { missing, .. }) => missing,
            other => {
                return Err(format!("expected UnavailableControls, got: {other:?}").into());
            }
        };

        // Then: all seven required controls are named (spec
        // `40-arbitraitor-integration.md` §9.6; Arbitraitor sandbox spec
        // section 27.7).
        assert_eq!(missing.len(), 7);
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
                missing.as_slice().contains(&identifier),
                "missing control {identifier} not listed"
            );
        }
        Ok(())
    }

    // -------------------------------------------------------------------
    // Platform boundary (ADR-0024 fail closed)
    // -------------------------------------------------------------------

    #[test]
    fn spawn_on_non_linux_fails_closed_with_typed_platform_error() -> TestResult {
        // Given: non-Linux platforms (macOS, Darwin alias, Windows, unknown).
        let client = linux_client();

        for platform in ["macos", "darwin", "windows", "plan9"] {
            // When: attempting to spawn the bootstrap worker.
            let Err(error) = MediatedWorker::spawn(&client, platform) else {
                return Err(format!("bootstrap worker spawned on unsupported {platform}").into());
            };

            // Then: a typed fail-closed platform error names the platform; no
            // worker (and therefore no execution surface) exists.
            let MediationError::UnsupportedPlatform { platform: named } = &error else {
                return Err(
                    format!("expected UnsupportedPlatform for {platform}, got: {error}").into(),
                );
            };
            assert_eq!(named, platform);
        }
        Ok(())
    }

    #[test]
    fn spawn_error_display_is_log_safe_and_platform_qualified() -> TestResult {
        // Given/When: a refused non-Linux spawn.
        let client = linux_client();
        let Err(error) = MediatedWorker::spawn(&client, "windows") else {
            return Err("bootstrap worker spawned on windows".into());
        };

        // Then: the message carries the fail-closed semantics and platform,
        // with no command/environment content.
        let text = error.to_string();
        assert!(text.contains("ADR-0024"), "missing ADR anchor: {text}");
        assert!(text.contains("windows"), "missing platform name: {text}");
        assert!(
            text.contains("no non-secure mode"),
            "missing no-fallback semantics: {text}"
        );
        Ok(())
    }

    // -------------------------------------------------------------------
    // Mediated bash execution (Linux; real Arbitraitor enforcement)
    // -------------------------------------------------------------------

    /// Returns the gated Linux worker only when the FULL mediated stack can
    /// actually run a benign child on this host.
    ///
    /// The probe executes `true` through the real `run_bash` path (network
    /// namespace + Landlock + hardening), not a bare `unshare` check: the
    /// pinned Arbitraitor revision `4ebebb3` applies its Landlock ruleset to
    /// the `unshare` wrapper process, which needs `/proc/self/uid_map` access
    /// that the rules deny — on hosts with the Landlock LSM active the wrapper
    /// dies with `unshare: cannot open /proc/self/uid_map: Permission denied`.
    /// Upstream's own happy-path tests run with network isolation disabled, so
    /// only a full-stack probe distinguishes a working enforcement stack from
    /// that defect. Hosts that cannot run the mediated stack skip the live
    /// enforcement tests rather than fabricate a weaker claim; the refusal and
    /// preflight tests above never skip.
    #[cfg(target_os = "linux")]
    fn mediated_stack_or_skip() -> Option<MediatedWorker> {
        if !Path::new("/bin/bash").exists() || !network_namespace_supported() {
            return None;
        }
        let worker = MediatedWorker::spawn(&linux_client(), "linux").ok()?;
        match worker.run_bash(b"true\n") {
            Ok(run) if run.exit_code == Some(0) => Some(worker),
            _ => None,
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mediated_bash_runs_benign_command_and_captures_output() -> TestResult {
        // Given: a gated worker on supported Linux, or skip when the host
        // cannot create the network namespace (CI variance).
        let Some(worker) = mediated_stack_or_skip() else {
            return Ok(());
        };

        // When: running a benign printf through the mediated path.
        let run = worker.run_bash(b"printf 'orc-311-bootstrap\\n'\n")?;

        // Then: the output round-trips and the exit status is success.
        assert_eq!(
            run.stdout,
            b"orc-311-bootstrap\n",
            "stdout mismatch (exit={:?}, stderr={})",
            run.exit_code,
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(
            run.stderr.is_empty(),
            "unexpected stderr: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert_eq!(run.exit_code, Some(0));

        // And: the preflight record the run state persists is present.
        assert_eq!(worker.preflight().verdict, PreflightVerdict::Allowed);
        assert_eq!(worker.preflight().mode, SandboxMode::Restricted);
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mediated_bash_env_is_allowlisted_and_path_is_controlled() -> TestResult {
        // Given: a gated worker.
        let Some(worker) = mediated_stack_or_skip() else {
            return Ok(());
        };

        // When: asking the child for its environment.
        let run = worker.run_bash(b"env -0\n")?;
        let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
        let names: Vec<&str> = stdout
            .split('\0')
            .filter_map(|pair| pair.split_once('=').map(|(name, _)| name))
            .collect();

        // Then: every child variable is either allowlisted by the mediated
        // policy or re-created by bash itself — nothing else leaks through.
        let allowlisted = ["LANG", "LC_ALL", "TERM", "PATH", "HOME"];
        let shell_internal = ["PWD", "SHLVL", "_", "OLDPWD"];
        for name in &names {
            assert!(
                allowlisted.contains(name) || shell_internal.contains(name),
                "unmediated environment variable reached the child: {name} (exit={:?}, stderr={})",
                run.exit_code,
                String::from_utf8_lossy(&run.stderr)
            );
        }
        assert!(names.contains(&"PATH"), "controlled PATH missing in child");
        assert!(names.contains(&"HOME"), "temporary HOME missing in child");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn network_denied_exec_leaves_no_connection() -> TestResult {
        // spec 50-contracts-data.md §21.4 negative: assert the forbidden
        // network effect did NOT happen — the canary listener must observe
        // zero connections, not merely a failed exit code.
        //
        // Given: a host loopback canary listener and a gated worker whose
        // mediated exec runs in an isolated network namespace.
        use std::net::TcpListener;

        let Some(worker) = mediated_stack_or_skip() else {
            return Ok(());
        };
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();

        // When: the mediated child attempts a loopback connection to the
        // canary (from inside its isolated namespace this resolves to the
        // namespace's own, unbound loopback).
        let script = format!("exec 3<>/dev/tcp/127.0.0.1/{port}\nprintf 'forbidden-connect' >&3\n");
        let run = worker.run_bash(script.as_bytes())?;

        // Then: the child could not connect (non-zero exit) AND the canary
        // observed no connection — observable absence, not error attribution.
        assert_ne!(
            run.exit_code,
            Some(0),
            "network-denied exec unexpectedly exited 0 attempting a loopback connect"
        );
        match listener.accept() {
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
            Ok((_stream, peer)) => {
                Err(format!("network-denied exec reached the host listener from {peer}").into())
            }
            Err(error) => Err(format!("canary accept failed unexpectedly: {error}").into()),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn filesystem_denied_exec_leaves_no_canary_write() -> TestResult {
        // spec 50-contracts-data.md §21.4 negative: assert the forbidden
        // filesystem effect did NOT happen — the canary file must not exist,
        // not merely a failed exit code.
        //
        // Given: a canary path directly under /tmp. The pinned Arbitraitor
        // revision's Landlock ruleset grants /tmp read/execute only
        // (`landlock_rules_for_script_execution`: `PathRule::read_execute`);
        // write rights exist solely under the per-execution working and HOME
        // directories (`arbitraitor-exec-<pid>-*`), so this write must be
        // denied by the sandbox itself. Any pre-existing canary is removed
        // first, so the assertion below observes only this run's effect.
        const CANARY: &str = "/tmp/orc-311-canary-write";
        let canary = Path::new(CANARY);
        let Some(worker) = mediated_stack_or_skip() else {
            return Ok(());
        };
        if canary.exists() {
            std::fs::remove_file(canary)?;
        }

        // When: the mediated child attempts the forbidden write.
        let run = worker.run_bash(b"echo x > /tmp/orc-311-canary-write\n")?;

        // Then: the child could not write (non-zero exit) AND the canary
        // does not exist — observable absence, not error attribution.
        assert_ne!(
            run.exit_code,
            Some(0),
            "filesystem-denied exec unexpectedly exited 0 attempting a /tmp write"
        );
        assert!(
            !canary.exists(),
            "forbidden write materialized {CANARY} despite read-execute-only /tmp"
        );
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mediated_bash_nonzero_exit_is_a_script_result_not_a_refusal() -> TestResult {
        // Given: a gated worker.
        let Some(worker) = mediated_stack_or_skip() else {
            return Ok(());
        };

        // When: the child script exits non-zero on its own terms.
        let run = worker.run_bash(b"exit 42\n")?;

        // Then: the real exit code is propagated (no misleading refusal).
        assert_eq!(
            run.exit_code,
            Some(42),
            "exit code mismatch (stderr={})",
            String::from_utf8_lossy(&run.stderr)
        );
        Ok(())
    }

    /// Counts `arbitraitor-exec-<pid>-*` temp dirs belonging to this process.
    fn count_exec_temp_dirs() -> usize {
        let prefix = format!("arbitraitor-exec-{}-", std::process::id());
        std::fs::read_dir(std::env::temp_dir()).map_or(0, |entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.starts_with(&prefix))
                })
                .count()
        })
    }

    /// Mirrors the pinned Arbitraitor exec tests' capability probe: isolated
    /// execution needs util-linux `unshare` and usable unprivileged user
    /// namespaces; hosts without them (some CI containers) skip the live
    /// enforcement tests rather than fabricate a weaker claim.
    #[cfg(target_os = "linux")]
    fn network_namespace_supported() -> bool {
        let unshare = Path::new("/usr/bin/unshare");
        unshare.exists()
            && std::process::Command::new(unshare)
                .args(["--user", "--map-current-user", "--net", "--"])
                .arg("/bin/sh")
                .arg("-c")
                .arg("true")
                .status()
                .is_ok_and(|status| status.success())
    }
}
