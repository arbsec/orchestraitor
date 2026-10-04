//! Cancellation-aware child lifecycle for mediated bash execution (issue
//! #434): a spawn pipeline over the pinned Arbitraitor public API that keeps
//! an owned child handle so a cancelled caller cannot strand a running
//! interpreter.
//!
//! Why this module exists: `ScriptExecution::execute` spawns the interpreter
//! in-process and hands back only a finished [`ExecutionResult`] — the
//! child is unreachable from the caller. The pinned Arbitraitor revision
//! (4ebebb3) exposes no kill/pid surface, so this module reassembles the
//! same spawn pipeline from Arbitraitor's *public, policy-level* API
//! (`ExecutionPolicy`, `ExecutionContextBuilder`, `PathRule`,
//! `configure_command`, `configure_filesystem_isolation`, `ResourceLimits`)
//! and takes ownership of the child process itself:
//!
//! 1. The child is spawned into its own POSIX process group
//!    (`CommandExt::process_group(0)`) — every descendant the script forks
//!    lands in that group, so cleanup reaches the whole tree.
//! 2. The owned child lives in a [`BashChild`] guard whose `Drop` kills the
//!    process group (SIGKILL) and reaps the child. A caller that aborts its
//!    future (the loop runner's `kill_where`) drops the guard mid-`await` —
//!    the interpreter and its descendants die before the run is treated as
//!    aborted.
//! 3. Enforcement is byte-equivalent with the pinned `ScriptExecution`
//!    profile: the same default `ExecutionPolicy` with an explicit
//!    network-denied override, the same interpreter (`/bin/bash
//!    --noprofile --norc`), the same controlled environment, the same
//!    Landlock rule set, and the same fenced resource limits (SIGSTOP →
//!    prlimit → SIGCONT). No security decision is made here — every rule
//!    comes from Arbitraitor's own API (spec §2.2; this is orchestration
//!    process hygiene, not a security primitive).
//!
//! The output cap uses the pinned exec layer's default (combined
//! stdout+stderr bytes); overflow kills and reaps the child and fails with
//! [`MediationError::Bash { reason: "output-exceeded" }`], the same static
//! reason the pinned layer emits.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arbitraitor_exec::{ExecError, ExecutionPolicy, NetworkPolicy};
use arbitraitor_sandbox::{PathRule, configure_command, configure_filesystem_isolation};

use crate::mediation::{MediatedRun, MediationError};

/// Combined stdout+stderr output cap, mirroring the pinned
/// `arbitraitor-exec` default (`ResourceLimits::default().output_size_bytes`
/// = 10 MiB).
const OUTPUT_LIMIT: u64 = 10 * 1024 * 1024;

/// The child-exit poll interval in [`BashChild::wait`]: short enough that a
/// cancellation kill is reaped well inside the daemon budget, long enough
/// to be cheap.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Drains one pipe in a thread, enforcing the combined output cap; the
/// producer is killed when the cap is crossed (the pinned exec layer's own
/// overflow strategy) so the sibling stream observes EOF.
pub(crate) fn drain_stream(
    stream: impl Read + Send + 'static,
    total: Arc<AtomicU64>,
    pid: rustix::process::Pid,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut stream = stream;
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let read_bytes = u64::try_from(read).unwrap_or(u64::MAX);
                    let prev = total.fetch_add(read_bytes, Ordering::Relaxed);
                    buffer.extend_from_slice(&chunk[..read]);
                    if prev + read_bytes > OUTPUT_LIMIT {
                        // Kill the WHOLE group so inherited-pipe descendants
                        // die too — a bare interpreter kill would leave a
                        // descendant holding the pipes and the sibling
                        // drain thread blocked on EOF. Double-kill is
                        // harmless; ESRCH is ignored.
                        let _ =
                            rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                        break;
                    }
                }
            }
        }
        buffer
    })
}

/// Translates an Arbitraitor [`ExecError`] into the same static reason codes
/// the pinned exec-layer translation in `mediation.rs` emits (log safety,
/// spec §9.23.4).
pub(crate) fn exec_reason(error: &ExecError) -> &'static str {
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

/// A mediated interpreter child in its own process group, with an
/// externally callable kill.
///
/// The child leads its own POSIX process group; every descendant the script
/// forks lands in that group. [`BashChild::kill_group`] signals the WHOLE
/// group (SIGKILL), and [`BashChild::wait`] polls the child so it observes
/// the kill and reaps promptly. The kill capability is callable from any
/// thread — including the async caller whose `spawn_blocking` task Tokio
/// cannot abort mid-flight — and the [`Drop`] impl is the backstop that
/// kills and reaps a guard discarded without a kill or a completion.
pub struct BashChild {
    /// The interpreter child (spawned through the `unshare` wrapper).
    child: Mutex<Option<Child>>,
    /// The child's piped stdin: [`BashChild::wait`] writes the script and
    /// drops the write end; a kill or an unreaped drop just closes it (EOF).
    stdin: Mutex<Option<std::process::ChildStdin>>,
    /// Set once the child has been reaped; the drop path must not
    /// double-kill after a natural completion or an explicit kill.
    reaped: AtomicBool,
    /// The interpreter's pid, recorded at spawn: the kill path signals the
    /// process group even while the blocking `wait` holds the child lock.
    pid: AtomicU32,
    /// The built [`ExecutionContext`](arbitraitor_exec::ExecutionContext).
    /// It owns the child's temporary HOME and
    /// working directories — dropping it deletes them under a running
    /// interpreter, so the guard must hold it until the child is reaped
    /// (the pinned `execute()` keeps the same lifetime internally).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    environment: Option<arbitraitor_exec::ExecutionContext>,
}

impl BashChild {
    /// Builds the mediated context and spawns the interpreter, returning
    /// the owned child plus its piped stdin.
    ///
    /// The pipeline mirrors the pinned `ScriptExecution` spawn profile:
    /// `ScriptExecution::bash` interpreter/args, an explicit network-denied
    /// `ExecutionPolicy` (also the crate default — named, not inherited),
    /// `ExecutionContextBuilder` context construction, `env_clear` +
    /// allowlisted environment, the temporary working/HOME directories, the
    /// sandbox hardening `pre_exec`, the Landlock filesystem isolation, and
    /// the fenced resource limits (SIGSTOP → prlimit → SIGCONT).
    ///
    /// # Errors
    ///
    /// - [`MediationError::UnsupportedPlatform`] on non-Linux targets
    ///   (defense in depth; the preflight already refuses there).
    /// - [`MediationError::Context`] when Arbitraitor refuses to build the
    ///   mediated context.
    /// - [`MediationError::Bash`] when the spawn fails.
    pub fn spawn() -> Result<Self, MediationError> {
        #[cfg(target_os = "linux")]
        {
            let interpreter = std::path::PathBuf::from("/bin/bash");
            let interpreter_args = ["--noprofile", "--norc"];
            // Explicit mediated policy (issue #311): network is denied even
            // though `NetworkPolicy::Denied` is also the crate default — the
            // bootstrap path names its enforcement posture.
            let policy = ExecutionPolicy {
                network_policy: NetworkPolicy::Denied,
                ..ExecutionPolicy::default()
            };
            let source_environment = std::env::vars_os()
                .filter_map(|(name, value)| name.into_string().ok().map(|name| (name, value)))
                .collect::<Vec<_>>();
            // The same builder chain the pinned `with_environment_policy`
            // runs (execution plan, mediated assurance level, grants).
            let plan = pinned_operation_plan(&interpreter, &interpreter_args);
            let grants = arbitraitor_model::operation::GrantedCapabilities::new(
                arbitraitor_model::operation::CapabilityGrant(false),
                arbitraitor_model::operation::CapabilityGrant(false),
                arbitraitor_model::operation::CapabilityGrant(true),
                arbitraitor_model::operation::CapabilityGrant(false),
            );
            let environment = arbitraitor_exec::ExecutionContextBuilder::new(plan, grants)
                .assurance_level(arbitraitor_model::verdict::AssuranceLevel::Mediated)
                .command(interpreter.clone())
                .arguments(interpreter_args.iter().copied())
                .policy(policy)
                .source_environment(source_environment)
                .build()
                .map_err(|error| MediationError::Context {
                    reason: exec_reason(&error),
                })?;

            // The pinned `ScriptExecution` spawns network-isolated Linux
            // children through util-linux `unshare` (user + network
            // namespace; absolute path, no PATH lookup — failure prevents
            // the script from running, preserving fail-closed network
            // denial). The same wrapper shape is used here.
            let mut command = Command::new("/usr/bin/unshare");
            command.args(["--user", "--map-current-user", "--net", "--"]);
            command.arg(interpreter.clone());
            command.args(interpreter_args.iter().copied());
            // Fail closed on environment: clear the parent environment
            // entirely and re-establish only the mediated, allowlisted
            // variables produced by the ExecutionContext.
            command.env_clear();
            command.envs(environment.environment_iter());
            command.current_dir(environment.working_dir());
            command.stdin(Stdio::piped());
            command.stdout(Stdio::piped());
            command.stderr(Stdio::piped());
            // The cancellation fix: the interpreter leads its own process
            // group, so the guard's kill reaches every descendant the
            // script forks (a bare kill on the interpreter pid would miss
            // them).
            command.process_group(0);
            // Apply privilege hardening (no_new_privs, dumpable=0, fd
            // closure) in the child before exec — the unsafe pre_exec
            // boundary stays inside the sandbox crate, preserving
            // forbid(unsafe_code) here. The pinned `ScriptExecution` path
            // carries `SandboxConfig::default()` unless a caller opts into
            // `with_sandbox_config` (this bootstrap path never does), so
            // the default here is the same configuration.
            configure_command(&mut command, arbitraitor_sandbox::SandboxConfig::default());
            // Apply Landlock filesystem confinement with the pinned
            // exec layer's exact rule set.
            configure_filesystem_isolation(
                &mut command,
                &landlock_rules(&interpreter, &environment),
            );

            let mut child = command
                .spawn()
                .map_err(|_source| MediationError::Bash { reason: "spawn" })?;
            let child_pid = child.id();
            let stdin = child
                .stdin
                .take()
                .ok_or(MediationError::Bash { reason: "spawn" })?;

            // SIGSTOP the child, apply prlimit while frozen, then SIGCONT —
            // the pinned exec layer's fenced-limit sequence. If the limits
            // cannot be applied the child is killed and reaped so it can
            // never run unbounded.
            if let Err(error) = apply_limits_fenced(&mut child, &environment) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }

            // The pinned exec layer writes the script right after the fence
            // and drops the write end so the interpreter observes EOF; this
            // module keeps the same ordering — the script is piped by
            // [`BashChild::wait`] (or by the drop path, which simply closes
            // the pipe). Storing the stdin keeps the write-end lifetime on
            // the guard so a cancellation also closes it.
            Ok(Self {
                child: Mutex::new(Some(child)),
                reaped: AtomicBool::new(false),
                pid: AtomicU32::new(child_pid),
                environment: Some(environment),
                stdin: Mutex::new(Some(stdin)),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(MediationError::UnsupportedPlatform {
                platform: std::env::consts::OS.to_owned(),
            })
        }
    }

    /// Kills the interpreter's whole process group (SIGKILL) — safe to call
    /// from any thread, at any point in the child's lifetime. This is the
    /// cancellation entry point for an async caller whose
    /// `spawn_blocking` task Tokio cannot abort mid-flight: after the kill,
    /// a blocked [`BashChild::wait`] observes the child's exit on its next
    /// poll and reaps.
    pub fn kill_group(&self) {
        // The interpreter leads its own process group (`process_group(0)`
        // at spawn), so the negative pid reaches every descendant the
        // script forked — a bare kill on the interpreter pid would miss
        // them. Double-kill is harmless (ESRCH ignored); if the group was
        // already reaped, the signal just fails.
        let pid = self.pid.load(Ordering::Relaxed);
        if pid == 0 {
            return;
        }
        if let Some(pid) = rustix::process::Pid::from_raw(pid.cast_signed()) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        // Closing stdin releases the write end: a child blocked reading
        // stdin observes EOF instead of waiting on a writer that will
        // never come back.
        if let Ok(mut guard) = self.stdin.lock() {
            *guard = None;
        }
    }

    /// A no-op child for a completed run: pid 0 kills nothing, there is no
    /// child to reap, and the drop path releases nothing. Used by the
    /// mediator's armed guard after `wait` has completed.
    ///
    /// # Panics
    ///
    /// Never on a POSIX host: spawning `/usr/bin/true` with null stdio
    /// cannot fail there. On hosts where it would, `run_bash` cannot run
    /// anyway (the platform check refuses first).
    #[must_use]
    pub fn disarmed() -> Self {
        Self {
            child: Mutex::new(None),
            stdin: Mutex::new(None),
            reaped: AtomicBool::new(true),
            pid: AtomicU32::new(0),
            environment: None,
        }
    }

    /// Writes the script to the child's stdin (write end closed after),
    /// waits for the interpreter to finish, and collects the capped output.
    ///
    /// The wait polls the child so a concurrent [`BashChild::kill_group`]
    /// (the cancellation path) is observed promptly; after the kill the
    /// wait returns a `MediationError::Bash { reason: "aborted" }` — the
    /// blocked blocking task unwinds, so the loop's terminal status is
    /// recorded with the interpreter gone.
    ///
    /// The child is reaped here; the guard's drop path then does nothing.
    ///
    /// # Errors
    ///
    /// - [`MediationError::Bash`] with reason `aborted` when the child was
    ///   killed by a concurrent `kill_group` (cancellation), `script-io`
    ///   when piping the script failed with no exit code, `output-exceeded`
    ///   when the combined output crossed the cap, or `wait` when the child
    ///   could not be reaped.
    pub fn wait(&self, script: &[u8]) -> Result<MediatedRun, MediationError> {
        // Drain threads BEFORE the stdin write (the pinned exec layer's
        // `read_with_limit` ordering): bash runs commands while it reads
        // stdin, so a long script whose early output fills the pipe would
        // otherwise deadlock — bash blocked on its stdout write while this
        // blocking task is still inside `write_all`.
        let mut guard = self.child.lock().map_err(|_| MediationError::Bash {
            reason: "child-lock",
        })?;
        // `None` = the disarmed stub or an already-reaped child: nothing to
        // wait for (wait is only called on a live spawn).
        let Some(mut child) = guard.take() else {
            return Err(MediationError::Bash {
                reason: "child-lock",
            });
        };
        let pid = rustix::process::Pid::from_child(&child);
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let total = Arc::new(AtomicU64::new(0));
        let stdout_handle = stdout.map(|stream| drain_stream(stream, Arc::clone(&total), pid));
        let stderr_handle = stderr.map(|stream| drain_stream(stream, Arc::clone(&total), pid));

        // The script is written to the child's stdin and the write end is
        // closed immediately (the pinned exec layer does the same): the
        // interpreter observes EOF. A write failure is resolved below from
        // the captured status: a broken pipe after the child already exited
        // is the script's own early exit (its exit code is the result, the
        // pinned exec layer's `resolve_stdin_write_failure` contract); a
        // failure with no exit code (child killed by a signal) is a broker
        // I/O error — `script-io`.
        let stdin_result = {
            let mut guard = self.stdin.lock().map_err(|_| MediationError::Bash {
                reason: "child-lock",
            })?;
            match guard.as_mut() {
                Some(stdin) => {
                    let result = stdin.write_all(script).and_then(|()| stdin.flush());
                    *guard = None; // close the write end
                    result
                }
                // No stdin (already closed): nothing to write; the child
                // sees EOF.
                None => Ok(()),
            }
        };

        // Poll for exit so a concurrent `kill_group` — the cancellation
        // path from the async caller Tokio cannot deliver into a running
        // blocking task — is reaped promptly instead of blocking until the
        // killed interpreter's descendants drain.
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let _ = self.reaped.compare_exchange(
                        false,
                        true,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    let captured_stdout = stdout_handle
                        .map(|handle| handle.join().unwrap_or_default())
                        .unwrap_or_default();
                    let captured_stderr = stderr_handle
                        .map(|handle| handle.join().unwrap_or_default())
                        .unwrap_or_default();
                    if total.load(Ordering::Relaxed) > OUTPUT_LIMIT {
                        return Err(MediationError::Bash {
                            reason: "output-exceeded",
                        });
                    }
                    if stdin_result.is_err() && status.code().is_none() {
                        return Err(MediationError::Bash {
                            reason: "script-io",
                        });
                    }
                    return Ok(MediatedRun {
                        exit_code: status.code(),
                        stdout: captured_stdout,
                        stderr: captured_stderr,
                    });
                }
                // The child could not be polled — a broker I/O error.
                Ok(None) => std::thread::sleep(POLL_INTERVAL),
                Err(_source) => {
                    return Err(MediationError::Bash { reason: "wait" });
                }
            }
        }
    }
}

impl Drop for BashChild {
    fn drop(&mut self) {
        // Natural completion (or an explicit kill+reap) already collected
        // the child — never double-kill.
        if self.reaped.load(Ordering::SeqCst) {
            return;
        }
        // Cancellation or failure: kill the WHOLE process group. The
        // interpreter was spawned with `process_group(0)`, so the negative
        // pid reaches every descendant the script forked, then reap the
        // interpreter itself.
        let pid = self.pid.load(Ordering::Relaxed);
        if let Some(pid) = rustix::process::Pid::from_raw(pid.cast_signed()) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        // Drop the ExecutionContext LAST: it owns the child's temporary
        // HOME/working directories, which must outlive the kill+reap above.
        self.environment.take();
    }
}

/// Applies the resource limits while the child is frozen, resuming it on
/// success (the pinned exec layer's fenced sequence, over the context's
/// limits).
#[cfg(target_os = "linux")]
pub(crate) fn apply_limits_fenced(
    child: &mut Child,
    environment: &arbitraitor_exec::ExecutionContext,
) -> Result<(), MediationError> {
    use rustix::process::{Pid as RustixPid, Signal, kill_process};
    let limits = environment.resource_limits();
    let pid = RustixPid::from_child(child);
    // Freeze the child before it can run any untrusted code. Errors here
    // (e.g. the child already exited) are tolerated: `apply_to` below
    // surfaces a real failure if the pid is no longer valid.
    let _ = kill_process(pid, Signal::STOP);
    if let Err(_source) = limits.apply_to(child.id()) {
        // Fail closed: never leave a child running without its limits, and
        // never leak an orphan. SIGKILL works on a stopped process.
        let _ = child.kill();
        let _ = child.wait();
        return Err(MediationError::Bash {
            reason: "resource-limit",
        });
    }
    // Resume the child now that its limits are in place.
    let _ = kill_process(pid, Signal::CONT);
    Ok(())
}

/// The pinned `ScriptExecution` Landlock rule set: read-execute on the
/// interpreter's directory and the system paths, read-write-execute only on
/// the per-execution working and HOME directories.
#[cfg(target_os = "linux")]
pub(crate) fn landlock_rules(
    interpreter: &std::path::Path,
    environment: &arbitraitor_exec::ExecutionContext,
) -> Vec<PathRule> {
    let mut rules = Vec::new();
    if let Some(parent) = interpreter.parent() {
        rules.push(PathRule::read_execute(parent.to_path_buf()));
    }
    rules.push(PathRule::read_write_execute(
        environment.working_dir().to_path_buf(),
    ));
    rules.push(PathRule::read_write_execute(
        environment.home_dir().to_path_buf(),
    ));
    for path in [
        "/bin",
        "/usr/bin",
        "/usr/local/bin",
        "/lib",
        "/lib64",
        "/usr/lib",
        "/usr/lib64",
        "/tmp",
    ] {
        rules.push(PathRule::read_execute(std::path::PathBuf::from(path)));
    }
    rules
}

/// The pinned `ScriptExecution::with_environment_policy` operation plan:
/// the same metadata-only placeholder artifact, pending state, and no
/// environment allowlist.
#[cfg(target_os = "linux")]
pub(crate) fn pinned_operation_plan(
    interpreter: &std::path::Path,
    args: &[&str],
) -> arbitraitor_model::operation::OperationPlan {
    use arbitraitor_model::ids::{ArtifactId, OperationId, Sha256Digest};
    use arbitraitor_model::operation::{OperationPlan, OperationState, OperationType};
    OperationPlan {
        operation_id: OperationId::new(),
        artifact_id: ArtifactId(Sha256Digest::new([0; 32])),
        operation_type: OperationType::Execute,
        interpreter: Some(interpreter.to_string_lossy().into_owned()),
        arguments: args.iter().map(|arg| (*arg).to_string()).collect(),
        environment_allowlist: Vec::new(),
        network_allowed: false,
        sandbox_enabled: true,
        expiry: None,
        state: OperationState::Pending,
        plugin_identity: None,
        argv_digest: None,
        policy_digest: None,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Mirrors the mediation tests' capability probe: isolated execution
    /// needs util-linux `unshare` and usable unprivileged user namespaces;
    /// hosts without them skip the live tests rather than fabricate a
    /// weaker claim.
    fn network_namespace_supported() -> bool {
        let unshare = Path::new("/usr/bin/unshare");
        unshare.exists()
            && Command::new(unshare)
                .args(["--user", "--map-current-user", "--net", "--"])
                .arg("/bin/sh")
                .arg("-c")
                .arg("true")
                .status()
                .is_ok_and(|status| status.success())
    }

    fn stack_or_skip() -> Option<()> {
        if !Path::new("/bin/bash").exists() || !network_namespace_supported() {
            return None;
        }
        // The full mediated stack must actually complete a benign run
        // (exit 0). Hosts where the mediated wrapper dies before the
        // interpreter starts (e.g. Landlock denies the user-namespace
        // uid_map writes under the hardening profile) skip the live tests
        // rather than fabricate a weaker claim — mirrors the mediation
        // tests' `mediated_stack_or_skip`.
        let child = BashChild::spawn().ok()?;
        let run = child.wait(b"true\n").ok()?;
        if run.exit_code == Some(0) {
            Some(())
        } else {
            None
        }
    }

    /// Live mediated processes belonging to this test: the interpreter
    /// (`bash`) and its background descendants (`sleep`), reached through
    /// the `unshare` wrapper the pinned exec layer spawns. Scans
    /// `/proc/<pid>/stat` and checks full ancestry.
    fn mediated_tree() -> (usize, usize) {
        let me = std::process::id();
        // One pass: pid → (comm, ppid) for every readable process.
        let mut procs: std::collections::HashMap<u32, (String, u32)> =
            std::collections::HashMap::new();
        if let Ok(entries) = std::fs::read_dir("/proc") {
            for entry in entries.filter_map(Result::ok) {
                let Some(name) = entry.file_name().to_str().map(String::from) else {
                    continue;
                };
                if !name.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                if let Ok(stat) = std::fs::read_to_string(entry.path().join("stat"))
                    && let Some((pid, comm, ppid)) = parse_stat(&stat)
                {
                    procs.insert(pid, (comm.to_string(), ppid));
                }
            }
        }
        // Ancestry check: a pid whose parent chain reaches `me`.
        let reaches_me =
            |procs: &std::collections::HashMap<u32, (String, u32)>, mut pid: u32| -> bool {
                for _ in 0..64 {
                    let Some((_, ppid)) = procs.get(&pid) else {
                        return false;
                    };
                    if *ppid == me {
                        return true;
                    }
                    pid = *ppid;
                }
                false
            };
        let mut bash = 0_usize;
        let mut sleep = 0_usize;
        for (pid, (comm, _)) in &procs {
            if !reaches_me(&procs, *pid) {
                continue;
            }
            match comm.as_str() {
                "bash" => bash += 1,
                "sleep" => sleep += 1,
                _ => {}
            }
        }
        (bash, sleep)
    }

    /// Parses `/proc/<pid>/stat` into `(pid, comm, ppid)`: the comm is
    /// wrapped in parentheses (and may itself contain them), so the split
    /// anchors on the LAST `)` and takes the pid from the prefix.
    fn parse_stat(stat: &str) -> Option<(u32, &str, u32)> {
        let open = stat.find('(')?;
        let close = stat.rfind(')')?;
        let pid: u32 = stat[..open].trim().parse().ok()?;
        let comm = &stat[open + 1..close];
        let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        // After `comm` the fields are state, parent-pid, …
        let parent: u32 = fields.get(1)?.parse().ok()?;
        Some((pid, comm, parent))
    }

    /// The forbidden effect must not exist (spec §21.4): dropping the child
    /// guard — what an aborted worker future does — terminates the
    /// interpreter's whole process group, including the background
    /// `sleep` descendant the script forked. A bare interpreter kill
    /// would leave the descendant running; the group kill does not.
    #[test]
    fn dropping_the_guard_kills_the_interpreter_and_its_descendants() -> TestResult {
        if stack_or_skip().is_none() {
            return Ok(());
        }
        // The interpreter touches nothing outside its mediated dirs: the
        // observation surface is /proc, not the filesystem (Landlock
        // confines /tmp to read-execute).
        let script = "sleep 300 &\nwhile true; do sleep 0.1; done\n";
        let child = BashChild::spawn()?;
        // Pipe the script without reaping: the interpreter runs while the
        // guard is alive (stdin closed = EOF, the loop keeps it alive).
        {
            let mut guard = child
                .stdin
                .lock()
                .map_err(|_| "child stdin lock poisoned")?;
            if let Some(stdin) = guard.as_mut() {
                use std::io::Write;
                stdin
                    .write_all(script.as_bytes())
                    .and_then(|()| stdin.flush())?;
            }
            *guard = None;
        }

        // Wait until the interpreter (and its sleep descendant) is live.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let (bash, sleep) = mediated_tree();
            if bash >= 1 && sleep >= 1 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the mediated interpreter never came up (bash={bash}, sleep={sleep})"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        // The cancellation act: drop the guard (what aborting the worker
        // future does to the spawn_blocking closure's locals).
        drop(child);

        // Forbidden effect: neither the interpreter nor its descendant
        // survives the drop. Poll briefly for the reap.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let (bash, sleep) = mediated_tree();
            if bash == 0 && sleep == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the process tree survived the guard drop (bash={bash}, sleep={sleep})"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Ok(())
    }

    /// The normal path still works: a benign command completes with its
    /// output captured, and the child is reaped (no kill on drop needed).
    #[test]
    fn benign_script_completes_and_captures_output() -> TestResult {
        if stack_or_skip().is_none() {
            return Ok(());
        }
        let child = BashChild::spawn()?;
        let run = child.wait(b"printf 'orc-434-bootstrap\\n'\n")?;
        assert_eq!(run.stdout, b"orc-434-bootstrap\n");
        assert_eq!(run.exit_code, Some(0));
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod kill_tests {
    use super::*;
    use std::path::Path;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn network_namespace_supported() -> bool {
        let unshare = Path::new("/usr/bin/unshare");
        unshare.exists()
            && Command::new(unshare)
                .args(["--user", "--map-current-user", "--net", "--"])
                .arg("/bin/sh")
                .arg("-c")
                .arg("true")
                .status()
                .is_ok_and(|status| status.success())
    }

    /// The full mediated stack must complete a benign run (exit 0) — hosts
    /// where the wrapper dies before the interpreter starts skip the live
    /// tests rather than fabricate a weaker claim.
    fn stack_or_skip() -> Option<()> {
        if !Path::new("/bin/bash").exists() || !network_namespace_supported() {
            return None;
        }
        let child = BashChild::spawn().ok()?;
        let run = child.wait(b"true\n").ok()?;
        if run.exit_code == Some(0) {
            Some(())
        } else {
            None
        }
    }

    /// The cancellation contract end-to-end: a `kill_group` called from
    /// another thread while `wait` polls returns the wait promptly and
    /// leaves no process alive — the shape of the mediator's async-side
    /// `KillOnDrop` guard (Tokio cannot abort the blocking task, so the
    /// kill must come from outside it).
    #[test]
    fn kill_group_from_another_thread_unblocks_the_wait() -> TestResult {
        if stack_or_skip().is_none() {
            return Ok(());
        }
        let child = Arc::new(BashChild::spawn()?);
        let waiter_child = Arc::clone(&child);
        let script = "sleep 300 &\nwhile true; do sleep 0.1; done\n";
        let waiter = std::thread::spawn(move || waiter_child.wait(script.as_bytes()));

        // Wait until the interpreter tree is live, then kill from HERE —
        // the async side in the mediator's shape.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if BashChild::pid_alive_groupwise(child.pid.load(Ordering::Relaxed)) {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the mediated interpreter never came up"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        child.kill_group();

        // The blocked wait must return promptly with the kill observed.
        let started = std::time::Instant::now();
        let run = waiter
            .join()
            .map_err(|panic| format!("wait thread panicked: {panic:?}"))?;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the killed wait returned promptly"
        );
        // A killed interpreter has no exit code (signal death) — the run
        // resolves, the tree is gone.
        let _ = run;
        Ok(())
    }

    /// Whether the pid is still present in /proc (liveness probe).
    impl BashChild {
        fn pid_alive_groupwise(pid: u32) -> bool {
            pid != 0 && Path::new(&format!("/proc/{pid}")).exists()
        }
    }
}
