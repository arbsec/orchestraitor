//! Typed errors for the board provider.
//!
//! Error text carries no tokens, headers, cookies, or board-item bodies
//! (spec §9.23.4). Board-derived content is untrusted input (spec §6.1):
//! only short, operator-supplied names and numeric identifiers appear in
//! error messages.

use std::path::PathBuf;

use thiserror::Error;

/// Errors surfaced by the GitHub Projects v2 board provider.
#[derive(Debug, Error)]
pub enum BoardError {
    /// No `.agents/project/github-project.local.toml` was found while walking
    /// up from the given directory.
    #[error(
        "board project config not found; copy .agents/project/github-project.example.toml \
         to .agents/project/github-project.local.toml (searched upward from `{}`)",
        .searched_from.display()
    )]
    ConfigNotFound {
        /// Directory the upward search started from.
        searched_from: PathBuf,
    },
    /// The board project config could not be read from disk.
    #[error("board project config `{}` could not be read", .path.display())]
    ConfigIo {
        /// Config file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The board project config TOML did not parse.
    #[error("board project config `{}` is invalid TOML", .path.display())]
    ConfigParse {
        /// Config file path.
        path: PathBuf,
        /// Underlying TOML parse error.
        #[source]
        source: Box<toml::de::Error>,
    },
    /// The board project config parsed but is missing required values.
    #[error("board project config `{}` is invalid: {reason}", .path.display())]
    ConfigInvalid {
        /// Config file path.
        path: PathBuf,
        /// Human-readable validation failure.
        reason: String,
    },
    /// The `[auth].token` secret URI is absent from the board config.
    #[error(
        "board auth is not configured; set `[auth].token` to a `secret://env/<VAR>` URI in \
         .agents/project/github-project.local.toml"
    )]
    AuthNotConfigured,
    /// The `[auth].token` value is not a supported secret URI.
    #[error("board auth token is not a supported secret URI (expected `secret://env/<VAR>`)")]
    AuthSecretUri,
    /// The configured environment variable did not resolve to a token.
    #[error("board auth environment variable `{var}` is unavailable or empty")]
    AuthEnvVar {
        /// Environment variable name, never its value.
        var: String,
    },
    /// Keyring resolution is not available in the bootstrap auth stub.
    #[error(
        "board auth keyring resolution is unavailable in the bootstrap provider; \
         use `secret://env/<VAR>` until the GitHub App identity module lands"
    )]
    AuthKeyringUnavailable,
    /// The resolved token cannot be encoded as an HTTP header value.
    #[error("board auth token is not a valid HTTP header value")]
    AuthTokenInvalid,
    /// The HTTP client could not be constructed.
    #[error("GitHub GraphQL HTTP client construction failed")]
    HttpClient(#[source] Box<reqwest::Error>),
    /// The GraphQL request failed at the transport level.
    #[error("GitHub GraphQL request failed")]
    Transport(#[source] Box<reqwest::Error>),
    /// The GraphQL endpoint returned a non-success HTTP status.
    #[error("GitHub GraphQL endpoint returned HTTP status {status}")]
    HttpStatus {
        /// Observed HTTP status code.
        status: u16,
    },
    /// The GraphQL response body exceeded the byte limit.
    #[error("GitHub GraphQL response exceeds the {limit} byte limit")]
    ResponseTooLarge {
        /// Configured byte limit.
        limit: u64,
    },
    /// The GraphQL response reported top-level errors.
    #[error("GitHub GraphQL errors: {}", .messages.join("; "))]
    GraphQl {
        /// Error messages reported by the GraphQL response.
        messages: Vec<String>,
    },
    /// The GraphQL response JSON did not match the expected shape.
    #[error("GitHub GraphQL response has an unexpected shape during {context}")]
    ResponseShape {
        /// Which resolution or query step was decoding.
        context: &'static str,
        /// Underlying JSON decoding error.
        #[source]
        source: Box<serde_json::Error>,
    },
    /// The organization project did not resolve to a node id.
    #[error("GitHub project {number} not found in organization `{org}`")]
    ProjectNotFound {
        /// Organization login from the board config.
        org: String,
        /// Project number from the board config.
        number: u64,
    },
    /// A required single-select field name was not resolved on the project.
    #[error("project field `{name}` was not found on the shared board")]
    FieldNotFound {
        /// Human-readable field name from the board config.
        name: String,
    },
    /// A required field option name was not resolved on the field.
    #[error("option `{option}` was not found on project field `{field}`")]
    OptionNotFound {
        /// Human-readable field name.
        field: String,
        /// Human-readable option name.
        option: String,
    },
    /// The issue does not exist in any configured repository.
    #[error("issue #{number} was not found in the configured board repositories")]
    IssueNotFound {
        /// Issue number requested on the command line.
        number: u64,
    },
    /// The issue exists but is not a member of the configured project.
    #[error("issue #{number} is not an item on the configured board")]
    ItemNotOnBoard {
        /// Issue number requested on the command line.
        number: u64,
    },
    /// The read-back Status value did not match the requested option.
    #[error(
        "status write verification failed: read-back field `{field}` does not equal `{expected}`"
    )]
    StatusVerification {
        /// Human-readable field name that was read back.
        field: String,
        /// Status name the write was supposed to set.
        expected: String,
    },
    /// Node-id cache I/O failed.
    #[error("board node-id cache I/O failed at `{}`", .path.display())]
    CacheIo {
        /// Cache file path.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl From<reqwest::Error> for BoardError {
    fn from(source: reqwest::Error) -> Self {
        Self::Transport(Box::new(source))
    }
}

impl From<orchestraitor_core::ConfigError> for BoardError {
    fn from(_source: orchestraitor_core::ConfigError) -> Self {
        Self::AuthSecretUri
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_render_without_secret_material() {
        let token = "gho_secret-test-token";
        let rendered = format!(
            "{}\n{}\n{}",
            BoardError::AuthEnvVar {
                var: "BOARD_TOKEN".to_string(),
            },
            BoardError::HttpStatus { status: 401 },
            BoardError::GraphQl {
                messages: vec!["Bad credentials".to_string()],
            },
        );
        assert!(!rendered.contains(token));
        assert!(!rendered.contains("bearer"));
        assert!(!rendered.contains("Authorization"));
    }
}
