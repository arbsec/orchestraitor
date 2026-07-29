//! Core configuration, error, tracing, and secret URI infrastructure.
//!
//! This crate contains no I/O adapters, async runtime dependency, or security
//! enforcement. Security decisions remain Arbitraitor-owned.

#![forbid(unsafe_code)]

pub mod config;
pub mod error;
pub mod secret;
pub mod trace;

pub use config::{ConfigLayer, ConfigResolver, ConfigSource, OrchestraitorConfig, ResolvedValue};
pub use error::{ConfigError, OrchestraitorError, Retryability, StructuredError};
pub use secret::{SecretStore, SecretUri};
pub use trace::{TracingFormat, TracingInit, TracingOptions, is_redacted_field};
