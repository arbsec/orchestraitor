//! Worker task model and the pluggable task-loading seam.
//!
//! `--task <id>` resolves a task description through [`TaskSource`]. The
//! bootstrap ships [`FixtureTaskSource`] (JSON files under a task directory);
//! DAG/backlog integration replaces the source in a later lane without
//! touching the loop.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A leaf task handed to the worker loop.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorkerTask {
    /// Stable task identifier (matches the requested id).
    pub id: String,
    /// Filesystem-safe slug used for evidence/delivery naming.
    pub slug: String,
    /// Task description the loop works against (untrusted content, spec
    /// `40-arbitraitor-integration.md` §6.1: carried as data, never executed).
    pub description: String,
}

/// Pluggable task-loading seam (DAG/backlog integration lands in a later lane).
pub trait TaskSource {
    /// Loads the task identified by `task_id`.
    ///
    /// # Errors
    ///
    /// Returns [`TaskLoadError`] when the id is invalid, unknown, or the
    /// stored task is malformed.
    fn load(&self, task_id: &str) -> Result<WorkerTask, TaskLoadError>;
}

/// Task-loading failures; reason codes are static and log-safe.
#[derive(Debug, Error)]
pub enum TaskLoadError {
    /// The requested id is not a safe task identifier.
    #[error("invalid task id: {reason}")]
    InvalidId {
        /// Static reason code (`empty`, `too-long`, `bad-characters`).
        reason: &'static str,
    },
    /// No task exists for the requested id.
    #[error("unknown task id")]
    Unknown,
    /// The stored task file could not be read or parsed, or its embedded id
    /// does not match the requested id (fail closed on mismatch).
    #[error("stored task is malformed: {reason}")]
    Malformed {
        /// Static reason code (`read-io`, `invalid-json`, `id-mismatch`).
        reason: &'static str,
    },
}

/// Fixture task source: `<dir>/<task-id>.json` files holding [`WorkerTask`] JSON.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureTaskSource {
    dir: PathBuf,
}

impl FixtureTaskSource {
    /// Creates a fixture source rooted at `dir`.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Returns the directory tasks load from.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl TaskSource for FixtureTaskSource {
    fn load(&self, task_id: &str) -> Result<WorkerTask, TaskLoadError> {
        validate_task_id(task_id)?;
        let path = self.dir.join(format!("{task_id}.json"));
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(TaskLoadError::Unknown);
            }
            Err(_) => return Err(TaskLoadError::Malformed { reason: "read-io" }),
        };
        let task: WorkerTask =
            serde_json::from_slice(&bytes).map_err(|_| TaskLoadError::Malformed {
                reason: "invalid-json",
            })?;
        if task.id != task_id {
            return Err(TaskLoadError::Malformed {
                reason: "id-mismatch",
            });
        }
        validate_task_id(&task.slug).map_err(|_| TaskLoadError::Malformed {
            reason: "invalid-slug",
        })?;
        Ok(task)
    }
}

/// Validates a task id (also reused for slugs): 1–64 chars, ASCII
/// alphanumeric start, then alphanumerics plus `.`/`_`/`-`. The id becomes a
/// file name, so anything else fails closed against path traversal.
fn validate_task_id(task_id: &str) -> Result<(), TaskLoadError> {
    if task_id.is_empty() {
        return Err(TaskLoadError::InvalidId { reason: "empty" });
    }
    if task_id.len() > 64 {
        return Err(TaskLoadError::InvalidId { reason: "too-long" });
    }
    let mut chars = task_id.chars();
    let Some(first) = chars.next() else {
        return Err(TaskLoadError::InvalidId { reason: "empty" });
    };
    if !first.is_ascii_alphanumeric() {
        return Err(TaskLoadError::InvalidId {
            reason: "bad-characters",
        });
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) {
        return Err(TaskLoadError::InvalidId {
            reason: "bad-characters",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn write_task(dir: &Path, id: &str, slug: &str) {
        let task = WorkerTask {
            id: id.to_string(),
            slug: slug.to_string(),
            description: "do the thing".to_string(),
        };
        let json = serde_json::to_string(&task).unwrap();
        fs::write(dir.join(format!("{id}.json")), json).unwrap();
    }

    #[test]
    fn fixture_source_round_trips_a_task() {
        let temp = tempfile::tempdir().unwrap();
        write_task(temp.path(), "t-1", "t-1-slug");
        let source = FixtureTaskSource::new(temp.path().to_path_buf());
        let task = source.load("t-1").unwrap();
        assert_eq!(task.id, "t-1");
        assert_eq!(task.slug, "t-1-slug");
    }

    #[test]
    fn fixture_source_rejects_traversal_shaped_ids() {
        let temp = tempfile::tempdir().unwrap();
        let source = FixtureTaskSource::new(temp.path().to_path_buf());
        for bad in ["../escape", "a/b", "", ".hidden", "-lead", "a b"] {
            assert!(
                matches!(source.load(bad), Err(TaskLoadError::InvalidId { .. })),
                "id {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn fixture_source_unknown_id_is_typed() {
        let temp = tempfile::tempdir().unwrap();
        let source = FixtureTaskSource::new(temp.path().to_path_buf());
        assert!(matches!(source.load("nope"), Err(TaskLoadError::Unknown)));
    }

    #[test]
    fn fixture_source_fails_closed_on_id_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        write_task(temp.path(), "other", "slug");
        let bytes = fs::read_to_string(temp.path().join("other.json")).unwrap();
        fs::write(
            temp.path().join("requested.json"),
            bytes.replace("\"other\"", "\"different\""),
        )
        .unwrap();
        let source = FixtureTaskSource::new(temp.path().to_path_buf());
        assert!(matches!(
            source.load("requested"),
            Err(TaskLoadError::Malformed {
                reason: "id-mismatch"
            })
        ));
    }
}
