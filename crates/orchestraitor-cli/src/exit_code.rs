//! Stable exit codes for machine-oriented `orc` commands (spec MVP-8).
//!
//! Exit codes distinguish security blocks, verification failures,
//! configuration errors, and infrastructure failures. `0` always indicates
//! success; any non-zero value indicates failure.

#![forbid(unsafe_code)]

use std::fmt;

use miette::Diagnostic;

/// Stable exit codes for `orc` commands (spec MVP-8).
///
/// These codes are part of the stable CLI contract. CI pipelines and
/// automation may branch on them. Adding new variants is allowed; changing
/// the numeric value of an existing variant is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    /// Command completed successfully.
    Success = 0,
    /// Unclassified failure that does not fit another category.
    GeneralFailure = 1,
    /// Configuration parsing, resolution, or validation failed.
    ConfigError = 2,
    /// One or more project-configured verification commands failed.
    VerificationFailure = 3,
    /// Arbitraitor policy denied or blocked the operation.
    SecurityBlock = 4,
    /// Daemon, transport, or storage infrastructure unavailable.
    InfrastructureFailure = 5,
}

impl ExitCode {
    /// Returns the numeric exit code suitable for `std::process::exit`.
    #[must_use]
    pub const fn as_i32(self) -> i32 {
        self as i32
    }
}

impl From<ExitCode> for std::process::ExitCode {
    fn from(code: ExitCode) -> Self {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "all variants are non-negative and fit in u8"
        )]
        let value = code.as_i32() as u8;
        std::process::ExitCode::from(value)
    }
}

impl fmt::Display for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_i32())
    }
}

/// Error carrying both a diagnostic report and a stable exit code.
///
/// The diagnostic is rendered for humans; the exit code is consumed by
/// `main()` and CI automation. This type is the single error type returned
/// by all `orc` command implementations.
#[derive(Debug)]
pub struct OrcError {
    report: miette::Report,
    exit_code: ExitCode,
}

impl OrcError {
    /// Creates a new error from a diagnostic and an exit code.
    #[must_use]
    pub fn new(report: miette::Report, exit_code: ExitCode) -> Self {
        Self { report, exit_code }
    }

    /// Creates a configuration error (exit code 2).
    #[must_use]
    pub fn config(report: miette::Report) -> Self {
        Self::new(report, ExitCode::ConfigError)
    }

    /// Creates a verification failure (exit code 3).
    #[must_use]
    pub fn verification(report: miette::Report) -> Self {
        Self::new(report, ExitCode::VerificationFailure)
    }

    /// Creates a security block (exit code 4).
    #[must_use]
    pub fn security_block(report: miette::Report) -> Self {
        Self::new(report, ExitCode::SecurityBlock)
    }

    /// Creates an infrastructure failure (exit code 5).
    #[must_use]
    pub fn infrastructure(report: miette::Report) -> Self {
        Self::new(report, ExitCode::InfrastructureFailure)
    }

    /// Returns the stable exit code for this error.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        self.exit_code
    }
}

impl fmt::Display for OrcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.report, f)
    }
}

impl std::error::Error for OrcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.report.source()
    }
}

impl Diagnostic for OrcError {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.report.code()
    }

    fn severity(&self) -> Option<miette::Severity> {
        self.report.severity()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.report.help()
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.report.url()
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.report.source_code()
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        self.report.labels()
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn Diagnostic> + 'a>> {
        self.report.related()
    }

    fn diagnostic_source(&self) -> Option<&dyn Diagnostic> {
        self.report.diagnostic_source()
    }
}

impl From<miette::Report> for OrcError {
    fn from(report: miette::Report) -> Self {
        Self::new(report, ExitCode::GeneralFailure)
    }
}

/// Result alias for `orc` command implementations.
pub type OrcResult<T> = Result<T, OrcError>;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn exit_codes_are_stable_and_documented() {
        assert_eq!(ExitCode::Success.as_i32(), 0);
        assert_eq!(ExitCode::GeneralFailure.as_i32(), 1);
        assert_eq!(ExitCode::ConfigError.as_i32(), 2);
        assert_eq!(ExitCode::VerificationFailure.as_i32(), 3);
        assert_eq!(ExitCode::SecurityBlock.as_i32(), 4);
        assert_eq!(ExitCode::InfrastructureFailure.as_i32(), 5);
    }

    #[test]
    fn orc_error_carries_exit_code() {
        let error = OrcError::config(miette::miette!("bad config"));
        assert_eq!(error.exit_code(), ExitCode::ConfigError);
    }

    #[test]
    fn orc_error_converts_to_report_for_test_compat() {
        let error = OrcError::security_block(miette::miette!("blocked"));
        let _report: miette::Report = error.into();
    }
}
