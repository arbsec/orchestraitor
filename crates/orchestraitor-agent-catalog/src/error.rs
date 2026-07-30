//! Error types for the agent catalog crate.

use thiserror::Error;

/// Errors returned by catalog parsing, detection, and route resolution.
#[derive(Debug, Error)]
pub enum AgentCatalogError {
    /// Embedded detection rule TOML failed to parse.
    #[error("failed to parse built-in detection rules: {0}")]
    DetectionRules(#[from] toml::de::Error),

    /// No route matched and no project/global default was configured.
    #[error("no model route matched domain '{domain}' and role '{role}'")]
    MissingRoute {
        /// Requested domain id.
        domain: String,
        /// Requested role id.
        role: String,
    },
}
