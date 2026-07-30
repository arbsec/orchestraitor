//! `orc verify` implementation (spec MVP-8, §9.5).
//!
//! Detects the project-configured verification registry and reports it.
//! The same registry works locally and in CI. Verification command
//! execution requires an Arbitraitor sandbox (spec §6.7, §16.2); until
//! the sandbox is available, `orc verify` reports the detected registry
//! and exits without executing untrusted commands.

use std::collections::BTreeSet;
use std::io::Write;

use miette::{IntoDiagnostic, Result};
use serde::Serialize;

use crate::cli::{ConfigPaths, VerifyArgs};
use crate::commands::config::load_layers;
use crate::exit_code::{ExitCode, OrcError, OrcResult};
use crate::scanner::LocalArtifacts;

/// Runs `orc verify`.
///
/// # Errors
///
/// Returns an [`OrcError`] with [`ExitCode::ConfigError`] when configuration
/// cannot be loaded, or [`ExitCode::GeneralFailure`] for I/O errors.
pub fn run<W: Write>(
    paths: &ConfigPaths,
    args: &VerifyArgs,
    writer: &mut W,
) -> OrcResult<ExitCode> {
    let registry = detect_registry(paths).map_err(OrcError::config)?;
    let report = VerifyReport::from_registry(&registry, paths);

    if args.json {
        serde_json::to_writer_pretty(&mut *writer, &report)
            .into_diagnostic()
            .map_err(OrcError::infrastructure)?;
        writeln!(writer).into_diagnostic().map_err(infra_error)?;
    } else if !args.quiet {
        render_text(writer, &report).map_err(infra_error)?;
    }

    Ok(ExitCode::Success)
}

fn infra_error(e: miette::Report) -> OrcError {
    OrcError::infrastructure(e)
}

/// Detected verification registry entry.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyEntry {
    /// Human-readable name.
    pub name: String,
    /// Command string that would run inside the sandbox.
    pub command: String,
    /// Configuration files that triggered detection.
    pub trigger_files: Vec<String>,
    /// Whether this entry came from project config or built-in detection.
    pub source: VerifySource,
}

/// Origin of a verification entry.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerifySource {
    /// Explicitly configured in `orchestraitor.toml`.
    ProjectConfig,
    /// Detected from recognized configuration files.
    BuiltInDetection,
}

/// Full verification registry report.
#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    /// All detected verification entries.
    pub entries: Vec<VerifyEntry>,
    /// Whether sandbox execution is available.
    pub sandbox_available: bool,
    /// Project directory that was scanned.
    pub project_dir: String,
}

impl VerifyReport {
    fn from_registry(registry: &[VerifyEntry], paths: &ConfigPaths) -> Self {
        Self {
            entries: registry.to_vec(),
            sandbox_available: false,
            project_dir: paths.project_dir.display().to_string(),
        }
    }
}

fn detect_registry(paths: &ConfigPaths) -> Result<Vec<VerifyEntry>> {
    let mut entries = Vec::new();
    entries.extend(detect_from_config(paths)?);
    entries.extend(detect_from_files(paths)?);
    deduplicate(&mut entries);
    Ok(entries)
}

fn detect_from_config(paths: &ConfigPaths) -> Result<Vec<VerifyEntry>> {
    let layers = load_layers(paths)?;
    let config = layers.resolver.resolve_config().map_err(|error| {
        miette::miette!(
            "configuration validation failed: {}",
            error.structured().cause
        )
    })?;
    let commands = config.verification.map(|v| v.commands).unwrap_or_default();
    Ok(commands
        .into_iter()
        .map(|c| VerifyEntry {
            name: c.name,
            command: c.command,
            trigger_files: c.trigger_files,
            source: VerifySource::ProjectConfig,
        })
        .collect())
}

fn detect_from_files(paths: &ConfigPaths) -> Result<Vec<VerifyEntry>> {
    let project_root = &paths.project_dir;
    let artifacts = LocalArtifacts::collect(project_root)?;
    let paths_set = &artifacts.paths;
    let mut entries = Vec::new();

    for (trigger, name, command) in built_in_registry() {
        if paths_set.contains(*trigger) {
            entries.push(VerifyEntry {
                name: name.to_string(),
                command: command.to_string(),
                trigger_files: vec![trigger.to_string()],
                source: VerifySource::BuiltInDetection,
            });
        }
    }

    detect_package_manager_variants(paths_set, &mut entries);
    Ok(entries)
}

fn built_in_registry() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        ("Cargo.toml", "cargo-test", "cargo test"),
        ("package.json", "npm-test", "npm test"),
        ("pyproject.toml", "pytest", "pytest"),
        ("go.mod", "go-test", "go test ./..."),
        ("pom.xml", "maven-test", "mvn test"),
    ]
}

fn detect_package_manager_variants(paths: &BTreeSet<String>, entries: &mut Vec<VerifyEntry>) {
    if paths.contains("package.json") {
        for (lockfile, pm, cmd) in [
            ("pnpm-lock.yaml", "pnpm", "pnpm test"),
            ("yarn.lock", "yarn", "yarn test"),
            ("bun.lockb", "bun", "bun test"),
        ] {
            if paths.contains(lockfile) {
                entries.push(VerifyEntry {
                    name: format!("{pm}-test"),
                    command: cmd.to_string(),
                    trigger_files: vec![lockfile.to_string()],
                    source: VerifySource::BuiltInDetection,
                });
            }
        }
        if paths.contains("uv.lock") {
            entries.push(VerifyEntry {
                name: "uv-pytest".to_string(),
                command: "uv run pytest".to_string(),
                trigger_files: vec!["uv.lock".to_string()],
                source: VerifySource::BuiltInDetection,
            });
        }
    }
}

fn deduplicate(entries: &mut Vec<VerifyEntry>) {
    let mut seen = BTreeSet::new();
    entries.retain(|e| seen.insert(e.name.clone()));
}

fn render_text<W: Write>(writer: &mut W, report: &VerifyReport) -> Result<()> {
    if report.entries.is_empty() {
        writeln!(writer, "No verification commands detected.").into_diagnostic()?;
        writeln!(
            writer,
            "Configure [[verification.commands]] in orchestraitor.toml to add custom checks."
        )
        .into_diagnostic()?;
        return Ok(());
    }
    writeln!(
        writer,
        "Verification registry ({} entries):",
        report.entries.len()
    )
    .into_diagnostic()?;
    for entry in &report.entries {
        let source_label = match entry.source {
            VerifySource::ProjectConfig => "config",
            VerifySource::BuiltInDetection => "detected",
        };
        writeln!(
            writer,
            "  [{}] {} -> `{}` ({})",
            source_label,
            entry.name,
            entry.command,
            join_files(&entry.trigger_files)
        )
        .into_diagnostic()?;
    }
    if !report.sandbox_available {
        writeln!(writer).into_diagnostic()?;
        writeln!(
            writer,
            "Note: sandbox execution not available. Commands are detected but not executed."
        )
        .into_diagnostic()?;
        writeln!(
            writer,
            "Arbitraitor sandbox is required for verification execution (spec §6.7, §16.2)."
        )
        .into_diagnostic()?;
    }
    Ok(())
}

fn join_files(files: &[String]) -> String {
    if files.is_empty() {
        "no trigger files".to_string()
    } else {
        files.join(", ")
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn write_config(dir: &Path, toml: &str) {
        std::fs::write(dir.join("orchestraitor.toml"), toml).unwrap();
    }

    #[test]
    fn detects_cargo_test_from_cargo_toml() -> Result<()> {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();

        let paths = ConfigPaths {
            config_dir: temp.path().join(".orchestraitor"),
            project_dir: temp.path().to_path_buf(),
            models_dev_endpoint: None,
        };
        let entries = detect_from_files(&paths)?;

        assert!(entries.iter().any(|e| e.name == "cargo-test"));
        Ok(())
    }

    #[test]
    fn detects_custom_commands_from_config() -> Result<()> {
        let temp = TempDir::new().unwrap();
        write_config(
            temp.path(),
            r#"
[[verification.commands]]
name = "custom-lint"
command = "make lint"
trigger_files = ["Makefile"]
"#,
        );

        let paths = ConfigPaths {
            config_dir: temp.path().join(".orchestraitor"),
            project_dir: temp.path().to_path_buf(),
            models_dev_endpoint: None,
        };
        let entries = detect_from_config(&paths)?;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "custom-lint");
        assert_eq!(entries[0].source, VerifySource::ProjectConfig);
        Ok(())
    }

    #[test]
    fn deduplicates_by_name() {
        let mut entries = vec![
            VerifyEntry {
                name: "dup".to_string(),
                command: "a".to_string(),
                trigger_files: vec![],
                source: VerifySource::ProjectConfig,
            },
            VerifyEntry {
                name: "dup".to_string(),
                command: "b".to_string(),
                trigger_files: vec![],
                source: VerifySource::BuiltInDetection,
            },
            VerifyEntry {
                name: "unique".to_string(),
                command: "c".to_string(),
                trigger_files: vec![],
                source: VerifySource::BuiltInDetection,
            },
        ];
        deduplicate(&mut entries);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "dup");
        assert_eq!(entries[1].name, "unique");
    }
}
