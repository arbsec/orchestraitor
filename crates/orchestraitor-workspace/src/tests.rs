use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{WorkspaceController, WorkspaceError};

#[test]
fn snapshot_has_no_dot_git() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a trusted repository with one tracked file.
    let repo = TestRepo::new()?;
    repo.write("src/lib.rs", "pub fn answer() -> u8 { 42 }\n")?;
    repo.git(&["add", "."])?;
    repo.git(&["commit", "-m", "initial"])?;
    let dest = repo.temp.path().join("snapshot");

    // When: creating a materialized snapshot.
    let snapshot = WorkspaceController::new(repo.path()).create_snapshot("HEAD", &dest);

    // Then: worker files exist and Git metadata is not exposed.
    let snapshot = snapshot?;
    assert!(snapshot.workspace_root.join("src/lib.rs").exists());
    assert!(!snapshot.workspace_root.join(".git").exists());
    Ok(())
}

#[test]
fn bail_if_untrusted_refuses_malicious_fsmonitor()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a repository config with an untrusted fsmonitor command.
    let repo = TestRepo::new()?;
    repo.write("file.txt", "safe\n")?;
    repo.git(&["add", "."])?;
    repo.git(&["commit", "-m", "initial"])?;
    repo.git(&["config", "core.fsmonitor", "sh -c 'exit 99'"])?;
    let dest = repo.temp.path().join("snapshot");

    // When: opening through the controller.
    let result = WorkspaceController::new(repo.path()).create_snapshot("HEAD", &dest);

    // Then: gix trust checks refuse the repository before exporting.
    assert!(matches!(result, Err(WorkspaceError::OpenRepository(_))));
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_cannot_escape_workspace_root() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a repository containing an escaping symlink.
    let repo = TestRepo::new()?;
    repo.symlink("escape", "../host-secret")?;
    repo.git(&["add", "."])?;
    repo.git(&["commit", "-m", "escaping symlink"])?;
    let dest = repo.temp.path().join("snapshot");

    // When: creating a snapshot.
    let result = WorkspaceController::new(repo.path()).create_snapshot("HEAD", &dest);

    // Then: the controller rejects the escape instead of materializing it.
    assert!(matches!(
        result,
        Err(WorkspaceError::EscapingSymlink { .. })
    ));
    assert!(!dest.join("escape").exists());
    Ok(())
}

#[test]
fn reconciliation_detects_base_branch_drift_and_mutation()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a snapshot from an initial commit.
    let repo = TestRepo::new()?;
    repo.write("file.txt", "one\n")?;
    repo.git(&["add", "."])?;
    repo.git(&["commit", "-m", "one"])?;
    let dest = repo.temp.path().join("snapshot");
    let controller = WorkspaceController::new(repo.path());
    let snapshot = controller.create_snapshot("HEAD", &dest)?;

    // When: the trusted branch moves and a worker file changes.
    repo.write("other.txt", "two\n")?;
    repo.git(&["add", "."])?;
    repo.git(&["commit", "-m", "two"])?;
    fs::write(dest.join("file.txt"), "worker\n")?;
    let report = controller.reconcile_external_mutations(&snapshot)?;

    // Then: both drift classes are surfaced without silent overwrite.
    assert!(report.base_branch_drifted);
    assert_eq!(report.external_mutations, vec![PathBuf::from("file.txt")]);
    Ok(())
}

struct TestRepo {
    temp: tempfile::TempDir,
    repo_path: PathBuf,
}

impl TestRepo {
    fn new() -> std::io::Result<Self> {
        let temp = tempfile::tempdir()?;
        let repo_path = temp.path().join("repo");
        fs::create_dir(&repo_path)?;
        let repo = Self { temp, repo_path };
        repo.git(&["init"])?;
        repo.git(&["config", "user.email", "test@example.invalid"])?;
        repo.git(&["config", "user.name", "Test User"])?;
        Ok(repo)
    }

    fn path(&self) -> &Path {
        &self.repo_path
    }

    fn write(&self, rel: &str, data: &str) -> std::io::Result<()> {
        let path = self.repo_path.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, data)
    }

    #[cfg(unix)]
    fn symlink(&self, rel: &str, target: &str) -> std::io::Result<()> {
        use std::os::unix::fs::symlink;

        symlink(target, self.repo_path.join(rel))
    }

    fn git(&self, args: &[&str]) -> std::io::Result<()> {
        let status = Command::new("git")
            .args(args)
            .current_dir(&self.repo_path)
            .status()?;
        assert!(status.success(), "git {args:?} failed with {status}");
        Ok(())
    }
}
