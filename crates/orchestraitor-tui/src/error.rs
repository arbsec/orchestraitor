//! Error types for the TUI crate.

use thiserror::Error;

/// Errors produced by the Orchestraitor TUI.
#[derive(Debug, Error)]
pub enum TuiError {
    /// Terminal initialization or teardown failed.
    #[error("terminal I/O error: {0}")]
    TerminalIo(String),
    /// A crossterm event stream error occurred.
    #[error("event stream error: {0}")]
    EventStream(String),
    /// The user requested to quit.
    #[error("user requested quit")]
    UserQuit,
}

/// Convenience alias for `Result<T, TuiError>`.
pub type TuiResult<T> = Result<T, TuiError>;
