#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use crate::detection::{DetectionArtifact, DetectionRuleSet, Detector};
use crate::registry::{BUILT_IN_DOMAINS, BUILT_IN_ROLES};
use crate::routing::{MatchedStep, Route, RoutingRequest, RoutingResolver, RoutingTable};

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
