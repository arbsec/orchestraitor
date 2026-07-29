//! Precedence resolver for config layers.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::parse::flatten_config;
use crate::config::{
    AgentsConfig, ConfigLayer, ConfigResult, ConfigSource, NormalizationConfig,
    OrchestraitorConfig, ResolvedValue, RetryConfig, parse_toml_config,
};
use crate::error::ConfigError;

#[derive(Debug, Clone)]
pub(crate) struct ConfigInput {
    pub(crate) source: ConfigSource,
    pub(crate) config: OrchestraitorConfig,
    pub(crate) keys: BTreeSet<String>,
}

/// Layered configuration resolver with source tracking.
#[derive(Debug, Clone, Default)]
pub struct ConfigResolver {
    inputs: Vec<ConfigInput>,
}

impl ConfigResolver {
    /// Creates an empty config resolver.
    #[must_use]
    pub const fn new() -> Self {
        Self { inputs: Vec::new() }
    }

    /// Adds a TOML config layer and records unknown-key warnings.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the TOML layer cannot be parsed.
    pub fn with_toml(mut self, source: ConfigSource, toml: &str) -> ConfigResult<Self> {
        let report = parse_toml_config(toml)?;
        if !report.unknown_keys.is_empty() {
            tracing::warn!(unknown_keys = ?report.unknown_keys, layer = ?source.layer, "unknown config keys");
        }
        self.inputs.push(ConfigInput {
            source,
            config: report.config,
            keys: report.keys,
        });
        Ok(self)
    }

    /// Adds an already parsed config layer.
    #[must_use]
    pub fn with_config(mut self, source: ConfigSource, config: OrchestraitorConfig) -> Self {
        let keys = flatten_config(&config);
        self.inputs.push(ConfigInput {
            source,
            config,
            keys,
        });
        self
    }

    /// Resolves a single config key using an extractor.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when same-precedence sources conflict.
    pub fn resolve_value<T, F>(
        &self,
        key: &str,
        extract: F,
    ) -> ConfigResult<Option<ResolvedValue<T>>>
    where
        T: Clone + Eq,
        F: Fn(&OrchestraitorConfig) -> Option<T>,
    {
        let mut resolved = None;
        let mut sorted = self.inputs.clone();
        sorted.sort_by_key(|input| input.source.layer);
        reject_ambiguous_conflicts(key, &sorted)?;
        for input in sorted {
            if let Some(value) = extract(&input.config) {
                resolved = Some(ResolvedValue {
                    value,
                    source: input.source,
                    inherited: false,
                });
            }
        }
        Ok(resolved)
    }

    /// Resolves the effective merged configuration.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when same-precedence sources conflict.
    pub fn resolve_config(&self) -> ConfigResult<OrchestraitorConfig> {
        reject_all_ambiguous_conflicts(&self.inputs)?;
        let mut sorted = self.inputs.clone();
        sorted.sort_by_key(|input| input.source.layer);
        let mut merged = OrchestraitorConfig::default();
        for input in sorted {
            merged.merge(input.config);
        }
        Ok(merged)
    }
}

impl OrchestraitorConfig {
    pub(crate) fn merge(&mut self, next: Self) {
        merge_option(
            &mut self.normalization,
            next.normalization,
            NormalizationConfig::merge,
        );
        merge_map(&mut self.providers, next.providers);
        merge_option(&mut self.agents, next.agents, AgentsConfig::merge);
        merge_map(&mut self.subscriptions, next.subscriptions);
        merge_map(&mut self.budgets, next.budgets);
        merge_map(&mut self.resource_limits, next.resource_limits);
        merge_option(&mut self.retry, next.retry, RetryConfig::merge);
        merge_map(&mut self.data_governance, next.data_governance);
        merge_map(&mut self.data_classification, next.data_classification);
    }
}

impl NormalizationConfig {
    fn merge(&mut self, next: Self) {
        merge_scalar(&mut self.format_on_write, next.format_on_write);
        merge_scalar(&mut self.max_passes, next.max_passes);
        merge_scalar(
            &mut self.safe_fix_classifications,
            next.safe_fix_classifications,
        );
    }
}

impl AgentsConfig {
    fn merge(&mut self, next: Self) {
        merge_map(&mut self.domains, next.domains);
    }
}

impl RetryConfig {
    fn merge(&mut self, next: Self) {
        merge_scalar(&mut self.max_attempts, next.max_attempts);
        merge_scalar(&mut self.backoff_ms, next.backoff_ms);
    }
}

fn merge_scalar<T>(current: &mut Option<T>, next: Option<T>) {
    if let Some(value) = next {
        *current = Some(value);
    }
}

fn merge_option<T, F>(current: &mut Option<T>, next: Option<T>, merge: F)
where
    F: FnOnce(&mut T, T),
{
    match (current.as_mut(), next) {
        (Some(current_value), Some(next_value)) => merge(current_value, next_value),
        (None, Some(next_value)) => *current = Some(next_value),
        (Some(_) | None, None) => {}
    }
}

fn merge_map<T>(current: &mut Option<BTreeMap<String, T>>, next: Option<BTreeMap<String, T>>) {
    match (current.as_mut(), next) {
        (Some(current_map), Some(next_map)) => current_map.extend(next_map),
        (None, Some(next_map)) => *current = Some(next_map),
        (Some(_) | None, None) => {}
    }
}

fn reject_all_ambiguous_conflicts(inputs: &[ConfigInput]) -> ConfigResult<()> {
    let keys = inputs
        .iter()
        .flat_map(|input| input.keys.iter())
        .collect::<BTreeSet<_>>();
    for key in keys {
        reject_ambiguous_conflicts(key, inputs)?;
    }
    Ok(())
}

fn reject_ambiguous_conflicts(key: &str, inputs: &[ConfigInput]) -> ConfigResult<()> {
    let mut by_layer: BTreeMap<ConfigLayer, Vec<&ConfigInput>> = BTreeMap::new();
    for input in inputs.iter().filter(|input| input.keys.contains(key)) {
        by_layer.entry(input.source.layer).or_default().push(input);
    }
    for layer_inputs in by_layer.into_values() {
        if layer_inputs.len() > 1 {
            let names = layer_inputs
                .iter()
                .map(|input| input.source.name.clone())
                .collect();
            return Err(ConfigError::AmbiguousConflict {
                key: key.to_string(),
                sources: names,
            }
            .into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::OrchestraitorError;

    fn source(layer: ConfigLayer, name: &str) -> ConfigSource {
        ConfigSource {
            layer,
            name: name.to_string(),
        }
    }

    #[test]
    fn layer_merge_is_monotonic_by_precedence() -> Result<(), OrchestraitorError> {
        let defaults = OrchestraitorConfig {
            retry: Some(RetryConfig {
                max_attempts: Some(2),
                backoff_ms: Some(100),
            }),
            ..OrchestraitorConfig::default()
        };
        let cli = OrchestraitorConfig {
            retry: Some(RetryConfig {
                max_attempts: Some(4),
                backoff_ms: None,
            }),
            ..OrchestraitorConfig::default()
        };
        let resolver = ConfigResolver::new()
            .with_config(source(ConfigLayer::CliFlag, "cli"), cli)
            .with_config(source(ConfigLayer::BuiltInDefaults, "built-in"), defaults);
        let value = resolver.resolve_value("retry.max_attempts", |config| {
            config.retry.as_ref()?.max_attempts
        })?;
        let config = resolver.resolve_config()?;
        assert_eq!(value.map(|value| value.value), Some(4));
        assert_eq!(config.retry.and_then(|retry| retry.backoff_ms), Some(100));
        Ok(())
    }

    #[test]
    fn ambiguous_conflicts_are_rejected() {
        let first = OrchestraitorConfig {
            retry: Some(RetryConfig {
                max_attempts: Some(2),
                backoff_ms: None,
            }),
            ..OrchestraitorConfig::default()
        };
        let second = OrchestraitorConfig {
            retry: Some(RetryConfig {
                max_attempts: Some(3),
                backoff_ms: None,
            }),
            ..OrchestraitorConfig::default()
        };
        let resolver = ConfigResolver::new()
            .with_config(source(ConfigLayer::Project, "a"), first)
            .with_config(source(ConfigLayer::Project, "b"), second);
        assert!(matches!(
            resolver.resolve_config(),
            Err(OrchestraitorError::Config(_))
        ));
    }
}
