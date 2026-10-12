//! Provider-free project initialization command.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};

use miette::{Context, IntoDiagnostic, Result};

use crate::cli::InitArgs;
use crate::detection::{DetectionSummary, detect_project};
use crate::render::render_config;
use crate::scanner::LocalArtifacts;

const CONFIG_DIR: &str = ".orchestraitor";
const CONFIG_FILE: &str = "orchestraitor.toml";

/// Runs `orc init` and writes user-facing output to `writer`.
///
/// # Errors
///
/// Returns an error when project inspection or config writing fails.
pub fn run(args: &InitArgs, writer: &mut impl IoWrite) -> Result<()> {
    let root = args
        .project
        .canonicalize()
        .into_diagnostic()
        .wrap_err("failed to resolve project root")?;
    let proposal = Proposal::detect(&root)?;
    let config = render_config(&proposal.detection);
    if args.dry_run {
        writeln!(
            writer,
            "Dry run: would write {}",
            config_path_display(&root).display()
        )
        .into_diagnostic()?;
        writeln!(writer, "{config}").into_diagnostic()?;
    } else {
        let config_dir = root.join(CONFIG_DIR);
        fs::create_dir_all(&config_dir)
            .into_diagnostic()
            .wrap_err("failed to create .orchestraitor directory")?;
        fs::write(config_dir.join(CONFIG_FILE), config.as_bytes())
            .into_diagnostic()
            .wrap_err("failed to write proposed Orchestraitor config")?;
        writeln!(writer, "Wrote {}", config_path_display(&root).display()).into_diagnostic()?;
    }
    proposal.write_summary(writer)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Proposal {
    detection: DetectionSummary,
    artifacts: LocalArtifacts,
}

impl Proposal {
    fn detect(root: &Path) -> Result<Self> {
        let artifacts = LocalArtifacts::collect(root)?;
        let detection = detect_project(root, &artifacts)?;
        Ok(Self {
            detection,
            artifacts,
        })
    }

    fn write_summary(&self, writer: &mut impl IoWrite) -> Result<()> {
        writeln!(writer, "Detected:").into_diagnostic()?;
        write_set(writer, "languages", &self.detection.languages)?;
        write_set(writer, "formatters", &self.detection.formatters)?;
        write_set(writer, "package managers", &self.detection.package_managers)?;
        write_set(writer, "Git layout", &self.detection.git_layout)?;
        write_set(writer, "toolchain files", &self.detection.toolchain_files)?;
        write_set(
            writer,
            "agent/MCP/IDE config",
            &self.detection.agent_configs,
        )?;
        write_set(writer, "sensitive paths", &self.detection.sensitive_paths)?;
        write_set(writer, "generated files", &self.detection.generated_paths)?;
        write_set(writer, "enabled domains", &self.detection.enabled_domains)?;
        write_set(writer, "uncertain", &self.detection.uncertain)?;
        writeln!(writer, "Scanned artifacts: {}", self.artifacts.paths.len()).into_diagnostic()?;
        writeln!(
            writer,
            "Provider setup: optional next step; no credential requested."
        )
        .into_diagnostic()
    }
}

fn write_set(writer: &mut impl IoWrite, label: &str, values: &BTreeSet<String>) -> Result<()> {
    let rendered = if values.is_empty() {
        "none".to_string()
    } else {
        values.iter().cloned().collect::<Vec<_>>().join(", ")
    };
    writeln!(writer, "  {label}: {rendered}").into_diagnostic()
}

fn config_path_display(root: &Path) -> PathBuf {
    root.join(CONFIG_DIR).join(CONFIG_FILE)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use tempfile::TempDir;

    use super::*;
    use crate::detection::GENERAL_DOMAIN;

    #[test]
    fn uncertain_classification_keeps_general_enabled() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\n",
        )
        .unwrap();

        let proposal = Proposal::detect(temp.path()).unwrap();

        assert_eq!(proposal.detection.detected_domain, GENERAL_DOMAIN);
        assert!(proposal.detection.enabled_domains.contains(GENERAL_DOMAIN));
        assert!(
            proposal
                .detection
                .uncertain
                .contains("domain classification")
        );
    }

    #[test]
    fn rendered_config_has_proposal_comments_and_no_provider_secret() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("package.json"), "{}\n").unwrap();

        let proposal = Proposal::detect(temp.path()).unwrap();
        let rendered = render_config(&proposal.detection);

        assert!(rendered.contains("# Proposed by orc init\n[agents.domains.general]"));
        assert!(rendered.contains("enabled = true"));
        assert!(!rendered.contains("api_key"));
    }
}
