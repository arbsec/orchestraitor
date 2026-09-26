//! Board project configuration loaded from
//! `.agents/project/github-project.local.toml`.
//!
//! The file carries human-readable NAME identity only — organization, project
//! number, field and option names, and the auth secret reference. GitHub node
//! IDs are never read from config: they are resolved at runtime via GraphQL
//! and cached outside the repository (spec §9.43).

use std::fs;
use std::path::{Path, PathBuf};

use orchestraitor_core::SecretUri;
use serde::Deserialize;

use crate::BoardError;

/// Config file name inside `.agents/project/`.
const CONFIG_FILE_NAME: &str = "github-project.local.toml";

/// Validated board identity and field/option names (spec §9.43, §9.40).
#[derive(Debug, Clone)]
pub struct BoardProjectConfig {
    /// Organization login that owns the shared project.
    pub organization: String,
    /// Project number within the organization.
    pub project_number: u64,
    /// Repositories whose issues are in scope (`org/name`), lowercase.
    pub repos: Vec<String>,
    /// Native issue-type names that are leaf-implementable (e.g. Task, Bug).
    pub leaf_types: Vec<String>,
    /// Board field name carrying the delivery target.
    pub target_field: String,
    /// Option name of the in-delivery target.
    pub target_value: String,
    /// Board field name carrying the workflow status.
    pub ready_field: String,
    /// Option name of the schedulable status.
    pub ready_value: String,
    /// Auth token reference, when configured.
    pub token_uri: Option<SecretUri>,
}

impl BoardProjectConfig {
    /// Finds `.agents/project/github-project.local.toml` by walking upward
    /// from `start`, then parses and validates it.
    ///
    /// The committed `github-project.example.toml` is documentation only and
    /// is never loaded, matching the skill-script convention: every board
    /// operation requires the operator's local copy.
    ///
    /// # Errors
    ///
    /// Returns a typed error when no local config is found, it cannot be read
    /// or parsed, or its values fail validation.
    pub fn load(start: &Path) -> Result<(Self, PathBuf), BoardError> {
        let path = locate_config(start).ok_or_else(|| BoardError::ConfigNotFound {
            searched_from: start.to_path_buf(),
        })?;
        let config = Self::load_from(&path)?;
        Ok((config, path))
    }

    /// Loads and validates the config at an exact path.
    ///
    /// # Errors
    ///
    /// Returns a typed error for I/O, parse, or validation failures.
    pub fn load_from(path: &Path) -> Result<Self, BoardError> {
        let text = fs::read_to_string(path).map_err(|source| BoardError::ConfigIo {
            path: path.to_path_buf(),
            source,
        })?;
        let raw: RawConfig = toml::from_str(&text).map_err(|source| BoardError::ConfigParse {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        raw.into_config(path)
    }

    /// Returns true when `repo` (any case) is one of the configured
    /// repositories.
    #[must_use]
    pub fn includes_repo(&self, repo: &str) -> bool {
        self.repos.iter().any(|known| known == &repo.to_lowercase())
    }
}

/// Walks ancestors of `start` until `.agents/project/github-project.local.toml`
/// is found, mirroring the skill scripts' `git rev-parse --show-toplevel`
/// lookup without shelling out.
fn locate_config(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(candidate) = dir {
        let path = candidate
            .join(".agents")
            .join("project")
            .join(CONFIG_FILE_NAME);
        if path.is_file() {
            return Some(path);
        }
        dir = candidate.parent();
    }
    None
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    project: Option<RawProject>,
    issue_types: Option<RawIssueTypes>,
    mvp: Option<RawMvp>,
    auth: Option<RawAuth>,
}

#[derive(Debug, Deserialize)]
struct RawProject {
    organization: Option<String>,
    number: Option<u64>,
    repos: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RawIssueTypes {
    leaf_implementable: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct RawMvp {
    target_field: Option<String>,
    target_value: Option<String>,
    ready_field: Option<String>,
    ready_value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawAuth {
    token: Option<String>,
}

impl RawConfig {
    fn into_config(self, path: &Path) -> Result<BoardProjectConfig, BoardError> {
        let invalid = |reason: &str| BoardError::ConfigInvalid {
            path: path.to_path_buf(),
            reason: reason.to_string(),
        };
        let required = |value: Option<String>, key: &str| -> Result<String, BoardError> {
            value
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
                .ok_or_else(|| invalid(&format!("{key} is required")))
        };
        let project = self
            .project
            .ok_or_else(|| invalid("[project] table is required"))?;
        let organization = required(project.organization, "[project].organization")?;
        let project_number = project
            .number
            .ok_or_else(|| invalid("[project].number is required"))?;
        let repos: Vec<String> = project
            .repos
            .unwrap_or_default()
            .into_iter()
            .map(|repo| repo.trim().to_lowercase())
            .filter(|repo| !repo.is_empty())
            .collect();
        if repos.is_empty() {
            return Err(invalid("[project].repos must list at least one repository"));
        }
        if repos
            .iter()
            .any(|repo| repo.matches('/').count() != 1 || repo.split('/').any(str::is_empty))
        {
            return Err(invalid("[project].repos entries must be `org/name`"));
        }
        let leaf_types: Vec<String> = self
            .issue_types
            .and_then(|types| types.leaf_implementable)
            .unwrap_or_default()
            .into_iter()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();
        if leaf_types.is_empty() {
            return Err(invalid("[issue_types].leaf_implementable is required"));
        }
        let mvp = self.mvp.ok_or_else(|| invalid("[mvp] table is required"))?;
        let target_field = required(mvp.target_field, "[mvp].target_field")?;
        let target_value = required(mvp.target_value, "[mvp].target_value")?;
        let ready_field = required(mvp.ready_field, "[mvp].ready_field")?;
        let ready_value = required(mvp.ready_value, "[mvp].ready_value")?;
        let token_uri = self
            .auth
            .and_then(|auth| auth.token)
            .filter(|token| !token.trim().is_empty())
            .map(|token| SecretUri::parse(token.trim()))
            .transpose()
            .map_err(|_source| invalid("[auth].token must be a `secret://` URI"))?;
        Ok(BoardProjectConfig {
            organization,
            project_number,
            repos,
            leaf_types,
            target_field,
            target_value,
            ready_field,
            ready_value,
            token_uri,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../.agents/project/github-project.example.toml");

    #[test]
    fn example_config_parses_and_validates() -> Result<(), BoardError> {
        let raw: RawConfig = toml::from_str(EXAMPLE).map_err(|source| BoardError::ConfigParse {
            path: PathBuf::from("github-project.example.toml"),
            source: Box::new(source),
        })?;
        let config = raw.into_config(Path::new("github-project.example.toml"))?;
        assert_eq!(config.organization, "arbsec");
        assert_eq!(config.project_number, 1);
        assert!(config.includes_repo("arbsec/orchestraitor"));
        assert!(config.includes_repo("ARBSEC/ORCHESTRAITOR"));
        assert!(!config.includes_repo("arbsec/arbitraitor"));
        assert_eq!(config.leaf_types, ["Task", "Bug"]);
        assert_eq!(config.target_field, "Target");
        assert_eq!(config.target_value, "MVP");
        assert_eq!(config.ready_field, "Status");
        assert_eq!(config.ready_value, "Ready");
        assert!(config.token_uri.is_none());
        Ok(())
    }

    #[test]
    fn missing_required_keys_are_typed_errors() -> Result<(), BoardError> {
        let text = "[project]\norganization = \"arbsec\"\n";
        let raw: RawConfig = toml::from_str(text).map_err(|source| BoardError::ConfigParse {
            path: PathBuf::from("test.toml"),
            source: Box::new(source),
        })?;
        let result = raw.into_config(Path::new("test.toml"));
        assert!(matches!(result, Err(BoardError::ConfigInvalid { .. })));
        Ok(())
    }

    #[test]
    fn discovery_walks_upward() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let nested = temp.path().join("a/b/c");
        fs::create_dir_all(&nested)?;
        let config_dir = temp.path().join(".agents/project");
        fs::create_dir_all(&config_dir)?;
        fs::write(config_dir.join(CONFIG_FILE_NAME), "[project]\n")?;
        assert_eq!(
            locate_config(&nested),
            Some(config_dir.join(CONFIG_FILE_NAME))
        );
        assert_eq!(locate_config(Path::new("/nonexistent-absolute")), None);
        Ok(())
    }
}
