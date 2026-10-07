// Test-only allowances mirror the workspace test harness convention: a
// failed expectation must fail the test loudly.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use super::*;
use crate::error::OrchestraitorError;
use crate::secret::SecretUri;

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

#[test]
fn github_app_fields_merge_across_layers() -> Result<(), OrchestraitorError> {
    let defaults = OrchestraitorConfig {
        github_app: Some(GitHubAppConfig {
            slug: Some(String::from("arbsec-agent")),
            enforcement: None,
            client_id: None,
            installation_id: None,
            private_key_uri: None,
        }),
        service_identities: Some(vec![String::from("arbsec-agent")]),
        ..OrchestraitorConfig::default()
    };
    let user = OrchestraitorConfig {
        github_app: Some(GitHubAppConfig {
            slug: None,
            enforcement: None,
            client_id: Some(String::from("Iv23linxUDbcc53QbFVK")),
            installation_id: Some(165_043_398),
            private_key_uri: Some("secret://keyring/orchestraitor-app-pem".parse()?),
        }),
        service_identities: None,
        ..OrchestraitorConfig::default()
    };

    let resolver = ConfigResolver::new()
        .with_config(source(ConfigLayer::BuiltInDefaults, "built-in"), defaults)
        .with_config(source(ConfigLayer::GlobalUser, "user"), user);
    let config = resolver.resolve_config()?;
    let github_app = config.github_app;

    assert_eq!(
        github_app.as_ref().and_then(|app| app.slug.as_deref()),
        Some("arbsec-agent")
    );
    assert_eq!(
        github_app.as_ref().and_then(|app| app.client_id.as_deref()),
        Some("Iv23linxUDbcc53QbFVK")
    );
    assert_eq!(
        github_app.as_ref().and_then(|app| app.installation_id),
        Some(165_043_398)
    );
    assert_eq!(
        github_app
            .as_ref()
            .and_then(|app| app.private_key_uri.as_ref())
            .map(SecretUri::as_uri)
            .as_deref(),
        Some("secret://keyring/orchestraitor-app-pem")
    );
    assert_eq!(
        config.service_identities.as_deref(),
        Some([String::from("arbsec-agent")].as_slice())
    );

    let value = resolver.resolve_value("github_app.client_id", |config| {
        config.github_app.as_ref()?.client_id.clone()
    })?;
    assert_eq!(
        value.map(|value| (value.value, value.source.name)),
        Some((String::from("Iv23linxUDbcc53QbFVK"), String::from("user")))
    );
    Ok(())
}

#[test]
fn role_routing_entries_merge_across_layers() -> Result<(), OrchestraitorError> {
    let defaults = OrchestraitorConfig {
        roles: Some(BTreeMap::from([(
            String::from("implement"),
            RoleConfig {
                routing: Some(RoutingConfig {
                    provider: Some(String::from("neuralwatt")),
                    model: Some(String::from("glm-5.2")),
                    profile: None,
                }),
            },
        )])),
        ..OrchestraitorConfig::default()
    };
    let project = OrchestraitorConfig {
        roles: Some(BTreeMap::from([(
            String::from("implement"),
            RoleConfig {
                routing: Some(RoutingConfig {
                    provider: None,
                    model: Some(String::from("glm-5.2-flash")),
                    profile: None,
                }),
            },
        )])),
        ..OrchestraitorConfig::default()
    };

    let resolver = ConfigResolver::new()
        .with_config(source(ConfigLayer::BuiltInDefaults, "built-in"), defaults)
        .with_config(source(ConfigLayer::Project, "project"), project);
    let config = resolver.resolve_config()?;
    let routing = config
        .roles
        .and_then(|roles| roles.get("implement").cloned())
        .and_then(|role| role.routing);

    assert_eq!(
        routing
            .as_ref()
            .and_then(|routing| routing.provider.as_deref()),
        Some("neuralwatt")
    );
    assert_eq!(
        routing
            .as_ref()
            .and_then(|routing| routing.model.as_deref()),
        Some("glm-5.2-flash")
    );
    let provider = resolver.resolve_value("roles.implement.routing.provider", |config| {
        config
            .roles
            .as_ref()?
            .get("implement")?
            .routing
            .as_ref()?
            .provider
            .clone()
    })?;
    assert_eq!(
        provider.map(|value| value.source.layer),
        Some(ConfigLayer::BuiltInDefaults)
    );
    Ok(())
}

#[test]
fn decision_provider_routing_keys_are_project_scoped_and_layer_merged()
-> Result<(), OrchestraitorError> {
    // The decision-provider block is per-project configuration: a built-in
    // (or user/org) layer can default it, and the project layer wins field
    // by field — a different project (or no `[routing]` at all) keeps
    // heuristic routing.
    let defaults = OrchestraitorConfig {
        routing: Some(RoutingDecisionProviderConfig {
            provider: Some("systemone".to_string()),
            base_url: Some("https://api.neuralwatt.com/v1".to_string()),
            model: Some("clef-flash".to_string()),
            api_key: Some(SecretUri::parse("secret://env/NEURALWATT_API_KEY").unwrap()),
        }),
        ..OrchestraitorConfig::default()
    };
    let project = OrchestraitorConfig {
        routing: Some(RoutingDecisionProviderConfig {
            provider: Some("systemone".to_string()),
            base_url: Some("http://mekbook.tail1e276.ts.net:8080/v1".to_string()),
            model: None, // inherits the lower layer's model field-wise.
            api_key: None,
        }),
        ..OrchestraitorConfig::default()
    };
    let resolver = ConfigResolver::new()
        .with_config(source(ConfigLayer::BuiltInDefaults, "built-in"), defaults)
        .with_config(source(ConfigLayer::Project, "orchestraitor.toml"), project);
    let config = resolver.resolve_config().unwrap();
    let routing = config.routing.as_ref().expect("project layer set routing");
    assert_eq!(routing.provider.as_deref(), Some("systemone"));
    assert_eq!(
        routing.base_url.as_deref(),
        Some("http://mekbook.tail1e276.ts.net:8080/v1"),
        "the project layer wins over the built-in default"
    );
    assert_eq!(
        routing.model.as_deref(),
        Some("clef-flash"),
        "unset project fields inherit lower layers field-wise"
    );
    // Field-wise merge semantics: `merge_scalar` only overwrites with
    // `Some`, so a project that leaves `api_key` unset INHERITS the lower
    // layer's key (an explicit opt-out is `routing.api_key = "none"` at
    // resolution time, per the `resolve_endpoint` mapping).
    assert!(
        routing.api_key.is_some(),
        "unset project fields inherit lower layers field-wise (opt-out is the \"none\" value)"
    );
    // Provenance: the winning base_url is attributed to the project layer.
    let base_url = resolver.resolve_value("routing.base_url", |config| {
        config.routing.as_ref()?.base_url.clone()
    })?;
    assert_eq!(
        base_url.map(|value| value.source.layer),
        Some(ConfigLayer::Project)
    );
    Ok(())
}

#[test]
fn a_project_without_routing_keeps_heuristic_routing() {
    // Default off: no [routing] block in any layer -> no decision provider,
    // byte-identical heuristic behavior.
    let resolver = ConfigResolver::new();
    let config = resolver.resolve_config().unwrap();
    assert!(
        config.routing.is_none(),
        "no [routing] anywhere means no decision provider is configured"
    );
}
