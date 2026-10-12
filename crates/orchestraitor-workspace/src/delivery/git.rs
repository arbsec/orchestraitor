//! `git` CLI plumbing for the delivery path.
//!
//! Linked-worktree creation and push are not covered by the `gix` 0.85 API
//! surface, so the trusted controller drives the `git` binary directly. Every
//! invocation runs with a CLEARED environment rebuilt from
//! [`scrubbed_git_env`] — system/global config redirected to the null device,
//! terminal prompts and askpass disabled, `GIT_DIR` / `GIT_WORK_TREE` removed,
//! ambient `GIT_CONFIG_*` env config neutralized — so ambient user
//! configuration, credential helpers included, can never influence the
//! delivery path. Repo-local configuration of the trusted controller checkout
//! remains a documented trusted surface. The scoped push credential is
//! injected through `GIT_CONFIG_COUNT`-keyed environment config
//! (`http.extraHeader`), keeping the token out of argv and off disk.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use secrecy::{ExposeSecret, SecretString};

use super::GitError;

/// Classification of a failed push.
#[derive(Debug)]
pub(crate) enum PushFailure {
    /// Remote refused a non-fast-forward update.
    Rejected,
    /// Remote denied the presented credential.
    Auth,
    /// Any other push failure.
    Other(GitError),
}

/// Runs `git` commands in `dir` under the scrubbed delivery environment.
pub(crate) struct Git {
    dir: PathBuf,
}

impl Git {
    pub(crate) fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// `git worktree add -b <branch> <dest> <base>`.
    pub(crate) fn worktree_add(
        &self,
        branch: &str,
        dest: &Path,
        base: &str,
    ) -> Result<(), GitError> {
        let dest_text = dest.to_string_lossy().into_owned();
        self.run(
            "worktree add",
            &["worktree", "add", "-b", branch, &dest_text, base],
            &[],
        )
        .map(|_| ())
    }

    /// `git add -A` inside a provisioned worktree.
    pub(crate) fn stage_all(&self) -> Result<(), GitError> {
        self.run("add", &["add", "-A"], &[]).map(|_| ())
    }

    /// True when the worktree has staged or unstaged changes.
    pub(crate) fn has_changes(&self) -> Result<bool, GitError> {
        let status = self.run("status", &["status", "--porcelain"], &[])?;
        Ok(!status.is_empty())
    }

    /// `git commit --no-verify` with explicit author/committer identity from
    /// the caller, never from ambient config. Hooks are denied per the
    /// default session policy (spec `20-harness-worker.md` §9.4).
    pub(crate) fn commit(&self, message: &str, name: &str, email: &str) -> Result<(), GitError> {
        self.run(
            "commit",
            &["commit", "--no-verify", "--cleanup=verbatim", "-m", message],
            &identity_env(name, email),
        )
        .map(|_| ())
    }

    /// Hex object id of the worktree `HEAD`.
    pub(crate) fn head(&self) -> Result<String, GitError> {
        self.run("rev-parse", &["rev-parse", "HEAD"], &[])
    }

    /// `git push --no-verify origin <branch>` with the scoped credential
    /// injected via environment config — never argv, never ambient.
    pub(crate) fn push(&self, branch: &str, credential: &SecretString) -> Result<(), PushFailure> {
        let header = format!("Authorization: Bearer {}", credential.expose_secret());
        let env = vec![
            (OsString::from("GIT_CONFIG_COUNT"), OsString::from("1")),
            (
                OsString::from("GIT_CONFIG_KEY_0"),
                OsString::from("http.extraHeader"),
            ),
            (OsString::from("GIT_CONFIG_VALUE_0"), OsString::from(header)),
        ];
        self.run("push", &["push", "--no-verify", "origin", branch], &env)
            .map(|_| ())
            .map_err(|error| classify_push_failure(&error))
    }

    fn run(
        &self,
        operation: &'static str,
        args: &[&str],
        extra_env: &[(OsString, OsString)],
    ) -> Result<String, GitError> {
        let mut command = Command::new("git");
        command
            .args(args)
            .current_dir(&self.dir)
            .env_clear()
            .envs(scrubbed_git_env())
            .envs(extra_env.iter().cloned())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        let output = command.output().map_err(|_| GitError {
            operation,
            exit_code: None,
            stderr: String::new(),
        })?;
        if !output.status.success() {
            return Err(GitError {
                operation,
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

/// Environment entries every delivery `git` invocation runs with. The child
/// environment is cleared first (`env_clear` in [`Git::run`]), so this set is
/// the COMPLETE child environment: `PATH` is passed through so the binary
/// resolves, system and global config are redirected to the null device (no
/// ambient credential helpers), and interactive prompts (terminal and
/// askpass) are disabled. Repo-local configuration of the trusted controller
/// checkout is intentionally not neutralized here — it is documented as a
/// trusted surface (spec `40-arbitraitor-integration.md` §6.2).
pub(crate) fn scrubbed_git_env() -> Vec<(OsString, OsString)> {
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut env = vec![
        (OsString::from("PATH"), OsString::from(default_path())),
        (OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")),
        (
            OsString::from("GIT_CONFIG_GLOBAL"),
            OsString::from(null_device),
        ),
        (
            OsString::from("GIT_CONFIG_SYSTEM"),
            OsString::from(null_device),
        ),
        (OsString::from("GIT_TERMINAL_PROMPT"), OsString::from("0")),
    ];
    for key in [
        "GIT_ASKPASS",
        "SSH_ASKPASS",
        "SSH_ASKPASS_REQUIRE",
        "GIT_SSH_COMMAND",
        "GIT_SSH",
        "GIT_RSH",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        env.push((OsString::from(key), OsString::new()));
    }
    for index in 0..16 {
        env.push((
            OsString::from(format!("GIT_CONFIG_KEY_{index}")),
            OsString::new(),
        ));
        env.push((
            OsString::from(format!("GIT_CONFIG_VALUE_{index}")),
            OsString::new(),
        ));
    }
    env
}

/// Parent `PATH` passthrough so the child can resolve sub-command helpers;
/// read from the trusted controller process, never from artifact content.
fn default_path() -> String {
    std::env::var("PATH").unwrap_or_default()
}

fn identity_env(name: &str, email: &str) -> [(OsString, OsString); 4] {
    [
        (OsString::from("GIT_AUTHOR_NAME"), OsString::from(name)),
        (OsString::from("GIT_AUTHOR_EMAIL"), OsString::from(email)),
        (OsString::from("GIT_COMMITTER_NAME"), OsString::from(name)),
        (OsString::from("GIT_COMMITTER_EMAIL"), OsString::from(email)),
    ]
}

pub(crate) fn classify_push_failure(error: &GitError) -> PushFailure {
    let stderr = error.stderr.as_str();
    if stderr.contains("non-fast-forward")
        || stderr.contains("fetch first")
        || stderr.contains("stale info")
    {
        PushFailure::Rejected
    } else if stderr.contains("Authentication failed")
        || stderr.contains("returned error: 401")
        || stderr.contains("returned error: 403")
    {
        PushFailure::Auth
    } else {
        PushFailure::Other(GitError {
            operation: error.operation,
            exit_code: error.exit_code,
            stderr: String::new(),
        })
    }
}
