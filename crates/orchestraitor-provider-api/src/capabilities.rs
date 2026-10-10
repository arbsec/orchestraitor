//! Provider discovery and capability support types.

use orchestraitor_model::{ModelId, ProviderId};
use serde::{Deserialize, Serialize};

/// Model discovered from a provider or advisory catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredModel {
    /// Provider serving this model.
    pub provider_id: ProviderId,
    /// Model id as configured by Orchestraitor.
    pub model_id: ModelId,
    /// Provider wire id if it differs from `model_id`.
    pub wire_model_id: Option<String>,
    /// Human-readable model display name.
    pub display_name: Option<String>,
    /// Context window in tokens when known.
    pub context_window: Option<u64>,
    /// Maximum output tokens when known.
    pub max_output_tokens: Option<u64>,
    /// Model capabilities.
    pub capabilities: ProviderCapabilities,
    /// Source of model metadata.
    pub metadata_source: ModelMetadataSource,
}

/// Source used to discover model metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelMetadataSource {
    /// User or project configuration.
    ExplicitConfig,
    /// Provider model-list endpoint.
    ProviderEndpoint,
    /// `models.dev` catalog.
    ModelsDev,
    /// Bundled offline fallback catalog.
    BundledFallback,
    /// Manual unknown-model mode.
    ManualUnknown,
}

/// Provider or model capability support map.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapabilities {
    /// Supports explicit reasoning effort or thinking budgets.
    pub reasoning_effort: CapabilitySupport,
    /// Supports prompt caching controls or cache accounting.
    pub prompt_caching: CapabilitySupport,
    /// Supports tool choice controls.
    pub tool_choice: CapabilitySupport,
    /// Supports structured outputs.
    pub structured_outputs: CapabilitySupport,
    /// Supports multimodal inputs.
    pub multimodal_inputs: CapabilitySupport,
    /// Supports provider-hosted tools.
    pub provider_hosted_tools: CapabilitySupport,
    /// Supports server-side conversation state.
    pub server_side_conversation_state: CapabilitySupport,
    /// Supports token counting.
    pub token_counting: CapabilitySupport,
}

/// Support state for a provider capability.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilitySupport {
    /// Capability support has not been observed or declared.
    #[default]
    Unknown,
    /// Capability is known unsupported.
    Unsupported,
    /// Capability is known supported.
    Supported,
}
