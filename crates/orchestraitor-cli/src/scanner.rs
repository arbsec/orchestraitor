use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use miette::{Context, IntoDiagnostic, Result};
use orchestraitor_agent_catalog::DetectionArtifact;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalArtifacts {
    pub(crate) detection: Vec<OwnedArtifact>,
    pub(crate) paths: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnedArtifact {
    path: String,
    contents: Option<String>,
}

impl LocalArtifacts {
    pub(crate) fn collect(root: &Path) -> Result<Self> {
        let mut artifacts = Self {
            detection: Vec::new(),
            paths: BTreeSet::new(),
        };
        collect_dir(root, root, &mut artifacts)?;
        Ok(artifacts)
    }
}

impl OwnedArtifact {
    pub(crate) fn as_detection(&self) -> DetectionArtifact<'_> {
        DetectionArtifact {
            path: self.path.as_str(),
            contents: self.contents.as_deref(),
        }
    }
}

fn collect_dir(root: &Path, dir: &Path, artifacts: &mut LocalArtifacts) -> Result<()> {
    let mut entries = fs::read_dir(dir)
        .into_diagnostic()
        .wrap_err("failed to read project directory")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .into_diagnostic()?;
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        let relative = relative_path(root, &path)?;
        if relative == ".git" {
            artifacts.paths.insert(relative);
            continue;
        }
        artifacts.paths.insert(relative.clone());
        if path.is_dir() {
            if is_generated_path(&relative) {
                continue;
            }
            collect_dir(root, &path, artifacts)?;
        } else {
            artifacts.detection.push(OwnedArtifact {
                path: relative,
                contents: read_detection_contents(&path),
            });
        }
    }
    Ok(())
}

fn read_detection_contents(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    if matches!(
        name.as_ref(),
        "Cargo.toml" | "package.json" | "pyproject.toml"
    ) {
        return fs::read_to_string(path).ok();
    }
    None
}

fn relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .into_diagnostic()
        .wrap_err("failed to compute repository-relative path")?;
    Ok(relative
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

pub(crate) fn is_generated_path(path: &str) -> bool {
    path.split('/').any(|component| {
        matches!(
            component,
            "node_modules" | "target" | "dist" | "build" | ".next" | "coverage"
        )
    })
}
