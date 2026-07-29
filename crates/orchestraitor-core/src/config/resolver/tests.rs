use std::collections::BTreeMap;

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

#[test]
fn dynamic_provider_entries_merge_field_wise_across_layers() -> Result<(), OrchestraitorError> {
    let defaults = OrchestraitorConfig {
        providers: Some(BTreeMap::from([(
            String::from("zai"),
            ProviderConfig {
                protocol: Some(String::from("openai-compatible")),
                endpoint: Some(String::from("https://api.z.ai/api/paas/v4")),
                models: None,
                env: None,
                api_key: None,
            },
        )])),
        ..OrchestraitorConfig::default()
    };
    let project = OrchestraitorConfig {
        providers: Some(BTreeMap::from([(
            String::from("zai"),
            ProviderConfig {
                protocol: None,
                endpoint: None,
                models: None,
                env: None,
                api_key: Some("secret://env/ZAI_API_KEY".parse()?),
            },
        )])),
        ..OrchestraitorConfig::default()
    };

    let resolver = ConfigResolver::new()
        .with_config(source(ConfigLayer::BuiltInDefaults, "built-in"), defaults)
        .with_config(source(ConfigLayer::Project, "project"), project);
    let config = resolver.resolve_config()?;
    let provider = config
        .providers
        .and_then(|providers| providers.get("zai").cloned());

    assert_eq!(
        provider
            .as_ref()
            .and_then(|provider| provider.protocol.as_deref()),
        Some("openai-compatible")
    );
    assert_eq!(
        provider
            .as_ref()
            .and_then(|provider| provider.api_key.as_ref()),
        Some(&"secret://env/ZAI_API_KEY".parse()?)
    );
    Ok(())
}

#[test]
fn dynamic_domain_entries_merge_nested_fields_across_layers() -> Result<(), OrchestraitorError> {
    let defaults = OrchestraitorConfig {
        agents: Some(AgentsConfig {
            domains: Some(BTreeMap::from([(
                String::from("code"),
                DomainConfig {
                    description: Some(String::from("coding tasks")),
                    roles: None,
                    routing: Some(RoutingConfig {
                        provider: Some(String::from("zai")),
                        model: None,
                        profile: None,
                    }),
                },
            )])),
        }),
        ..OrchestraitorConfig::default()
    };
    let project = OrchestraitorConfig {
        agents: Some(AgentsConfig {
            domains: Some(BTreeMap::from([(
                String::from("code"),
                DomainConfig {
                    description: None,
                    roles: Some(vec![String::from("implementer")]),
                    routing: Some(RoutingConfig {
                        provider: None,
                        model: Some(String::from("glm-4.5")),
                        profile: None,
                    }),
                },
            )])),
        }),
        ..OrchestraitorConfig::default()
    };

    let resolver = ConfigResolver::new()
        .with_config(source(ConfigLayer::BuiltInDefaults, "built-in"), defaults)
        .with_config(source(ConfigLayer::Project, "project"), project);
    let config = resolver.resolve_config()?;
    let domain = config
        .agents
        .and_then(|agents| agents.domains)
        .and_then(|domains| domains.get("code").cloned());

    assert_eq!(
        domain
            .as_ref()
            .and_then(|domain| domain.description.as_deref()),
        Some("coding tasks")
    );
    assert_eq!(
        domain
            .as_ref()
            .and_then(|domain| domain.routing.as_ref())
            .and_then(|routing| routing.provider.as_deref()),
        Some("zai")
    );
    assert_eq!(
        domain
            .as_ref()
            .and_then(|domain| domain.routing.as_ref())
            .and_then(|routing| routing.model.as_deref()),
        Some("glm-4.5")
    );
    Ok(())
}
