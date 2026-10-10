//! Structured error taxonomy for Orchestraitor core.

use std::error::Error as StdError;
use std::fmt;

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

/// Secret resolution failures (spec `40-arbitraitor-integration.md` §9.23).
///
/// No variant ever carries the resolved secret value; identifiers such as
/// environment-variable names and keyring ids are configuration references,
/// not secret material. `Debug` is hand-written below because deriving it
/// would forward to keyring-core's derived `Debug`, whose payload variants
/// (`BadEncoding(Vec<u8>)`, `BadDataFormat(Vec<u8>, ..)`) embed the raw
/// retrieved secret bytes (keyring-core 1.0.0).
#[derive(Error)]
pub enum SecretResolveError {
    /// The environment variable holding the secret is not set or is not valid
    /// Unicode; the underlying value is never inspected or reported.
    #[error("environment variable `{id}` is not set or is not valid unicode")]
    EnvMissing {
        /// Environment variable name, never its value.
        id: String,
    },
    /// The resolved secret value is empty.
    #[error("secret source `{label}` resolved to an empty value")]
    Empty {
        /// Non-secret source label (env var name or keyring entry reference).
        label: String,
    },
    /// Keyring support was compiled out (`secrets-keyring` feature disabled).
    #[error("keyring secret resolution is unavailable (enable the `secrets-keyring` feature)")]
    KeyringDisabled,
    /// The platform keyring lookup failed (store locked, entry absent, or
    /// storage backend unavailable).
    #[cfg(feature = "secrets-keyring")]
    #[error("keyring entry `{service}/{id}` is unavailable")]
    KeyringLookup {
        /// Keyring service label (`[secrets].keyring_service`, default `orchestraitor`).
        service: String,
        /// Store-specific secret identifier.
        id: String,
        /// Keyring backend error; forwarded only to `source()`, never to the
        /// redacted `Debug` rendering.
        #[source]
        source: Box<keyring::Error>,
    },
}

impl fmt::Debug for SecretResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EnvMissing { id } => formatter
                .debug_struct("EnvMissing")
                .field("id", id)
                .finish(),
            Self::Empty { label } => formatter
                .debug_struct("Empty")
                .field("label", label)
                .finish(),
            Self::KeyringDisabled => formatter.write_str("KeyringDisabled"),
            #[cfg(feature = "secrets-keyring")]
            Self::KeyringLookup { service, id, .. } => formatter
                .debug_struct("KeyringLookup")
                .field("service", service)
                .field("id", id)
                .field("source", &format_args!("[redacted keyring error]"))
                .finish(),
        }
    }
}

/// GitHub App service-identity authentication failures (spec
/// `10-orchestrator.md` §9.25.2).
///
/// No variant ever carries the App private key PEM, a minted JWT, or an
/// installation token.
#[derive(Debug, Error)]
pub enum GitHubAppError {
    /// A required `github_app.*` configuration key is not set.
    #[error("github app configuration key `github_app.{key}` is not set")]
    MissingConfig {
        /// Leaf key name under `github_app`.
        key: String,
    },
    /// The App private key could not be resolved from its `secret://` URI;
    /// resolution failure is fail-closed with no ambient-credential fallback.
    #[error("github app private key resolution failed for `{uri}`")]
    PrivateKeyResolution {
        /// The non-secret `secret://` URI reference.
        uri: String,
        /// Resolution failure detail.
        #[source]
        source: Box<SecretResolveError>,
    },
    /// The resolved private key is not a valid RSA PEM.
    #[error("github app private key is not a valid RSA PEM")]
    InvalidPrivateKeyPem,
    /// RS256 JWT signing failed.
    #[error("github app JWT signing failed")]
    JwtSigning,
    /// The token-mint HTTP transport failed before a complete response was
    /// received; request and response bodies are never captured.
    #[error("github app installation token request transport failed ({kind})")]
    Transport {
        /// Static failure classification (e.g. `connect`, `timeout`, `decode`).
        kind: &'static str,
    },
    /// GitHub rejected the token-mint request.
    #[error("github app installation token request failed with HTTP status {status}")]
    MintRequest {
        /// HTTP status code; response bodies are never captured.
        status: u16,
    },
    /// The token-mint response payload was malformed.
    #[error("github app installation token response is malformed: {detail}")]
    MalformedResponse {
        /// Static detail label, never response content.
        detail: &'static str,
    },
    /// The minted token expiry could not be parsed as RFC 3339.
    #[error("github app installation token expiry timestamp is not RFC 3339")]
    ExpiryUnparseable,
    /// The minted token expiry exceeds the accepted 1h maximum lifetime.
    #[error("github app installation token expiry exceeds the 1 hour maximum")]
    ExpiryExceedsMaximum,
    /// The token cache mutex was poisoned by a panic during a previous mint.
    #[error("github app installation token cache is unavailable after a panic")]
    CachePoisoned,
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

    #[cfg(feature = "secrets-keyring")]
    #[test]
    fn keyring_lookup_debug_never_renders_keyring_payload_bytes() {
        // Regression for the blocking review finding: keyring-core 1.0.0
        // derives `Debug`, so `BadEncoding(Vec<u8>)` renders the raw retrieved
        // secret blob as decimal bytes. The manual `Debug` impl on
        // `SecretResolveError` must redact the source, and the redaction must
        // hold transitively through `GitHubAppError`'s derived `Debug`.
        let marker: &[u8] = b"ORCHKEYRINGMARKER0017";
        let rendered_payload = format!("{:?}", marker.to_vec());
        let marker_text = std::str::from_utf8(marker).unwrap_or("");

        let error = SecretResolveError::KeyringLookup {
            service: "orchestraitor".to_string(),
            id: "gh-app-pem".to_string(),
            source: Box::new(keyring::Error::BadEncoding(marker.to_vec())),
        };
        let debug = format!("{error:?}");
        assert!(!debug.contains("BadEncoding"));
        assert!(!debug.contains("BadDataFormat"));
        assert!(!debug.contains(&rendered_payload));
        assert!(!debug.contains(marker_text));
        assert!(debug.contains("redacted"));

        let wrapped = GitHubAppError::PrivateKeyResolution {
            uri: "secret://keyring/gh-app-pem".to_string(),
            source: Box::new(error),
        };
        let wrapped_debug = format!("{wrapped:?}");
        assert!(!wrapped_debug.contains("BadEncoding"));
        assert!(!wrapped_debug.contains("BadDataFormat"));
        assert!(!wrapped_debug.contains(&rendered_payload));
        assert!(!wrapped_debug.contains(marker_text));
        assert!(wrapped_debug.contains("gh-app-pem"));

        // The miette `Display`/`source()` chain stays intact and payload-free.
        assert!(!format!("{wrapped}").contains(marker_text));
        let mut chain_texts = Vec::new();
        let mut current = wrapped.source();
        while let Some(source) = current {
            chain_texts.push(source.to_string());
            current = source.source();
        }
        assert!(
            chain_texts.iter().any(
                |text| text.contains("keyring entry `orchestraitor/gh-app-pem` is unavailable")
            )
        );
        for text in &chain_texts {
            assert!(!text.contains(marker_text));
        }
    }

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
