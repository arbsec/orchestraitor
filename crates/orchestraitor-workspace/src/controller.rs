use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gix::bstr::ByteSlice;

use crate::history::{
    BlameLine, HistoryDiff, LogEntry, PathChange, PathChangeKind, collect_revision_entries,
};
use crate::materialize::{
    MaterializeContext, digest_path, ensure_no_dot_git, materialize_tree, open_dest,
    prepare_destination,
};
use crate::types::{
    FileDigest, ReconciliationReport, Result, Snapshot, SnapshotOptions, WorkspaceError,
};

/// Trusted controller for snapshot workspaces and Git history RPCs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceController {
    repo_path: PathBuf,
}

impl WorkspaceController {
    /// Creates a controller for an existing trusted repository path.
    #[must_use]
    pub fn new(repo_path: impl Into<PathBuf>) -> Self {
        Self {
            repo_path: repo_path.into(),
        }
    }

    /// Exports `base_commit` into `dest_dir` without exposing `.git` to workers.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the repository is untrusted, the revision
    /// cannot be resolved, or a tree entry would escape the snapshot root.
    pub fn create_snapshot(&self, base_commit: &str, dest_dir: &Path) -> Result<Snapshot> {
        self.create_snapshot_with_options(base_commit, dest_dir, &SnapshotOptions::default())
    }

    /// Exports `base_commit` into `dest_dir` with sparse path filtering.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when repository access, object loading, or
    /// materialization fails.
    pub fn create_snapshot_with_options(
        &self,
        base_commit: &str,
        dest_dir: &Path,
        options: &SnapshotOptions,
    ) -> Result<Snapshot> {
        let repo = self.open_repo()?;
        let commit_id = repo.rev_parse_single(base_commit).map_err(|source| {
            WorkspaceError::ResolveRevision {
                revision: base_commit.to_owned(),
                source: Box::new(source),
            }
        })?;
        let commit = commit_id
            .object()
            .map_err(|source| WorkspaceError::LoadObject {
                object: base_commit.to_owned(),
                source: Box::new(source),
            })?
            .try_into_commit()
            .map_err(|source| WorkspaceError::LoadObject {
                object: base_commit.to_owned(),
                source: Box::new(source),
            })?;
        let tree = commit.tree().map_err(|source| WorkspaceError::LoadObject {
            object: format!("{}^{{tree}}", commit_id.to_hex()),
            source: Box::new(source),
        })?;

        prepare_destination(dest_dir)?;
        let dest = open_dest(dest_dir)?;
        let manifest = RefCell::new(BTreeMap::<PathBuf, FileDigest>::new());
        let context = MaterializeContext {
            root: dest_dir,
            options,
            manifest: &manifest,
        };
        materialize_tree(&repo, &tree, &dest, Path::new(""), &context)?;
        ensure_no_dot_git(dest_dir)?;

        Ok(Snapshot {
            base_commit: commit_id.to_hex().to_string(),
            source_head_at_creation: self.current_head()?,
            workspace_root: dest_dir.to_path_buf(),
            manifest: manifest.into_inner(),
        })
    }

    /// Computes path-level diff data through trusted `gix` access.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when either revision cannot be resolved.
    pub fn diff(&self, old_revision: &str, new_revision: &str) -> Result<HistoryDiff> {
        let repo = self.open_repo()?;
        let old = collect_revision_entries(&repo, old_revision)?;
        let new = collect_revision_entries(&repo, new_revision)?;
        let mut changes = Vec::new();

        for (path, old_entry) in &old {
            match new.get(path) {
                Some(new_entry) if new_entry == old_entry => {}
                Some(_) => changes.push(PathChange {
                    path: path.clone(),
                    kind: PathChangeKind::Modified,
                }),
                None => changes.push(PathChange {
                    path: path.clone(),
                    kind: PathChangeKind::Removed,
                }),
            }
        }
        for path in new.keys().filter(|path| !old.contains_key(*path)) {
            changes.push(PathChange {
                path: path.clone(),
                kind: PathChangeKind::Added,
            });
        }
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(HistoryDiff {
            old_revision: old_revision.to_owned(),
            new_revision: new_revision.to_owned(),
            changes,
        })
    }

    /// Returns commit summaries reachable from `revision`.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the revision or commits cannot be read.
    pub fn log(&self, revision: &str, max_entries: usize) -> Result<Vec<LogEntry>> {
        let repo = self.open_repo()?;
        let start =
            repo.rev_parse_single(revision)
                .map_err(|source| WorkspaceError::ResolveRevision {
                    revision: revision.to_owned(),
                    source: Box::new(source),
                })?;
        let mut entries = Vec::new();
        let mut next = Some(start.detach());

        while let Some(id) = next.take() {
            if entries.len() >= max_entries {
                break;
            }
            let commit = repo
                .find_commit(id)
                .map_err(|source| WorkspaceError::LoadObject {
                    object: id.to_hex().to_string(),
                    source: Box::new(source),
                })?;
            let raw_message =
                commit
                    .message_raw()
                    .map_err(|source| WorkspaceError::LoadObject {
                        object: id.to_hex().to_string(),
                        source: Box::new(source),
                    })?;
            let summary = raw_message.lines().next().map_or_else(String::new, |line| {
                String::from_utf8_lossy(line).to_string()
            });
            next = commit.parent_ids().next().map(gix::Id::detach);
            entries.push(LogEntry {
                id: id.to_hex().to_string(),
                summary,
            });
        }
        Ok(entries)
    }

    /// Returns deterministic line attribution for a file at `revision`.
    ///
    /// This MVP attribution is conservative: each line is attributed to the
    /// requested commit, keeping history access behind the controller RPC while
    /// later versions can replace the internals with full line-origin walking.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when the revision or file cannot be read.
    pub fn blame(&self, revision: &str, path: &Path) -> Result<Vec<BlameLine>> {
        let repo = self.open_repo()?;
        let commit_id =
            repo.rev_parse_single(revision)
                .map_err(|source| WorkspaceError::ResolveRevision {
                    revision: revision.to_owned(),
                    source: Box::new(source),
                })?;
        let entries = collect_revision_entries(&repo, revision)?;
        let entry = entries
            .get(path)
            .ok_or_else(|| WorkspaceError::LoadObject {
                object: path.display().to_string(),
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "path not found in revision",
                )),
            })?;
        let text = String::from_utf8_lossy(&entry.data);
        Ok(text
            .lines()
            .enumerate()
            .map(|(index, line)| BlameLine {
                line_number: index.saturating_add(1),
                commit_id: commit_id.to_hex().to_string(),
                text: line.to_owned(),
            })
            .collect())
    }

    /// Detects base-branch drift and worker-directory mutations by digest.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceError`] when source `HEAD` or snapshot files cannot be
    /// inspected.
    pub fn reconcile_external_mutations(
        &self,
        snapshot: &Snapshot,
    ) -> Result<ReconciliationReport> {
        let current_source_head = self.current_head()?;
        let base_branch_drifted = current_source_head != snapshot.source_head_at_creation;
        let mut external_mutations = Vec::new();

        for (path, expected) in &snapshot.manifest {
            let actual = digest_path(&snapshot.workspace_root.join(path))?;
            if actual != *expected {
                external_mutations.push(path.clone());
            }
        }

        Ok(ReconciliationReport {
            base_branch_drifted,
            current_source_head,
            external_mutations,
        })
    }

    fn open_repo(&self) -> Result<gix::Repository> {
        let repo = gix::open_opts(
            &self.repo_path,
            gix::open::Options::default().bail_if_untrusted(true),
        )
        .map_err(|source| WorkspaceError::OpenRepository(Box::new(source)))?;
        reject_fsmonitor_config(&self.repo_path)?;
        Ok(repo)
    }

    fn current_head(&self) -> Result<String> {
        let repo = self.open_repo()?;
        let head =
            repo.rev_parse_single("HEAD")
                .map_err(|source| WorkspaceError::ResolveRevision {
                    revision: "HEAD".to_owned(),
                    source: Box::new(source),
                })?;
        Ok(head.to_hex().to_string())
    }
}

fn reject_fsmonitor_config(repo_path: &Path) -> Result<()> {
    let config_path = repo_path.join(".git").join("config");
    let config = match std::fs::read_to_string(&config_path) {
        Ok(config) => config,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(WorkspaceError::OpenRepository(Box::new(source)));
        }
    };
    if config.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("fsmonitor") || trimmed.starts_with("core.fsmonitor")
    }) {
        return Err(WorkspaceError::OpenRepository(Box::new(
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "core.fsmonitor is not allowed for trusted controller snapshots",
            ),
        )));
    }
    Ok(())
}
