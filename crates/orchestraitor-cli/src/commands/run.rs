//! `orc run` implementation (spec MVP-8).
//!
//! Executes a task, optionally without TUI interaction. In non-interactive
//! mode, approvals follow the configured non-interactive policy (default:
//! block). The full agent loop is not yet wired; this command sets up the
//! execution context, reports the approval policy, and fails closed when
//! the required capabilities are unavailable.

use std::io::Write;

use miette::{IntoDiagnostic, Result};
use serde::Serialize;

use crate::cli::{ConfigPaths, NonInteractiveApprovalMode, RunArgs};
use crate::exit_code::{ExitCode, OrcError, OrcResult};

/// Runs `orc run`.
///
/// # Errors
///
/// Returns [`ExitCode::SecurityBlock`] when a non-interactive run requires
/// approval and the policy is set to block (the default).
pub fn run<W: Write>(paths: &ConfigPaths, args: &RunArgs, writer: &mut W) -> OrcResult<ExitCode> {
    let task = args.task.clone().unwrap_or_default();
    let approval_mode = if args.non_interactive {
        args.approval
    } else {
        NonInteractiveApprovalMode::Block
    };

    let report = RunReport {
        task: task.clone(),
        non_interactive: args.non_interactive,
        approval_mode,
        project_dir: paths.project_dir.display().to_string(),
        agent_loop_available: false,
    };

    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &report)
            .into_diagnostic()
            .map_err(OrcError::infrastructure)?;
        writeln!(writer).into_diagnostic().map_err(infra_error)?;
    } else if !args.quiet {
        render_text(writer, &report).map_err(infra_error)?;
    }

    if args.non_interactive && approval_mode == NonInteractiveApprovalMode::Block {
        return Err(OrcError::security_block(miette::miette!(
            "non-interactive run blocked: approval policy is 'block' (default). \
             Use --approval allow to permit non-interactive approvals, or run interactively."
        )));
    }

    if !report.agent_loop_available {
        return Err(OrcError::infrastructure(miette::miette!(
            "agent loop is not yet available. The native orchestration runtime is required \
             to execute tasks (spec MVP-7, §9.24)."
        )));
    }

    Ok(ExitCode::Success)
}

fn infra_error(e: miette::Report) -> OrcError {
    OrcError::infrastructure(e)
}

fn render_text<W: Write>(writer: &mut W, report: &RunReport) -> Result<()> {
    let mode = if report.non_interactive {
        "non-interactive"
    } else {
        "interactive"
    };
    let approval = match report.approval_mode {
        NonInteractiveApprovalMode::Block => "block",
        NonInteractiveApprovalMode::Allow => "allow",
    };
    writeln!(
        writer,
        "Task: {}",
        if report.task.is_empty() {
            "(none specified)"
        } else {
            &report.task
        }
    )
    .into_diagnostic()?;
    writeln!(writer, "Mode: {mode}").into_diagnostic()?;
    writeln!(writer, "Approval policy: {approval}").into_diagnostic()?;
    writeln!(writer, "Project: {}", report.project_dir).into_diagnostic()?;
    if !report.agent_loop_available {
        writeln!(writer, "Agent loop: not available").into_diagnostic()?;
    }
    Ok(())
}

/// JSON report for `orc run`.
#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    /// Task description or prompt.
    pub task: String,
    /// Whether the run is non-interactive.
    pub non_interactive: bool,
    /// Approval policy for non-interactive mode.
    pub approval_mode: NonInteractiveApprovalMode,
    /// Project directory.
    pub project_dir: String,
    /// Whether the native agent loop is available.
    pub agent_loop_available: bool,
}

impl Serialize for NonInteractiveApprovalMode {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(match self {
            Self::Block => "block",
            Self::Allow => "allow",
        })
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn paths() -> ConfigPaths {
        let temp = TempDir::new().unwrap();
        ConfigPaths {
            config_dir: temp.path().join(".orchestraitor"),
            project_dir: temp.path().to_path_buf(),
            models_dev_endpoint: None,
        }
    }

    #[test]
    fn non_interactive_block_returns_security_block() {
        let p = paths();
        let args = RunArgs {
            task: Some("do something".to_string()),
            non_interactive: true,
            approval: NonInteractiveApprovalMode::Block,
            json: false,
            quiet: true,
        };
        let mut output = Vec::new();
        let result = run(&p, &args, &mut output);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.exit_code(), ExitCode::SecurityBlock);
    }

    #[test]
    fn non_interactive_allow_returns_infrastructure_failure() {
        let p = paths();
        let args = RunArgs {
            task: Some("do something".to_string()),
            non_interactive: true,
            approval: NonInteractiveApprovalMode::Allow,
            json: false,
            quiet: true,
        };
        let mut output = Vec::new();
        let result = run(&p, &args, &mut output);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.exit_code(), ExitCode::InfrastructureFailure);
    }
}
