//! Process-execution seam (I/O-injectable, matching the delivery modules'
//! executor pattern).

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::report::ToolStatus;

/// Errors surfaced by the simplify pass.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SimplifyError {
    /// Configuration failed construction-time validation.
    #[error("invalid simplify configuration: {detail}")]
    InvalidConfig {
        /// Static, log-safe detail (never tool output).
        detail: String,
    },
    /// A tool invocation failed to spawn or timed out (fail-open: the pass
    /// records the status and continues).
    #[error("simplify tool `{tool}` could not run: {reason}")]
    ToolUnavailable {
        /// Static tool label.
        tool: &'static str,
        /// Static failure classification (`spawn`, `timeout`, `utf8`).
        reason: &'static str,
    },
}

/// One declared tool invocation: the program plus its arguments.
///
/// The pass declares argv-shaped invocations only — no shell interpretation,
/// no string interpolation of paths into arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    /// Executable name (resolved on `PATH` by the executor).
    pub program: &'static str,
    /// Argument list.
    pub args: &'static [&'static str],
    /// Static label used in reports and reason codes.
    pub label: &'static str,
}

impl ToolSpec {
    /// A tool invocation rooted at `dir`.
    #[must_use]
    pub fn command(&self, dir: &Path) -> std::process::Command {
        let mut command = std::process::Command::new(self.program);
        command
            .current_dir(dir)
            .args(self.args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        command
    }
}

/// Captured tool output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    /// Process exit code, when the child exited normally.
    pub code: Option<i32>,
    /// Captured stdout (raw; parsing modules decode).
    pub stdout: String,
    /// Captured stderr (raw; parsing modules decode).
    pub stderr: String,
}

impl ExecOutput {
    /// Whether the tool exited successfully.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// The outcome of one declared tool: either captured output or a typed
/// unavailability. Fail-open by construction — the caller records the
/// status and keeps going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutcome {
    /// The tool ran; output captured.
    Ran(ExecOutput),
    /// The tool could not run (absent, spawn failure, timeout).
    Unavailable(SimplifyError),
}

/// Injectable process executor. Tests script outcomes; production spawns
/// real processes with a wall-clock cap.
pub trait SimplifyExecutor: Send + Sync {
    /// Runs one declared tool under `dir`, capturing its output.
    fn run(&self, spec: &ToolSpec, dir: &Path, timeout: Duration) -> ToolOutcome;

    /// Whether an executable name can be resolved on `PATH` (used to skip
    /// tools that are not installed without attempting them).
    fn available(&self, program: &str) -> bool;
}

/// Reads a pipe to end on a reader thread (the buffer is joined after the
/// child exits).
fn read_all<R: std::io::Read + Send + 'static>(mut pipe: R) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = std::io::Read::read_to_end(&mut pipe, &mut buffer);
    buffer
}

/// Default executor: spawns real processes with the given wall-clock cap.
///
/// On unix, each child leads its own process group
/// ([`std::os::unix::process::CommandExt::process_group`]) and a timeout
/// kills that WHOLE group: `cargo` descendants (`rustc`, `clippy-driver`)
/// would otherwise survive a bare child kill, keep the pipes open, and
/// leave the drain threads blocked past the deadline. Non-unix platforms
/// fall back to the child-only kill (their toolchains rarely fork past
/// `cargo` in this repository's usage, and the wall-clock cap still
/// returns a typed status).
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessExecutor;

impl SimplifyExecutor for ProcessExecutor {
    fn run(&self, spec: &ToolSpec, dir: &Path, timeout: Duration) -> ToolOutcome {
        let mut command = spec.command(dir);
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0000_0200); // CREATE_NEW_PROCESS_GROUP
        }
        let Ok(mut child) = command.spawn() else {
            return ToolOutcome::Unavailable(SimplifyError::ToolUnavailable {
                tool: spec.label,
                reason: "spawn",
            });
        };
        // Drain both pipes on reader threads from the start: a child that
        // fills a pipe buffer must never block on a parent that is only
        // polling (a deadlock would surface as a spurious timeout).
        let stdout_handle = child
            .stdout
            .take()
            .map(|pipe| std::thread::spawn(move || read_all(pipe)));
        let stderr_handle = child
            .stderr
            .take()
            .map(|pipe| std::thread::spawn(move || read_all(pipe)));
        let read_pipe = |handle: Option<std::thread::JoinHandle<Vec<u8>>>| -> Vec<u8> {
            match handle {
                Some(handle) => handle.join().unwrap_or_else(|_| Vec::new()),
                None => Vec::new(),
            }
        };
        // Bounded wait: a hung tool yields a typed status, never a hang.
        let deadline = std::time::Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break None,
            }
        };
        let Some(status) = status else {
            terminate_tree(&mut child);
            let _ignore = child.wait();
            // Join the drain threads: group death closes every pipe end, so
            // the readers finish promptly and their buffers are reclaimed.
            let _stdout = read_pipe(stdout_handle);
            let _stderr = read_pipe(stderr_handle);
            return ToolOutcome::Unavailable(SimplifyError::ToolUnavailable {
                tool: spec.label,
                reason: "timeout",
            });
        };
        // The child has exited; the pipes are closed and the readers finish
        // promptly. join() reclaims both buffers before we render output.
        let stdout = read_pipe(stdout_handle);
        let stderr = read_pipe(stderr_handle);
        ToolOutcome::Ran(ExecOutput {
            code: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    fn available(&self, program: &str) -> bool {
        // Probe via `--version`-free PATH resolution: spawn would run the
        // tool; instead check each PATH entry for an executable file.
        let Some(path_var) = std::env::var_os("PATH") else {
            return false;
        };
        std::env::split_paths(&path_var).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file()
        })
    }
}

/// Rendered static reason code for an unavailable tool (report-facing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnavailableReason {
    /// Static tool label.
    pub tool: String,
    /// Static failure classification (`spawn`, `timeout`, `utf8`).
    pub reason: String,
}

impl ToolStatus {
    /// Builds a `Ran`/`Unavailable` status from a [`ToolOutcome`].
    #[must_use]
    pub fn from_outcome(label: &'static str, outcome: &ToolOutcome) -> Self {
        match outcome {
            ToolOutcome::Ran(output) => Self::Ran {
                tool: label.to_string(),
                exit_code: output.code,
            },
            ToolOutcome::Unavailable(error) => {
                let reason = match error {
                    SimplifyError::ToolUnavailable { reason, .. } => (*reason).to_string(),
                    SimplifyError::InvalidConfig { .. } => "config".to_string(),
                };
                Self::Unavailable {
                    tool: label.to_string(),
                    reason,
                }
            }
        }
    }
}

/// Stops the tool and its descendants before joining their inherited pipes.
fn terminate_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = rustix::process::Pid::from_raw(child.id().cast_signed()) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    #[cfg(windows)]
    {
        // taskkill /T includes descendants; /F also handles console tools.
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    // Also reap the direct child if group cleanup found it already exiting.
    let _ = child.kill();
}
