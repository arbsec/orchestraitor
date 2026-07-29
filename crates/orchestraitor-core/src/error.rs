//! Structured error taxonomy for Orchestraitor core.

use std::error::Error as StdError;

use orchestraitor_model::error_codes::ErrorComponent;
use thiserror::Error;

/// Retry classification for structured errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retryability {
    /// The operation may be retried without user action.
    Retriable,
    /// Retrying cannot succeed without a state change.
    NotRetriable,
    /// The user must take an explicit action before retrying.
    NeedsUserAction,
}

/// Stable structured error metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuredError {
    /// Stable error code.
    pub code: String,
    /// Human-readable top-level cause.
    pub cause: String,
    /// Underlying source chain, from nearest source outward.
    pub source_chain: Vec<String>,
    /// Component that produced the error.
    pub component: ErrorComponent,
    /// Retry classification.
    pub retryability: Retryability,
    /// Concrete next step for the user or caller.
    pub suggested_action: String,
    /// Relevant configuration key, if any.
    pub relevant_config: Option<String>,
    /// Optional trace or correlation reference.
    pub trace_reference: Option<String>,
}

/// Configuration-layer failures.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// TOML parsing failed.
    #[error("configuration TOML is invalid")]
    Toml(#[source] Box<toml::de::Error>),
    /// TOML serialization failed.
    #[error("configuration TOML serialization failed")]
    TomlSerialize(#[source] Box<toml::ser::Error>),
    /// Format-preserving TOML parsing failed.
    #[error("configuration document is invalid")]
    TomlEdit(#[source] Box<toml_edit::TomlError>),
    /// Figment provider extraction failed.
    #[error("layered configuration provider failed")]
    Figment(#[source] Box<figment::Error>),
    /// Two same-precedence layers set the same key ambiguously.
    #[error("ambiguous configuration conflict for key `{key}` from sources {sources:?}")]
    AmbiguousConflict {
        /// Dotted config key with the conflict.
        key: String,
        /// Same-precedence sources that set the key.
        sources: Vec<String>,
    },
    /// Secret URI syntax is invalid.
    #[error("invalid secret URI shape")]
    SecretUri,
}

/// Tracing initialization failures.
#[derive(Debug, Error)]
pub enum TracingError {
    /// Environment filter syntax was invalid.
    #[error("tracing environment filter is invalid")]
    EnvFilter(#[source] Box<tracing_subscriber::filter::ParseError>),
    /// Global tracing subscriber was already initialized.
    #[error("tracing subscriber is already initialized")]
    AlreadyInitialized(#[source] Box<tracing::subscriber::SetGlobalDefaultError>),
}

/// Top-level library error taxonomy for Orchestraitor.
#[derive(Debug, Error)]
pub enum OrchestraitorError {
    /// Configuration parsing, validation, or resolution failed.
    #[error("configuration error: {0}")]
    Config(#[source] Box<ConfigError>),
    /// Authentication setup failed before any credential value was exposed.
    #[error("authentication configuration error")]
    Auth,
    /// Arbitraitor policy integration rejected or lacked a required capability.
    #[error("policy integration error")]
    Policy,
    /// Provider configuration or adapter selection failed.
    #[error("provider configuration error")]
    Provider,
    /// Tracing initialization failed.
    #[error("tracing initialization error: {0}")]
    Tracing(#[source] Box<TracingError>),
    /// Internal invariant failed without exposing sensitive data.
    #[error("internal orchestraitor error")]
    Internal,
}

impl From<ConfigError> for OrchestraitorError {
    fn from(error: ConfigError) -> Self {
        Self::Config(Box::new(error))
    }
}

impl From<TracingError> for OrchestraitorError {
    fn from(error: TracingError) -> Self {
        Self::Tracing(Box::new(error))
    }
}

impl OrchestraitorError {
    /// Returns stable structured metadata for this error.
    #[must_use]
    pub fn structured(&self) -> StructuredError {
        let mut structured = match self {
            Self::Config(error) => config_structured(error),
            Self::Auth => StructuredError {
                code: ErrorComponent::Provider.code(1),
                cause: String::new(),
                source_chain: Vec::new(),
                component: ErrorComponent::Provider,
                retryability: Retryability::NeedsUserAction,
                suggested_action: "Review provider authentication references".to_string(),
                relevant_config: Some("providers".to_string()),
                trace_reference: None,
            },
            Self::Policy => StructuredError {
                code: ErrorComponent::Daemon.code(1),
                cause: String::new(),
                source_chain: Vec::new(),
                component: ErrorComponent::Daemon,
                retryability: Retryability::NeedsUserAction,
                suggested_action: "Check Arbitraitor capability reports".to_string(),
                relevant_config: None,
                trace_reference: None,
            },
            Self::Provider => StructuredError {
                code: ErrorComponent::Provider.code(2),
                cause: String::new(),
                source_chain: Vec::new(),
                component: ErrorComponent::Provider,
                retryability: Retryability::NotRetriable,
                suggested_action: "Inspect resolved provider configuration".to_string(),
                relevant_config: Some("providers".to_string()),
                trace_reference: None,
            },
            Self::Tracing(_) => StructuredError {
                code: ErrorComponent::Daemon.code(2),
                cause: String::new(),
                source_chain: Vec::new(),
                component: ErrorComponent::Daemon,
                retryability: Retryability::NeedsUserAction,
                suggested_action: "Review tracing filter configuration".to_string(),
                relevant_config: None,
                trace_reference: None,
            },
            Self::Internal => StructuredError {
                code: ErrorComponent::Daemon.code(500),
                cause: String::new(),
                source_chain: Vec::new(),
                component: ErrorComponent::Daemon,
                retryability: Retryability::NotRetriable,
                suggested_action: "Run `orc bug-report` with the correlation id".to_string(),
                relevant_config: None,
                trace_reference: None,
            },
        };
        structured.cause = self.to_string();
        structured.source_chain = source_chain(self);
        structured
    }
}

fn source_chain(error: &(dyn StdError + 'static)) -> Vec<String> {
    let mut chain = Vec::new();
    let mut current = error.source();
    while let Some(source) = current {
        chain.push(source.to_string());
        current = source.source();
    }
    chain
}

fn config_structured(error: &ConfigError) -> StructuredError {
    let relevant_config = match error {
        ConfigError::AmbiguousConflict { key, sources: _ } => Some(key.clone()),
        ConfigError::Toml(_)
        | ConfigError::TomlSerialize(_)
        | ConfigError::TomlEdit(_)
        | ConfigError::Figment(_)
        | ConfigError::SecretUri => None,
    };
    StructuredError {
        code: ErrorComponent::Config.code(1),
        cause: String::new(),
        source_chain: Vec::new(),
        component: ErrorComponent::Config,
        retryability: Retryability::NeedsUserAction,
        suggested_action: "Run `orc config validate` and edit the reported key".to_string(),
        relevant_config,
        trace_reference: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_do_not_render_secret_values() {
        let error = OrchestraitorError::Auth;
        let rendered = error.to_string();
        assert!(!rendered.contains("token"));
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("authorization"));
    }

    #[test]
    fn structured_error_includes_cause_and_source_chain() {
        let error = OrchestraitorError::from(ConfigError::AmbiguousConflict {
            key: String::from("providers.zai.protocol"),
            sources: vec![String::from("project"), String::from("cli")],
        });

        let structured = error.structured();

        assert_eq!(
            structured.cause,
            "configuration error: ambiguous configuration conflict for key `providers.zai.protocol` from sources [\"project\", \"cli\"]"
        );
        assert_eq!(
            structured.source_chain,
            vec![
                "ambiguous configuration conflict for key `providers.zai.protocol` from sources [\"project\", \"cli\"]"
            ]
        );
    }
}
