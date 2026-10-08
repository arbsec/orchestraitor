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

/// Default executor: spawns real processes with the given wall-clock cap.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessExecutor;

impl SimplifyExecutor for ProcessExecutor {
    fn run(&self, spec: &ToolSpec, dir: &Path, timeout: Duration) -> ToolOutcome {
        let mut command = spec.command(dir);
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        command.stdin(std::process::Stdio::null());
        let Ok(mut child) = command.spawn() else {
            return ToolOutcome::Unavailable(SimplifyError::ToolUnavailable {
                tool: spec.label,
                reason: "spawn",
            });
        };
        // Bounded wait: a hung tool yields a typed status, never a hang.
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let (stdout, stderr) = match (child.stdout.take(), child.stderr.take()) {
                        (Some(mut out), Some(mut err)) => {
                            let mut stdout = Vec::new();
                            let mut stderr = Vec::new();
                            // The child has exited; pipes are closed, so
                            // read_to_end returns promptly.
                            let _ = std::io::Read::read_to_end(&mut out, &mut stdout);
                            let _ = std::io::Read::read_to_end(&mut err, &mut stderr);
                            (stdout, stderr)
                        }
                        _ => (Vec::new(), Vec::new()),
                    };
                    return ToolOutcome::Ran(ExecOutput {
                        code: status.code(),
                        stdout: String::from_utf8_lossy(&stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    });
                }
                Ok(None) => {
                    if std::time::Instant::now() >= deadline {
                        let _ignore = child.kill();
                        let _ignore = child.wait();
                        return ToolOutcome::Unavailable(SimplifyError::ToolUnavailable {
                            tool: spec.label,
                            reason: "timeout",
                        });
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => {
                    return ToolOutcome::Unavailable(SimplifyError::ToolUnavailable {
                        tool: spec.label,
                        reason: "spawn",
                    });
                }
            }
        }
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
