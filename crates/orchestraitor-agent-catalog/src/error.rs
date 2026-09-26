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

    /// Role id is not one of the six built-in orchestration roles.
    #[error("unknown orchestration role '{role}'; built-in roles are: {known}")]
    UnknownRole {
        /// Rejected role id.
        role: String,
        /// Comma-separated list of the built-in role ids.
        known: String,
    },

    /// A role routing table entry is missing one of its required sub-keys.
    #[error("role routing entry is incomplete: missing configuration key `{key}`")]
    MissingRoutingKey {
        /// Dotted configuration key that must be set.
        key: String,
    },

    /// A role routing table value failed validation.
    #[error("invalid routing value for `{key}`: {reason}")]
    InvalidRoutingValue {
        /// Dotted configuration key holding the invalid value.
        key: String,
        /// Validation failure description, including accepted shapes or sets.
        reason: String,
    },

    /// Layered configuration resolution failed while routing a role.
    #[error("role routing configuration resolution failed: {0}")]
    Config(#[from] orchestraitor_core::OrchestraitorError),

    /// The `SQLite` routing decision store failed.
    #[error("routing decision store error")]
    DecisionStore(#[from] rusqlite::Error),

    /// A filesystem operation for the routing decision store failed.
    #[error("routing decision store I/O error")]
    DecisionStoreIo(#[from] std::io::Error),

    /// A freshly inserted decision record could not be read back.
    #[error("routing decision record {id} was inserted but could not be read back")]
    DecisionStoreReadback {
        /// Store-assigned row id that failed read-back.
        id: i64,
    },
}

/// Result alias for agent-catalog operations.
pub type AgentCatalogResult<T> = Result<T, AgentCatalogError>;
