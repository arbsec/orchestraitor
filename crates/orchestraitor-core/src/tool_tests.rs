//! Negative + adversarial tests for the declared-tool registry (issue #535
//! T2; spec `50-contracts-data.md` §21.1 obligations: trust boundary,
//! abuse cases, negative tests).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use super::{
    ConfigLayer, ConfigResolver, ResolvedEffort, ResolvedToolMechanism, ToolRegistry,
    ToolRegistryError, is_reserved_tool_id, is_valid_tool_id,
};
use crate::config::{ToolBudgetConfig, ToolConfig};
use crate::{ConfigSource, OrchestraitorConfig};

fn source(layer: ConfigLayer, name: &str) -> ConfigSource {
    ConfigSource {
        layer,
        name: name.to_string(),
    }
}

fn roles() -> Vec<String> {
    [
        "explore",
        "research",
        "plan",
        "implement",
        "review",
        "verify",
    ]
    .iter()
    .map(ToString::to_string)
    .collect()
}

fn command_toml(id: &str, argv: &str) -> String {
    format!("[tools.{id}]\nkind = \"command\"\ncommand = {argv}\n")
}

fn resolver_with(layer: ConfigLayer, toml: &str) -> ConfigResolver {
    ConfigResolver::new()
        .with_toml(source(layer, "test-layer"), toml)
        .unwrap()
}

// --- Layer-trust gate (the load-bearing rule) --------------------------------

#[test]
fn project_layer_tool_definition_is_a_typed_error() {
    let resolver = resolver_with(
        ConfigLayer::Project,
        &command_toml("explain-clippy", "[\"cargo\", \"clippy\"]"),
    );
    let error = ToolRegistry::from_resolver(&resolver, &roles(), false).unwrap_err();
    assert!(
        matches!(error, ToolRegistryError::UntrustedLayer { ref tool_id, .. } if tool_id == "explain-clippy"),
        "unexpected error: {error:?}"
    );
    // The typed error message names the tool and the layer: visible, not a
    // silent drop (spec §9.22.9).
    let message = error.to_string();
    assert!(message.contains("explain-clippy"));
    assert!(message.contains("project"));
}

#[test]
fn directory_domain_layer_tool_definition_is_rejected() {
    let resolver = resolver_with(
        ConfigLayer::DirectoryDomain,
        &command_toml("fmt", "[\"cargo\", \"fmt\"]"),
    );
    assert!(matches!(
        ToolRegistry::from_resolver(&resolver, &roles(), false),
        Err(ToolRegistryError::UntrustedLayer { .. })
    ));
}

#[test]
fn task_agent_layer_tool_definition_is_rejected() {
    let resolver = resolver_with(
        ConfigLayer::TaskAgent,
        &command_toml("fmt", "[\"cargo\", \"fmt\"]"),
    );
    assert!(matches!(
        ToolRegistry::from_resolver(&resolver, &roles(), false),
        Err(ToolRegistryError::UntrustedLayer { .. })
    ));
}

#[test]
fn trusted_layers_define_tools() {
    for layer in [
        ConfigLayer::BuiltInDefaults,
        ConfigLayer::PluginDefaults,
        ConfigLayer::GlobalUser,
        ConfigLayer::OrganizationTeam,
    ] {
        let resolver = resolver_with(
            layer,
            &command_toml("explain-clippy", "[\"cargo\", \"clippy\"]"),
        );
        let registry = ToolRegistry::from_resolver(&resolver, &roles(), false).unwrap();
        assert!(
            registry.get("explain-clippy").is_some(),
            "layer {layer:?} must be trusted"
        );
    }
}

#[test]
fn higher_trusted_layer_overrides_lower_but_lowest_wins_layer_gate() {
    // Org defines tool A; user ALSO defines tool A. Merge semantics: higher
    // layer (org, order 3) wins per scalar — the gate reads the WINNING
    // layer, so a project layer sneaking in below must still be caught when
    // it wins. Here both are trusted, so the org definition wins.
    let org = resolver_with(
        ConfigLayer::OrganizationTeam,
        &command_toml("a", "[\"org\"]"),
    );
    let user = resolver_with(ConfigLayer::GlobalUser, &command_toml("a", "[\"user\"]"));
    let resolver = ConfigResolver::new()
        .with_toml(
            source(ConfigLayer::GlobalUser, "user"),
            &command_toml("a", "[\"user\"]"),
        )
        .unwrap()
        .with_toml(
            source(ConfigLayer::OrganizationTeam, "org"),
            &command_toml("a", "[\"org\"]"),
        )
        .unwrap();
    let _ = (org, user);
    let registry = ToolRegistry::from_resolver(&resolver, &roles(), false).unwrap();
    let tool = registry.get("a").unwrap();
    let ResolvedToolMechanism::Command { argv } = &tool.mechanism else {
        panic!("expected command mechanism");
    };
    assert_eq!(argv[0], "org", "higher trusted layer wins per scalar merge");
}

// --- Mechanism validation ----------------------------------------------------

#[test]
fn command_tool_resolves_with_budget_and_visibility() {
    let toml = r#"
[tools.explain-clippy]
kind = "command"
command = ["cargo", "clippy", "--message-format", "json"]
visible_to = ["implement", "review"]

[tools.explain-clippy.budget]
max_turns = 1
wall_clock_secs = 600
max_result_bytes = 8192
"#;
    let resolver = resolver_with(ConfigLayer::GlobalUser, toml);
    let registry = ToolRegistry::from_resolver(&resolver, &roles(), false).unwrap();
    let resolved_tool = registry.get("explain-clippy").unwrap();
    let ResolvedToolMechanism::Command { argv } = &resolved_tool.mechanism else {
        panic!("expected command mechanism");
    };
    assert_eq!(argv, &["cargo", "clippy", "--message-format", "json"][..]);
    assert_eq!(resolved_tool.max_turns, Some(1));
    assert_eq!(resolved_tool.wall_clock_secs, Some(600));
    assert_eq!(resolved_tool.max_result_bytes, Some(8192));
    assert_eq!(
        resolved_tool
            .visible_to
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["implement", "review"]
    );
}

#[test]
fn command_tool_missing_argv_is_typed_error() {
    let resolver = resolver_with(ConfigLayer::GlobalUser, "[tools.fmt]\nkind = \"command\"\n");
    assert!(matches!(
        ToolRegistry::from_resolver(&resolver, &roles(), false),
        Err(ToolRegistryError::MalformedMechanism { .. })
    ));
}

#[test]
fn command_tool_with_subagent_field_is_typed_error() {
    let toml = "[tools.fmt]\nkind = \"command\"\ncommand = [\"ls\"]\nsubagent_role = \"review\"\n";
    let resolver = resolver_with(ConfigLayer::GlobalUser, toml);
    assert!(matches!(
        ToolRegistry::from_resolver(&resolver, &roles(), false),
        Err(ToolRegistryError::MalformedMechanism { .. })
    ));
}

#[test]
fn missing_kind_is_typed_error() {
    let resolver = resolver_with(ConfigLayer::GlobalUser, "[tools.fmt]\ncommand = [\"ls\"]\n");
    assert!(matches!(
        ToolRegistry::from_resolver(&resolver, &roles(), false),
        Err(ToolRegistryError::MalformedMechanism { .. })
    ));
}

#[test]
fn unknown_kind_is_typed_error() {
    let toml = "[tools.fmt]\nkind = \"hybrid\"\ncommand = [\"ls\"]\n";
    let resolver = resolver_with(ConfigLayer::GlobalUser, toml);
    let error = ToolRegistry::from_resolver(&resolver, &roles(), false).unwrap_err();
    assert!(matches!(
        error,
        ToolRegistryError::UnknownKind { ref kind, .. } if kind == "hybrid"
    ));
}

#[test]
fn subagent_tool_before_runtime_wired_is_typed_error() {
    let toml = "[tools.explain]\nkind = \"subagent\"\nsubagent_role = \"explore\"\n";
    let resolver = resolver_with(ConfigLayer::GlobalUser, toml);
    assert!(matches!(
        ToolRegistry::from_resolver(&resolver, &roles(), false),
        Err(ToolRegistryError::MalformedMechanism { .. })
    ));
}

#[test]
fn subagent_tool_resolves_when_runtime_wired() {
    let toml = "[tools.explain]\nkind = \"subagent\"\nsubagent_role = \"explore\"\nvisible_to = [\"implement\"]\neffort = \"low\"\nmax_summary_bytes = 4096\nstructured_summary = true\n";
    let resolver = resolver_with(ConfigLayer::GlobalUser, toml);
    let registry = ToolRegistry::from_resolver(&resolver, &roles(), true).unwrap();
    let resolved_tool = registry.get("explain").unwrap();
    let ResolvedToolMechanism::Subagent {
        role,
        internal_tools,
        instructions,
    } = &resolved_tool.mechanism
    else {
        panic!("expected subagent mechanism");
    };
    assert_eq!(role, "explore");
    // Absent allowlist = the read-only default.
    assert_eq!(internal_tools.len(), 2);
    assert_eq!(instructions, &None);
    assert_eq!(resolved_tool.effort, Some(ResolvedEffort::Low));
    assert_eq!(resolved_tool.max_summary_bytes, Some(4096));
    assert_eq!(resolved_tool.structured_summary, Some(true));
}

#[test]
fn subagent_unknown_role_is_typed_error() {
    let toml = "[tools.explain]\nkind = \"subagent\"\nsubagent_role = \"ghost\"\n";
    let resolver = resolver_with(ConfigLayer::GlobalUser, toml);
    let error = ToolRegistry::from_resolver(&resolver, &roles(), true).unwrap_err();
    assert!(matches!(
        error,
        ToolRegistryError::UnknownSubagentRole { ref role, .. } if role == "ghost"
    ));
}

#[test]
fn subagent_unknown_internal_tool_is_typed_error() {
    let toml = "[tools.explain]\nkind = \"subagent\"\nsubagent_role = \"explore\"\ninternal_tools = [\"write_file\"]\n";
    let resolver = resolver_with(ConfigLayer::GlobalUser, toml);
    let error = ToolRegistry::from_resolver(&resolver, &roles(), true).unwrap_err();
    assert!(matches!(
        error,
        ToolRegistryError::UnknownInternalTool { ref name, .. } if name == "write_file"
    ));
}

// --- Effort policy (owner-approved, issue #535) -------------------------------

#[test]
fn unknown_effort_value_is_typed_config_error() {
    let toml =
        "[tools.explain]\nkind = \"subagent\"\nsubagent_role = \"explore\"\neffort = \"maximum\"\n";
    let resolver = resolver_with(ConfigLayer::GlobalUser, toml);
    let error = ToolRegistry::from_resolver(&resolver, &roles(), true).unwrap_err();
    assert!(
        matches!(error, ToolRegistryError::UnknownEffort { ref effort, .. } if effort == "maximum"),
        "unexpected error: {error:?}"
    );
    assert!(error.to_string().contains("maximum"));
}

#[test]
fn every_effort_value_parses() {
    for (value, expected) in [
        ("low", ResolvedEffort::Low),
        ("medium", ResolvedEffort::Medium),
        ("high", ResolvedEffort::High),
    ] {
        let toml = format!(
            "[tools.explain]\nkind = \"subagent\"\nsubagent_role = \"explore\"\neffort = \"{value}\"\n"
        );
        let resolver = resolver_with(ConfigLayer::GlobalUser, &toml);
        let registry = ToolRegistry::from_resolver(&resolver, &roles(), true).unwrap();
        assert_eq!(registry.get("explain").unwrap().effort, Some(expected));
    }
}

// --- Id validation ------------------------------------------------------------

#[test]
fn reserved_and_invalid_ids_are_typed_errors() {
    for bad in ["bash", "finish", "board.query", "Has.Dot", "-lead"] {
        let toml = command_toml(bad, "[\"ls\"]");
        let resolver = resolver_with(ConfigLayer::GlobalUser, &toml);
        let result = ToolRegistry::from_resolver(&resolver, &roles(), false);
        assert!(result.is_err(), "id `{bad}` must be rejected");
    }
}

#[test]
fn visible_to_unknown_role_is_typed_error() {
    let toml = "[tools.fmt]\nkind = \"command\"\ncommand = [\"ls\"]\nvisible_to = [\"ghost\"]\n";
    let resolver = resolver_with(ConfigLayer::GlobalUser, toml);
    assert!(matches!(
        ToolRegistry::from_resolver(&resolver, &roles(), false),
        Err(ToolRegistryError::UnknownVisibleRole { .. })
    ));
}

#[test]
fn empty_visible_to_means_nobody() {
    let resolver = resolver_with(ConfigLayer::GlobalUser, &command_toml("fmt", "[\"ls\"]"));
    let registry = ToolRegistry::from_resolver(&resolver, &roles(), false).unwrap();
    assert!(registry.get("fmt").unwrap().visible_to.is_empty());
}

#[test]
fn zero_budget_values_are_rejected() {
    let toml = "[tools.fmt]\nkind = \"command\"\ncommand = [\"ls\"]\n\n[tools.fmt.budget]\nmax_turns = 0\n";
    let resolver = resolver_with(ConfigLayer::GlobalUser, toml);
    assert!(matches!(
        ToolRegistry::from_resolver(&resolver, &roles(), false),
        Err(ToolRegistryError::MalformedMechanism { .. })
    ));
}

// --- Config plumbing -----------------------------------------------------------

#[test]
fn tools_keys_are_known_and_unknown_subkeys_warn() {
    let toml = "[tools.fmt]\nkind = \"command\"\ncommand = [\"ls\"]\nbogus_key = 1\n";
    let report = crate::config::parse_toml_config(toml).unwrap();
    assert_eq!(report.unknown_keys, vec!["tools.fmt.bogus_key".to_string()]);
    assert!(report.config.tools.is_some());
}

#[test]
fn tools_merge_field_wise_across_layers() {
    let user = "[tools.fmt]\nkind = \"command\"\ncommand = [\"ls\"]\n";
    let org = "[tools.fmt]\nkind = \"command\"\nvisible_to = [\"review\"]\n\n[tools.fmt.budget]\nmax_turns = 3\n";
    let resolver = ConfigResolver::new()
        .with_toml(source(ConfigLayer::GlobalUser, "user"), user)
        .unwrap()
        .with_toml(source(ConfigLayer::OrganizationTeam, "org"), org)
        .unwrap();
    let registry = ToolRegistry::from_resolver(&resolver, &roles(), false).unwrap();
    let tool = registry.get("fmt").unwrap();
    let ResolvedToolMechanism::Command { argv } = &tool.mechanism else {
        panic!("expected command mechanism");
    };
    assert_eq!(argv, &["ls"][..]);
    assert_eq!(tool.max_turns, Some(3));
    assert_eq!(
        tool.visible_to
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["review"]
    );
    // Provenance: the winning definition's layer is the org layer.
    assert_eq!(tool.layer, ConfigLayer::OrganizationTeam);
}

#[test]
fn same_layer_conflicts_are_rejected_through_the_registry() {
    let first = "[tools.fmt]\nkind = \"command\"\ncommand = [\"a\"]\n";
    let second = "[tools.fmt]\nkind = \"command\"\ncommand = [\"b\"]\n";
    let resolver = ConfigResolver::new()
        .with_toml(source(ConfigLayer::GlobalUser, "a"), first)
        .unwrap()
        .with_toml(source(ConfigLayer::GlobalUser, "b"), second)
        .unwrap();
    assert!(ToolRegistry::from_resolver(&resolver, &roles(), false).is_err());
}

#[test]
fn builtin_registry_from_config() {
    let mut tools = BTreeMap::new();
    tools.insert(
        "fmt".to_string(),
        ToolConfig {
            kind: Some("command".to_string()),
            command: Some(vec!["cargo".to_string(), "fmt".to_string()]),
            budget: Some(ToolBudgetConfig {
                max_turns: Some(1),
                wall_clock_secs: Some(60),
                max_result_bytes: Some(4096),
            }),
            ..ToolConfig::default()
        },
    );
    let config = OrchestraitorConfig {
        tools: Some(tools),
        ..OrchestraitorConfig::default()
    };
    let registry = ToolRegistry::from_builtin(&config, &roles(), false).unwrap();
    assert_eq!(registry.len(), 1);
    assert!(registry.get("fmt").is_some());
}

#[test]
fn empty_config_yields_empty_registry() {
    let resolver = ConfigResolver::new();
    let registry = ToolRegistry::from_resolver(&resolver, &roles(), false).unwrap();
    assert!(registry.is_empty());
    assert_eq!(registry.ids(), Vec::<&str>::new());
}

#[test]
fn id_validation_rules() {
    assert!(is_valid_tool_id("explain-clippy"));
    assert!(is_valid_tool_id("review_diff"));
    assert!(is_valid_tool_id("a1"));
    assert!(!is_valid_tool_id(""));
    assert!(!is_valid_tool_id("-leading"));
    assert!(!is_valid_tool_id("has.dot"));
    assert!(!is_valid_tool_id("UPPER"));
    assert!(!is_valid_tool_id(&"x".repeat(65)));
    assert!(is_reserved_tool_id("bash"));
    assert!(is_reserved_tool_id("worker.delegate"));
    assert!(!is_reserved_tool_id("explain-clippy"));
}
