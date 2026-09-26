//! Layered configuration schema and resolver.

mod parse;
mod resolver;
mod schema;

pub use parse::{ConfigParseReport, parse_toml_config};
pub use resolver::ConfigResolver;
pub use schema::{
    AgentsConfig, BudgetConfig, ConfigLayer, ConfigSource, DataClassificationConfig,
    DataGovernanceConfig, DomainConfig, GitHubAppConfig, NormalizationConfig, OrchestraitorConfig,
    ProviderConfig, ResolvedValue, ResourceLimitConfig, RetryConfig, RoleConfig, RoutingConfig,
    SubscriptionConfig,
};

use crate::error::OrchestraitorError;

/// Result alias for config operations.
pub type ConfigResult<T> = Result<T, OrchestraitorError>;
