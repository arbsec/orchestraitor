//! Layered configuration schema and resolver.

pub use schema::SimplifyConfig;

mod parse;
mod resolver;
mod schema;

use crate::error::OrchestraitorError;
pub use parse::{ConfigParseReport, parse_toml_config};
pub use resolver::ConfigResolver;
pub use schema::LoopGuardrailsConfig;
pub use schema::{
    AgentsConfig, BudgetConfig, ConfigLayer, ConfigSource, DataClassificationConfig,
    DataGovernanceConfig, DomainConfig, GitHubAppConfig, NormalizationConfig, OrchestraitorConfig,
    ProviderConfig, ResolvedValue, ResourceLimitConfig, RetryConfig, RoleConfig, RoutingConfig,
    RoutingDecisionProviderConfig, ServiceIdentityEnforcement, SubscriptionConfig,
    ToolBudgetConfig, ToolConfig,
};

/// Result alias for config operations.
pub type ConfigResult<T> = Result<T, OrchestraitorError>;
