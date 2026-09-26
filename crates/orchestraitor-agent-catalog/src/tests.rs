#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use crate::decision_store::RoleRoutingDecisionStore;
use crate::detection::{DetectionArtifact, DetectionRuleSet, Detector};
use crate::error::AgentCatalogError;
use crate::registry::{BUILT_IN_DOMAINS, BUILT_IN_ORCHESTRATION_ROLES, BUILT_IN_ROLES};
use crate::role_routing::{BOOTSTRAP_MODEL, BOOTSTRAP_PROVIDER, RoleRouter};
use crate::routing::{MatchedStep, Route, RoutingRequest, RoutingResolver, RoutingTable};
use orchestraitor_core::{ConfigLayer, ConfigResolver, ConfigSource};

fn route(provider: &str, model: &str) -> Route {
    Route {
        provider: provider.to_string(),
        model: model.to_string(),
    }
}

fn request() -> RoutingRequest<'static> {
    RoutingRequest {
        agent_id: Some("agent-a"),
        domain: "backend",
        role: "implementing",
    }
}

#[test]
fn resolves_explicit_agent_override_before_all_other_routes() {
    let mut table = full_table();
    table
        .explicit_agent_overrides
        .insert("agent-a".to_string(), route("agent", "override"));

    let resolved = RoutingResolver::new(table).resolve(request()).unwrap();

    assert_eq!(resolved.provider, "agent");
    assert_eq!(resolved.model, "override");
    assert_eq!(resolved.matched_step, MatchedStep::ExplicitAgentOverride);
}

#[test]
fn resolves_domain_role_override_when_agent_override_absent() {
    let mut table = full_table();
    table.explicit_agent_overrides.clear();

    let resolved = RoutingResolver::new(table).resolve(request()).unwrap();

    assert_eq!(resolved.provider, "domain-role");
    assert_eq!(resolved.model, "override");
    assert_eq!(resolved.matched_step, MatchedStep::DomainRoleOverride);
}

#[test]
fn resolves_domain_default_when_more_specific_routes_absent() {
    let mut table = full_table();
    table.explicit_agent_overrides.clear();
    table.domain_role_overrides.clear();

    let resolved = RoutingResolver::new(table).resolve(request()).unwrap();

    assert_eq!(resolved.provider, "domain");
    assert_eq!(resolved.model, "default");
    assert_eq!(resolved.matched_step, MatchedStep::DomainDefault);
}

#[test]
fn resolves_role_default_when_domain_route_absent() {
    let mut table = full_table();
    table.explicit_agent_overrides.clear();
    table.domain_role_overrides.clear();
    table.domain_defaults.clear();

    let resolved = RoutingResolver::new(table).resolve(request()).unwrap();

    assert_eq!(resolved.provider, "role");
    assert_eq!(resolved.model, "default");
    assert_eq!(resolved.matched_step, MatchedStep::RoleDefault);
}

#[test]
fn resolves_project_default_when_role_route_absent() {
    let mut table = full_table();
    table.explicit_agent_overrides.clear();
    table.domain_role_overrides.clear();
    table.domain_defaults.clear();
    table.role_defaults.clear();

    let resolved = RoutingResolver::new(table).resolve(request()).unwrap();

    assert_eq!(resolved.provider, "project");
    assert_eq!(resolved.model, "default");
    assert_eq!(resolved.matched_step, MatchedStep::ProjectDefault);
}

#[test]
fn resolves_global_default_when_project_default_absent() {
    let mut table = full_table();
    table.explicit_agent_overrides.clear();
    table.domain_role_overrides.clear();
    table.domain_defaults.clear();
    table.role_defaults.clear();
    table.project_default = None;

    let resolved = RoutingResolver::new(table).resolve(request()).unwrap();

    assert_eq!(resolved.provider, "global");
    assert_eq!(resolved.model, "default");
    assert_eq!(resolved.matched_step, MatchedStep::GlobalDefault);
}

#[test]
fn cargo_toml_fixture_detects_general_fallback() {
    let detector = Detector::built_in().unwrap();
    let detected = detector.detect([DetectionArtifact {
        path: "Cargo.toml",
        contents: Some("[package]\nname = \"fixture\"\n"),
    }]);

    assert_eq!(detected.domain, "general");
    assert_eq!(detected.score, 0);
}

#[test]
fn below_threshold_returns_general_fallback() {
    let rules = DetectionRuleSet {
        threshold: 99,
        rules: DetectionRuleSet::built_in().unwrap().rules,
    };
    let detector = Detector::new(rules);

    let detected = detector.detect([DetectionArtifact {
        path: "package.json",
        contents: None,
    }]);

    assert_eq!(detected.domain, "general");
    assert_eq!(detected.score, 0);
}

#[test]
fn built_ins_contain_no_per_brand_agent_names() {
    let forbidden = ["sisyphus", "oracle", "metis"];
    let domain_text = BUILT_IN_DOMAINS
        .iter()
        .map(|domain| format!("{} {}", domain.id, domain.description).to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");
    let role_text = BUILT_IN_ROLES
        .iter()
        .map(|role| format!("{} {}", role.id, role.description).to_lowercase())
        .collect::<Vec<_>>()
        .join("\n");

    for name in forbidden {
        assert!(!domain_text.contains(name));
        assert!(!role_text.contains(name));
    }
}

#[test]
fn built_in_security_domain_is_analysis_only() {
    let security = BUILT_IN_DOMAINS
        .iter()
        .find(|domain| domain.id == "security")
        .expect("security domain exists");

    assert!(security.analysis_only);
}

fn full_table() -> RoutingTable {
    RoutingTable {
        explicit_agent_overrides: BTreeMap::new(),
        domain_role_overrides: BTreeMap::from([(
            RoutingTable::domain_role_key("backend", "implementing"),
            route("domain-role", "override"),
        )]),
        domain_defaults: BTreeMap::from([("backend".to_string(), route("domain", "default"))]),
        role_defaults: BTreeMap::from([("implementing".to_string(), route("role", "default"))]),
        project_default: Some(route("project", "default")),
        global_default: Some(route("global", "default")),
    }
}

fn resolver_from(layers: &[(ConfigLayer, &str, &str)]) -> ConfigResolver {
    let mut resolver = ConfigResolver::new();
    for (layer, name, toml) in layers {
        resolver = resolver
            .with_toml(
                ConfigSource {
                    layer: *layer,
                    name: name.to_string(),
                },
                toml,
            )
            .unwrap();
    }
    resolver
}

fn role_routes_toml(provider: &str, model: &str) -> String {
    BUILT_IN_ORCHESTRATION_ROLES
        .iter()
        .map(|role| {
            format!(
                "[roles.{}.routing]\nprovider = \"{provider}\"\nmodel = \"{model}\"\n",
                role.id
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn six_built_in_roles_resolve_and_persist_six_distinct_records() {
    let resolver = resolver_from(&[(
        ConfigLayer::BuiltInDefaults,
        "built-in",
        &role_routes_toml("neuralwatt", "glm-5.2"),
    )]);
    let store = RoleRoutingDecisionStore::open_in_memory().unwrap();
    let router = RoleRouter::new(&resolver);

    for role in BUILT_IN_ORCHESTRATION_ROLES {
        let decision = router.resolve(role.id).unwrap();
        assert_eq!(decision.provider, "neuralwatt");
        assert_eq!(decision.model, "glm-5.2");
        assert!(decision.fallback_reason.is_none());
        store.record(&decision).unwrap();
    }

    let records = store.list().unwrap();
    assert_eq!(records.len(), 6);
    let distinct_roles: std::collections::BTreeSet<_> =
        records.iter().map(|record| record.role.as_str()).collect();
    assert_eq!(distinct_roles.len(), 6);
    for role in BUILT_IN_ORCHESTRATION_ROLES {
        assert!(distinct_roles.contains(role.id));
        assert_eq!(store.for_role(role.id).unwrap().len(), 1);
    }
}

#[test]
fn missing_entry_resolves_via_documented_fallback_and_records_reason() {
    let resolver = resolver_from(&[(
        ConfigLayer::Project,
        "project",
        "[retry]\nmax_attempts = 3\n",
    )]);
    let decision = RoleRouter::new(&resolver).resolve("explore").unwrap();

    assert_eq!(decision.provider, BOOTSTRAP_PROVIDER);
    assert_eq!(decision.model, BOOTSTRAP_MODEL);
    assert_eq!(decision.precedence_path, "bootstrap-default");
    let reason = decision.fallback_reason.unwrap();
    assert!(reason.contains("roles.explore.routing"));
}

#[test]
fn partial_entry_errors_naming_the_missing_key() {
    let resolver = resolver_from(&[(
        ConfigLayer::Project,
        "project",
        "[roles.implement.routing]\nprovider = \"neuralwatt\"\n",
    )]);
    let error = RoleRouter::new(&resolver).resolve("implement").unwrap_err();

    assert!(matches!(error, AgentCatalogError::MissingRoutingKey { .. }));
    assert!(error.to_string().contains("roles.implement.routing.model"));
}

#[test]
fn partial_entry_model_only_errors_naming_the_provider_key() {
    let resolver = resolver_from(&[(
        ConfigLayer::Project,
        "project",
        "[roles.review.routing]\nmodel = \"glm-5.2\"\n",
    )]);
    let error = RoleRouter::new(&resolver).resolve("review").unwrap_err();

    assert!(matches!(error, AgentCatalogError::MissingRoutingKey { .. }));
    assert!(error.to_string().contains("roles.review.routing.provider"));
}

#[test]
fn higher_layer_overrides_and_the_precedence_path_names_the_layer() {
    let resolver = resolver_from(&[
        (
            ConfigLayer::BuiltInDefaults,
            "built-in",
            &role_routes_toml("neuralwatt", "glm-5.2"),
        ),
        (
            ConfigLayer::Project,
            "project",
            "[providers.acme]\nendpoint = \"https://example.invalid/v1\"\n\
             [roles.review.routing]\nprovider = \"acme\"\nmodel = \"acme-pro\"\n",
        ),
    ]);
    let decision = RoleRouter::new(&resolver).resolve("review").unwrap();

    assert_eq!(decision.provider, "acme");
    assert_eq!(decision.model, "acme-pro");
    assert_eq!(
        decision.precedence_path,
        "roles.review.routing (provider: project, model: project)"
    );
    assert!(decision.fallback_reason.is_none());
}

#[test]
fn provider_outside_the_allowed_set_is_a_typed_error_naming_the_key() {
    let resolver = resolver_from(&[(
        ConfigLayer::Project,
        "project",
        "[providers.acme]\nendpoint = \"https://example.invalid/v1\"\n\
         [roles.plan.routing]\nprovider = \"unknownco\"\nmodel = \"x\"\n",
    )]);
    let error = RoleRouter::new(&resolver).resolve("plan").unwrap_err();

    assert!(matches!(
        error,
        AgentCatalogError::InvalidRoutingValue { .. }
    ));
    let text = error.to_string();
    assert!(text.contains("roles.plan.routing.provider"));
    assert!(text.contains("acme"));
    assert!(text.contains("neuralwatt"));
}

#[test]
fn invalid_model_identifier_shape_is_a_typed_error_naming_the_key() {
    let resolver = resolver_from(&[(
        ConfigLayer::Project,
        "project",
        "[roles.verify.routing]\nprovider = \"neuralwatt\"\nmodel = \"glm 5.2\"\n",
    )]);
    let error = RoleRouter::new(&resolver).resolve("verify").unwrap_err();

    assert!(matches!(
        error,
        AgentCatalogError::InvalidRoutingValue { .. }
    ));
    assert!(error.to_string().contains("roles.verify.routing.model"));
}

#[test]
fn unknown_role_is_a_typed_error_listing_the_built_ins() {
    let resolver = resolver_from(&[(ConfigLayer::Project, "project", "")]);
    let error = RoleRouter::new(&resolver)
        .resolve("warpcouncil")
        .unwrap_err();

    assert!(matches!(error, AgentCatalogError::UnknownRole { .. }));
    let text = error.to_string();
    assert!(text.contains("warpcouncil"));
    for role in BUILT_IN_ORCHESTRATION_ROLES {
        assert!(text.contains(role.id));
    }
}

#[test]
fn resolution_is_deterministic_for_identical_inputs() {
    let toml = role_routes_toml("neuralwatt", "glm-5.2");
    let first = resolver_from(&[(ConfigLayer::BuiltInDefaults, "built-in", &toml)]);
    let second = resolver_from(&[(ConfigLayer::BuiltInDefaults, "built-in", &toml)]);

    let first_decision = RoleRouter::new(&first).resolve("plan").unwrap();
    let second_decision = RoleRouter::new(&second).resolve("plan").unwrap();

    assert_eq!(first_decision, second_decision);
}

#[test]
fn decision_store_reopens_a_file_with_idempotent_migrations() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("routing.db");
    let resolver = resolver_from(&[(ConfigLayer::Project, "project", "")]);
    let decision = RoleRouter::new(&resolver).resolve("verify").unwrap();

    {
        let store = RoleRoutingDecisionStore::open(&path).unwrap();
        store.record(&decision).unwrap();
    }
    let reopened = RoleRoutingDecisionStore::open(&path).unwrap();
    let records = reopened.list().unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].role, "verify");
    assert!(!records[0].created_at.is_empty());
}
