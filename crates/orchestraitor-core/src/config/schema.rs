//! Serializable configuration schema.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::secret::SecretUri;

/// Root Orchestraitor configuration schema.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OrchestraitorConfig {
    /// Format and safe-fix normalization behavior.
    pub normalization: Option<NormalizationConfig>,
    /// Provider definitions keyed by provider id.
    pub providers: Option<BTreeMap<String, ProviderConfig>>,
    /// Agent domain routing and role templates.
    pub agents: Option<AgentsConfig>,
    /// Subscription definitions keyed by subscription id.
    pub subscriptions: Option<BTreeMap<String, SubscriptionConfig>>,
    /// Budget definitions keyed by budget id.
    pub budgets: Option<BTreeMap<String, BudgetConfig>>,
    /// Resource limit definitions keyed by limit id.
    pub resource_limits: Option<BTreeMap<String, ResourceLimitConfig>>,
    /// Retry behavior defaults.
    pub retry: Option<RetryConfig>,
    /// Data governance rules keyed by rule id.
    pub data_governance: Option<BTreeMap<String, DataGovernanceConfig>>,
    /// Data classification rules keyed by rule id.
    pub data_classification: Option<BTreeMap<String, DataClassificationConfig>>,
}

/// Normalization configuration block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NormalizationConfig {
    /// Whether files are formatted before transactional promotion.
    pub format_on_write: Option<bool>,
    /// Maximum normalization passes before reporting failure.
    pub max_passes: Option<u32>,
    /// Names of fix classes considered safe by this profile.
    pub safe_fix_classifications: Option<Vec<String>>,
}

/// Provider configuration block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderConfig {
    /// Explicit provider protocol name.
    pub protocol: Option<String>,
    /// Provider endpoint URL or local endpoint label.
    pub endpoint: Option<String>,
    /// Supported model identifiers.
    pub models: Option<Vec<String>>,
    /// Secret URI environment variables accepted by the provider.
    pub env: Option<Vec<SecretUri>>,
    /// Secret URI for a provider credential reference.
    pub api_key: Option<SecretUri>,
}

/// Agent configuration block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentsConfig {
    /// Domain configurations keyed by domain id.
    pub domains: Option<BTreeMap<String, DomainConfig>>,
}

/// Agent domain configuration block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DomainConfig {
    /// Human-readable domain description.
    pub description: Option<String>,
    /// Roles available inside this domain.
    pub roles: Option<Vec<String>>,
    /// Routing defaults for this domain.
    pub routing: Option<RoutingConfig>,
}

/// Model routing configuration block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoutingConfig {
    /// Provider identifier selected for this route.
    pub provider: Option<String>,
    /// Model identifier selected for this route.
    pub model: Option<String>,
    /// Optional profile name used by the route.
    pub profile: Option<String>,
}

/// Subscription configuration block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscriptionConfig {
    /// Provider id this subscription applies to.
    pub provider: Option<String>,
    /// Budget id attached to this subscription.
    pub budget: Option<String>,
}

/// Token and cost budget configuration block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BudgetConfig {
    /// Optional token cap.
    pub token_cap: Option<u64>,
    /// Optional cost cap in minor currency units.
    pub cost_cap: Option<u64>,
}

/// Resource limit configuration block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceLimitConfig {
    /// Maximum memory bytes for the named resource class.
    pub memory_bytes: Option<u64>,
    /// Maximum CPU milliseconds for the named resource class.
    pub cpu_ms: Option<u64>,
    /// Maximum output bytes for the named resource class.
    pub output_bytes: Option<u64>,
}

/// Retry configuration block.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RetryConfig {
    /// Maximum attempts before surfacing the failure.
    pub max_attempts: Option<u32>,
    /// Backoff in milliseconds between attempts.
    pub backoff_ms: Option<u64>,
}

/// Data governance configuration block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DataGovernanceConfig {
    /// Retention policy label.
    pub retention: Option<String>,
    /// Provenance policy label.
    pub provenance: Option<String>,
}

/// Data classification configuration block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DataClassificationConfig {
    /// Classification label.
    pub label: Option<String>,
    /// Whether this class may leave the local machine.
    pub exportable: Option<bool>,
}

/// Configuration precedence layer.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum ConfigLayer {
    /// Built-in defaults shipped as data.
    BuiltInDefaults,
    /// Plugin-provided defaults inserted below explicit user/project config.
    PluginDefaults,
    /// Global user configuration.
    GlobalUser,
    /// Organization or team policy layer.
    OrganizationTeam,
    /// Project configuration layer.
    Project,
    /// Directory or domain configuration layer.
    DirectoryDomain,
    /// Task or agent override layer.
    TaskAgent,
    /// Explicit CLI flag layer.
    CliFlag,
}

/// Source metadata for a resolved value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigSource {
    /// Precedence layer that supplied the value.
    pub layer: ConfigLayer,
    /// Human-readable source name.
    pub name: String,
}

/// Resolved config value plus provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResolvedValue<T> {
    /// Effective value.
    pub value: T,
    /// Source layer and name that supplied the value.
    pub source: ConfigSource,
    /// Whether the value was inherited from a lower-precedence layer.
    pub inherited: bool,
}
