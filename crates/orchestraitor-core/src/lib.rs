//! Core configuration, error, tracing, secret URI, and GitHub App
//! service-identity infrastructure.
//!
//! This crate contains no network I/O adapters or async runtime dependency.
//! Security decisions remain Arbitraitor-owned.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod github_app;
pub mod secret;
pub mod trace;

pub use config::{ConfigLayer, ConfigResolver, ConfigSource, OrchestraitorConfig, ResolvedValue};
pub use error::{
    ConfigError, GitHubAppError, OrchestraitorError, Retryability, SecretResolveError,
    StructuredError,
};
pub use github_app::{
    AccessTokenResponse, GitHubAppAuth, InstallationToken, InstallationTokenTransport,
    JWT_LIFETIME_SECS, PemResolver, TOKEN_REFRESH_SKEW_SECS, mint_app_jwt,
};
pub use secret::{DEFAULT_KEYRING_SERVICE, SecretStore, SecretUri};
pub use trace::{TracingFormat, TracingInit, TracingOptions, is_redacted_field};
