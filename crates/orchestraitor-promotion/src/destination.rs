//! Trust-sensitive destination detection (spec §9.14).
//!
//! A destination is trust-sensitive when it lives in a location that affects
//! host configuration, tool execution, CI, or the agent harness itself. This
//! is a **descriptive** flag — it is input to Arbitraitor's policy engine, not
//! an enforcement decision (spec §9.14, §2.2).

use std::path::Path;

/// Whether a destination path is in a trust-sensitive location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationSensitivity {
    /// Ordinary project source — not trust-sensitive.
    Ordinary,
    /// The path affects host config, tool execution, CI, or the harness.
    /// Promotion to this destination requires heightened review.
    TrustSensitive,
}

impl DestinationSensitivity {
    /// Returns `true` when the destination is trust-sensitive.
    #[must_use]
    pub const fn is_trust_sensitive(self) -> bool {
        matches!(self, Self::TrustSensitive)
    }
}

/// Detects whether a destination path is trust-sensitive.
///
/// Trust-sensitive destinations include: shell configuration, Git hooks, Git
/// configuration, IDE configuration, agent configuration, CI workflows,
/// build-system plugins, environment files, and credential-shaped paths.
/// Ordinary source, tests, generated source, executables, package archives,
/// and lockfiles are not trust-sensitive by destination alone.
#[must_use]
pub fn detect_sensitivity(path: &Path) -> DestinationSensitivity {
    let lower = path
        .to_str()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);

    let trust_sensitive = path.starts_with(".git/hooks/")
        || path.starts_with(".git/")
        || path.starts_with(".vscode/")
        || path.starts_with(".idea/")
        || path.starts_with(".agents/")
        || path.starts_with(".github/workflows/")
        || path.starts_with(".circleci/")
        || matches!(
            file_name,
            ".envrc"
                | ".env"
                | ".bashrc"
                | ".zshrc"
                | ".bash_profile"
                | ".zprofile"
                | ".profile"
                | ".bash_aliases"
                | ".bash_login"
                | ".inputrc"
                | ".gitconfig"
                | ".gitattributes"
                | ".gitignore"
                | ".gitmodules"
                | "agents.md"
                | "claude.md"
                | "copilot.md"
                | ".cursorrules"
                | ".windsurfrules"
                | ".gitlab-ci.yml"
                | "jenkinsfile"
                | ".drone.yml"
                | "build.rs"
                | "makefile"
                | "cmakelists.txt"
                | "build.gradle"
                | "build.gradle.kts"
                | "pom.xml"
                | "build.sbt"
                | "taskfile.yml"
                | "taskfile.yaml"
                | "justfile"
                | "gulpfile.js"
                | "gruntfile.js"
                | "webpack.config.js"
                | "vite.config.ts"
                | "vite.config.js"
                | "rollup.config.js"
                | "esbuild.config.js"
        )
        || file_name.starts_with(".env.")
        || file_name.ends_with(".code-workspace");

    if trust_sensitive {
        DestinationSensitivity::TrustSensitive
    } else {
        DestinationSensitivity::Ordinary
    }
}
