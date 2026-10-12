//! Node-id resolution cache stored outside the repository (spec §9.43).
//!
//! Cache path: `$XDG_CACHE_HOME/orchestraitor/gh-project-fields.json` with a
//! `~/.cache/orchestraitor/` fallback — the same convention the
//! github-project-workflow skill documents for its scripts. The file contains
//! only GitHub node IDs keyed by names; it carries no secrets and is never
//! committed.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name of the node-id cache within the orchestraitor cache directory.
const CACHE_FILE: &str = "gh-project-fields.json";

const CACHE_FORMAT_VERSION: u32 = 1;

/// On-disk cache file mapping project identity to resolved node IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeIdCacheFile {
    /// Cache format version.
    version: u32,
    /// Resolved node IDs keyed by `<organization>/<project-number>`.
    #[serde(default)]
    projects: BTreeMap<String, ProjectNodeIds>,
}

impl Default for NodeIdCacheFile {
    fn default() -> Self {
        Self {
            version: CACHE_FORMAT_VERSION,
            projects: BTreeMap::new(),
        }
    }
}

/// Resolved node IDs for one project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectNodeIds {
    /// `ProjectV2` node id.
    pub project_id: String,
    /// Single-select field IDs keyed by field name.
    #[serde(default)]
    pub fields: BTreeMap<String, FieldNodeIds>,
}

/// Resolved node IDs for one single-select field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldNodeIds {
    /// Field node id.
    pub field_id: String,
    /// Option node ids keyed by option name.
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

impl NodeIdCacheFile {
    /// Reads the cache file; a missing or corrupt file resolves to empty.
    ///
    /// # Errors
    ///
    /// Returns `BoardError::CacheIo` only for genuine I/O failures (the file
    /// exists but cannot be read). Corrupt JSON is treated as a cache miss so
    /// IDs are re-resolved from the API.
    pub fn load(path: &Path) -> Result<Self, crate::BoardError> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(crate::BoardError::CacheIo {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(cache) if cache.version == CACHE_FORMAT_VERSION => Ok(cache),
            _ => Ok(Self::default()),
        }
    }

    /// Writes the cache file, creating the parent directory as needed.
    ///
    /// # Errors
    ///
    /// Returns `BoardError::CacheIo` when the directory or file cannot be
    /// written.
    pub fn store(&self, path: &Path) -> Result<(), crate::BoardError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| crate::BoardError::CacheIo {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut bytes =
            serde_json::to_vec_pretty(self).map_err(|source| crate::BoardError::CacheIo {
                path: path.to_path_buf(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, source),
            })?;
        bytes.push(b'\n');
        fs::write(path, bytes).map_err(|source| crate::BoardError::CacheIo {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Returns cached node IDs for `(organization, project_number)`.
    #[must_use]
    pub fn project(&self, organization: &str, project_number: u64) -> Option<&ProjectNodeIds> {
        self.projects
            .get(&format!("{organization}/{project_number}"))
    }

    /// Inserts or replaces node IDs for `(organization, project_number)`.
    pub fn upsert(&mut self, organization: &str, project_number: u64, ids: ProjectNodeIds) {
        self.projects
            .insert(format!("{organization}/{project_number}"), ids);
    }
}

/// Computes the default cache file path following the XDG base directory
/// specification. Returns `None` when neither `$XDG_CACHE_HOME` nor `$HOME`
/// is set (the client then runs without a disk cache).
#[must_use]
pub fn default_cache_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".cache"))
        })?;
    Some(base.join("orchestraitor").join(CACHE_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_cache_is_a_miss_not_an_error() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("gh-project-fields.json");
        fs::write(&path, b"{not json")?;
        let cache = NodeIdCacheFile::load(&path)?;
        assert!(cache.project("arbsec", 1).is_none());
        Ok(())
    }

    #[test]
    fn round_trip_preserves_ids() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("nested/gh-project-fields.json");
        let mut cache = NodeIdCacheFile::default();
        let mut fields = BTreeMap::new();
        fields.insert(
            "Status".to_string(),
            FieldNodeIds {
                field_id: "PVTSSF_example".to_string(),
                options: BTreeMap::from([("Ready".to_string(), "opt_ready".to_string())]),
            },
        );
        cache.upsert(
            "arbsec",
            1,
            ProjectNodeIds {
                project_id: "PVT_example".to_string(),
                fields,
            },
        );
        cache.store(&path)?;

        let loaded = NodeIdCacheFile::load(&path)?;
        let project = loaded.project("arbsec", 1);
        assert!(project.is_some());
        Ok(())
    }
}
