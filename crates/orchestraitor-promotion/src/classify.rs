//! Descriptive classification of changed paths and content (spec §9.14).
//!
//! This module categorizes worker output into the 17 output classes defined by
//! [`orchestraitor_model::OutputClass`]. The classification is **descriptive** —
//! it is input to Arbitraitor's policy engine, never an enforcement decision
//! (spec §9.14, §2.2).

use orchestraitor_core::is_redacted_field;
use orchestraitor_model::OutputClass;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Filesystem metadata available to the classifier.
///
/// The trusted controller supplies this; Orchestraitor never stats the worker
/// filesystem directly (spec §6.2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMetadata {
    /// Whether the file has the executable bit set.
    pub is_executable: bool,
    /// Whether the path is a symbolic link.
    pub is_symlink: bool,
    /// Whether the path is a device node or other special file.
    pub is_device: bool,
}

/// Input for classifying a single changed artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassificationInput<'a> {
    /// Repository-relative path of the changed artifact.
    pub path: &'a Path,
    /// File content, if available. Binary content is classified by path and
    /// metadata alone.
    pub content: Option<&'a [u8]>,
    /// Filesystem metadata from the trusted controller.
    pub metadata: Option<&'a FileMetadata>,
}

/// The result of classifying a changed artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// The assigned output class.
    pub output_class: OutputClass,
    /// Repository-relative path that was classified.
    pub path: String,
    /// Whether the content matched credential-shaped patterns.
    pub credential_shaped: bool,
}

/// Classifies a changed path and optional content into an [`OutputClass`].
///
/// Classification precedence follows security sensitivity: device nodes and
/// symlinks are detected first (they are always refused or normalized), then
/// credential-shaped content, then path-based classes, then content-based
/// generated-source markers, then the executable bit, defaulting to ordinary
/// source.
///
/// # Errors
///
/// Returns [`PromotionError::Classification`] only if the path cannot be
/// rendered as UTF-8.
pub fn classify(input: &ClassificationInput<'_>) -> Result<Classification, PromotionError> {
    let path_str = input
        .path
        .to_str()
        .ok_or_else(|| PromotionError::Classification {
            path: format!("{}", input.path.display()),
        })?;
    let metadata = input.metadata.cloned().unwrap_or_default();
    let content_text = input
        .content
        .and_then(|bytes| std::str::from_utf8(bytes).ok());

    let credential_shaped = content_text.is_some_and(is_credential_shaped);

    let output_class = if metadata.is_device {
        OutputClass::DeviceOrSpecialFile
    } else if metadata.is_symlink {
        OutputClass::Symlink
    } else if credential_shaped {
        OutputClass::CredentialShapedData
    } else {
        classify_by_path(path_str, &metadata)
            .or_else(|| content_text.and_then(classify_by_content))
            .unwrap_or(if metadata.is_executable {
                OutputClass::Executable
            } else {
                OutputClass::OrdinarySource
            })
    };

    Ok(Classification {
        output_class,
        path: path_str.to_string(),
        credential_shaped,
    })
}

/// Path-based classification — returns `None` when no path rule matches.
fn classify_by_path(path: &str, metadata: &FileMetadata) -> Option<OutputClass> {
    let lower = path.to_ascii_lowercase();
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);

    if path.starts_with(".git/hooks/") {
        return Some(OutputClass::GitHook);
    }
    if matches!(file_name, ".envrc") || file_name == ".env" || file_name.starts_with(".env.") {
        return Some(OutputClass::EnvironmentFile);
    }
    if is_shell_config(file_name) {
        return Some(OutputClass::ShellConfiguration);
    }
    if matches!(
        file_name,
        ".gitconfig" | ".gitattributes" | ".gitignore" | ".gitmodules"
    ) {
        return Some(OutputClass::GitConfiguration);
    }
    if path.starts_with(".vscode/")
        || path.starts_with(".idea/")
        || file_name.ends_with(".code-workspace")
    {
        return Some(OutputClass::IdeConfiguration);
    }
    if matches!(
        file_name,
        "agents.md" | "claude.md" | "copilot.md" | ".cursorrules" | ".windsurfrules"
    ) || path.starts_with(".agents/")
    {
        return Some(OutputClass::AgentConfiguration);
    }
    if path.starts_with(".github/workflows/")
        || matches!(file_name, ".gitlab-ci.yml" | "jenkinsfile" | ".drone.yml")
        || path.starts_with(".circleci/")
    {
        return Some(OutputClass::CiWorkflow);
    }
    if is_build_plugin(file_name) {
        return Some(OutputClass::BuildSystemPlugin);
    }
    if is_lockfile(file_name) {
        return Some(OutputClass::DependencyLockfile);
    }
    if is_package_archive(file_name) {
        return Some(OutputClass::PackageArchive);
    }
    if is_test_path(path, file_name) {
        return Some(OutputClass::Tests);
    }
    if metadata.is_executable {
        return Some(OutputClass::Executable);
    }
    None
}

/// Content-based classification — currently only detects generated-source markers.
fn classify_by_content(content: &str) -> Option<OutputClass> {
    let head = content.lines().take(20).collect::<Vec<_>>().join("\n");
    if head.contains("@generated")
        || head.contains("Code generated")
        || head.contains("DO NOT EDIT")
        || head.contains("Auto-generated")
        || head.contains("Automatically generated")
    {
        return Some(OutputClass::GeneratedSource);
    }
    None
}

/// Returns `true` when content matches credential-shaped patterns.
///
/// Detects PEM private-key blocks and `KEY=value` assignments where the key
/// matches the workspace redaction field set (spec §9.23.4).
fn is_credential_shaped(content: &str) -> bool {
    if content.contains("-----BEGIN ") && content.contains(" PRIVATE KEY-----") {
        return true;
    }
    content.lines().take(50).any(|line| {
        if let Some((key, _)) = line.split_once('=') {
            let trimmed = key.trim();
            if !trimmed.is_empty() && is_redacted_field(trimmed) {
                return true;
            }
        }
        false
    })
}

fn is_shell_config(file_name: &str) -> bool {
    matches!(
        file_name,
        ".bashrc"
            | ".zshrc"
            | ".bash_profile"
            | ".zprofile"
            | ".profile"
            | ".bash_aliases"
            | ".bash_login"
            | ".inputrc"
    )
}

fn is_build_plugin(file_name: &str) -> bool {
    matches!(
        file_name,
        "build.rs"
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
}

fn is_lockfile(file_name: &str) -> bool {
    matches!(
        file_name,
        "cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "poetry.lock"
            | "gemfile.lock"
            | "composer.lock"
            | "go.sum"
            | "flake.lock"
            | "uv.lock"
    )
}

fn is_package_archive(file_name: &str) -> bool {
    [
        ".tar", ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tar.xz", ".txz", ".zip", ".whl", ".egg",
        ".7z", ".rar", ".gz", ".bz2", ".xz",
    ]
    .iter()
    .any(|ext| file_name.ends_with(ext))
}

fn is_test_path(path: &str, file_name: &str) -> bool {
    path.contains("/tests/")
        || path.contains("/test/")
        || path.starts_with("tests/")
        || path.starts_with("test/")
        || file_name.starts_with("test_")
        || file_name.starts_with("test-")
        || file_name.ends_with("_test.go")
        || file_name.ends_with(".test.ts")
        || file_name.ends_with(".test.tsx")
        || file_name.ends_with(".test.js")
        || file_name.ends_with(".spec.ts")
        || file_name.ends_with(".spec.tsx")
        || file_name.ends_with(".spec.js")
        || file_name.ends_with("_test.rs")
}

use crate::PromotionError;
