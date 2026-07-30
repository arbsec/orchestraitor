use orchestraitor_agent_catalog::BUILT_IN_DOMAINS;
use orchestraitor_core::OrchestraitorConfig;
use orchestraitor_core::config::{AgentsConfig, DomainConfig};
use orchestraitor_model::{SecurityMode, ShellMode, WorkspaceMode};
use toml_edit::DocumentMut;

use crate::detection::DetectionSummary;

pub(crate) fn render_config(summary: &DetectionSummary) -> String {
    let config = OrchestraitorConfig {
        agents: Some(AgentsConfig {
            domains: Some(
                summary
                    .enabled_domains
                    .iter()
                    .map(|domain| (domain.clone(), DomainConfig::default()))
                    .collect(),
            ),
        }),
        ..OrchestraitorConfig::default()
    };
    let known_domains = BUILT_IN_DOMAINS
        .iter()
        .filter(|domain| domain.fallback)
        .count();
    let mut body = format!(
        "schema_version = \"0.1\"\nprofile = \"standard\"\nsecurity_mode = \"{}\"\nworkspace_mode = \"{}\"\nshell_mode = \"{}\"\n\n[normalization]\nformat_on_write = true\nmax_passes = 2\n",
        SecurityMode::Standard,
        WorkspaceMode::Snapshot,
        ShellMode::Standard,
    );
    for domain in &summary.enabled_domains {
        body.push_str("\n[agents.domains.");
        body.push_str(domain);
        body.push_str("]\nenabled = true\nproposed_by = \"orc init\"\n");
    }
    body.push_str("\n[init]\nschema_probe = ");
    body.push_str(if config.agents.is_some() {
        "true"
    } else {
        "false"
    });
    body.push_str("\nfallback_domains_known = \"");
    body.push_str(&known_domains.to_string());
    body.push_str("\"\ndetected_domain = \"");
    body.push_str(&summary.detected_domain);
    body.push_str("\"\n");
    match body.parse::<DocumentMut>() {
        Ok(document) => annotate_proposal(&document.to_string()),
        Err(_) => annotate_proposal(&body),
    }
}

fn annotate_proposal(toml: &str) -> String {
    let mut rendered = String::new();
    for line in toml.lines() {
        if line.trim().is_empty() {
            rendered.push('\n');
        } else {
            rendered.push_str("# Proposed by orc init\n");
            rendered.push_str(line);
            rendered.push('\n');
        }
    }
    rendered
}
