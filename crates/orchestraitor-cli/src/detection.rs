use std::collections::BTreeSet;
use std::path::Path;

use miette::{IntoDiagnostic, Result};
use orchestraitor_agent_catalog::Detector;

use crate::scanner::{LocalArtifacts, is_generated_path};

pub(crate) const GENERAL_DOMAIN: &str = "general";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetectionSummary {
    pub(crate) detected_domain: String,
    pub(crate) enabled_domains: BTreeSet<String>,
    pub(crate) languages: BTreeSet<String>,
    pub(crate) package_managers: BTreeSet<String>,
    pub(crate) formatters: BTreeSet<String>,
    pub(crate) git_layout: BTreeSet<String>,
    pub(crate) toolchain_files: BTreeSet<String>,
    pub(crate) agent_configs: BTreeSet<String>,
    pub(crate) sensitive_paths: BTreeSet<String>,
    pub(crate) generated_paths: BTreeSet<String>,
    pub(crate) uncertain: BTreeSet<String>,
}

pub(crate) fn detect_project(root: &Path, artifacts: &LocalArtifacts) -> Result<DetectionSummary> {
    let detected = Detector::built_in().into_diagnostic()?.detect(
        artifacts
            .detection
            .iter()
            .map(|artifact| artifact.as_detection()),
    );
    let mut enabled_domains = BTreeSet::from([GENERAL_DOMAIN.to_string()]);
    if detected.domain != GENERAL_DOMAIN {
        enabled_domains.insert(detected.domain.clone());
    }
    let mut summary = DetectionSummary {
        detected_domain: detected.domain,
        enabled_domains,
        languages: detect_languages(&artifacts.paths),
        package_managers: detect_package_managers(&artifacts.paths),
        formatters: detect_formatters(&artifacts.paths),
        git_layout: detect_git_layout(root, &artifacts.paths),
        toolchain_files: detect_toolchain_files(&artifacts.paths),
        agent_configs: detect_agent_configs(&artifacts.paths),
        sensitive_paths: artifacts
            .paths
            .iter()
            .filter(|path| is_sensitive_path(path))
            .cloned()
            .collect(),
        generated_paths: artifacts
            .paths
            .iter()
            .filter(|path| is_generated_path(path))
            .cloned()
            .collect(),
        uncertain: BTreeSet::new(),
    };
    populate_uncertain(&mut summary);
    Ok(summary)
}

fn populate_uncertain(summary: &mut DetectionSummary) {
    if summary.detected_domain == GENERAL_DOMAIN {
        summary
            .uncertain
            .insert("domain classification".to_string());
    }
    if summary.formatters.is_empty() {
        summary.uncertain.insert("formatter command".to_string());
    }
    if summary.package_managers.is_empty() {
        summary.uncertain.insert("package manager".to_string());
    }
}

fn detect_languages(paths: &BTreeSet<String>) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    insert_if(paths, &mut values, "Cargo.toml", "Rust");
    insert_if(paths, &mut values, "package.json", "TypeScript/JavaScript");
    insert_if(paths, &mut values, "pyproject.toml", "Python");
    insert_if(paths, &mut values, "go.mod", "Go");
    insert_if(paths, &mut values, "pom.xml", "Java");
    insert_suffix(paths, &mut values, ".csproj", ".NET");
    values
}

fn detect_package_managers(paths: &BTreeSet<String>) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    for (path, label) in [
        ("Cargo.lock", "cargo"),
        ("package-lock.json", "npm"),
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lockb", "bun"),
        ("uv.lock", "uv"),
        ("poetry.lock", "poetry"),
        ("go.mod", "go modules"),
    ] {
        insert_if(paths, &mut values, path, label);
    }
    values
}

fn detect_formatters(paths: &BTreeSet<String>) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    for (path, label) in [
        ("rustfmt.toml", "rustfmt"),
        (".prettierrc", "prettier"),
        ("biome.json", "biome"),
        ("ruff.toml", "ruff"),
        (".golangci.yml", "gofumpt/golangci-lint"),
    ] {
        insert_if(paths, &mut values, path, label);
    }
    insert_suffix(paths, &mut values, ".clang-format", "clang-format");
    values
}

fn detect_git_layout(root: &Path, paths: &BTreeSet<String>) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    insert_if(paths, &mut values, ".git", "Git repository");
    insert_if(paths, &mut values, ".gitmodules", "submodules");
    insert_if(
        paths,
        &mut values,
        ".gitattributes",
        "Git attributes/LFS possible",
    );
    if root.join("crates").is_dir() && root.join("Cargo.toml").is_file() {
        values.insert("Cargo workspace/monorepo".to_string());
    }
    values
}

fn detect_toolchain_files(paths: &BTreeSet<String>) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    for (path, label) in [
        ("rust-toolchain.toml", "rust-toolchain"),
        (".mise.toml", "mise"),
        (".tool-versions", "asdf"),
        ("flake.nix", "nix flakes"),
        (".devcontainer/devcontainer.json", "devcontainer"),
        ("Dockerfile", "Dockerfile"),
        ("docker-compose.yml", "docker compose"),
    ] {
        insert_if(paths, &mut values, path, label);
    }
    values
}

fn detect_agent_configs(paths: &BTreeSet<String>) -> BTreeSet<String> {
    let mut values = BTreeSet::new();
    for (path, label) in [
        ("AGENTS.md", "AGENTS.md"),
        ("CLAUDE.md", "Claude instructions"),
        ("GEMINI.md", "Gemini instructions"),
        (".mcp.json", "MCP config"),
        (".vscode/mcp.json", "VS Code MCP config"),
        (".cursor/rules", "Cursor rules"),
        (".github/copilot-instructions.md", "Copilot instructions"),
    ] {
        insert_if(paths, &mut values, path, label);
    }
    insert_prefix(paths, &mut values, ".agents/skills/", "Agent Skills");
    insert_prefix(paths, &mut values, ".vscode/", "VS Code config");
    values
}

fn is_sensitive_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("/secrets/")
        || lower.starts_with("secrets/")
        || lower.contains("/.aws/")
        || lower.ends_with("env.local")
        || lower == ".env"
        || lower.ends_with("/.env")
        || lower.contains("credentials")
        || lower.contains("id_rsa")
}

fn insert_if(paths: &BTreeSet<String>, values: &mut BTreeSet<String>, path: &str, label: &str) {
    if paths.contains(path) {
        values.insert(label.to_string());
    }
}

fn insert_prefix(
    paths: &BTreeSet<String>,
    values: &mut BTreeSet<String>,
    prefix: &str,
    label: &str,
) {
    if paths.iter().any(|path| path.starts_with(prefix)) {
        values.insert(label.to_string());
    }
}

fn insert_suffix(
    paths: &BTreeSet<String>,
    values: &mut BTreeSet<String>,
    suffix: &str,
    label: &str,
) {
    if paths.iter().any(|path| path.ends_with(suffix)) {
        values.insert(label.to_string());
    }
}
