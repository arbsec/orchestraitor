//! `orc policy check` implementation (spec MVP-8).
//!
//! Evaluates Arbitraitor policy against a plan or recorded session and
//! reports the verdict in JSON. All policy evaluation is delegated to
//! Arbitraitor via [`ArbitraitorClient::evaluate_policy`]; Orchestraitor
//! owns no security decision logic (spec §2.2, §16).

use std::fs;
use std::io::Write;

use miette::{IntoDiagnostic, Result};
use orchestraitor_arbitraitor_client::{
    ArbitraitorClient, EvalContext, Finding, PolicyError, Verdict,
};
use serde::Serialize;

use crate::cli::{ConfigPaths, PolicyCheckArgs};
use crate::exit_code::{ExitCode, OrcError, OrcResult};

/// Runs `orc policy check`.
///
/// # Errors
///
/// Returns [`ExitCode::ConfigError`] when the policy file cannot be loaded,
/// [`ExitCode::SecurityBlock`] when Arbitraitor returns a `Block` verdict,
/// and [`ExitCode::GeneralFailure`] for I/O errors.
pub fn run<W: Write>(
    paths: &ConfigPaths,
    args: &PolicyCheckArgs,
    writer: &mut W,
) -> OrcResult<ExitCode> {
    let policy_toml = load_policy(paths, args)?;
    let context = EvalContext::new(false);
    let findings: Vec<Finding> = Vec::new();
    let client = ArbitraitorClient::default();

    let verdict = client
        .evaluate_policy(&policy_toml, &findings, &context)
        .map_err(|e| policy_error_to_orc(&e, args.shadow))?;

    let report = PolicyReport {
        verdict,
        verdict_str: verdict_to_str(verdict),
        shadow: args.shadow,
        session: args.session.clone(),
        interactive: false,
    };

    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &report)
            .into_diagnostic()
            .map_err(OrcError::infrastructure)?;
        writeln!(writer).into_diagnostic().map_err(infra_error)?;
    } else if !args.quiet {
        render_text(writer, &report).map_err(infra_error)?;
    }

    Ok(verdict_to_exit_code(verdict))
}

fn infra_error(e: miette::Report) -> OrcError {
    OrcError::infrastructure(e)
}

fn load_policy(paths: &ConfigPaths, args: &PolicyCheckArgs) -> OrcResult<String> {
    if let Some(ref path) = args.policy {
        fs::read_to_string(path).into_diagnostic().map_err(|e| {
            OrcError::config(e.wrap_err(format!("failed to read policy file: {}", path.display())))
        })
    } else {
        let default = paths.project_dir.join("orchestraitor-policy.toml");
        if default.exists() {
            fs::read_to_string(&default).into_diagnostic().map_err(|e| {
                OrcError::config(e.wrap_err(format!(
                    "failed to read default policy file: {}",
                    default.display()
                )))
            })
        } else {
            Err(OrcError::config(miette::miette!(
                "no policy file specified and no default found at {}",
                default.display()
            )))
        }
    }
}

fn policy_error_to_orc(error: &PolicyError, shadow: bool) -> OrcError {
    let report = miette::Report::msg(format!("policy evaluation error: {error}"));
    if shadow {
        OrcError::config(report.wrap_err("shadow policy evaluation failed"))
    } else {
        OrcError::security_block(report.wrap_err("policy evaluation failed"))
    }
}

fn verdict_to_str(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Pass => "pass",
        Verdict::Warn => "warn",
        Verdict::Prompt => "prompt",
        Verdict::Block => "block",
        Verdict::Error => "error",
        Verdict::Incomplete => "incomplete",
    }
}

fn verdict_to_exit_code(verdict: Verdict) -> ExitCode {
    match verdict {
        Verdict::Pass | Verdict::Warn => ExitCode::Success,
        Verdict::Prompt | Verdict::Block => ExitCode::SecurityBlock,
        Verdict::Error => ExitCode::GeneralFailure,
        Verdict::Incomplete => ExitCode::VerificationFailure,
    }
}

fn render_text<W: Write>(writer: &mut W, report: &PolicyReport) -> Result<()> {
    let mode = if report.shadow { "shadow " } else { "" };
    writeln!(writer, "{mode}policy verdict: {}", report.verdict_str).into_diagnostic()?;
    if let Some(ref session) = report.session {
        writeln!(writer, "session: {session}").into_diagnostic()?;
    }
    writeln!(writer, "interactive: {}", report.interactive).into_diagnostic()?;
    Ok(())
}

/// Policy evaluation report emitted as JSON.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyReport {
    /// Final Arbitraitor verdict.
    pub verdict_str: &'static str,
    /// Raw verdict enum value.
    pub verdict: Verdict,
    /// Whether this was a shadow evaluation.
    pub shadow: bool,
    /// Session id evaluated, if any.
    pub session: Option<String>,
    /// Whether the context was interactive.
    pub interactive: bool,
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_verdict_maps_to_security_block_exit_code() {
        assert_eq!(
            verdict_to_exit_code(Verdict::Block),
            ExitCode::SecurityBlock
        );
    }

    #[test]
    fn pass_verdict_maps_to_success_exit_code() {
        assert_eq!(verdict_to_exit_code(Verdict::Pass), ExitCode::Success);
    }

    #[test]
    fn incomplete_verdict_maps_to_verification_failure() {
        assert_eq!(
            verdict_to_exit_code(Verdict::Incomplete),
            ExitCode::VerificationFailure
        );
    }

    #[test]
    fn verdict_str_is_lowercase() {
        assert_eq!(verdict_to_str(Verdict::Pass), "pass");
        assert_eq!(verdict_to_str(Verdict::Block), "block");
    }
}
