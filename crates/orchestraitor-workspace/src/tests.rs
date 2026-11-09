use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{SnapshotOptions, WorkspaceController, WorkspaceError};

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

#[cfg(unix)]
#[test]
fn link_entry_fails_closed_without_writing_snapshot()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a repository whose tree has regular entries and a symlink entry.
    let repo = TestRepo::new()?;
    repo.write("a-file.txt", "must not be materialized\n")?;
    repo.write("m-dir/b-file.txt", "must not be materialized\n")?;
    repo.symlink("z-link", "a-file.txt")?;
    repo.git(&["add", "."])?;
    repo.git(&["commit", "-m", "link entry"])?;
    let dest = repo.temp.path().join("snapshot");

    // When: creating a snapshot.
    let result = WorkspaceController::new(repo.path()).create_snapshot("HEAD", &dest);

    // Then: the whole plan is refused because Arbitraitor owns link projection.
    let Err(error) = result else {
        return Err("expected the snapshot to fail closed on a link entry".into());
    };
    match error {
        WorkspaceError::ProjectionCapabilityUnavailable { path, link, target } => {
            assert_eq!(path, "z-link");
            assert_eq!(link, "z-link");
            assert_eq!(target, "a-file.txt");
        }
        other => return Err(format!("unexpected error: {other:?}").into()),
    }

    // And: the refusal occurred before any write, so no partial tree exists.
    let created = directory_entries(&dest)?;
    assert!(
        created.is_empty(),
        "refused plan left a partial snapshot: {created:?}"
    );
    assert!(!dest.join("z-link").exists());
    assert!(fs::symlink_metadata(dest.join("z-link")).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn nested_link_entry_fails_closed_without_writing_snapshot()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a link nested one level down, after a sibling blob in tree order.
    let repo = TestRepo::new()?;
    repo.write("a-file.txt", "must not be materialized\n")?;
    repo.write("m-dir/b-file.txt", "must not be materialized\n")?;
    repo.symlink("m-dir/z-link", "b-file.txt")?;
    repo.git(&["add", "."])?;
    repo.git(&["commit", "-m", "nested link entry"])?;
    let dest = repo.temp.path().join("snapshot");

    // When: creating a snapshot.
    let result = WorkspaceController::new(repo.path()).create_snapshot("HEAD", &dest);

    // Then: the recursive arm refuses the plan and names the entry vs link name.
    let Err(error) = result else {
        return Err("expected the snapshot to fail closed on a nested link entry".into());
    };
    match error {
        WorkspaceError::ProjectionCapabilityUnavailable { path, link, target } => {
            assert_eq!(path, "m-dir/z-link");
            assert_eq!(link, "z-link");
            assert_eq!(target, "b-file.txt");
        }
        other => return Err(format!("unexpected error: {other:?}").into()),
    }

    // And: no partial tree exists, not even the refused link's parent directory.
    let created = directory_entries(&dest)?;
    assert!(
        created.is_empty(),
        "refused plan left a partial snapshot: {created:?}"
    );
    assert!(!dest.join("m-dir").exists());
    assert!(fs::symlink_metadata(dest.join("m-dir")).is_err());
    Ok(())
}

#[test]
fn regular_tree_entries_still_materialize() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a repository with a directory, a regular file, and an executable.
    let repo = TestRepo::new()?;
    repo.write("dir/nested.txt", "nested\n")?;
    repo.write("plain.txt", "plain\n")?;
    repo.write("run.sh", "#!/bin/sh\nexit 0\n")?;
    set_executable(&repo.path().join("run.sh"))?;
    repo.git(&["add", "."])?;
    repo.git(&["commit", "-m", "regular entries"])?;
    let dest = repo.temp.path().join("snapshot");

    // When: creating a snapshot.
    let snapshot = WorkspaceController::new(repo.path()).create_snapshot("HEAD", &dest)?;

    // Then: blob, blob-executable, and directory entries materialize normally.
    assert_eq!(
        fs::read_to_string(snapshot.workspace_root.join("dir/nested.txt"))?,
        "nested\n"
    );
    assert_eq!(
        fs::read_to_string(snapshot.workspace_root.join("plain.txt"))?,
        "plain\n"
    );
    assert!(snapshot.manifest.contains_key(Path::new("dir/nested.txt")));
    assert!(snapshot.manifest.contains_key(Path::new("plain.txt")));
    assert!(snapshot.manifest.contains_key(Path::new("run.sh")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(snapshot.workspace_root.join("run.sh"))?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    Ok(())
}

#[test]
fn submodule_placeholder_is_written_through_the_plan()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a repository with a gitlink entry.
    let repo = TestRepo::new()?;
    repo.write("file.txt", "base\n")?;
    repo.git(&["add", "."])?;
    repo.git(&["commit", "-m", "base"])?;
    let submodule_commit = repo.git_output(&["rev-parse", "HEAD"])?;
    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{submodule_commit},vendor/sub"),
    ])?;
    repo.git(&["commit", "-m", "add gitlink"])?;
    let dest = repo.temp.path().join("snapshot");

    // When: creating a snapshot.
    let snapshot = WorkspaceController::new(repo.path()).create_snapshot("HEAD", &dest)?;

    // Then: the gitlink becomes the exact submodule placeholder in file and manifest.
    let expected = format!("submodule {submodule_commit}\n");
    assert_eq!(
        fs::read_to_string(snapshot.workspace_root.join("vendor/sub"))?,
        expected
    );
    assert!(snapshot.manifest.contains_key(Path::new("vendor/sub")));
    Ok(())
}

#[test]
fn sparse_paths_materialize_only_the_selected_subtree()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a repository with two sibling directories.
    let repo = TestRepo::new()?;
    repo.write("keep/inner.txt", "kept\n")?;
    repo.write("skip/other.txt", "filtered\n")?;
    repo.git(&["add", "."])?;
    repo.git(&["commit", "-m", "sparse fixture"])?;
    let dest = repo.temp.path().join("snapshot");
    let options = SnapshotOptions {
        sparse_paths: vec![PathBuf::from("keep")],
    };

    // When: creating a snapshot with a sparse prefix.
    let snapshot = WorkspaceController::new(repo.path())
        .create_snapshot_with_options("HEAD", &dest, &options)?;

    // Then: only the selected subtree materializes and is recorded.
    assert_eq!(
        fs::read_to_string(snapshot.workspace_root.join("keep/inner.txt"))?,
        "kept\n"
    );
    assert!(!snapshot.workspace_root.join("skip").exists());
    assert!(snapshot.manifest.contains_key(Path::new("keep/inner.txt")));
    assert!(!snapshot.manifest.contains_key(Path::new("skip/other.txt")));
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

#[cfg(unix)]
fn directory_entries(path: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> std::io::Result<()> {
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
        // Host git config may force SSH commit signing (e.g. a 1Password
        // `gpg.ssh.program`), which blocks indefinitely in headless/CI
        // sessions. Fixture commits are unsigned: signing is a
        // developer-workstation setting, never a behavior under test.
        repo.git(&["config", "commit.gpgsign", "false"])?;
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

    fn git_output(&self, args: &[&str]) -> std::io::Result<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.repo_path)
            .output()?;
        assert!(
            output.status.success(),
            "git {args:?} failed with {}",
            output.status
        );
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}
