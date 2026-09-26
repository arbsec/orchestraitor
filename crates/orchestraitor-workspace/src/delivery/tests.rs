//! Integration tests for the delivery path against a local bare remote
//! fixture (spec `50-contracts-data.md` §21.3 — no network, no live GitHub).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use secrecy::{ExposeSecret, SecretString};

use super::git::scrubbed_git_env;
use super::{
    CommitIdentity, CredentialError, Delivery, DeliveryError, DeliveryRequest,
    DraftPullRequestSpec, NewPullRequest, PullRequestHandle, PullRequestTransport,
    PushCredentialProvider,
};

const BOT_NAME: &str = "Orc Bot";
const BOT_EMAIL: &str = "bot@arbsec.invalid";

fn identity() -> Result<CommitIdentity, DeliveryError> {
    CommitIdentity::new(BOT_NAME, BOT_EMAIL)
}

struct StaticProvider {
    token: SecretString,
}

impl PushCredentialProvider for StaticProvider {
    fn push_credential(&self) -> Result<SecretString, CredentialError> {
        Ok(self.token.clone())
    }
}

struct FailingProvider;

impl PushCredentialProvider for FailingProvider {
    fn push_credential(&self) -> Result<SecretString, CredentialError> {
        Err(CredentialError::Expired {
            reason: "installation token expired 2026-09-27T00:00:00Z".to_owned(),
        })
    }
}

#[derive(Debug)]
struct RecordedCall {
    draft: bool,
    head_branch: String,
    base_branch: String,
    body: String,
    credential: String,
}

struct RecordingTransport {
    calls: Mutex<Vec<RecordedCall>>,
}

impl RecordingTransport {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl PullRequestTransport for RecordingTransport {
    fn create_pull_request(
        &self,
        request: &NewPullRequest,
    ) -> Result<PullRequestHandle, Box<dyn std::error::Error + Send + Sync>> {
        let recorded = RecordedCall {
            draft: request.draft,
            head_branch: request.head_branch.clone(),
            base_branch: request.base_branch.clone(),
            body: request.body.clone(),
            credential: request.credential.expose_secret().to_owned(),
        };
        let mut calls = match self.calls.lock() {
            Ok(calls) => calls,
            Err(error) => return Err(Box::new(io::Error::other(error.to_string()))),
        };
        calls.push(recorded);
        Ok(PullRequestHandle {
            number: 7,
            url: "https://github.invalid/arbsec/orchestraitor/pull/7".to_owned(),
        })
    }
}

/// A trusted seed clone with `main` pushed to a local bare remote.
struct Fixture {
    _temp: tempfile::TempDir,
    repo_path: PathBuf,
    remote_path: PathBuf,
}

impl Fixture {
    fn new() -> io::Result<Self> {
        let temp = tempfile::tempdir()?;
        let remote_path = temp.path().join("remote.git");
        let repo_path = temp.path().join("repo");
        run_git(
            temp.path(),
            &[
                "init",
                "--bare",
                "--initial-branch=main",
                &remote_path.display().to_string(),
            ],
        )?;
        run_git(
            temp.path(),
            &[
                "init",
                "--initial-branch=main",
                &repo_path.display().to_string(),
            ],
        )?;
        fs::write(repo_path.join("README.md"), "fixture\n")?;
        run_git(&repo_path, &["add", "."])?;
        run_git(&repo_path, &["commit", "-m", "initial"])?;
        run_git(
            &repo_path,
            &[
                "remote",
                "add",
                "origin",
                &remote_path.display().to_string(),
            ],
        )?;
        run_git(&repo_path, &["push", "-u", "origin", "main"])?;
        Ok(Self {
            _temp: temp,
            repo_path,
            remote_path,
        })
    }

    fn delivery(&self) -> Delivery {
        Delivery::new(&self.repo_path)
    }

    fn worktree_dest(&self) -> PathBuf {
        self.remote_path
            .parent()
            .map_or_else(|| self.repo_path.join("wt"), |root| root.join("wt"))
    }

    fn ls_remote(&self, reference: &str) -> io::Result<String> {
        run_git(&self.repo_path, &["ls-remote", "origin", reference])
    }
}

fn run_git(dir: &Path, args: &[&str]) -> io::Result<String> {
    let output = Command::new("git")
        .arg("-c")
        .arg("user.name=Fixture")
        .arg("-c")
        .arg("user.email=fixture@arbsec.invalid")
        .args(args)
        .current_dir(dir)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "git {args:?} failed with {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn delivery_request(
    owner: &str,
    anchors: &[&str],
    evidence: &[&str],
) -> Result<DeliveryRequest, DeliveryError> {
    Ok(DeliveryRequest {
        commit_message: "feat(worker): deliver task output".to_owned(),
        identity: identity()?,
        pull_request: DraftPullRequestSpec {
            owner: owner.to_owned(),
            repo: "orchestraitor".to_owned(),
            title: "feat(worker): deliver task output".to_owned(),
            base_branch: "main".to_owned(),
            spec_anchors: anchors.iter().map(ToString::to_string).collect(),
            evidence_paths: evidence.iter().map(PathBuf::from).collect(),
        },
    })
}

#[test]
fn happy_path_delivers_commit_push_and_draft_pr()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a fixture repository with a bare remote and a provisioned
    // worktree containing worker output.
    let fixture = Fixture::new()?;
    let delivery = fixture.delivery();
    let worktree = delivery.provision_worktree("feat/happy", "main", &fixture.worktree_dest())?;
    fs::write(worktree.path().join("output.txt"), "worker output\n")?;
    let token = SecretString::from("scoped-installation-token");
    let provider = StaticProvider {
        token: token.clone(),
    };
    let transport = RecordingTransport::new();
    let request = delivery_request(
        "arbsec",
        &["docs/spec/20-harness-worker.md §9.4"],
        &[".omo/evidence/task/delivery-happy.txt"],
    )?;

    // When: running the composite delivery path.
    let outcome = delivery.deliver(&worktree, &request, &provider, &transport)?;

    // Then: the commit lands on the remote branch with the bot identity and
    // DCO trailer, and the draft PR handle is returned.
    let remote_ref = fixture.ls_remote("refs/heads/feat/happy")?;
    assert!(remote_ref.starts_with(&outcome.commit));
    let author = run_git(worktree.path(), &["log", "-1", "--format=%an|%ae|%cn|%ce"])?;
    assert_eq!(
        author,
        format!("{BOT_NAME}|{BOT_EMAIL}|{BOT_NAME}|{BOT_EMAIL}")
    );
    let message = run_git(worktree.path(), &["log", "-1", "--format=%B"])?;
    assert!(message.contains("Signed-off-by: Orc Bot <bot@arbsec.invalid>"));
    assert_eq!(outcome.pull_request.number, 7);
    assert_eq!(
        outcome.pull_request.url,
        "https://github.invalid/arbsec/orchestraitor/pull/7"
    );
    let calls = transport
        .calls
        .lock()
        .map_err(|_| io::Error::other("lock"))?;
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert!(call.draft);
    assert_eq!(call.head_branch, "feat/happy");
    assert_eq!(call.base_branch, "main");
    assert!(call.body.contains("docs/spec/20-harness-worker.md §9.4"));
    assert!(call.body.contains(".omo/evidence/task/delivery-happy.txt"));
    assert_eq!(call.credential, *token.expose_secret());
    Ok(())
}

#[test]
fn expired_credential_fails_without_push_and_preserves_worktree()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a provisioned worktree with output and a provider whose
    // credential is expired.
    let fixture = Fixture::new()?;
    let delivery = fixture.delivery();
    let worktree = delivery.provision_worktree("feat/expired", "main", &fixture.worktree_dest())?;
    fs::write(worktree.path().join("output.txt"), "worker output\n")?;
    let transport = RecordingTransport::new();
    let request = delivery_request("arbsec", &[], &[])?;

    // When: delivery reaches the credential step.
    let result = delivery.deliver(&worktree, &request, &FailingProvider, &transport);

    // Then: a typed credential failure surfaces, no push happened on any
    // ambient credential (the remote has no such ref), the draft PR step was
    // never reached, and the worktree keeps its commit for retry.
    assert!(matches!(
        result,
        Err(DeliveryError::Credential {
            source: CredentialError::Expired { .. }
        })
    ));
    assert!(fixture.ls_remote("refs/heads/feat/expired")?.is_empty());
    let calls = transport
        .calls
        .lock()
        .map_err(|_| io::Error::other("lock"))?;
    assert!(calls.is_empty());
    assert!(worktree.path().join("output.txt").exists());
    let subject = run_git(worktree.path(), &["log", "-1", "--format=%s"])?;
    assert_eq!(subject, "feat(worker): deliver task output");
    Ok(())
}

#[test]
fn non_dco_identity_is_rejected_before_any_git_mutation()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: identity inputs that cannot produce a valid DCO sign-off.
    let empty_name = CommitIdentity::new("", BOT_EMAIL);
    let missing_at = CommitIdentity::new(BOT_NAME, "not-an-email");
    let header_injection = CommitIdentity::new("Bot\nBcc: evil@example.invalid", BOT_EMAIL);

    // When/Then: each is rejected at construction, before any git invocation.
    assert!(matches!(
        empty_name,
        Err(DeliveryError::InvalidIdentity { .. })
    ));
    assert!(matches!(
        missing_at,
        Err(DeliveryError::InvalidIdentity { .. })
    ));
    assert!(matches!(
        header_injection,
        Err(DeliveryError::InvalidIdentity { .. })
    ));

    // And: an invalid branch name aborts provisioning before the worktree
    // exists.
    let fixture = Fixture::new()?;
    let dest = fixture.worktree_dest();
    let result = fixture
        .delivery()
        .provision_worktree("wip/escape", "main", &dest);
    assert!(matches!(result, Err(DeliveryError::InvalidBranch { .. })));
    assert!(!dest.exists());
    Ok(())
}

#[test]
fn non_fast_forward_push_is_typed_rejection() -> std::result::Result<(), Box<dyn std::error::Error>>
{
    // Given: a delivered branch that a rival commit has since replaced on the
    // remote.
    let fixture = Fixture::new()?;
    let delivery = fixture.delivery();
    let worktree = delivery.provision_worktree("feat/rival", "main", &fixture.worktree_dest())?;
    fs::write(worktree.path().join("output.txt"), "first\n")?;
    let token = SecretString::from("scoped-installation-token");
    let provider = StaticProvider { token };
    let transport = RecordingTransport::new();
    let request = delivery_request("arbsec", &[], &[])?;
    delivery.deliver(&worktree, &request, &provider, &transport)?;
    run_git(&fixture.repo_path, &["checkout", "-q", "-b", "rival"])?;
    fs::write(fixture.repo_path.join("rival.txt"), "rival\n")?;
    run_git(&fixture.repo_path, &["add", "."])?;
    run_git(&fixture.repo_path, &["commit", "-m", "rival commit"])?;
    run_git(
        &fixture.repo_path,
        &["push", "--force", "origin", "rival:refs/heads/feat/rival"],
    )?;
    fs::write(worktree.path().join("output.txt"), "second\n")?;
    let commit = delivery.commit_all(&worktree, "feat(worker): revise output", &identity()?)?;

    // When: pushing the now-divergent branch again.
    let credential = provider.push_credential().map_err(io::Error::other)?;
    let result = delivery.push(&worktree, &credential);

    // Then: the refusal is a typed non-fast-forward rejection and the
    // worktree keeps its commit.
    assert!(matches!(result, Err(DeliveryError::PushRejected { .. })));
    let head = run_git(worktree.path(), &["rev-parse", "HEAD"])?;
    assert_eq!(head, commit);
    Ok(())
}

#[test]
fn clean_worktree_is_nothing_to_commit() -> std::result::Result<(), Box<dyn std::error::Error>> {
    // Given: a provisioned worktree with no worker output.
    let fixture = Fixture::new()?;
    let delivery = fixture.delivery();
    let worktree = delivery.provision_worktree("feat/clean", "main", &fixture.worktree_dest())?;

    // When: committing without changes.
    let result = delivery.commit_all(&worktree, "feat(worker): empty", &identity()?);

    // Then: the failure is typed instead of a raw git error.
    assert!(matches!(result, Err(DeliveryError::NothingToCommit { .. })));
    Ok(())
}

#[test]
fn scrubbed_env_disables_ambient_config_and_prompts() {
    // Given/When/Then: every delivery git invocation pins the variables that
    // neutralize ambient credential helpers and interactive prompts.
    let env = scrubbed_git_env();
    let value_of = |key: &str| {
        env.iter()
            .find(|(name, _)| name.as_os_str() == key)
            .map(|(_, value)| value.to_string_lossy().into_owned())
    };
    assert_eq!(value_of("GIT_CONFIG_NOSYSTEM").as_deref(), Some("1"));
    assert_eq!(value_of("GIT_TERMINAL_PROMPT").as_deref(), Some("0"));
    let null_device = if cfg!(windows) { "NUL" } else { "/dev/null" };
    assert_eq!(value_of("GIT_CONFIG_GLOBAL").as_deref(), Some(null_device));
    assert_eq!(value_of("GIT_CONFIG_SYSTEM").as_deref(), Some(null_device));
}
