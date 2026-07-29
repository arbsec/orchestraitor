//! TOML parsing and unknown-key reporting.

use std::collections::BTreeSet;

use toml_edit::{DocumentMut, Item};

use crate::config::OrchestraitorConfig;
use crate::error::{ConfigError, OrchestraitorError};

/// Config parse report including unknown-key warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigParseReport {
    /// Parsed config.
    pub config: OrchestraitorConfig,
    /// Unknown dotted keys that callers should warn about.
    pub unknown_keys: Vec<String>,
    /// Dotted keys explicitly present in the TOML.
    pub keys: BTreeSet<String>,
}

/// Parses TOML into config while reporting unknown keys.
///
/// # Errors
///
/// Returns a configuration error when TOML parsing or typed deserialization fails.
pub fn parse_toml_config(toml: &str) -> Result<ConfigParseReport, OrchestraitorError> {
    let document = toml
        .parse::<DocumentMut>()
        .map_err(|error| ConfigError::TomlEdit(Box::new(error)))?;
    let keys = collect_keys(&document);
    let unknown_keys = unknown_keys(&document);
    let config = toml::from_str::<OrchestraitorConfig>(toml)
        .map_err(|error| ConfigError::Toml(Box::new(error)))?;
    Ok(ConfigParseReport {
        config,
        unknown_keys,
        keys,
    })
}

pub(crate) fn flatten_config(config: &OrchestraitorConfig) -> BTreeSet<String> {
    let Ok(toml) = toml::to_string(config) else {
        return BTreeSet::new();
    };
    let Ok(document) = toml.parse::<DocumentMut>() else {
        return BTreeSet::new();
    };
    collect_keys(&document)
}

fn collect_keys(document: &DocumentMut) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    collect_item_keys(None, document.as_item(), &mut keys);
    keys
}

fn collect_item_keys(prefix: Option<&str>, item: &Item, keys: &mut BTreeSet<String>) {
    if let Some(table) = item.as_table_like() {
        for (key, value) in table.iter() {
            let path = match prefix {
                Some(prefix) => format!("{prefix}.{key}"),
                None => key.to_string(),
            };
            if value.is_table_like() {
                collect_item_keys(Some(&path), value, keys);
            } else {
                keys.insert(path);
            }
        }
    }
}

fn unknown_keys(document: &DocumentMut) -> Vec<String> {
    let mut warnings = Vec::new();
    for key in collect_keys(document) {
        if !is_known_key(&key) {
            warnings.push(key);
        }
    }
    warnings
}

fn is_known_key(key: &str) -> bool {
    key == "normalization.format_on_write"
        || key == "normalization.max_passes"
        || key == "normalization.safe_fix_classifications"
        || key == "retry.max_attempts"
        || key == "retry.backoff_ms"
        || matches_dynamic_key(
            key,
            "providers",
            &["protocol", "endpoint", "models", "env", "api_key"],
        )
        || matches_agent_domain_routing_key(key)
        || matches_dynamic_key(key, "agents.domains", &["description", "roles"])
        || matches_dynamic_key(key, "subscriptions", &["provider", "budget"])
        || matches_dynamic_key(key, "budgets", &["token_cap", "cost_cap"])
        || matches_dynamic_key(
            key,
            "resource_limits",
            &["memory_bytes", "cpu_ms", "output_bytes"],
        )
        || matches_dynamic_key(key, "data_governance", &["retention", "provenance"])
        || matches_dynamic_key(key, "data_classification", &["label", "exportable"])
}

fn matches_agent_domain_routing_key(key: &str) -> bool {
    let Some(rest) = key.strip_prefix("agents.domains.") else {
        return false;
    };
    let parts = rest.split('.').collect::<Vec<_>>();
    matches!(
        parts.as_slice(),
        [_domain, "routing", "provider" | "model" | "profile"]
    )
}

fn matches_dynamic_key(key: &str, prefix: &str, fields: &[&str]) -> bool {
    let Some(rest) = key
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('.'))
    else {
        return false;
    };
    let mut segments = rest.rsplitn(2, '.');
    let Some(field) = segments.next() else {
        return false;
    };
    segments.next().is_some() && fields.contains(&field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parses_and_roundtrips() -> Result<(), OrchestraitorError> {
        let toml = r#"
[normalization]
format_on_write = true
max_passes = 2

[providers.zai]
protocol = "openai-compatible"
endpoint = "https://example.invalid/v1"
models = ["glm"]
env = ["secret://env/ZHIPU_API_KEY"]

[agents.domains.general.routing]
provider = "zai"
model = "glm"

[retry]
max_attempts = 3
backoff_ms = 250
"#;
        let report = parse_toml_config(toml)?;
        assert!(report.unknown_keys.is_empty());
        let rendered = toml::to_string(&report.config)
            .map_err(|error| ConfigError::TomlSerialize(Box::new(error)))?;
        let reparsed = toml::from_str::<OrchestraitorConfig>(&rendered)
            .map_err(|error| ConfigError::Toml(Box::new(error)))?;
        assert_eq!(report.config, reparsed);
        Ok(())
    }

    #[test]
    fn unknown_keys_are_reported() -> Result<(), OrchestraitorError> {
        let report = parse_toml_config("[retry]\nmax_attempts = 1\nunknown = true\n")?;
        assert_eq!(report.unknown_keys, vec!["retry.unknown".to_string()]);
        Ok(())
    }
}
