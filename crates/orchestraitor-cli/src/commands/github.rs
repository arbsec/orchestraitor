//! `orc github` implementation.

use std::io::Write;
use std::process::Command;
use std::time::Duration;

use base64::Engine as _;
use miette::{IntoDiagnostic, Result, bail, miette};
use orchestraitor_core::GitHubAppError;
use orchestraitor_core::github_app::{
    AccessTokenResponse, GitHubAppAuth, InstallationToken, InstallationTokenTransport,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use crate::cli::{ApiArgs, ConfigPaths, GitHubCommand, PushBranchArgs, VerifyIdentityArgs};
use crate::commands::config::layers::load_layers;
use orchestraitor_core::config::{
    GitHubAppConfig, OrchestraitorConfig, ServiceIdentityEnforcement,
};

/// GitHub REST API base URL.
const DEFAULT_GITHUB_API_BASE_URL: &str = "https://api.github.com";

/// HTTP methods accepted by `orc github api`.
const API_METHODS: [&str; 5] = ["GET", "POST", "PATCH", "PUT", "DELETE"];

/// Runs an `orc github` subcommand.
///
/// # Errors
/// Returns a diagnostic when configuration, secret resolution, or the mint
/// request fails. No diagnostic ever contains key material or tokens.
pub fn run<W: Write>(paths: &ConfigPaths, command: GitHubCommand, writer: &mut W) -> Result<()> {
    match command {
        GitHubCommand::MintToken => mint_token(paths, writer),
        GitHubCommand::Api(args) => api(paths, &args, writer),
        GitHubCommand::GhEnv(args) => gh_env(paths, &args),
        GitHubCommand::GhEnvPrCreateChain(args) => gh_env_pr_create_chain(paths, &args),
        GitHubCommand::CommitAuthor => commit_author(paths, writer),
        GitHubCommand::PushBranch(args) => push_branch(paths, &args),
        GitHubCommand::VerifyIdentity(args) => verify_identity(paths, &args),
    }
}

fn mint_token<W: Write>(paths: &ConfigPaths, writer: &mut W) -> Result<()> {
    let (installation_id, token) = mint_installation_token(paths)?;
    writeln!(writer, "minted GitHub App installation token").into_diagnostic()?;
    writeln!(writer, "installation_id = {installation_id}").into_diagnostic()?;
    writeln!(writer, "expires_at = {}", token.expires_at_rfc3339()).into_diagnostic()?;
    writeln!(writer, "expires_at_epoch = {}", token.expires_at_epoch()).into_diagnostic()?;
    writeln!(
        writer,
        "token_sha256_prefix = {}",
        token.fingerprint_prefix(12)
    )
    .into_diagnostic()?;
    writeln!(
        writer,
        "token is held in memory only; it is never printed, logged, or persisted"
    )
    .into_diagnostic()
}

/// Resolves the layered config (same path the mint commands use) so
/// enforcement decisions read the effective `github_app` values.
pub(crate) fn resolved_config(paths: &ConfigPaths) -> Result<OrchestraitorConfig> {
    let layers = load_layers(paths)?;
    layers.resolver.resolve_config().map_err(|error| {
        miette!(
            "configuration validation failed: {}",
            error.structured().cause
        )
    })
}

/// Resolves the layered config into the `github_app` block, failing typed
/// when absent, and constructs the [`GitHubAppAuth`] minter from it. Shared
/// by every subcommand that needs App credentials so validation and the
/// missing-credential diagnostics stay identical across commands.
///
/// # Errors
/// Returns a typed diagnostic when the layered config fails to resolve, the
/// `github_app` block is absent, or any required credential key is missing.
fn github_app_auth(config: &OrchestraitorConfig) -> Result<GitHubAppAuth> {
    let github_app = config.github_app.as_ref().ok_or_else(|| {
        miette!(
            "github_app configuration is not set (need client_id, installation_id, private_key_uri; see docs/cli/orc-github.md)"
        )
    })?;
    let client_id = required(github_app.client_id.as_ref(), "client_id")?.clone();
    let installation_id = *required(github_app.installation_id.as_ref(), "installation_id")?;
    let private_key_uri = required(github_app.private_key_uri.as_ref(), "private_key_uri")?.clone();
    Ok(GitHubAppAuth::new(
        client_id,
        installation_id,
        private_key_uri,
    ))
}

/// Effective service-identity enforcement mode (`recommended` when unset).
fn enforcement_mode(config: &OrchestraitorConfig) -> ServiceIdentityEnforcement {
    config
        .github_app
        .as_ref()
        .and_then(|app| app.enforcement)
        .unwrap_or_default()
}

/// Fail-closed gate for `required` enforcement: a typed error when the
/// `github_app` config does not resolve (absent or partial). In
/// `recommended` mode this is a no-op — the labelled personal fallback
/// remains available to callers.
///
/// # Errors
/// Returns a typed diagnostic naming the missing keys when enforcement is
/// `required` and the config block is absent or incomplete.
fn require_service_identity<'a>(
    config: &'a OrchestraitorConfig,
    context: &str,
) -> Result<&'a GitHubAppConfig> {
    let Some(github_app) = config.github_app.as_ref() else {
        bail!(
            "service-identity enforcement is `required`: refusing to {context} without a \
             complete github_app configuration (need enforcement-eligible client_id, \
             installation_id, private_key_uri; see docs/cli/orc-github.md)"
        );
    };
    let mut missing = Vec::new();
    if github_app.client_id.is_none() {
        missing.push("client_id");
    }
    if github_app.installation_id.is_none() {
        missing.push("installation_id");
    }
    if github_app.private_key_uri.is_none() {
        missing.push("private_key_uri");
    }
    if !missing.is_empty() {
        bail!(
            "service-identity enforcement is `required`: refusing to {context}; github_app \
             config is incomplete (missing: {}; see docs/cli/orc-github.md)",
            missing.join(", ")
        );
    }
    Ok(github_app)
}

/// Bot commit-identity check for agent `git commit` paths delegated through
/// `gh-env` (the skill scripts' `orc_lib_gh_service` wrapper). Verifies the
/// repo-local `git config user.email` equals the service-identity bot's
/// canonical noreply email (`<bot-id>+<slug>[bot]@users.noreply.github.com`,
/// resolved from the live App identity via `commit_author_identity`) so
/// locally-created commits cannot carry personal attribution. The bot id is
/// not guessable, so a suffix match would accept an unrelated
/// `anything+<slug>[bot]@…` address — the comparison is exact.
///
/// On success returns the resolved canonical `(name, email)` so the caller
/// can pin the child's git identity environment: a child could otherwise
/// override attribution per-invocation with `git -c user.email=… commit`
/// (config is beaten by the `GIT_AUTHOR_*`/`GIT_COMMITTER_*` environment
/// variables, which is why the caller sets them).
///
/// # Errors
/// Returns a typed diagnostic when the identity cannot be resolved or the
/// configured email is not the bot's canonical noreply address.
fn require_bot_git_identity(paths: &ConfigPaths, program: &str) -> Result<(String, String)> {
    let output = Command::new("git")
        .args(["config", "--get", "user.email"])
        .output()
        .map_err(|error| miette!("failed to run `git config user.email`: {error}"))?;
    let email = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || email.is_empty() {
        bail!(
            "service-identity enforcement is `required`: refusing to delegate `{program}` \
             because `git config user.email` is unset — set the per-repo gitconfig to the \
             service-identity bot (user.name = arbsec-agent[bot], \
             user.email = <id>+arbsec-agent[bot]@users.noreply.github.com)"
        );
    }
    let expected = commit_author_identity(paths)?;
    if email != expected.1 {
        bail!(
            "service-identity enforcement is `required`: refusing to delegate `{program}` \
             because `git config user.email` ({email}) is not the service-identity bot's \
             canonical noreply address — set user.email to {}",
            expected.1
        );
    }
    Ok(expected)
}

/// Resolves the `github_app` config and mints one installation token. The
/// token value stays a `SecretString` and is only ever injected into an
/// `Authorization` header or a child environment — never printed or logged.
pub(crate) fn mint_installation_token(paths: &ConfigPaths) -> Result<(u64, InstallationToken)> {
    let config = resolved_config(paths)?;
    let auth = github_app_auth(&config)?;
    let github_app = config
        .github_app
        .as_ref()
        .ok_or_else(|| miette!("github_app configuration is not set"))?;
    let installation_id = *required(github_app.installation_id.as_ref(), "installation_id")?;
    let transport =
        ReqwestInstallationTransport::new(paths.github_api_endpoint.clone()).into_diagnostic()?;
    let token = auth.installation_token(&transport).into_diagnostic()?;
    Ok((installation_id, token))
}

fn required<'a, T>(value: Option<&'a T>, key: &str) -> Result<&'a T> {
    value.ok_or_else(|| {
        miette!(
            "{} (set it at the user/org layer; see docs/cli/orc-github.md)",
            GitHubAppError::MissingConfig {
                key: key.to_string()
            }
        )
    })
}

/// `orc github api`: one authenticated GitHub REST call as the App
/// installation. Prints the response body verbatim; the exit code maps to
/// the HTTP status class. The error path never echoes Authorization headers.
fn api<W: Write>(paths: &ConfigPaths, args: &ApiArgs, writer: &mut W) -> Result<()> {
    validate_api_method(&args.method)?;
    let method = args.method.to_ascii_uppercase();
    validate_api_path(&args.path)?;
    let mut path = args.path.trim_start_matches('/').to_string();
    let body = build_request_body(args)?;
    // GET/DELETE carry `--field` pairs as URL query parameters (the `gh api`
    // shape) instead of a silently-ignored JSON body.
    if matches!(method.as_str(), "GET" | "DELETE") && !args.fields.is_empty() {
        let query = build_request_query(&args.fields)?;
        let separator = if path.contains('?') { "&" } else { "?" };
        path.push_str(separator);
        path.push_str(&query);
    }
    let (_, token) = mint_installation_token(paths)?;
    let transport = ApiTransport::new(paths.github_api_endpoint.clone())?;
    let (status, response_body) = transport.request(&method, &path, &token, body)?;
    writer
        .write_all(response_body.as_bytes())
        .into_diagnostic()?;
    writer.flush().into_diagnostic()?;
    if !status.is_success() {
        // The body is the caller's business and was printed above; the
        // diagnostic carries only the status class and endpoint shape —
        // never request headers (which hold the token) and never a second
        // copy of the body.
        bail!("github api request returned HTTP {status} ({method} /{path})");
    }
    Ok(())
}

/// Builds the JSON request body from `--input FILE|-` and `--field k=v`.
///
/// `-f/--field` behaves like `gh api -f/--raw-field`: the value is ALWAYS
/// sent as a string, never JSON-parsed (`-f body=123` sends `"123"`, not
/// `123`). For body-less request classes (GET, DELETE) fields become URL
/// query parameters instead — the `gh api` shape — rather than being
/// silently dropped into a JSON body the endpoint ignores.
fn build_request_body(args: &ApiArgs) -> Result<Option<String>> {
    let upper_method = args.method.to_ascii_uppercase();
    let takes_body = !matches!(upper_method.as_str(), "GET" | "DELETE");
    if !takes_body {
        if args.input.is_some() {
            bail!(
                "`--input` cannot be combined with {upper_method}: GitHub ignores request \
                 bodies on {upper_method}; use query fields (`-f key=value`) instead"
            );
        }
        // Fields ride the URL query string instead (see `build_request_query`).
        return Ok(None);
    }
    let mut body: Option<Value> = None;
    if let Some(input) = &args.input {
        let raw = if input == "-" {
            let mut buffer = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer).into_diagnostic()?;
            buffer
        } else {
            std::fs::read_to_string(input).into_diagnostic()?
        };
        let value: Value = serde_json::from_str(raw.trim())
            .map_err(|error| miette!("`--input` is not valid JSON: {error}"))?;
        body = Some(value);
    }
    for field in &args.fields {
        let Some((key, value)) = field.split_once('=') else {
            bail!("`--field` must be `key=value`, got `{field}`");
        };
        // `gh api -f` semantics: the value is always a string.
        let parsed = Value::String(value.to_string());
        let Some(Value::Object(map)) = body.as_mut() else {
            if body.is_some() {
                bail!("cannot combine `--input` body with `--field` on a non-object body");
            }
            body = Some(Value::Object(serde_json::Map::new()));
            let Some(Value::Object(map)) = body.as_mut() else {
                bail!("cannot build an object request body");
            };
            map.insert(key.to_string(), parsed);
            continue;
        };
        map.insert(key.to_string(), parsed);
    }
    let serialized = match body.as_ref() {
        Some(value) => Some(
            serde_json::to_string(value)
                .map_err(|error| miette!("request body is not serializable JSON: {error}"))?,
        ),
        None => None,
    };
    Ok(serialized)
}

/// Builds the URL query string from `--field k=v` pairs for body-less
/// request classes (GET, DELETE) — the `gh api` shape. Percent-encodes both
/// keys and values; the token never travels in the URL.
fn build_request_query(fields: &[String]) -> Result<String> {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            bail!("`--field` must be `key=value`, got `{field}`");
        };
        query.append_pair(key, value);
    }
    Ok(query.finish())
}

/// `orc github gh-env`: execute one child command with `GH_TOKEN` set to a
/// freshly minted installation token. The token never appears in orc's own
/// output; it exists only in the child's environment.
fn gh_env(paths: &ConfigPaths, args: &crate::cli::GhEnvArgs) -> Result<()> {
    if args.command.is_empty() {
        bail!(
            "gh-env requires a child command after `--` (e.g. `orc github gh-env -- gh pr view 1`)"
        );
    }
    // Validate the child command exists before minting (fail closed before
    // any secret is minted) — a typed error, not a spawn failure.
    let program = &args.command[0];
    let which = which_executable(program);
    if which.is_none() {
        bail!("child command `{program}` not found on PATH");
    }
    // Enforcement gate reads the effective enforcement mode before any token
    // minting: `required` refuses to delegate at all when the App config does
    // not resolve, and refuses when the repo git identity would stamp
    // personal attribution onto agent-created commits. The layered config is
    // the default source; `ORC_GITHUB_APP_ENFORCEMENT` (the wrapper-level
    // declaration used where the layered config cannot carry it) OVERRIDES it
    // — an env-pinned `required` must run the same identity gate, never a
    // weaker layered value. Invalid env values fail closed. `gh pr create`
    // children are REFUSED in required mode (the deterministic PR-labeling
    // chain would be bypassed): the ONLY sanctioned path is the dedicated
    // `gh-env-pr-create-chain` subcommand, which executes the labeling
    // obligation ITSELF — a caller attestation (env var or flag) would be
    // trivially forgeable and is not accepted. AMBIGUOUS argv is refused
    // too (fail closed): an unparseable shape must never delegate with the
    // minted token. In `recommended` mode the personal fallback is a
    // documented, warned path and the guard does not fire.
    if effective_required_mode(paths)?
        && !matches!(child_command_is_pr_create(&args.command), PrCreateShape::No)
    {
        bail!(
            "gh-env refuses to run `gh pr create`: in `required` enforcement mode PR \
             creation MUST go through `orc github gh-env-pr-create-chain <owner/name> -- \
             gh pr create …`, which executes the deterministic PR-labeling chain \
             (`pr-labels`, `pr-labels-from-issue`) against the created PR — or use the \
             `pr-create` lifecycle script, which calls it — or pass explicit `--label` \
             flags to `pr-create`. See docs/cli/orc-github.md"
        );
    }
    let (bot_identity, token) = gh_env_gates_and_mint(paths, &args.command)?;
    gh_env_spawn_child(&args.command, &token, bot_identity.as_ref(), false)?;
    Ok(())
}

/// Effective `required` enforcement decision, shared by the gh-env guard and
/// the delegation gate so the two cannot diverge: the layered config value
/// (`github_app.enforcement`) with the `ORC_GITHUB_APP_ENFORCEMENT` pin
/// OVERRIDING it (an env-pinned `required` must run the same gate, never a
/// weaker layered value). Invalid pin values fail closed.
fn effective_required_mode(paths: &ConfigPaths) -> Result<bool> {
    let config = resolved_config(paths)?;
    let mut required_mode = enforcement_mode(&config) == ServiceIdentityEnforcement::Required;
    if let Ok(pinned) = std::env::var("ORC_GITHUB_APP_ENFORCEMENT") {
        match pinned.as_str() {
            "required" => required_mode = true,
            "recommended" => {}
            other => {
                bail!(
                    "invalid ORC_GITHUB_APP_ENFORCEMENT value `{other}` (expected `recommended` \
                     or `required`); refusing to delegate under an ambiguous enforcement \
                     declaration"
                );
            }
        }
    }
    Ok(required_mode)
}

/// `orc github gh-env-pr-create-chain`: the ONLY sanctioned `gh pr create`
/// path in `required` enforcement. Executes the child `gh pr create` with
/// the minted installation token (the same delegation semantics as
/// `gh-env`), then ENFORCES the deterministic labeling obligation itself:
/// `pr-labels` and `pr-labels-from-issue` run against the created PR. The
/// obligation is executed here — never attested by a caller-supplied env
/// var or flag, which would be trivially forgeable. A labeling-step failure
/// never orphans the created PR: it is a loud typed warning naming the
/// exact manual commands (the labeling scripts are non-fatal by contract),
/// and gh-env-pr-create-chain still exits 0 after a successful creation.
fn gh_env_pr_create_chain(
    paths: &ConfigPaths,
    args: &crate::cli::GhEnvPrCreateChainArgs,
) -> Result<()> {
    if args.command.is_empty() {
        bail!(
            "gh-env-pr-create-chain requires the child command after `--` (e.g. `orc github \
             gh-env-pr-create-chain arbsec/orchestraitor -- gh pr create --draft`)"
        );
    }
    if !matches!(
        child_command_is_pr_create(&args.command),
        PrCreateShape::Yes
    ) {
        bail!(
            "gh-env-pr-create-chain runs ONLY `gh pr create` children (got `{:?}`); other \
             commands go through plain `gh-env`",
            args.command.first()
        );
    }
    // The label scripts are repo skill scripts: resolve them relative to the
    // repo root (git toplevel of the CWD), matching how the lifecycle skill
    // invokes them.
    let repo_root = git_repo_root()?;
    let pr_labels = repo_root.join(".agents/skills/github-pr-lifecycle/scripts/pr-labels");
    let pr_labels_from_issue =
        repo_root.join(".agents/skills/github-pr-lifecycle/scripts/pr-labels-from-issue");
    if !pr_labels.is_file() || !pr_labels_from_issue.is_file() {
        bail!(
            "gh-env-pr-create-chain requires the pr-lifecycle skill scripts at \
             .agents/skills/github-pr-lifecycle/scripts/ (resolved from the repo root {}) — \
             pr-labels: {}, pr-labels-from-issue: {}",
            repo_root.display(),
            pr_labels.display(),
            pr_labels_from_issue.display()
        );
    }
    // Delegate with the SAME enforcement gates as plain gh-env: this
    // subcommand is itself a `gh pr create` child, so the required-mode
    // gates (config completeness + canonical bot git identity) run before
    // any mint — and the child command is validated to exist BEFORE minting
    // (the documented no-token-for-a-missing-command rule). In
    // `recommended` mode the personal fallback applies as documented.
    let program = &args.command[0];
    if which_executable(program).is_none() {
        bail!("child command `{program}` not found on PATH");
    }
    let (bot_identity, token) = gh_env_gates_and_mint(paths, &args.command)?;
    let pr_url = gh_env_spawn_child(&args.command, &token, bot_identity.as_ref(), true)?;
    // The captured stdout IS the caller's business (the `pr-create` script
    // reads the PR URL through command substitution) — re-emit it verbatim
    // and flush BEFORE the labeling steps run.
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(pr_url.as_bytes()).into_diagnostic()?;
    stdout.flush().into_diagnostic()?;

    // --- The labeling obligation (executed and enforced HERE) --------------
    // The URL carries BOTH the created PR's repository and its number:
    // label ONLY the PR that was actually created (a `-R other/repo` child
    // creates a PR elsewhere — labeling #N in `args.repo` would hit a
    // different PR).
    let url_parts: Vec<&str> = pr_url.trim().rsplit('/').collect();
    let pr_number = url_parts.first().copied().unwrap_or_default();
    let url_repo = match (url_parts.get(2), url_parts.get(3)) {
        (Some(name), Some(owner)) => format!("{owner}/{name}"),
        _ => String::new(),
    };
    if pr_number.is_empty() || !pr_number.chars().all(|c| c.is_ascii_digit()) {
        writeln!(
            std::io::stderr().lock(),
            "WARNING: could not derive a PR number from '{pr_url}'; automatic labeling was \
             skipped — apply manually with pr-labels and pr-labels-from-issue"
        )
        .into_diagnostic()?;
        return Ok(());
    }
    if url_repo.is_empty() || !url_repo.eq_ignore_ascii_case(&args.repo) {
        writeln!(
            std::io::stderr().lock(),
            "WARNING: the created PR ({pr_url}) is not in {}; automatic labeling was skipped — \
             label the created PR manually with pr-labels and pr-labels-from-issue",
            args.repo
        )
        .into_diagnostic()?;
        return Ok(());
    }
    run_labeling_chain(pr_number, &args.repo, &pr_labels, &pr_labels_from_issue)?;
    Ok(())
}

/// Runs the two labeling scripts against the created PR — the enforced
/// obligation of `gh-env-pr-create-chain`. A labeling failure (spawn
/// failure included) NEVER fails the command after the PR exists: a
/// non-zero exit would push callers into a retry that creates a duplicate
/// PR. Every failure class is the same loud warning path naming the exact
/// manual command. The child's stdout is CAPTURED (its JSON receipt must
/// never leak into orc's stdout — `pr-create` reads THIS command's stdout
/// as the PR URL) and forwarded to stderr; the child's stderr inherits,
/// matching the pass-through contract.
fn run_labeling_chain(
    pr_number: &str,
    repo: &str,
    pr_labels: &std::path::Path,
    pr_labels_from_issue: &std::path::Path,
) -> Result<()> {
    for (script, name) in [
        (pr_labels, "pr-labels"),
        (pr_labels_from_issue, "pr-labels-from-issue"),
    ] {
        let output = match Command::new("bash")
            .arg(script)
            .arg(pr_number)
            .arg("--repo")
            .arg(repo)
            .stderr(std::process::Stdio::inherit())
            .output()
        {
            Ok(output) => output,
            Err(error) => {
                let mut stderr = std::io::stderr().lock();
                writeln!(
                    stderr,
                    "WARNING: automatic labeling of PR #{pr_number} failed ({name}: {error}); \
                     apply manually:"
                )
                .into_diagnostic()?;
                writeln!(
                    stderr,
                    "  bash {} {pr_number} --repo {repo}",
                    script.display()
                )
                .into_diagnostic()?;
                continue;
            }
        };
        if !output.stdout.is_empty() {
            std::io::stderr()
                .write_all(&output.stdout)
                .into_diagnostic()?;
        }
        if !output.status.success() {
            let mut stderr = std::io::stderr().lock();
            writeln!(
                stderr,
                "WARNING: automatic labeling of PR #{pr_number} failed ({name}); apply manually:"
            )
            .into_diagnostic()?;
            writeln!(
                stderr,
                "  bash {} {pr_number} --repo {repo}",
                script.display()
            )
            .into_diagnostic()?;
        }
    }
    Ok(())
}
/// Shared gh-env gates: enforcement resolution (`ORC_GITHUB_APP_ENFORCEMENT`
/// pin honored), the required-mode service-identity + canonical bot
/// git-identity checks, and the installation-token mint. Returns the
/// resolved bot identity (for the child's git-identity env pin) and the
/// minted token. Child existence on PATH must be checked by the caller
/// (before minting).
fn gh_env_gates_and_mint(
    paths: &ConfigPaths,
    command: &[String],
) -> Result<(Option<(String, String)>, InstallationToken)> {
    let config = resolved_config(paths)?;
    let mut bot_identity: Option<(String, String)> = None;
    if effective_required_mode(paths)? {
        require_service_identity(&config, "delegate a gh-env child command")?;
        // Exact-identity check before delegating: the repo git identity must
        // equal the service-identity bot's canonical noreply email (resolved
        // from the live App) so any agent-driven `git commit` path cannot
        // stamp personal or look-alike attribution onto commits.
        let program = command.first().map(String::as_str).unwrap_or_default();
        bot_identity = Some(require_bot_git_identity(paths, program)?);
    }
    let (_, token) = mint_installation_token(paths)?;
    Ok((bot_identity, token))
}

/// Spawns the gh-env child with the minted token (and the pinned git
/// identity env when in required mode), passing the child's stdout/stderr
/// through. When `capture_stdout` is set the child's stdout is captured and
/// returned (the PR-URL contract for the labeling chain); otherwise it passes
/// through verbatim. The child's exit code propagates (a missing exit code
/// maps to 1) BEFORE any captured-stdout value is meaningful: a failed child
/// never returns.
fn gh_env_spawn_child(
    command: &[String],
    token: &InstallationToken,
    bot_identity: Option<&(String, String)>,
    capture_stdout: bool,
) -> Result<String> {
    let program = command.first().map(String::as_str).unwrap_or_default();
    let Some(executable) = which_executable(program) else {
        bail!("child command `{program}` not found on PATH");
    };
    let mut child = Command::new(executable);
    child
        .args(&command[1..])
        .env("GH_TOKEN", token.token().expose_secret());
    // Pin the git identity for the child: `git -c user.email=… commit` beats
    // repo config but NOT the GIT_AUTHOR_*/GIT_COMMITTER_* environment
    // variables, so env-pinning guarantees bot attribution even for a child
    // that rewrites its identity per invocation. Identity values are
    // non-secret (public bot login + noreply email).
    if let Some((name, email)) = bot_identity {
        child
            .env("GIT_AUTHOR_NAME", name)
            .env("GIT_AUTHOR_EMAIL", email)
            .env("GIT_COMMITTER_NAME", name)
            .env("GIT_COMMITTER_EMAIL", email);
    }
    if capture_stdout {
        // Pipe ONLY stdout (the PR URL); stderr inherits so child
        // diagnostics stay visible (matching the pass-through contract),
        // and the exit status still propagates before the URL is used.
        let output = child
            .stderr(std::process::Stdio::inherit())
            .output()
            .map_err(|error| miette!("failed to spawn child command `{program}`: {error}"))?;
        if !output.status.success() {
            std::process::exit(output.status.code().unwrap_or(1));
        }
        return Ok(String::from_utf8_lossy(&output.stdout).to_string());
    }
    let status = child
        .status()
        .map_err(|error| miette!("failed to spawn child command `{program}`: {error}"))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(String::new())
}

/// Resolves the enclosing git repository's working-tree root (typed error
/// outside a repo or when `git` cannot run).
fn git_repo_root() -> Result<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| miette!("failed to run `git rev-parse --show-toplevel`: {error}"))?;
    if !output.status.success() {
        bail!(
            "gh-env-pr-create-chain must run inside the target git repository (the pr-lifecycle \
             skill scripts are resolved from the repo root)"
        );
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        bail!("`git rev-parse --show-toplevel` returned an empty path");
    }
    Ok(std::path::PathBuf::from(root))
}

/// The detector's verdict for a gh-env child argv: proven `gh pr create`,
/// proven NOT a `gh pr create`, or ambiguous (the shape cannot be proven
/// either way from the argv — an unknown flag, a separator mid-shape). The
/// caller's policy: `required` enforcement refuses BOTH `Yes` and
/// `Ambiguous` (fail closed — the labeling chain must not be bypassable via
/// an unparseable argv); the chain subcommand runs only a `Yes` child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrCreateShape {
    Yes,
    No,
    Ambiguous,
}

/// Classifies the `gh pr create` shape in a gh-env child argv: getopt's
/// first-two-positional convention (the first non-option token is the
/// command — matched by basename, so `/usr/bin/gh` and `gh.exe` count — and
/// the second is the subcommand) must yield `pr` followed by `create` (or
/// its documented alias `new`). Shape-based, never a substring scan, and
/// three-outcome on ambiguity: only KNOWN flags are skipped. `gh` (Cobra)
/// accepts global/persistent flags interleaved with the subcommand
/// (`gh -R o/r pr create`, `gh pr -R o/r create` both reach `pr create`);
/// a flag the walk does not recognize makes the shape AMBIGUOUS — it can
/// neither be confirmed nor excluded, so the caller must refuse (never
/// silently delegate under required enforcement).
fn child_command_is_pr_create(command: &[std::string::String]) -> PrCreateShape {
    /// gh global flags consumed as `--flag value` (two argv tokens);
    /// `=`-attached values never consume a token.
    const VALUE_LONG_FLAGS: [&str; 4] = ["repo", "hostname", "jq", "template"];
    /// gh boolean short flags (no value argument follows).
    const BOOLEAN_SHORT_FLAGS: [char; 2] = ['h', 'v'];
    /// gh short flags consumed as `-X value` (two argv tokens).
    const VALUE_SHORT_FLAGS: [char; 2] = ['R', 'H'];

    let mut depth = 0usize;
    // 0 = expecting the `gh` command, 1 = `pr`, 2 = `create`
    let mut stage = 0u8;
    for arg in command {
        let arg = arg.as_str();
        if depth > 0 {
            // Inside a value-taking flag's argument: never a command token.
            depth -= 1;
            continue;
        }
        if arg == "--" {
            // End-of-flags separator: everything after is positional. The
            // subcommand tokens seen so far decide (they must already be
            // complete — tokens after `--` can never extend the shape).
            return match stage {
                2 => PrCreateShape::Yes,
                _ => PrCreateShape::Ambiguous,
            };
        }
        if let Some(name) = arg.strip_prefix("--") {
            // An `=`-attached value (`--repo=o/r`) is complete in this token:
            // it must NOT consume the next argv element as a value — gh
            // accepts `gh --repo=o/r pr create`, and consuming `pr` here
            // would hide the pr-create shape.
            let bare = name.split('=').next().unwrap_or(name);
            depth = if arg.contains('=') {
                0
            } else if VALUE_LONG_FLAGS.contains(&bare) {
                1
            } else {
                // An unknown long flag makes the shape unprovable.
                return PrCreateShape::Ambiguous;
            };
            continue;
        }
        if arg == "-" {
            // gh treats a bare `-` as a positional operand; the shape
            // cannot continue through it.
            return PrCreateShape::Ambiguous;
        }
        if let Some(rest) = arg.strip_prefix('-') {
            // Short-flag clusters: every char must be a KNOWN gh flag, and
            // only a trailing value-taking flag consumes the next argv
            // element. An unknown flag makes the shape unprovable — fail
            // closed (ambiguous).
            let mut value_flag = false;
            for c in rest.chars() {
                if BOOLEAN_SHORT_FLAGS.contains(&c) {
                    continue;
                }
                if VALUE_SHORT_FLAGS.contains(&c) && c == rest.chars().next_back().unwrap_or(' ') {
                    value_flag = true;
                } else if VALUE_SHORT_FLAGS.contains(&c) {
                    // A value-taking flag inside a cluster is ambiguous.
                    return PrCreateShape::Ambiguous;
                } else {
                    return PrCreateShape::Ambiguous;
                }
            }
            depth = usize::from(value_flag);
            continue;
        }
        match stage {
            0 if arg == "gh"
                || std::path::Path::new(arg)
                    .file_name()
                    .is_some_and(|name| name == "gh" || name == "gh.exe") =>
            {
                stage = 1;
            }
            1 if arg == "pr" => stage = 2,
            // `gh pr new` is a documented built-in alias of `gh pr create`.
            2 => {
                return if arg == "create" || arg == "new" {
                    PrCreateShape::Yes
                } else {
                    PrCreateShape::No
                };
            }
            _ => return PrCreateShape::No,
        }
    }
    PrCreateShape::Ambiguous
}

/// Resolves a program name to an executable path on `PATH`. A file without
/// the executable bit is rejected: pre-mint validation must not pass a child
/// that is guaranteed to fail at spawn time (after the token was minted).
fn which_executable(program: &str) -> Option<std::path::PathBuf> {
    let is_executable = |path: &std::path::Path| {
        #[cfg(unix)]
        {
            std::fs::metadata(path).is_ok_and(|metadata| {
                // A directory with an execute bit would mint a token for a
                // child that can never spawn: require a regular file too.
                metadata.is_file()
                    && std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o111 != 0
            })
        }
        #[cfg(not(unix))]
        {
            path.is_file()
        }
    };
    if program.contains(std::path::MAIN_SEPARATOR) {
        let path = std::path::PathBuf::from(program);
        return is_executable(&path).then_some(path);
    }
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

/// `orc github commit-author`: print ONLY the identity pair, derived from the
/// authenticated App — never hardcoded. `GET /app` is an App-level endpoint:
/// GitHub rejects installation tokens with 401, so it authenticates with a
/// freshly minted App JWT (itself secret material — same never-printed rules
/// as the token). `GET /app` carries the App `slug` but NOT the bot user id,
/// so the numeric id is resolved with an unauthenticated `GET /users/{slug}[bot]`
/// (public bot-user profile; no credential is spent on it).
fn commit_author<W: Write>(paths: &ConfigPaths, writer: &mut W) -> Result<()> {
    let (name, email) = commit_author_identity(paths)?;
    writeln!(writer, "name={name}").into_diagnostic()?;
    writeln!(writer, "email={email}").into_diagnostic()
}

/// Resolves the App's canonical commit identity: `GET /app` (App JWT bearer)
/// for the slug, then an installation-token-authenticated
/// `GET /users/{slug}[bot]` for the bot user id. Both responses are
/// validated; failures are typed and token-free.
fn commit_author_identity(paths: &ConfigPaths) -> Result<(String, String)> {
    let config = resolved_config(paths)?;
    let auth = github_app_auth(&config)?;
    let jwt = auth.app_jwt().into_diagnostic()?;
    let transport = ApiTransport::new(paths.github_api_endpoint.clone())?;
    let (status, body) = transport.request_bearer("GET", "app", jwt.expose_secret(), None)?;
    if !status.is_success() {
        bail!("github api request returned HTTP {status} (GET /app)");
    }
    drop(jwt);
    let app: Value = serde_json::from_str(&body)
        .map_err(|_| miette!("github app response is malformed: unexpected JSON shape"))?;
    let slug = app_slug(&app)?;
    let bot_login = format!("{slug}[bot]");
    // The bot user id is not part of the `GET /app` payload: resolve it from
    // the bot-user profile. Authenticated with the installation token for
    // the configured organization's installation: GitHub rejects the App
    // JWT on `GET /users` (401), and on an Enterprise Managed Users
    // organization the profile is NOT publicly visible — an unauthenticated
    // request answers 404 even though the App is correctly configured. The
    // token stays in memory and rides only this one Authorization header.
    let (_, token) = mint_installation_token(paths)?;
    let (user_status, user_body) =
        transport.request("GET", &format!("users/{bot_login}"), &token, None)?;
    if !user_status.is_success() {
        bail!("github api request returned HTTP {user_status} (GET /users/{bot_login})");
    }
    let bot_user: Value = serde_json::from_str(&user_body)
        .map_err(|_| miette!("github user response is malformed: unexpected JSON shape"))?;
    derive_commit_identity(slug, &bot_user)
}

// --- `orc github verify-identity`: commit-identity gate ----------------------

/// Compares the ambient git commit identity (`git config user.name` /
/// `user.email` as a commit in `path` would resolve them) against the
/// App-derived canonical commit identity — the Rust twin of the
/// `commit-identity` skill script. On mismatch the typed error carries the
/// exact `git -c user.name=… -c user.email=…` fix; secrets are never part of
/// the identity, so the fix line is safe to print.
///
/// The comparison is exact for BOTH fields: a name-only match (or an email
/// noreply look-alike with a different bot id) is a mismatch — attribution
/// is either the canonical pair or it is a violation.
fn verify_identity(paths: &ConfigPaths, args: &VerifyIdentityArgs) -> Result<()> {
    let (expected_name, expected_email) = commit_author_identity(paths)?;
    let (ambient_name, ambient_email) = ambient_git_identity(args.path.as_deref())?;
    if ambient_name == expected_name && ambient_email == expected_email {
        writeln!(
            std::io::stderr(),
            "identity OK: {ambient_name} <{ambient_email}> matches the App commit identity"
        )
        .into_diagnostic()?;
        return Ok(());
    }
    bail!(
        "commit identity mismatch in {}:\n  ambient:  {} <{}>\n  expected: {} <{}>\n\
         commits here would NOT be authored as the service-identity bot (a fresh worktree \
         inherits the global gitconfig). Fix with either:\n  \
         git -c user.name='{expected_name}' -c user.email='{expected_email}' commit …\n  \
         git config user.name '{expected_name}' && git config user.email '{expected_email}'",
        args.path.as_deref().unwrap_or("."),
        ambient_name,
        ambient_email,
        expected_name,
        expected_email,
    )
}

/// Resolves the ambient commit identity for `path` the way `git commit`
/// would: `git -C <path> config user.name` / `user.email` (repo-local,
/// worktree, global — git's normal precedence). An unset value is a typed
/// error naming the key; a failed `git` invocation is a typed error carrying
/// git's stderr (never credential material).
fn ambient_git_identity(path: Option<&str>) -> Result<(String, String)> {
    let read_field = |key: &str| -> Result<String> {
        let mut command = Command::new("git");
        if let Some(dir) = path {
            command.arg("-C").arg(dir);
        }
        let output = command
            .args(["config", "--get", key])
            .output()
            .map_err(|error| miette!("failed to run `git config --get {key}`: {error}"))?;
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if value.is_empty() {
            bail!(
                "`git config {key}` is unset for {} (a commit would fail or fall back to an \
                 unintended identity) — set it to the App commit identity printed by \
                 `orc github commit-author`",
                path.unwrap_or(".")
            );
        }
        Ok(value)
    };
    let name = read_field("user.name")?;
    let email = read_field("user.email")?;
    Ok((name, email))
}

// --- `orc github push-branch`: App-signed branch landing ---------------------

/// Computes the branch tree, lands it as ONE App-signed squashed commit on
/// the remote branch via GraphQL `createCommitOnBranch`, force-moves the ref
/// when the branch already exists, and gates on tree equality and
/// `verified = true` — fail-closed with typed errors, never touching plain
/// `git push`.
///
/// New-commit policy (skill `verified-commit-path.md`): commits are created
/// via the App API from the start, never pushed unsigned and replayed. The
/// local branch keeps its own history; the REMOTE branch receives exactly one
/// squashed commit whose tree is byte-identical to the local branch tree.
#[allow(clippy::too_many_lines)]
fn push_branch(paths: &ConfigPaths, args: &PushBranchArgs) -> Result<()> {
    // Argument defaults resolved from the ambient checkout: owner/repo from
    // the `origin` remote URL, the local branch from HEAD. An unresolvable
    // origin only matters when --owner/--repo are not given (typed error
    // then); an explicit flag always wins. A resolvable origin with a
    // non-GitHub shape is reported but overridden by explicit flags.
    let (origin_owner, origin_repo) = origin_owner_repo();
    let (owner, repo) = resolve_repo_slug(
        args.owner.as_deref(),
        args.repo.as_deref(),
        origin_owner.as_deref(),
        origin_repo.as_deref(),
    )?;
    let local_branch = match &args.branch {
        Some(branch) => branch.clone(),
        None => current_branch()?,
    };
    let remote_branch = args
        .remote_branch
        .clone()
        .unwrap_or_else(|| local_branch.clone());
    let body = match &args.body_file {
        Some(file) => read_body_file(file)?,
        None => String::new(),
    };

    // Fail-closed gate: identical to the other mutating subcommands — the
    // landing path exists for the App service identity, so `required`
    // enforcement refuses to run it without a complete `github_app` config.
    let config = resolved_config(paths)?;
    if enforcement_mode(&config) == ServiceIdentityEnforcement::Required {
        require_service_identity(&config, "land an App-signed branch commit")?;
    }

    // 1. The intended tree: the branch's committed tree object (no index,
    //    no textconv) — exactly the identity the remote must match
    //    (`pr-convergence.md`).
    let local_tree = git(None, &["rev-parse", &format!("{local_branch}^{{tree}}")])?;

    // 1b. Fail-closed repo-identity gate (#529): the branch and its tree
    //     were resolved from the AMBIENT process cwd — a wrong cwd (or an
    //     ambient `GIT_DIR` redirect, or a colonized branch ref) silently
    //     points the landing at a different tree, and the remote-side
    //     tree-equality gate compares against that same (wrong) ref, so it
    //     cannot catch the corruption. When the caller pins the tree it
    //     intends to land, refuse on ANY divergence before mutating.
    verify_expect_tree(&local_tree, args.expect_tree.as_deref(), &local_branch)?;

    // 2. Mint and drive the remote: one GraphQL endpoint, installation-token
    //    bearer, every mutation's result validated. The token rides only the
    //    Authorization header and never enters a diagnostic.
    let (_, token) = mint_installation_token(paths)?;
    let transport = ApiTransport::new(paths.github_graphql_endpoint.clone())?;
    let graphql = GraphqlSession {
        transport: &transport,
        bearer: token.token().expose_secret(),
        owner: owner.clone(),
        repo: repo.clone(),
    };

    // Read the remote branch's current head (absent for a new branch). The
    // empty-diff gate runs against the REMOTE head, not the base: a base-tree
    // match alone says nothing when a landing already moved the remote past
    // the base (that landing must DELETE the files the remote head has).
    let existing = graphql.branch_ref(&owner, &repo, &remote_branch)?;
    let (diff_base_tree, base_commit) = if existing.is_some() {
        // Existing remote branch: the remote head tree is ALWAYS the diff
        // base — an earlier landing may have moved the remote tree past the
        // base, and that landing's files must be deletable here. An explicit
        // --base is still honored as a fallback source for the head objects
        // (see `remote_head_tree`).
        let tree = remote_head_tree(
            token.token().expose_secret(),
            existing.as_ref(),
            args.base.as_deref(),
            &remote_branch,
            &owner,
            &repo,
        )?;
        let head = existing
            .as_ref()
            .and_then(|ref_payload| ref_payload.pointer("/target/oid"))
            .and_then(Value::as_str)
            .map(str::to_string);
        (tree, head)
    } else {
        // New remote branch: the diff base is --base, else the repository's
        // default branch. A branch-name base is resolved through the API (a
        // LOCAL `main` ref may be stale — behind origin — which would
        // compute the wrong change set); an explicit revision the API ref
        // probe cannot resolve (a raw SHA, `origin/main`, a tag) falls back
        // to a local `git rev-parse` so `--base <sha>` keeps working. The
        // resolved head commit is fetched so its tree and blobs are locally
        // readable.
        let base_ref = match &args.base {
            Some(base) => base.clone(),
            None => graphql.default_branch(&owner, &repo)?,
        };
        let base_head = match graphql.default_branch_head(&owner, &repo, &base_ref) {
            // Remote branch resolved through the API (never a stale local ref).
            Ok(Some(head)) => head,
            // ONLY an absent remote branch falls back to a local revision of
            // an explicit --base (a raw SHA, origin/main, a tag). API
            // transport/HTTP/GraphQL errors propagate: an API failure must
            // never be mistaken for "no such branch" and diffed against a
            // possibly-stale local ref.
            Ok(None) if args.base.is_some() => git(None, &["rev-parse", "--verify", &base_ref])
                .map_err(|_| {
                    miette!(
                        "cannot resolve the --base revision `{base_ref}`: it is neither a \
                         remote branch of {owner}/{repo} nor a local revision"
                    )
                })?,
            Ok(None) => {
                bail!(
                    "the default branch of {owner}/{repo} resolved to {base_ref} but no such \
                     remote branch exists — pass --base <ref> explicitly"
                )
            }
            //     whose NEW commits carry two or more CONSECUTIVE identical
            //     subjects is refused — review-fix rounds must land as distinct
            //     Conventional Commits subjects (or be squashed through this
            //     command's single-commit landing). The comparison head is the
            //     RESOLVED landing base (args.base or the default branch), and the
            //     fetch targets the SELECTED repository (--repo), so the guard
            //     never compares against the wrong history.
            Err(error) => return Err(error),
        };
        //     whose NEW commits carry two or more CONSECUTIVE identical
        //     subjects is refused — review-fix rounds must land as distinct
        //     Conventional Commits subjects (or be squashed through this
        //     command's single-commit landing). The comparison head is the
        //     RESOLVED landing base (--base or the default branch), and the
        //     fetch targets the SELECTED repository (--repo), so the guard
        //     never compares against the wrong history.
        let repo_url = format!("https://github.com/{owner}/{repo}.git");
        if !git_fetch_authenticated(token.token().expose_secret(), &owner, &repo, &base_head) {
            // Fall through: the rev-parse below produces the typed error
            // when the object is truly unavailable.
        }
        let tree = git(None, &["rev-parse", &format!("{base_head}^{{tree}}")]).map_err(|_| {
            miette!(
                "cannot read the base {base_ref} head {base_head} tree locally after the fetch \
                 — run `git fetch {repo_url}` and retry"
            )
        })?;
        (tree, Some(base_head))
    };
    // 1c. Duplicate-subject guard (owner directive 2026-10-15): a push
    //     whose NEW commits carry two or more CONSECUTIVE identical
    //     subjects is refused — review-fix rounds must land as distinct
    //     Conventional Commits subjects (or be squashed through this
    //     command's single-commit landing). The comparison head is the
    //     RESOLVED base head (the remote head for an existing branch, the
    //     --base/default head for a new branch) from the SELECTED
    //     repository, so the guard never compares against the wrong
    //     history. Runs for new AND existing remote branches.
    // The remote head's subject (the one commit already on the remote
    // branch): fetch the remote head authenticated and read its subject
    // from FETCH_HEAD. Unavailable → None (no comparison, guard passes).
    let remote_head_subject = existing.as_ref().and_then(|ref_payload| {
        ref_payload
            .pointer("/target/oid")
            .and_then(Value::as_str)
            .and_then(|oid| {
                if git_fetch_authenticated(token.token().expose_secret(), &owner, &repo, oid) {
                    git(None, &["log", "-1", "--format=%s", "FETCH_HEAD"]).ok()
                } else {
                    None
                }
            })
    });
    reject_duplicate_subjects(remote_head_subject.as_deref(), &args.message, &local_branch)?;
    let changes = diff_tree_changes(None, &diff_base_tree, &local_tree)?;
    if changes.is_empty() && !args.re_land {
        writeln!(
            std::io::stderr(),
            "branch `{local_branch}` tree {local_tree} is identical to the remote head tree — \
             nothing to land (pass --re-land to re-sign the head with an empty verified commit)"
        )
        .into_diagnostic()?;
        return Ok(());
    }
    if changes.is_empty() {
        // --dry-run must exit before any mutation — including the re-land
        // path, whose helper can create temp refs and commits on the remote.
        // The re-land path also requires an existing remote branch: report
        // that contract here too instead of a plan the real run would refuse.
        if args.dry_run {
            if existing.is_none() {
                bail!(
                    "--re-land requires an existing remote branch; `{remote_branch}` does \
                     not exist on the remote"
                );
            }
            writeln!(
                std::io::stderr(),
                "dry-run: would re-land {owner}/{repo} {remote_branch} (empty diff, \
                 re-signing the head) at tree {local_tree}\n\
                 message: {}",
                args.message,
            )
            .into_diagnostic()?;
            return Ok(());
        }
        return re_land_unsigned_head(
            &graphql,
            existing.as_ref(),
            &remote_branch,
            &owner,
            &repo,
            args,
            &body,
            &local_tree,
        );
    }

    // Build the fileChanges payload. Contents come from the branch's
    // committed blobs (`git cat-file blob`), never the working directory.
    let file_changes = build_file_changes(None, &changes, &local_branch, &diff_base_tree)?;

    // --dry-run: print the resolved landing plan and stop before any
    // mutation (the branch-ref read and — for a new branch — the
    // default-branch lookup already happened, but nothing was written).
    if args.dry_run {
        writeln!(
            std::io::stderr(),
            "dry-run: would land {owner}/{repo} {remote_branch} at tree {local_tree}\n\
             base tree for the change set: {diff_base_tree}\n\
             message: {}\n\
             fileChanges: {} addition(s), {} deletion(s)",
            args.message,
            file_changes
                .get("additions")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
            file_changes
                .get("deletions")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        )
        .into_diagnostic()?;
        return Ok(());
    }

    let repository_id = graphql.repository_id(&owner, &repo)?;

    // 4. Land via a TEMPORARY branch so the tree/verification gates run
    //    BEFORE the real ref moves: createCommitOnBranch advances whatever
    //    branch it lands on, so landing directly would move the real branch
    //    even when a gate later fails. The temp ref is deleted on every
    //    path; the real ref then moves fast-forward-only (see below).
    //    Unique per invocation: two concurrent landings with the same tree
    //    but different remote heads must not delete or collide on each
    //    other's live temp ref.
    let temp_branch = format!("push-branch/tmp-{}", uuid::Uuid::new_v4());
    // The temp branch MUST start at the remote head when the branch exists:
    // the change set was computed as diff remote-head-tree -> local-tree and
    // is applied on top of the temp head, so bootstrapping at the base commit
    // would DROP the remote head's commits from the landed tree. For a new
    // branch the remote head does not exist and the base commit is correct.
    let temp_bootstrap = match existing
        .as_ref()
        .and_then(|ref_payload| ref_payload.pointer("/target/oid").and_then(Value::as_str))
        .or(base_commit.as_deref())
    {
        Some(oid) => oid.to_string(),
        None => bail!(
            "cannot resolve a bootstrap commit for the new branch: the base head is unknown — \
             pass --base <ref>"
        ),
    };
    let (temp_head, temp_ref_id) = bootstrap_temp_branch(
        &graphql,
        &owner,
        &repo,
        &repository_id,
        &temp_branch,
        &temp_bootstrap,
    )?;
    let new_head = land_on_temp_branch(
        &graphql,
        &repository_id,
        &temp_branch,
        &temp_ref_id,
        &temp_head,
        &args.message,
        &body,
        &file_changes,
        &local_tree,
        &local_branch,
    )?;

    // 5. Move the real branch ref onto the verified commit with an
    //    exact-head precondition (updateRefs/RefUpdate.beforeOid = observed
    //    head, force=false): a concurrent advance OR rewind of the real
    //    branch in the window fails the precondition — nothing is
    //    overwritten. A gate or mutation failure inside the landing helper
    //    already deleted the temp ref; on the success path it is deleted
    //    after the swing.
    let observed_head = existing
        .as_ref()
        .and_then(|ref_payload| ref_payload.pointer("/target/oid").and_then(Value::as_str))
        .map(str::to_string);
    let swing = match observed_head.as_deref() {
        Some(observed) => graphql.move_ref_with_precondition(
            &owner,
            &repo,
            &repository_id,
            &remote_branch,
            observed,
            &new_head,
        ),
        // New branch: the ref never existed on the remote before this run —
        // create the real ref directly at the verified commit.
        None => graphql
            .create_ref(
                &repository_id,
                &format!("refs/heads/{remote_branch}"),
                &new_head,
            )
            .map(|_| ()),
    };
    let _ = graphql.delete_ref(&temp_ref_id);
    swing?;

    writeln!(
        std::io::stderr(),
        "landed {remote_branch} at {new_head} (App-signed, tree {local_tree})"
    )
    .into_diagnostic()?;
    Ok(())
}

#[allow(clippy::too_many_lines)]
/// Refuses a push whose LANDING headline (the one commit push-branch
/// creates) duplicates the REMOTE branch head's subject — consecutive
/// identical subjects on the remote branch defeat PR review convergence
/// (owner directive 2026-10-15: review-fix rounds landed with only the
/// body differing). Local commit subjects NEVER reach the remote
/// (push-branch squashes), so local history is not examined.
///
/// # Errors
///
/// Typed diagnostic naming the duplicated subject when the landing
/// headline equals the remote head's subject.
fn reject_duplicate_subjects(
    remote_head_subject: Option<&str>,
    landing_headline: &str,
    local_branch: &str,
) -> Result<()> {
    let Some(remote_subject) = remote_head_subject else {
        return Ok(());
    };
    if remote_subject == landing_headline {
        bail!(
            "refusing to push: the landing commit on `{local_branch}` would carry the subject \
             `{remote_subject}` — identical to the existing remote head's subject (consecutive \
             identical subjects defeat PR review convergence). Distinct subjects are required; \
             squash remediation rounds into one differently-titled commit"
        );
    }
    Ok(())
}

/// The pure duplicate-subject check over an oldest-first subject list.
///
/// Resolves the target `(owner, repo)` slug. `--repo owner/name` is used
/// as-is; a bare `--repo name` combines with the owner (explicit `--owner`
/// or the origin-derived one). With no flags both come from origin.
///
/// # Errors
/// Typed error when owner or repo cannot be resolved from flags or origin.
fn resolve_repo_slug(
    owner_flag: Option<&str>,
    repo_flag: Option<&str>,
    origin_owner: Option<&str>,
    origin_repo: Option<&str>,
) -> Result<(String, String)> {
    let owner_flag = owner_flag.map(str::trim).filter(|owner| !owner.is_empty());
    // `--repo owner/name` wins verbatim — never re-prepend an owner on top
    // of the origin-derived one (the double-owner `arbsec/arbsec/x` bug).
    if let Some((owner, repo)) = repo_flag.map(str::trim).and_then(|r| r.split_once('/')) {
        let (owner, repo) = (owner.trim(), repo.trim());
        if owner.is_empty() || repo.is_empty() {
            bail!(
                "invalid --repo value `{}`: expected `owner/name` or `name`",
                repo_flag.unwrap_or("<missing>")
            );
        }
        return Ok((owner.to_string(), repo.to_string()));
    }
    let repo = match repo_flag.map(str::trim).filter(|repo| !repo.is_empty()) {
        Some(repo) => repo.to_string(),
        None => origin_repo
            .ok_or_else(|| {
                miette!(
                    "cannot resolve the target repository: {}/{}, and the origin remote of \
                     the current directory does not name an owner/repo — pass --owner \
                     <owner> --repo <name>",
                    owner_flag.or(origin_owner).unwrap_or("<missing>"),
                    repo_flag.unwrap_or("<missing>"),
                )
            })?
            .to_string(),
    };
    let owner = owner_flag
        .or(origin_owner)
        .ok_or_else(|| {
            miette!(
                "cannot resolve the repository owner for --repo {repo}: no --owner flag and \
                 the origin remote of the current directory does not name an owner/repo"
            )
        })?
        .to_string();
    Ok((owner, repo))
}

/// Resolves the `(owner, repo)` pair from the `origin` remote URL of the
/// current directory (`git@github.com:owner/repo.git` or an https URL).
/// `None` on either side when the origin is absent or not that shape —
/// callers decide whether that is fatal (no explicit `--owner`/`--repo`).
pub(crate) fn origin_owner_repo() -> (Option<String>, Option<String>) {
    let Ok(url) = git(None, &["remote", "get-url", "origin"]) else {
        return (None, None);
    };
    match parse_github_owner_repo(&url) {
        Some((owner, repo)) => (Some(owner), Some(repo)),
        None => (None, None),
    }
}

/// Parses `owner/name` out of a GitHub remote URL (ssh or https on
/// github.com, with or without `.git`). Returns `None` for anything that is
/// not that shape (other hosts, local paths).
fn parse_github_owner_repo(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let tail = if let Some((_, rest)) = trimmed.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        // The authority is `[user@]host` — an ssh:// URL carries the git user
        // (`ssh://git@github.com/owner/repo.git`); strip it before validating
        // the host. Only github.com is accepted.
        let host = authority.rsplit('@').next()?;
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        Some(path)
    } else if let Some((host, rest)) = trimmed.split_once(':') {
        // scp-like ssh form: git@github.com:owner/repo — reject absolute
        // paths (/owner/repo) and drive letters (C:\...).
        if host
            .rsplit('@')
            .next()
            .is_some_and(|host| host.eq_ignore_ascii_case("github.com"))
            && !rest.starts_with('/')
        {
            Some(rest)
        } else {
            None
        }
    } else {
        None
    }?;
    let (owner, repo) = tail.trim_end_matches('/').split_once('/')?;
    let (owner, repo) = (owner.trim(), repo.trim());
    (!owner.is_empty() && !repo.is_empty() && !owner.contains(char::is_whitespace))
        .then(|| (owner.to_string(), repo.to_string()))
}

/// Resolves the current branch of the working directory (symbolic-ref).
/// Detached HEAD is a typed error — the landing targets a branch.
fn current_branch() -> Result<String> {
    git(None, &["symbolic-ref", "--quiet", "--short", "HEAD"]).map_err(|_| {
        miette!(
            "cannot resolve the current branch (detached HEAD or not a git repository) — \
             pass --branch <name>"
        )
    })
}

/// Reads the `--body-file` value (`-` = stdin) for the commit message body.
fn read_body_file(file: &str) -> Result<String> {
    if file == "-" {
        let mut buffer = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer).into_diagnostic()?;
        Ok(buffer)
    } else {
        std::fs::read_to_string(file).into_diagnostic()
    }
}

/// Re-lands a rebased PR branch whose tree already matches the remote head
/// (issue #498): the head chain may carry unsigned commits, so the PR head
/// moves to a verified commit on top of the current remote head.
///
/// Limitation (documented in `--help` and `docs/cli/orc-github.md`): ONE
/// empty signed commit on the tip verifies the head commit GitHub's
/// `required_signatures` rule evaluates on push; it does NOT rewrite
/// unsigned ancestors into verified objects — a range check that inspects
/// every commit can still block the PR. This tool cannot mint verified
/// replacements for historical commits (the App API cannot re-sign existing
/// objects); a range-level repair needs the manual temp-branch replay from
/// `references/verified-commit-path.md`.
///
/// Flow mirrors the ordinary landing: the empty commit lands on a TEMP
/// branch (gate before the real ref moves), then the real ref CAS-moves.
#[allow(clippy::too_many_arguments)]
fn re_land_unsigned_head(
    graphql: &GraphqlSession<'_>,
    existing: Option<&Value>,
    remote_branch: &str,
    owner: &str,
    repo: &str,
    args: &PushBranchArgs,
    body: &str,
    local_tree: &str,
) -> Result<()> {
    let Some(ref_payload) = existing else {
        bail!(
            "--re-land requires an existing remote branch; `{remote_branch}` does not exist on \
             the remote"
        );
    };
    let remote_head = ref_payload
        .pointer("/target/oid")
        .and_then(Value::as_str)
        .ok_or_else(|| miette!("branch ref response is malformed: missing target.oid"))?;
    let head_commit = graphql.head_commit(owner, repo, remote_head)?;
    let already_verified = head_commit
        .pointer("/signature/isValid")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if already_verified {
        writeln!(
            std::io::stderr(),
            "remote head {remote_head} of `{remote_branch}` is already verified — --re-land is a \
             no-op"
        )
        .into_diagnostic()?;
        return Ok(());
    }
    // Resolved ONLY after the verified-head no-op check: the documented
    // no-op must not fail on a repository-ID read error.
    let repository_id = graphql.repository_id(owner, repo)?;

    // Temp branch absorbs the mutation so the gate runs BEFORE the real ref
    // moves (same failure-containment shape as the ordinary landing). The
    // empty commit's tree equals the remote head tree, which equals
    // `local_tree` here — the tree gate in `verify_landed_commit` holds.
    let temp_branch = format!("push-branch/tmp-re-land-{}", uuid::Uuid::new_v4());
    let (temp_head, temp_ref_id) = bootstrap_temp_branch(
        graphql,
        owner,
        repo,
        &repository_id,
        &temp_branch,
        remote_head,
    )?;
    let empty_changes = serde_json::json!({"additions": [], "deletions": []});
    let new_head = match land_on_temp_branch(
        graphql,
        &repository_id,
        &temp_branch,
        &temp_ref_id,
        &temp_head,
        &args.message,
        body,
        &empty_changes,
        local_tree,
        remote_branch,
    ) {
        Ok(oid) => oid,
        Err(error) => {
            let _ = graphql.delete_ref(&temp_ref_id);
            return Err(error);
        }
    };
    let _ = graphql.delete_ref(&temp_ref_id);
    // CAS the real branch onto the verified commit. The empty commit's
    // parent IS the observed remote head, so `beforeOid` = remote head makes
    // the move a fast-forward: a concurrent advance or rewind fails the
    // precondition — nothing is overwritten.
    graphql.move_ref_with_precondition(
        owner,
        repo,
        &repository_id,
        remote_branch,
        remote_head,
        &new_head,
    )?;

    writeln!(
        std::io::stderr(),
        "re-landed {remote_branch} at {new_head} (App-signed empty commit, parent {remote_head}; \
         head commit now verified — note: unsigned ANCESTORS in the range are not rewritten, \
         a range-level required_signatures check may still block the merge)"
    )
    .into_diagnostic()?;
    Ok(())
}

/// Creates the landing commit on the temporary branch and runs the
/// tree/verification gates against it BEFORE the real branch ref moves.
/// `createCommitOnBranch` advances whatever branch it lands on, so the temp
/// branch absorbs the mutation: a failed gate (or a failed mutation) leaves
/// the real remote branch untouched and the temp ref is deleted on every
/// path. Returns the verified commit's oid.
#[allow(clippy::too_many_arguments)]
fn land_on_temp_branch(
    graphql: &GraphqlSession<'_>,
    _repository_id: &str,
    temp_branch: &str,
    temp_ref_id: &str,
    temp_head: &str,
    message: &str,
    body: &str,
    file_changes: &Value,
    local_tree: &str,
    branch: &str,
) -> Result<String> {
    let commit = graphql.create_commit_on_branch(
        &graphql.owner,
        &graphql.repo,
        temp_branch,
        temp_head,
        message,
        body,
        file_changes,
    );
    let commit = match commit {
        Ok(commit) => commit,
        // The mutation itself failed (nothing landed): surface the typed
        // error after cleaning up the temp ref.
        Err(error) => {
            let _ = graphql.delete_ref(temp_ref_id);
            return Err(error);
        }
    };
    // The commit landed on the temp branch; run the gates BEFORE the real
    // ref moves.
    if let Err(error) = verify_landed_commit(&commit, local_tree, branch) {
        let _ = graphql.delete_ref(temp_ref_id);
        return Err(error);
    }
    commit_oid(&commit).map(str::to_string)
}

/// Runs `git diff-tree -r -z --no-renames --name-status` between two trees
/// and parses the records; `--no-commit-id` against two trees means no
/// leading oid field. `dir` anchors the invocation at a fixture repo (tests);
/// `None` resolves the repo from the process cwd (production).
fn diff_tree_changes(
    dir: Option<&std::path::Path>,
    base_tree: &str,
    tree: &str,
) -> Result<Vec<(String, std::path::PathBuf)>> {
    let raw = git_raw(
        dir,
        &[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--no-commit-id",
            "--name-status",
            base_tree,
            tree,
        ],
    )?;
    Ok(parse_diff_tree_z(&raw))
}

/// Runs `git fetch` against `https://github.com/<owner>/<repo>.git`
/// AUTHENTICATED as the App installation: the token rides the
/// `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` environment
/// pair scoped to `http.https://github.com/.extraheader` — the token NEVER
/// appears in process arguments (invisible to other local users via the
/// argv list), on disk, or in any diagnostic. Required for private
/// repositories; a public-repo fetch behaves identically.
///
/// `what` is a ref-ish spec (an OID, a branch name); failure is the caller's
/// decision (callers retry or fall back), so the result is just a bool.
fn git_fetch_authenticated(token: &str, owner: &str, repo: &str, what: &str) -> bool {
    const URL_PREFIX: &str = "http.https://github.com/.extraheader";
    let authorization = format!(
        "basic {}",
        base64_encode(format!("x-access-token:{token}").as_bytes())
    );
    let mut command = Command::new("git");
    command
        .args([
            "fetch",
            "--quiet",
            &format!("https://github.com/{owner}/{repo}.git"),
            what,
        ])
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", URL_PREFIX)
        .env("GIT_CONFIG_VALUE_0", authorization);
    // Same ambient-location scrub as `git_output`: a hook-exported GIT_DIR or
    // GIT_OBJECT_DIRECTORY must not redirect the fetch's object writes away
    // from the intended repository (#553/#529 corruption class) — otherwise
    // the fetched objects land in the hook's gitdir while the scrubbed
    // rev-parse calls read the cwd repo and never see them.
    for variable in AMBIENT_GIT_LOCATION_VARS {
        command.env_remove(variable);
    }
    let output = command.output();
    matches!(output, Ok(out) if out.status.success())
}

/// Standard base64 (RFC 4648 with padding) for the HTTP basic-auth header.
fn base64_encode(input: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(input)
}

/// Resolves the tree the landing change set is computed against: the remote
/// head's tree when the branch exists on the remote (an earlier landing may
/// have moved the remote tree past the base; a file removed locally that the
/// base never had must still be deleted remotely), or the base branch tree
/// for a new branch. The remote head commit is fetched first so its tree and
/// blobs are locally available; a raw-OID fetch falls back to a branch-name
/// fetch. An explicit `--base` ref is only trusted when its fetch demonstrably
/// brought the HEAD object itself in (`rev-parse --verify <head>` succeeds) —
/// the base's own tree is NOT a stand-in for the head tree: the remote head
/// may hold remote-only paths the base never had, and diffing against the
/// base would report an empty change set and silently drop them. A head whose
/// objects are still locally unavailable is a typed error.
fn remote_head_tree(
    token: &str,
    existing: Option<&Value>,
    fallback_base: Option<&str>,
    remote_branch: &str,
    owner: &str,
    repo: &str,
) -> Result<String> {
    let Some(ref_payload) = existing else {
        return Ok(String::new());
    };
    let head = ref_payload
        .pointer("/target/oid")
        .and_then(Value::as_str)
        .ok_or_else(|| miette!("branch ref response is malformed: missing target.oid"))?;
    // Fetch directly from the target repository URL — never the ambient
    // `origin` remote, which may point at a different repository (or not
    // exist) in this worktree. The fetch is App-authenticated (private
    // repos); token never appears in URLs or diagnostics.
    if !git_fetch_authenticated(token, owner, repo, head) {
        git_fetch_authenticated(token, owner, repo, remote_branch);
    }
    if let Ok(tree) = git(None, &["rev-parse", &format!("{head}^{{tree}}")]) {
        return Ok(tree);
    }
    // Last resort: fetch the explicit --base ref, then re-check the HEAD —
    // the base fetch may carry the head's history into the local object
    // store even when the direct head fetch failed. The base is NEVER used
    // as a stand-in for the head tree: only a head whose object is now
    // locally verified proceeds (falling back to the base's own tree would
    // mask remote-only paths as an empty diff).
    if let Some(base) = fallback_base
        && git_fetch_authenticated(token, owner, repo, base)
        && git(None, &["rev-parse", "--verify", "--quiet", head]).is_ok()
        && let Ok(tree) = git(None, &["rev-parse", &format!("{head}^{{tree}}")])
    {
        return Ok(tree);
    }
    bail!(
        "cannot read the remote head {head} tree of `{remote_branch}` locally — run \
         `git fetch https://github.com/{owner}/{repo}.git` first or pass --base <ref> \
         (its fetch may deliver the head's objects)"
    )
}

/// Builds the GraphQL `FileChanges` input from the parsed diff records:
/// additions/modifications carry base64 contents of the branch's COMMITTED
/// blobs (`git cat-file blob <branch>:<path>` — never the working directory,
/// which may carry uncommitted edits or a different checkout), deletions
/// carry the path. An unreadable blob or an unknown diff status is a typed
/// error — nothing is landed on a partial change set.
///
/// Mode handling: `createCommitOnBranch` `FileAddition` CANNOT set the
/// executable bit — an ADDED file that is 100755 locally lands as 100644.
/// That is warned about on stderr (tree gate then catches any mismatch);
/// for a file that already exists in the diff base with 100755, the
/// modification keeps GitHub's stored mode and no warning is needed.
///
/// `dir` anchors every git invocation at a fixture repo (tests); `None`
/// resolves the repo from the process cwd (production).
fn build_file_changes(
    dir: Option<&std::path::Path>,
    changes: &[(String, std::path::PathBuf)],
    branch: &str,
    diff_base_tree: &str,
) -> Result<Value> {
    let base_modes = base_tree_modes(dir, diff_base_tree);
    let mut additions = Vec::new();
    let mut deletions = Vec::new();
    for (status, path) in changes {
        match status.as_str() {
            "A" | "M" => {
                let spec = format!("{branch}:{}", path.display());
                // Reject non-regular-file entries: symlinks (120000) and
                // submodules (160000) cannot be carried by the FileAddition
                // contents payload — landing would silently materialize them
                // as regular files or fail opaquely.
                let object_type = git(dir, &["cat-file", "-t", &spec])?;
                if object_type != "blob" {
                    bail!(
                        "cannot land `{spec}`: it is a {object_type}, not a regular file blob — \
                         symlinks and submodules are not supported by the GraphQL fileChanges \
                         payload"
                    );
                }
                let bytes = git_raw(dir, &["cat-file", "blob", &spec]).map_err(|error| {
                    miette!("failed to read blob `{spec}` for the landing commit: {error}")
                })?;
                let mode = tree_entry_mode(&local_tree_modes(dir, branch)?, path);
                let path_key = path.display().to_string();
                if status == "A"
                    && mode == "100755"
                    && base_modes.get(&path_key).map(String::as_str) != Some("100755")
                {
                    writeln!(
                        std::io::stderr(),
                        "WARNING: `{path_key}` is executable (100755) locally but is being ADDED — \
                         GitHub's createCommitOnBranch FileAddition always lands 100644, so the \
                         remote tree will differ from the local tree (the tree gate below will \
                         refuse the landing). Commit the file as 100644 locally, or land it \
                         first with the mode already 100644 and chmod locally afterwards.",
                    )
                    .into_diagnostic()?;
                }
                additions.push(serde_json::json!({
                    "path": path.display().to_string(),
                    "contents": BASE64_ENGINE.encode(&bytes),
                }));
            }
            "D" => {
                // FileDeletion input shape: {"path": "…"}, not a bare string.
                deletions.push(serde_json::json!({"path": path.display().to_string()}));
            }
            other => {
                bail!(
                    "unexpected diff-tree status `{other}` for `{}` — refusing to land an ambiguous \
                     change set",
                    path.display()
                );
            }
        }
    }
    Ok(serde_json::json!({"additions": additions, "deletions": deletions}))
}

/// Reads the local tree's entry modes (`git ls-tree -r -z <tree>`): path ->
/// "100644"|"100755". Unreadable trees are a typed error (callers already
/// validated the tree exists; only a concurrent object-store change fails).
/// `dir` anchors the invocation at a fixture repo (tests); `None` resolves
/// the repo from the process cwd (production).
fn local_tree_modes(
    dir: Option<&std::path::Path>,
    tree: &str,
) -> Result<std::collections::HashMap<String, String>> {
    let raw = git_raw(dir, &["ls-tree", "-r", "-z", tree])?;
    Ok(parse_ls_tree_z(&raw))
}

/// Reads the diff-base tree's entry modes, for the added-file mode check.
/// An unreadable base tree is not fatal: the mode check then treats every
/// added file as new (the conservative warning path). `dir` anchors the
/// invocation at a fixture repo (tests); `None` resolves the repo from the
/// process cwd (production).
fn base_tree_modes(
    dir: Option<&std::path::Path>,
    diff_base_tree: &str,
) -> std::collections::HashMap<String, String> {
    if diff_base_tree.is_empty() {
        return std::collections::HashMap::new();
    }
    match git_raw(dir, &["ls-tree", "-r", "-z", diff_base_tree]) {
        Ok(raw) => parse_ls_tree_z(&raw),
        Err(_) => std::collections::HashMap::new(),
    }
}

/// Parses `git ls-tree -r -z` records: `MODE TYPE OID\tpath\0` per entry.
fn parse_ls_tree_z(raw: &[u8]) -> std::collections::HashMap<String, String> {
    let mut modes = std::collections::HashMap::new();
    for record in raw.split(|byte| *byte == 0_u8) {
        if record.is_empty() {
            continue;
        }
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let (meta, path) = (&record[..tab], &record[tab + 1..]);
        let meta = String::from_utf8_lossy(meta);
        let Some(mode) = meta.split(' ').next() else {
            continue;
        };
        modes.insert(String::from_utf8_lossy(path).to_string(), mode.to_string());
    }
    modes
}

/// Looks up one path's mode in an `ls-tree` map ("100644" when absent).
fn tree_entry_mode(
    modes: &std::collections::HashMap<String, String>,
    path: &std::path::Path,
) -> String {
    modes
        .get(&path.display().to_string())
        .map_or_else(|| "100644".to_string(), Clone::clone)
}

/// Creates the temporary landing branch at `bootstrap_oid` for
/// `owner/repo`, after deleting any stale temp ref left by a previous run
/// with the same tree (landing on top of it would append to the WRONG
/// commit). Returns `(head oid, ref id)`.
fn bootstrap_temp_branch(
    graphql: &GraphqlSession<'_>,
    owner: &str,
    repo: &str,
    repository_id: &str,
    temp_branch: &str,
    bootstrap_oid: &str,
) -> Result<(String, String)> {
    // The ref name is invocation-unique (UUID), so an existing ref here is
    // an unexpected collision — never delete a ref this invocation did not
    // create (a concurrent landing may own it).
    if graphql.branch_ref(owner, repo, temp_branch)?.is_some() {
        bail!(
            "temporary branch `{temp_branch}` already exists (unexpected collision) — refusing \
             to touch it; retry to pick a fresh name"
        );
    }
    let created = graphql.create_ref(
        repository_id,
        &format!("refs/heads/{temp_branch}"),
        bootstrap_oid,
    )?;
    let head = created
        .pointer("/target/oid")
        .and_then(Value::as_str)
        .ok_or_else(|| miette!("createRef response is malformed: missing target.oid"))?
        .to_string();
    let ref_id = created
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| miette!("createRef response is malformed: missing ref.id"))?
        .to_string();
    Ok((head, ref_id))
}

/// Verifies a landed commit against the gates: tree equality with the local
/// branch tree, and GitHub-web-flow verification (`signature.isValid`). Any
/// gate failure is a typed error; the caller has NOT yet moved the ref, so a
/// failed gate on an existing branch leaves the remote head unchanged, and on
/// a new branch leaves only the base-pinned bootstrap ref (reported in the
/// error text).
fn verify_landed_commit(commit: &Value, local_tree: &str, branch: &str) -> Result<()> {
    let oid = commit_oid(commit)?;
    let landed_tree = commit
        .pointer("/tree/oid")
        .and_then(Value::as_str)
        .ok_or_else(|| miette!("landing commit response is malformed: missing tree.oid"))?;
    if landed_tree != local_tree {
        bail!(
            "landing commit tree mismatch: remote commit {oid} tree is {landed_tree}, local \
             branch `{branch}` tree is {local_tree} — refusing to move the ref; the remote \
             branch is unchanged"
        );
    }
    let verified = commit
        .get("signature")
        .and_then(|signature| signature.get("isValid"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !verified {
        bail!(
            "landing commit {oid} is NOT verified (signature missing or invalid) — refusing to \
             move the ref; the remote branch is unchanged; the required_signatures ruleset \
             would reject this commit at merge time"
        );
    }
    Ok(())
}

/// Extracts the landed commit oid from a `createCommitOnBranch` payload.
fn commit_oid(commit: &Value) -> Result<&str> {
    commit
        .get("oid")
        .and_then(Value::as_str)
        .ok_or_else(|| miette!("landing commit response is malformed: missing commit.oid"))
}

/// Environment variables that redirect `git` away from the intended
/// repository location or its object/ref store. Inherited values from a
/// hook or wrapper shell (a pre-push hook exports `GIT_DIR`/
/// `GIT_WORK_TREE`/`GIT_INDEX_FILE`) would otherwise bind every child
/// `git` to the caller's ambient gitdir instead of the process cwd — the
/// #553/#529 corruption class. `GIT_OBJECT_DIRECTORY`,
/// `GIT_ALTERNATE_OBJECT_DIRECTORIES`, and `GIT_COMMON_DIR` redirect
/// object and ref lookups even when the gitdir itself resolves correctly,
/// and `GIT_NAMESPACE` narrows refs to a namespace, so a landed tree and
/// its blobs could silently come from a different store.
const AMBIENT_GIT_LOCATION_VARS: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

/// Fail-closed repo-identity gate for `push-branch` (#529): the local
/// branch and its tree are resolved from the AMBIENT process cwd, so a
/// wrong cwd (or an inherited `GIT_DIR`, or a colonized branch ref) silently
/// points the landing at a different tree — and the remote-side
/// tree-equality gate compares against that same (wrong) ref, so it cannot
/// catch the corruption. When the caller pins the tree it intends to land
/// (`--expect-tree`), ANY divergence refuses the landing before anything is
/// mutated. A missing flag (`None`) means the caller opted out; an explicitly
/// provided EMPTY or whitespace-only pin is a misuse (e.g. a failed
/// command substitution) and is a typed error — silently treating it as
/// absent would disable the fail-closed gate (#529 intent; local review,
/// head 4b26b0e).
///
/// # Errors
/// Typed diagnostic naming both trees when the gate refuses, or a typed
/// error when the pin is present but blank.
fn verify_expect_tree(
    local_tree: &str,
    expect_tree: Option<&str>,
    local_branch: &str,
) -> Result<()> {
    let Some(expect_tree) = expect_tree else {
        return Ok(());
    };
    let expect_tree = expect_tree.trim();
    if expect_tree.is_empty() {
        bail!(
            "expect-tree gate: `--expect-tree` was passed but is empty/whitespace — a failed \
             command substitution would silently disable the repo-identity gate (#529). Pass \
             the intended tree OID explicitly, or omit the flag to opt out. Nothing was mutated."
        );
    }
    if expect_tree.eq_ignore_ascii_case(local_tree) {
        return Ok(());
    }
    bail!(
        "expect-tree gate refused the landing: the local branch `{local_branch}` resolves \
         to tree {local_tree} in the current directory, but the caller expects {expect_tree} \
         — the ambient cwd (or an inherited GIT_DIR) is redirecting `git` away from the \
         intended worktree; re-run from the intended worktree or pass --branch explicitly. \
         Nothing was mutated."
    );
}

/// Runs `git` with the given arguments and returns trimmed stdout; a non-zero
/// exit is a typed error carrying stderr (never credential material).
///
/// `dir` anchors the invocation at a repository directory (`git -C <dir>`);
/// `None` resolves the repository from the process cwd, which is correct for
/// the CLI. Tests pass a fixture directory so the real builder chain runs
/// WITHOUT chdiring the whole process (#529: a concurrent worktree's branch
/// would be colonized by the fixture's commits).
pub(crate) fn git(dir: Option<&std::path::Path>, args: &[&str]) -> Result<String> {
    git_output(|command| {
        if let Some(dir) = dir {
            command.arg("-C").arg(dir);
        }
        command.args(args)
    })
    .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Like [`git`], but returns raw bytes (for NUL-delimited output).
fn git_raw(dir: Option<&std::path::Path>, args: &[&str]) -> Result<Vec<u8>> {
    git_output(|command| {
        if let Some(dir) = dir {
            command.arg("-C").arg(dir);
        }
        command.args(args)
    })
    .map(|output| output.stdout)
}

/// Spawns a scrubbed `git` child assembled by `assemble` and validates its
/// exit status. Every production `git` invocation goes through here so an
/// inherited repository-location variable (`GIT_DIR`, `GIT_WORK_TREE`,
/// `GIT_INDEX_FILE`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES`,
/// `GIT_COMMON_DIR`, `GIT_NAMESPACE` — a hook or wrapper shell exports them)
/// can never redirect the child away from the intended repository or its
/// object store (#553/#529 corruption class).
fn git_output(assemble: impl FnOnce(&mut Command) -> &mut Command) -> Result<std::process::Output> {
    let mut command = Command::new("git");
    assemble(&mut command);
    for variable in AMBIENT_GIT_LOCATION_VARS {
        command.env_remove(variable);
    }
    let output = command
        .output()
        .map_err(|error| miette!("failed to run git: {error}"))?;
    if !output.status.success() {
        bail!(
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

/// Parses `git diff-tree -r -z --no-renames --name-status` output: NUL-
/// separated `STATUS\0path\0` records. git omits the final NUL after the
/// last path AND may emit a dangling final `STATUS` with no path when the
/// record count is odd, so both the `STATUS,path` and dangling-`STATUS`
/// shapes are handled. Statuses: A (added), M (modified), D (deleted).
fn parse_diff_tree_z(raw: &[u8]) -> Vec<(String, std::path::PathBuf)> {
    let mut changes = Vec::new();
    let mut fields = raw.split(|byte| *byte == 0_u8).peekable();
    while let Some(field) = fields.next() {
        if field.is_empty() {
            continue;
        }
        let status = String::from_utf8_lossy(field).to_string();
        match fields.next() {
            Some(path) if !path.is_empty() => {
                changes.push((
                    status,
                    std::path::PathBuf::from(String::from_utf8_lossy(path).as_ref()),
                ));
            }
            _ => break, // dangling status with no path: malformed tail, skip
        }
    }
    changes
}

/// One GraphQL session over the shared [`ApiTransport`]: installation-token
/// bearer, POST to the configured endpoint, every response validated. The
/// bearer is held for the session lifetime and never enters a diagnostic.
struct GraphqlSession<'a> {
    transport: &'a ApiTransport,
    bearer: &'a str,
    /// Target repository coordinates for post-move ref verification.
    owner: String,
    repo: String,
}

impl GraphqlSession<'_> {
    /// Executes one operation and returns the `data` object; errors are
    /// typed and carry only status or GraphQL message strings.
    fn execute(&self, operation: &str, query: &str, variables: &Value) -> Result<Value> {
        let response = self.transport.request_bearer(
            "POST",
            "graphql",
            self.bearer,
            Some(serde_json::json!({"query": query, "variables": variables}).to_string()),
        )?;
        let (status, body) = response;
        if !status.is_success() {
            bail!("github api request returned HTTP {status} ({operation})");
        }
        let envelope: Value = serde_json::from_str(&body)
            .map_err(|_| miette!("github api response is malformed: unexpected JSON shape"))?;
        let messages: Vec<String> = envelope
            .get("errors")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|error| error.get("message").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        if !messages.is_empty() {
            bail!(
                "github api request failed ({operation}): {}",
                messages.join("; ")
            );
        }
        envelope
            .get("data")
            .cloned()
            .ok_or_else(|| miette!("github api response is malformed: missing data object"))
    }

    /// Resolves the repository node id (createRef needs it for new branches).
    fn repository_id(&self, owner: &str, repo: &str) -> Result<String> {
        let data = self.execute(
            "repository query",
            "query($owner:String!,$repo:String!){repository(owner:$owner,name:$repo){id}}",
            &serde_json::json!({"owner": owner, "repo": repo}),
        )?;
        data.get("repository")
            .and_then(|repository| repository.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                miette!(
                    "response is missing repository.id — verify the owner/repo arguments \
                     ({owner}/{repo})"
                )
            })
    }

    /// Reads the remote branch ref; `Ok(None)` when the branch does not
    /// exist on the remote.
    fn branch_ref(&self, owner: &str, repo: &str, branch: &str) -> Result<Option<Value>> {
        let data = self.execute(
            "branch ref query",
            "query($owner:String!,$repo:String!,$qualified:String!){repository(owner:$owner,\
             name:$repo){ref(qualifiedName:$qualified){id target{... on Commit{oid}}}}}",
            &serde_json::json!({
                "owner": owner,
                "repo": repo,
                "qualified": format!("refs/heads/{branch}"),
            }),
        )?;
        Ok(data
            .pointer("/repository/ref")
            .filter(|ref_value| !ref_value.is_null())
            .cloned())
    }

    /// Appends one squashed commit to the branch with an expectedHeadOid
    /// compare-and-swap; returns the `commit` payload.
    #[allow(clippy::too_many_arguments)]
    fn create_commit_on_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        expected_head_oid: &str,
        headline: &str,
        body: &str,
        file_changes: &Value,
    ) -> Result<Value> {
        let data = self.execute(
            "createCommitOnBranch",
            "mutation($input:CreateCommitOnBranchInput!){createCommitOnBranch(input:$input)\
             {commit{oid tree{oid} signature{... on GpgSignature{isValid} \
             ... on SmimeSignature{isValid} ... on SshSignature{isValid}}}}}",
            &serde_json::json!({
                "input": {
                    "branch": {
                        "repositoryNameWithOwner": format!("{owner}/{repo}"),
                        "branchName": branch,
                    },
                    "expectedHeadOid": expected_head_oid,
                    "message": {"headline": headline, "body": body},
                    "fileChanges": file_changes,
                }
            }),
        )?;
        graphql_mutation_field(&data, "createCommitOnBranch", "commit").cloned()
    }

    /// Reads the repository's default branch name (`defaultBranchRef.name`).
    fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        let data = self.execute(
            "default branch query",
            "query($owner:String!,$repo:String!){repository(owner:$owner,name:$repo)\
             {defaultBranchRef{name}}}",
            &serde_json::json!({"owner": owner, "repo": repo}),
        )?;
        data.pointer("/repository/defaultBranchRef/name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                miette!(
                    "response is missing repository.defaultBranchRef.name — verify the \
                     owner/repo arguments ({owner}/{repo})"
                )
            })
    }

    /// Resolves a branch name to its head commit oid via the API (for a base
    /// ref that does not resolve locally after a fetch — fetched refs land
    /// in `FETCH_HEAD`, not under a local name). `Ok(None)` ONLY when the
    /// remote branch is absent (`repository.ref` is null); transport, HTTP,
    /// and GraphQL errors propagate so callers never mistake an API failure
    /// for "no such branch" and silently fall back to a stale local ref.
    fn default_branch_head(&self, owner: &str, repo: &str, branch: &str) -> Result<Option<String>> {
        let data = self.execute(
            "base head query",
            "query($owner:String!,$repo:String!,$qualified:String!){repository(owner:$owner,\
             name:$repo){ref(qualifiedName:$qualified){target{... on Commit{oid}}}}}",
            &serde_json::json!({
                "owner": owner,
                "repo": repo,
                "qualified": format!("refs/heads/{branch}"),
            }),
        )?;
        let absent = data.pointer("/repository/ref").is_some_and(Value::is_null);
        if absent {
            return Ok(None);
        }
        data.pointer("/repository/ref/target/oid")
            .and_then(Value::as_str)
            .map(|oid| Some(oid.to_string()))
            .ok_or_else(|| miette!("base ref query response is malformed: missing ref.target.oid"))
    }

    /// Reads one commit's verification signature state (`signature.isValid`
    /// across the Gpg/Smime/Ssh union). Used by `push-branch --re-land` to
    /// detect an already-verified remote head (re-land is a no-op there).
    fn head_commit(&self, owner: &str, repo: &str, oid: &str) -> Result<Value> {
        let data = self.execute(
            "head commit query",
            "query($owner:String!,$repo:String!,$oid:GitObjectID!){repository(owner:$owner,\
             name:$repo){object(oid:$oid){... on Commit{signature{... on GpgSignature{isValid} \
             ... on SmimeSignature{isValid} ... on SshSignature{isValid}}}}}}",
            &serde_json::json!({
                "owner": owner,
                "repo": repo,
                "oid": oid,
            }),
        )?;
        data.pointer("/repository/object")
            .cloned()
            .ok_or_else(|| miette!("head commit query response is malformed: missing object"))
    }

    /// Moves the real branch ref onto `oid` with an exact-head precondition:
    /// `updateRefs` with `RefUpdate.beforeOid` set to the OBSERVED remote
    /// head. A concurrent writer that advanced OR rewound the real branch in
    /// the window makes the precondition fail with a typed GraphQL error —
    /// neither direction can be overwritten. `force` stays `false`, so a
    /// non-fast-forward move is also rejected.
    fn move_ref_with_precondition(
        &self,
        owner: &str,
        repo: &str,
        repository_id: &str,
        remote_branch: &str,
        observed_head_oid: &str,
        new_oid: &str,
    ) -> Result<()> {
        let data = match self.execute(
            "updateRefs",
            "mutation($input:UpdateRefsInput!){updateRefs(input:$input){clientMutationId}}",
            &serde_json::json!({
                "input": {
                    "repositoryId": repository_id,
                    "refUpdates": [{
                        "name": format!("refs/heads/{remote_branch}"),
                        "afterOid": new_oid,
                        "beforeOid": observed_head_oid,
                        "force": false,
                    }],
                }
            }),
        ) {
            Ok(data) => data,
            // FALSE-NEGATIVE GUARD: a GraphQL error envelope here is NOT
            // proof the ref did not move — GitHub has answered updateRefs
            // with a spurious "malformed: missing updateRefs.clientMutationId"
            // AFTER applying the mutation, and `execute` surfaces such
            // envelopes as typed errors. The mutation actually moving the
            // ref must never be reported as a failure: re-read the branch
            // ref through the API; when it provably points at `new_oid`,
            // the landing succeeded and the error degrades to a warning.
            Err(error) => {
                // Best-effort probe: if the re-read itself fails, the
                // original mutation error stands (never masked).
                let moved = self
                    .branch_ref(owner, repo, remote_branch)
                    .ok()
                    .flatten()
                    .as_ref()
                    .and_then(|ref_payload| ref_payload.pointer("/target/oid"))
                    .and_then(Value::as_str)
                    .is_some_and(|oid| oid.eq_ignore_ascii_case(new_oid));
                if moved {
                    writeln!(
                        std::io::stderr(),
                        "WARNING: the updateRefs response errored ({error:#}) but the branch \
                         `{remote_branch}` provably points at {new_oid} — treating the landing \
                         as successful (the ref moved; the error response was spurious)"
                    )
                    .into_diagnostic()?;
                    return Ok(());
                }
                return Err(error);
            }
        };
        // UpdateRefsPayload carries ONLY the nullable clientMutationId (no
        // refs list exists on the payload type), so the response cannot
        // confirm the move by itself. Verify the move OUTCOME instead:
        // re-read the branch ref and confirm it now points at `new_oid` —
        // a null payload with an absent clientMutationId must not be
        // reported as success.
        let client_mutation_id = graphql_mutation_field(&data, "updateRefs", "clientMutationId");
        if client_mutation_id.is_err() {
            // Same guard on the null-field path: verify the ref position
            // before believing any malformed-payload claim.
            let moved = self
                .branch_ref(owner, repo, remote_branch)?
                .as_ref()
                .and_then(|ref_payload| ref_payload.pointer("/target/oid"))
                .and_then(Value::as_str)
                .is_some_and(|oid| oid.eq_ignore_ascii_case(new_oid));
            if moved {
                writeln!(
                    std::io::stderr(),
                    "WARNING: the updateRefs payload is missing clientMutationId but the branch \
                     `{remote_branch}` provably points at {new_oid} — treating the landing as \
                     successful (the ref moved)"
                )
                .into_diagnostic()?;
                return Ok(());
            }
            client_mutation_id?;
        }
        let moved_head = self.branch_ref(owner, repo, remote_branch)?;
        let moved_oid = moved_head
            .as_ref()
            .and_then(|ref_payload| ref_payload.pointer("/target/oid"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                miette!(
                    "post-move ref read failed: the branch disappeared — treating the landing \
                     as failed"
                )
            })?;
        if !moved_oid.eq_ignore_ascii_case(new_oid) {
            bail!(
                "post-move verification failed: the branch points at {moved_oid}, expected \
                 {new_oid} — treating the landing as failed"
            );
        }
        Ok(())
    }

    /// Deletes a ref by node id (deleteRef) — used for the temporary landing
    /// branch on every path (success, gate failure, mutation failure).
    fn delete_ref(&self, ref_id: &str) -> Result<()> {
        let data = self.execute(
            "deleteRef",
            "mutation($input:DeleteRefInput!){deleteRef(input:$input){clientMutationId}}",
            &serde_json::json!({"input": {"refId": ref_id}}),
        )?;
        graphql_mutation_field(&data, "deleteRef", "clientMutationId")
            .map(|_| ())
            .or(Ok(()))
    }

    /// Bootstraps a new branch ref at `oid` (createRef; the branch must NOT
    /// already exist — callers check first).
    fn create_ref(&self, repository_id: &str, name: &str, oid: &str) -> Result<Value> {
        let data = self.execute(
            "createRef",
            "mutation($input:CreateRefInput!){createRef(input:$input){ref{id target{... on \
             Commit{oid}}}}}",
            &serde_json::json!({
                "input": {"repositoryId": repository_id, "name": name, "oid": oid}
            }),
        )?;
        graphql_mutation_field(&data, "createRef", "ref").cloned()
    }
}

/// Extracts `<mutation>.<field>` from a GraphQL `data` object, failing typed
/// when the field is null or absent.
fn graphql_mutation_field<'a>(data: &'a Value, mutation: &str, field: &str) -> Result<&'a Value> {
    data.get(mutation)
        .and_then(|payload| payload.get(field))
        .filter(|value| !value.is_null())
        .ok_or_else(|| miette!("github api response is malformed: missing {mutation}.{field}"))
}

/// Base64 engine for `FileAddition.contents` (the GraphQL `Base64String`
/// scalar): RFC 4648 standard alphabet with padding.
const BASE64_ENGINE: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Minimal request transport for the `api` passthrough and `GET /app`.
///
/// Distinct from [`ReqwestInstallationTransport`]: that one exists to mint
/// tokens with App-JWT bearer auth; this one carries an explicit bearer
/// credential — the installation token for the `api` passthrough, a freshly
/// minted App JWT for `GET /app` — and returns raw response bodies. Error
/// paths carry only transport classification and status codes — never
/// headers or credential material.
pub(crate) struct ApiTransport {
    client: reqwest::blocking::Client,
    base_url: String,
}

impl ApiTransport {
    pub(crate) fn new(endpoint_override: Option<String>) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("orchestraitor-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| miette!("failed to build the github api http client"))?;
        let base_url = endpoint_override
            .unwrap_or_else(|| DEFAULT_GITHUB_API_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        Ok(Self { client, base_url })
    }

    /// Executes one request authenticated with an installation token. The
    /// token is injected only into the `Authorization` header; it is never
    /// written to the URL, the error paths, or the response body.
    fn request(
        &self,
        method: &str,
        path: &str,
        token: &InstallationToken,
        body: Option<String>,
    ) -> Result<(reqwest::StatusCode, String)> {
        self.request_bearer(method, path, token.token().expose_secret(), body)
    }

    /// Executes one request with an explicit bearer credential — an
    /// installation token or an App JWT (`GET /app`). The credential is
    /// injected only into the `Authorization` header; it is never written to
    /// the URL, the error paths, or the response body. An empty credential
    /// sends no `Authorization` header at all (unauthenticated request).
    pub(crate) fn request_bearer(
        &self,
        method: &str,
        path: &str,
        bearer: &str,
        body: Option<String>,
    ) -> Result<(reqwest::StatusCode, String)> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let request = self
            .client
            .request(
                reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|_| miette!("unsupported http method `{method}`"))?,
                &url,
            )
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        let request = if bearer.is_empty() {
            request
        } else {
            request.bearer_auth(bearer)
        };
        let request = match body {
            Some(body) => request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body),
            None => request,
        };
        let response = request.send().map_err(|error| {
            miette!(
                "github api request transport failed ({})",
                classify_transport(&error)
            )
        })?;
        let status = response.status();
        let response_body = response
            .text()
            .map_err(|_| miette!("github api response could not be decoded as utf-8 text"))?;
        Ok((status, response_body))
    }
}

/// Validates an HTTP method for `orc github api` (extracted for tests).
fn validate_api_method(method: &str) -> Result<()> {
    let upper = method.to_ascii_uppercase();
    if API_METHODS.contains(&upper.as_str()) {
        Ok(())
    } else {
        bail!(
            "unsupported method `{method}` (expected one of {})",
            API_METHODS.join(", ")
        );
    }
}

/// Validates an API path (extracted for tests).
fn validate_api_path(path: &str) -> Result<()> {
    if path.trim_start_matches('/').is_empty() {
        bail!("api path must not be empty");
    }
    Ok(())
}

/// Extracts the App slug from the `GET /app` payload.
fn app_slug(app: &Value) -> Result<&str> {
    app.get("slug")
        .and_then(Value::as_str)
        .filter(|slug| !slug.is_empty())
        .ok_or_else(|| miette!("github app response is malformed: missing `slug`"))
}

/// Derives the canonical commit identity from the App `slug` and the bot
/// user payload (the `GET /users/{slug}[bot]` response):
/// `slug[bot]` + `<bot-id>+<slug>[bot]@users.noreply.github.com`.
fn derive_commit_identity(slug: &str, bot_user: &Value) -> Result<(String, String)> {
    let bot_id = bot_user
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| miette!("github user response is malformed: missing `id`"))?;
    Ok((
        format!("{slug}[bot]"),
        format!("{bot_id}+{slug}[bot]@users.noreply.github.com"),
    ))
}

struct ReqwestInstallationTransport {
    client: reqwest::blocking::Client,
    base_url: String,
}

impl ReqwestInstallationTransport {
    fn new(endpoint_override: Option<String>) -> Result<Self, GitHubAppError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("orchestraitor-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| GitHubAppError::Transport {
                kind: "client-build",
            })?;
        let base_url = endpoint_override
            .unwrap_or_else(|| DEFAULT_GITHUB_API_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        Ok(Self { client, base_url })
    }
}

impl InstallationTokenTransport for ReqwestInstallationTransport {
    fn create_access_token(
        &self,
        installation_id: u64,
        app_jwt: &SecretString,
    ) -> Result<AccessTokenResponse, GitHubAppError> {
        // The only place the JWT leaves memory: injected into the Authorization
        // header of this single request (spec 40-arbitraitor-integration.md §9.23.4).
        let url = format!(
            "{}/app/installations/{installation_id}/access_tokens",
            self.base_url
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(app_jwt.expose_secret())
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&serde_json::json!({}))
            .send()
            .map_err(|error| GitHubAppError::Transport {
                kind: classify_transport(&error),
            })?;
        let status = response.status();
        if !status.is_success() {
            // Response bodies are dropped deliberately: they are never echoed
            // into errors or logs.
            return Err(GitHubAppError::MintRequest {
                status: status.as_u16(),
            });
        }
        response
            .json::<AccessTokenResponse>()
            .map_err(|_| GitHubAppError::Transport { kind: "decode" })
    }
}

fn classify_transport(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_request() || error.is_decode() {
        "request"
    } else {
        "send"
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use miette::IntoDiagnostic as _;
    use std::io::Read as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    /// Ignores a test server thread's `JoinError` (panic-in-server surfaces as
    /// a failed assertion inside the thread anyway).
    fn join_server(server: std::thread::JoinHandle<()>) {
        let _ = server.join();
    }

    /// A valid-shaped mint response; expiry must satisfy the 1h mint contract.
    fn fixture_token(marker: &str) -> InstallationToken {
        // 55-minute expiry: inside the 1h mint cap regardless of when the
        // test runs (same shape as the e2e mock in tests/cli.rs).
        let expires_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + 3_300;
        let expires_at =
            time::OffsetDateTime::from_unix_timestamp(i64::try_from(expires_epoch).unwrap_or(0))
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| "1970-01-02T00:00:00Z".to_string());
        AccessTokenResponse {
            token: format!("ghs_fixture{marker}"),
            expires_at,
        }
        .into_validated(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        )
        .into_diagnostic()
        .expect("fixture expiry is valid")
    }

    // --- build_request_body --------------------------------------------------

    #[test]
    fn field_values_are_always_strings_like_gh_raw_field() -> Result<()> {
        // `gh api -f` semantics: NO JSON parsing — `123` stays a string,
        // `null` stays a string, a JSON-looking comment body stays a string.
        let args = ApiArgs {
            method: "POST".to_string(),
            path: "/repos/o/r/pulls".to_string(),
            input: None,
            fields: vec![
                "title=Add service-identity writes".to_string(),
                "draft=true".to_string(),
                "body=123".to_string(),
                "name=null".to_string(),
                "payload={\"x\":1}".to_string(),
            ],
        };
        let body = build_request_body(&args)?.expect("fields produce a body");
        let parsed: Value = serde_json::from_str(&body).into_diagnostic()?;
        assert_eq!(
            parsed.get("title").and_then(Value::as_str),
            Some("Add service-identity writes")
        );
        for key in ["draft", "body", "name", "payload"] {
            assert!(
                parsed.get(key).is_some_and(Value::is_string),
                "-f must always produce a string value for {key}: {parsed}"
            );
        }
        assert_eq!(parsed.get("body").and_then(Value::as_str), Some("123"));
        assert_eq!(parsed.get("name").and_then(Value::as_str), Some("null"));
        assert_eq!(
            parsed.get("payload").and_then(Value::as_str),
            Some("{\"x\":1}")
        );
        Ok(())
    }

    #[test]
    fn field_without_equals_is_a_typed_error() {
        let args = ApiArgs {
            method: "POST".to_string(),
            path: "/repos/o/r/pulls".to_string(),
            input: None,
            fields: vec!["broken".to_string()],
        };
        let error = format!("{}", build_request_body(&args).unwrap_err());
        assert!(error.contains("`--field` must be `key=value`"), "{error}");
    }

    #[test]
    fn input_file_body_is_used_and_fields_merge_into_it() -> Result<()> {
        let temp = tempfile::tempdir().into_diagnostic()?;
        let file = temp.path().join("body.json");
        std::fs::write(&file, r#"{"base":"main"}"#).into_diagnostic()?;
        let args = ApiArgs {
            method: "POST".to_string(),
            path: "/repos/o/r/pulls".to_string(),
            input: Some(file.display().to_string()),
            fields: vec!["draft=false".to_string()],
        };
        let body = build_request_body(&args)?.expect("input produces a body");
        let parsed: Value = serde_json::from_str(&body).into_diagnostic()?;
        assert_eq!(parsed.get("base").and_then(Value::as_str), Some("main"));
        // `-f` values are always strings, even `false`.
        assert_eq!(parsed.get("draft").and_then(Value::as_str), Some("false"));
        Ok(())
    }

    #[test]
    fn field_on_non_object_input_body_is_a_typed_error() -> Result<()> {
        let temp = tempfile::tempdir().into_diagnostic()?;
        let file = temp.path().join("body.json");
        std::fs::write(&file, "[1,2,3]").into_diagnostic()?;
        let args = ApiArgs {
            method: "POST".to_string(),
            path: "/repos/o/r/pulls".to_string(),
            input: Some(file.display().to_string()),
            fields: vec!["draft=true".to_string()],
        };
        let error = format!("{}", build_request_body(&args).unwrap_err());
        assert!(error.contains("non-object body"), "{error}");
        Ok(())
    }

    #[test]
    fn no_input_and_no_fields_yield_no_body() -> Result<()> {
        let args = ApiArgs {
            method: "GET".to_string(),
            path: "/app".to_string(),
            input: None,
            fields: Vec::new(),
        };
        assert!(build_request_body(&args)?.is_none());
        Ok(())
    }

    #[test]
    fn get_fields_yield_no_body_and_ride_the_query_string() {
        let args = ApiArgs {
            method: "GET".to_string(),
            path: "/repos/o/r/issues".to_string(),
            input: None,
            fields: vec!["state=closed".to_string(), "per_page=100".to_string()],
        };
        // The fields never become a JSON body on GET — `build_request_query`
        // puts them on the URL instead (asserted end-to-end in the cli
        // e2e suite); the body stays empty.
        assert!(
            build_request_body(&args)
                .expect("GET fields must not error")
                .is_none()
        );
        let query = build_request_query(&args.fields).expect("well-formed fields build a query");
        assert_eq!(query, "state=closed&per_page=100");
    }

    #[test]
    fn delete_fields_yield_no_body() -> Result<()> {
        let args = ApiArgs {
            method: "DELETE".to_string(),
            path: "/repos/o/r/issues/1/labels/bug".to_string(),
            input: None,
            fields: vec!["foo=bar".to_string()],
        };
        assert!(build_request_body(&args)?.is_none());
        Ok(())
    }

    #[test]
    fn input_on_get_is_a_typed_error() {
        let args = ApiArgs {
            method: "GET".to_string(),
            path: "/repos/o/r/issues".to_string(),
            input: Some("-".to_string()),
            fields: Vec::new(),
        };
        let error = format!("{}", build_request_body(&args).unwrap_err());
        assert!(
            error.contains("`--input` cannot be combined with GET"),
            "{error}"
        );
    }

    #[test]
    fn query_builder_percent_encodes_keys_and_values() {
        let query = build_request_query(&[
            "labels=bug,help wanted".to_string(),
            "q=is:open in:title".to_string(),
        ])
        .expect("well-formed fields build a query");
        assert_eq!(query, "labels=bug%2Chelp+wanted&q=is%3Aopen+in%3Atitle");
    }

    #[test]
    fn query_builder_rejects_fields_without_equals() {
        let error = format!(
            "{}",
            build_request_query(&["broken".to_string()]).unwrap_err()
        );
        assert!(error.contains("`--field` must be `key=value`"), "{error}");
    }

    // --- method/path validation ---------------------------------------------

    #[test]
    fn method_validation_rejects_unknown_verbs() {
        for method in ["CONNECT", "TRACE", "options", ""] {
            let error = format!(
                "{}",
                validate_api_method(method).expect_err("unsupported verb")
            );
            assert!(error.contains("unsupported method"), "{error}");
        }
    }

    #[test]
    fn accepted_methods_cover_gh_api_minimum() {
        for method in API_METHODS {
            assert!(validate_api_method(method).is_ok(), "{method}");
        }
    }

    #[test]
    fn empty_path_is_a_typed_error() {
        let error = format!("{}", validate_api_path("").unwrap_err());
        assert!(error.contains("must not be empty"), "{error}");
    }

    // --- reject_duplicate_subjects (owner directive 2026-10-15) --------------

    // --- error paths never carry token material ------------------------------

    #[test]
    fn transport_error_diagnostic_never_contains_token_material() {
        let token = fixture_token("SECRETMARKER0001");
        // Nothing listens on port 1: the transport error must classify the
        // failure without echoing headers or the token.
        let transport = ApiTransport {
            client: reqwest::blocking::Client::new(),
            base_url: "http://127.0.0.1:1".to_string(),
        };
        let error = transport
            .request("GET", "app", &token, None)
            .expect_err("unreachable endpoint fails");
        let rendered = format!("{error:#}");
        assert!(rendered.contains("transport failed"), "{rendered}");
        assert!(!rendered.contains("SECRETMARKER0001"), "{rendered}");
        assert!(!rendered.contains("ghs_"), "{rendered}");
        assert!(
            !rendered.to_lowercase().contains("authorization"),
            "{rendered}"
        );
    }

    #[test]
    fn authorization_header_carries_token_and_metadata_stays_token_free() -> Result<()> {
        // Stub server echoes the Authorization header value back in the body.
        // This proves header shaping; the never-printed guarantee on orc's own
        // diagnostics is asserted separately.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").into_diagnostic()?;
        let addr = listener.local_addr().into_diagnostic()?;
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("one request arrives");
            let mut stream = stream;
            let mut buffer = [0u8; 4096];
            let read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let auth = request
                .lines()
                .find(|line| line.to_ascii_lowercase().starts_with("authorization:"))
                .and_then(|line| line.split_once(' '))
                .map(|(_, value)| value.trim().to_string())
                .unwrap_or_default();
            let body = format!(r#"{{"echoed":"{auth}"}}"#);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = std::io::Write::write_all(&mut stream, response.as_bytes());
        });
        let token = fixture_token("HEADERMARKER0003");
        let transport = ApiTransport {
            client: reqwest::blocking::Client::new(),
            base_url: format!("http://{addr}"),
        };
        let (status, body) = transport.request("GET", "app", &token, None)?;
        assert_eq!(status.as_u16(), 200);
        join_server(server);
        let echoed: Value = serde_json::from_str(&body).into_diagnostic()?;
        assert_eq!(
            echoed.get("echoed").and_then(Value::as_str),
            Some(format!("Bearer {}", token.token().expose_secret())).as_deref()
        );
        // orc's own metadata lines stay token-free.
        let metadata = format!(
            "expires_at = {} token_sha256_prefix = {}",
            token.expires_at_rfc3339(),
            token.fingerprint_prefix(12)
        );
        assert!(!metadata.contains("HEADERMARKER0003"));
        Ok(())
    }

    // --- commit-author derivation -------------------------------------------

    #[test]
    fn commit_author_is_derived_from_app_slug_and_bot_user_id_not_hardcoded() -> Result<()> {
        // A different slug/bot id than the real App (arbsec-agent/334074867)
        // proves the value comes from the /app + /users/{slug}[bot] payloads.
        let (name, email) = derive_commit_identity(
            "other-bot",
            &serde_json::json!({
                "id": 424_242,
                "login": "other-bot[bot]",
            }),
        )?;
        assert_eq!(name, "other-bot[bot]");
        assert_eq!(email, "424242+other-bot[bot]@users.noreply.github.com");
        Ok(())
    }

    #[test]
    fn commit_author_identity_is_two_single_lines() {
        let (name, email) = derive_commit_identity(
            "arbsec-agent",
            &serde_json::json!({ "id": 334_074_867, "login": "arbsec-agent[bot]" }),
        )
        .expect("well-formed payloads");
        assert_eq!(name, "arbsec-agent[bot]");
        assert_eq!(
            email,
            "334074867+arbsec-agent[bot]@users.noreply.github.com"
        );
        assert!(!name.contains('\n') && !email.contains('\n'));
    }

    #[test]
    fn malformed_app_payloads_are_typed_errors() {
        // Missing slug in the /app payload.
        let error = app_slug(&serde_json::json!({ "id": 1 })).unwrap_err();
        assert!(format!("{error}").contains("malformed"));
        let error = app_slug(&serde_json::json!({ "slug": "" })).unwrap_err();
        assert!(format!("{error}").contains("malformed"));
        // Missing id in the /users/{slug}[bot] payload.
        let error =
            derive_commit_identity("x", &serde_json::json!({ "login": "x[bot]" })).unwrap_err();
        assert!(format!("{error}").contains("malformed"));
    }

    // --- gh-env child spawn ---------------------------------------------------

    #[test]
    #[expect(clippy::too_many_lines, reason = "one assertion per argv shape")]
    fn child_command_is_pr_create_matches_shape_not_substrings() {
        let argv = |parts: &[&str]| -> Vec<String> {
            parts.iter().map(std::string::ToString::to_string).collect()
        };
        // The exact shape is CONFIRMED.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "pr", "create"])),
            PrCreateShape::Yes
        );
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "pr", "create", "--draft", "--title", "t",])),
            PrCreateShape::Yes
        );
        // Leading global flags precede the command token.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "--repo", "o/r", "pr", "create",])),
            PrCreateShape::Yes
        );
        // The command token matches by BASENAME: an absolute or relative
        // path to gh has the same shape.
        assert_eq!(
            child_command_is_pr_create(&argv(&["/usr/bin/gh", "pr", "create",])),
            PrCreateShape::Yes
        );
        assert_eq!(
            child_command_is_pr_create(&argv(&["./target/bin/gh", "pr", "create",])),
            PrCreateShape::Yes
        );
        // A path to a DIFFERENT binary is confirmed NOT pr-create.
        assert_eq!(
            child_command_is_pr_create(&argv(&["/usr/bin/hub", "pr", "create",])),
            PrCreateShape::No
        );
        // Other subcommands / single-token commands are confirmed No.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "pr", "view"])),
            PrCreateShape::No
        );
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "pr"])),
            PrCreateShape::Ambiguous
        );
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "issue", "create"])),
            PrCreateShape::No
        );
        // `gh pr new` is a documented built-in alias of `gh pr create`:
        // the alias fires the guard like the spelled-out form.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "pr", "new"])),
            PrCreateShape::Yes
        );
        // `pr create` inside an `api` PATH or a flag VALUE is confirmed No:
        // the shape walk stops at the first positional token.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "api", "repos/o/r/pr/create",])),
            PrCreateShape::No
        );
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "api", "-f", "title=pr create",])),
            PrCreateShape::No
        );
        assert_eq!(
            child_command_is_pr_create(&argv(&[
                "gh",
                "pr",
                "edit",
                "1",
                "--add-label",
                "pr create",
            ])),
            PrCreateShape::No
        );
        // An `=`-attached flag value is skipped, not read as a command token.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "--repo=o/r", "pr", "view",])),
            PrCreateShape::No
        );
        // An attached value does NOT consume the next token: the
        // `gh --repo=o/r pr create` form (accepted by gh) still fires.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "--repo=o/r", "pr", "create",])),
            PrCreateShape::Yes
        );
        // A value-taking global flag consumes its VALUE argument.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "--jq", ".number", "pr", "create",])),
            PrCreateShape::Yes
        );
        // Unknown leading flags before the command token are tolerated.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "-R", "o/r", "pr", "create"])),
            PrCreateShape::Yes
        );
        // Cobra accepts command-level persistent flags AFTER `pr` too:
        // `gh pr -R o/r create` still reaches `pr create` and must fire.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "pr", "-R", "o/r", "create"])),
            PrCreateShape::Yes
        );
        // AMBIGUOUS on unknown flags: the shape can neither be confirmed nor
        // excluded — gh-env refuses these in required mode (fail closed).
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "pr", "--color", "always", "create",])),
            PrCreateShape::Ambiguous
        );
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "-q", "pr", "create",])),
            PrCreateShape::Ambiguous
        );
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "--"])),
            PrCreateShape::Ambiguous
        );
        // Known boolean short flags stay tolerated (`-h`/`-v` are gh's
        // documented help/version flags — boolean, no value argument).
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "-h", "pr", "create"])),
            PrCreateShape::Yes
        );
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "-h", "-v", "pr", "create"])),
            PrCreateShape::Yes
        );
        // A bare `-` or `--` separator mid-shape is ambiguous.
        assert_eq!(
            child_command_is_pr_create(&argv(&["gh", "-", "pr", "create"])),
            PrCreateShape::Ambiguous
        );
    }

    #[test]
    fn gh_env_child_receives_gh_token_without_it_appearing_in_output() -> Result<()> {
        // Shell-script fixture child: writes only the LENGTH of GH_TOKEN to a
        // file — proving the env var reached the child without printing the
        // token anywhere (mirrors the exact env-injection gh_env performs).
        let temp = tempfile::tempdir().into_diagnostic()?;
        let script_path = temp.path().join("child.sh");
        std::fs::write(
            &script_path,
            "#!/bin/sh\nprintf '%s' \"$GH_TOKEN\" | wc -c | tr -d ' \"' > \"$OUT_FILE\"\n",
        )
        .into_diagnostic()?;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .into_diagnostic()?;
        let out_file = temp.path().join("child.out");
        let fake_token = "ghs_CHILDENVmarker0000444";
        let output = Command::new(&script_path)
            .env("GH_TOKEN", fake_token)
            .env("OUT_FILE", &out_file)
            .output()
            .into_diagnostic()?;
        assert!(output.status.success());
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
        let observed = std::fs::read_to_string(&out_file).into_diagnostic()?;
        assert_eq!(observed.trim(), fake_token.len().to_string());
        Ok(())
    }

    #[test]
    fn which_executable_rejects_non_executable_files() -> Result<()> {
        // A regular file without the executable bit must NOT resolve —
        // pre-mint validation would otherwise pass and mint a token for a
        // child that is guaranteed to fail at spawn time.
        let temp = tempfile::tempdir().into_diagnostic()?;
        let plain = temp.path().join("not-executable-orchestraitor-fixture");
        std::fs::write(&plain, "#!/bin/sh\nexit 0\n").into_diagnostic()?;
        let direct = which_executable(&plain.display().to_string());
        assert!(
            direct.is_none(),
            "non-executable direct path must not resolve"
        );

        // The by-name PATH lookup also rejects it (the uniquely-named fixture
        // cannot be shadowed by a same-named executable elsewhere on PATH).
        let on_path = which_executable("not-executable-orchestraitor-fixture");
        assert!(
            on_path.is_none(),
            "non-executable file on PATH must not resolve"
        );
        Ok(())
    }

    #[test]
    fn which_executable_rejects_directories_even_with_execute_bit() -> Result<()> {
        // A directory with the execute bit set would pass a bare mode check;
        // the pre-mint validation must reject it so no token is minted for a
        // child that can never spawn.
        let temp = tempfile::tempdir().into_diagnostic()?;
        let dir = temp.path().join("executable-directory-fixture");
        std::fs::create_dir(&dir).into_diagnostic()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))
                .into_diagnostic()?;
        }
        let direct = which_executable(&dir.display().to_string());
        assert!(direct.is_none(), "executable directory must not resolve");

        let on_path = which_executable("executable-directory-fixture");
        assert!(
            on_path.is_none(),
            "executable directory on PATH must not resolve"
        );
        Ok(())
    }

    #[test]
    fn which_executable_resolves_system_tools_and_rejects_unknown() {
        assert!(which_executable("sh").is_some());
        assert!(which_executable("definitely-not-a-real-binary-orchestraitor").is_none());
    }

    // --- push-branch: diff-tree -z parsing ------------------------------------

    #[test]
    fn parse_diff_tree_z_handles_added_modified_deleted() {
        let raw = b"M\0base.txt\0D\0old.txt\0A\0new.txt\0";
        let changes = parse_diff_tree_z(raw);
        assert_eq!(
            changes,
            vec![
                ("M".to_string(), std::path::PathBuf::from("base.txt")),
                ("D".to_string(), std::path::PathBuf::from("old.txt")),
                ("A".to_string(), std::path::PathBuf::from("new.txt")),
            ]
        );
    }

    #[test]
    fn parse_diff_tree_z_survives_dangling_status_without_path() {
        // git can emit a trailing status field with no path when the record
        // count is odd; it must be skipped, not paired with garbage.
        let raw = b"A\0kept.txt\0M\0";
        let changes = parse_diff_tree_z(raw);
        assert_eq!(
            changes,
            vec![("A".to_string(), std::path::PathBuf::from("kept.txt"))]
        );
    }

    #[test]
    fn parse_diff_tree_z_preserves_spaces_and_utf8_paths_unquoted() {
        // The -z format never quotes; the parser must keep the raw bytes.
        let raw = "A\0dir with space/ünïcode.txt\0".as_bytes();
        let changes = parse_diff_tree_z(raw);
        assert_eq!(
            changes,
            vec![(
                "A".to_string(),
                std::path::PathBuf::from("dir with space/ünïcode.txt")
            )]
        );
    }

    #[test]
    fn parse_diff_tree_z_empty_output_yields_no_changes() {
        assert_eq!(
            parse_diff_tree_z(b""),
            [] as [(String, std::path::PathBuf); 0]
        );
        assert_eq!(
            parse_diff_tree_z(b"\0"),
            [] as [(String, std::path::PathBuf); 0]
        );
    }

    #[test]
    fn base64_engine_encodes_standard_alphabet_with_padding() {
        assert_eq!(BASE64_ENGINE.encode("hello"), "aGVsbG8=");
    }

    #[test]
    fn build_file_changes_shapes_deletions_as_path_objects() {
        // This test covers only the D branch of the builder plus the payload
        // shape; an A record would need a real blob, so only D is exercised
        // here.
        let result = build_file_changes(
            None,
            &[("D".to_string(), std::path::PathBuf::from("base.txt"))],
            "any-branch",
            "",
        );
        let payload = result.expect("deletion-only payload must build");
        assert_eq!(
            payload,
            serde_json::json!({"additions": [], "deletions": [{"path": "base.txt"}]})
        );
    }

    // --- push-branch: FileChanges builder (corruption regression) -----------

    /// Builds a fixture repo whose base and branch trees differ by an added,
    /// a modified, and a deleted file, so the REAL builder runs against REAL
    /// git objects (the corruption bug was blob OIDs pasted as contents).
    /// Every git child is anchored at the fixture repo and scrubbed of
    /// inherited repository-location variables (#553): an ambient `GIT_DIR`
    /// from a hook environment would otherwise redirect the fixture's
    /// commits into the caller's real worktree.
    fn fixture_repo_with_changes() -> miette::Result<(tempfile::TempDir, String, String)> {
        let temp = tempfile::tempdir().into_diagnostic()?;
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(&repo).into_diagnostic()?;
        let run = |args: &[&str]| -> miette::Result<()> {
            let mut command = Command::new("git");
            command.arg("-C").arg(&repo).args(args);
            for variable in AMBIENT_GIT_LOCATION_VARS {
                command.env_remove(variable);
            }
            let status = command
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .status()
                .into_diagnostic()?;
            assert!(status.success(), "git {args:?} failed");
            Ok(())
        };
        run(&["init", "-q", "-b", "main"])?;
        std::fs::write(repo.join("mod.txt"), "old\n").into_diagnostic()?;
        std::fs::write(repo.join("del.txt"), "delete me\n").into_diagnostic()?;
        run(&["add", "."])?;
        run(&["commit", "-qm", "base"])?;
        run(&["checkout", "-qb", "feature"])?;
        std::fs::write(repo.join("mod.txt"), "new\n").into_diagnostic()?;
        std::fs::write(repo.join("add.txt"), "added\n").into_diagnostic()?;
        std::fs::remove_file(repo.join("del.txt")).into_diagnostic()?;
        run(&["add", "-A"])?;
        run(&["commit", "-qm", "feature"])?;
        let tree = |spec: &str| -> miette::Result<String> {
            let mut command = Command::new("git");
            command
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", &format!("{spec}^{{tree}}")]);
            for variable in AMBIENT_GIT_LOCATION_VARS {
                command.env_remove(variable);
            }
            let output = command.output().into_diagnostic()?;
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        };
        Ok((temp, tree("main")?, tree("feature")?))
    }

    // --- push-branch: expect-tree repo-identity gate (#529) -----------------

    #[test]
    fn expect_tree_gate_refuses_a_mismatched_tree_before_any_mutation() {
        // The #529 corruption shape: the caller pinned the tree it intends
        // to land, but the ambient cwd resolved the branch to a DIFFERENT
        // (colonized) tree. The gate must refuse — fail closed.
        let local_tree = "0123456789abcdef0123456789abcdef01234567";
        let expect_tree = "fedcba9876543210fedcba9876543210fedcba98";
        let error = format!(
            "{}",
            verify_expect_tree(local_tree, Some(expect_tree), "feature")
                .expect_err("mismatched tree must refuse the landing")
        );
        assert!(error.contains("expect-tree gate refused"), "{error}");
        assert!(error.contains(local_tree), "{error}");
        assert!(error.contains(expect_tree), "{error}");
        assert!(error.contains("Nothing was mutated"), "{error}");
    }

    #[test]
    fn expect_tree_gate_accepts_exact_and_case_insensitive_matches() {
        let local_tree = "0123456789abcdef0123456789abcdef01234567";
        verify_expect_tree(local_tree, Some(local_tree), "feature")
            .expect("identical tree must pass the gate");
        verify_expect_tree(local_tree, Some(&local_tree.to_uppercase()), "feature")
            .expect("the gate compares OIDs case-insensitively (git OIDs are hex)");
    }

    #[test]
    fn expect_tree_gate_treats_absent_pin_as_unchecked_but_blank_pin_as_error() {
        verify_expect_tree("0123456789abcdef0123456789abcdef01234567", None, "feature")
            .expect("no pin must pass the gate (back-compat)");
        // A PRESENT-but-blank pin is a misuse (e.g. a failed command
        // substitution): it must be a typed error, never a silent
        // opt-out — that would disable the fail-closed gate (#529 intent;
        // local review, head 4b26b0e).
        let error = format!(
            "{}",
            verify_expect_tree(
                "0123456789abcdef0123456789abcdef01234567",
                Some("   "),
                "feature"
            )
            .expect_err("a blank pin must be a typed error, not a silent opt-out")
        );
        assert!(
            error.contains("empty/whitespace"),
            "blank-pin error must name the misuse: {error}"
        );
        assert!(error.contains("Nothing was mutated"), "{error}");
    }

    #[test]
    fn expect_tree_gate_error_names_the_redirect_cause() {
        let error = format!(
            "{}",
            verify_expect_tree(
                "0123456789abcdef0123456789abcdef01234567",
                Some("fedcba9876543210fedcba9876543210fedcba98"),
                "feature"
            )
            .expect_err("mismatch must error")
        );
        // The remediation hint must point at the ambient-cwd/GIT_DIR cause.
        assert!(error.contains("GIT_DIR"), "{error}");
        assert!(error.contains("intended worktree"), "{error}");
    }

    #[test]
    fn build_file_changes_carries_base64_blob_contents_not_blob_oids() -> miette::Result<()> {
        // THE corruption regression: FileAddition.contents must be the
        // base64 of the blob BYTES (`git cat-file blob`), never the blob
        // OID string pasted as contents. The fixture is cwd-independent
        // (`git`/`git_raw` operate on the caller's process cwd, so the
        // gitdir is anchored with `git -C <repo>` — a process-wide chdir
        // would colonize concurrent worktrees, the #529 incident).
        let (temp, base_tree, feature_tree) = fixture_repo_with_changes()?;
        let repo = temp.path().join("repo");
        let changes = diff_tree_changes(Some(&repo), &base_tree, &feature_tree)?;
        let payload = build_file_changes(Some(&repo), &changes, "feature", &base_tree)?;
        let additions = payload
            .get("additions")
            .and_then(Value::as_array)
            .ok_or_else(|| miette!("additions must be an array: {payload}"))?;
        let added = additions
            .iter()
            .find(|entry| entry.get("path").and_then(Value::as_str) == Some("add.txt"))
            .ok_or_else(|| miette!("add.txt must be in the payload: {payload}"))?;
        let contents = added
            .get("contents")
            .and_then(Value::as_str)
            .ok_or_else(|| miette!("addition is missing contents"))?;
        assert_eq!(
            BASE64_ENGINE.decode(contents).into_diagnostic()?,
            b"added\n",
            "contents must decode to the blob bytes, not contain an OID"
        );
        // A 40-hex blob OID pasted as contents would decode to 40 bytes
        // of hex; the decoded payload must be the actual content.
        assert!(!contents.chars().all(|c| c.is_ascii_hexdigit()));
        // Modified and deleted records keep their shapes.
        let modified = additions
            .iter()
            .find(|entry| entry.get("path").and_then(Value::as_str) == Some("mod.txt"))
            .ok_or_else(|| miette!("mod.txt must be in the payload: {payload}"))?;
        assert_eq!(
            BASE64_ENGINE
                .decode(
                    modified
                        .get("contents")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                )
                .into_diagnostic()?,
            b"new\n"
        );
        let deletions = payload
            .get("deletions")
            .and_then(Value::as_array)
            .ok_or_else(|| miette!("deletions must be an array: {payload}"))?;
        assert_eq!(deletions.len(), 1, "one deletion: {payload}");
        assert_eq!(
            deletions[0].get("path").and_then(Value::as_str),
            Some("del.txt")
        );
        Ok(())
    }

    #[test]
    fn build_file_changes_deletion_only_never_touches_git() -> Result<()> {
        // Deletions carry only the path — no blob read, no mode read.
        let payload = build_file_changes(
            None,
            &[("D".to_string(), std::path::PathBuf::from("gone.txt"))],
            "any-branch",
            "",
        )?;
        assert_eq!(
            payload,
            serde_json::json!({"additions": [], "deletions": [{"path": "gone.txt"}]})
        );
        Ok(())
    }

    #[test]
    fn parse_ls_tree_z_extracts_modes_for_nested_paths() {
        // `MODE TYPE OID\tpath\0` records; -z keeps spaces and utf-8 raw.
        let mut raw: Vec<u8> = b"100755 blob abc\tdir with space/run.sh".to_vec();
        raw.push(0);
        raw.extend_from_slice(b"100644 blob def\tplain.txt");
        raw.push(0);
        let modes = parse_ls_tree_z(&raw);
        assert_eq!(
            modes.get("dir with space/run.sh"),
            Some(&"100755".to_string())
        );
        assert_eq!(modes.get("plain.txt"), Some(&"100644".to_string()));
        assert!(parse_ls_tree_z(b"").is_empty());
    }

    #[test]
    fn parse_github_owner_repo_handles_ssh_and_https_and_rejects_other_shapes() {
        assert_eq!(
            parse_github_owner_repo("git@github.com:arbsec/orchestraitor.git"),
            Some(("arbsec".to_string(), "orchestraitor".to_string()))
        );
        assert_eq!(
            parse_github_owner_repo("https://github.com/arbsec/orchestraitor.git"),
            Some(("arbsec".to_string(), "orchestraitor".to_string()))
        );
        assert_eq!(
            parse_github_owner_repo("https://github.com/arbsec/orchestraitor"),
            Some(("arbsec".to_string(), "orchestraitor".to_string()))
        );
        // The explicit ssh:// scheme with a user in the authority.
        assert_eq!(
            parse_github_owner_repo("ssh://git@github.com/arbsec/orchestraitor.git"),
            Some(("arbsec".to_string(), "orchestraitor".to_string()))
        );
        assert_eq!(
            parse_github_owner_repo("ssh://github.com/arbsec/orchestraitor"),
            Some(("arbsec".to_string(), "orchestraitor".to_string()))
        );
        // A non-GitHub host stays rejected in every scheme shape.
        assert_eq!(parse_github_owner_repo("ssh://git@gitlab.com/o/r"), None);
        // A local filesystem path or an absolute scp-ish path is NOT a repo.
        assert_eq!(parse_github_owner_repo("/tmp/.tmpabc/repo"), None);
        assert_eq!(parse_github_owner_repo("https://gitlab.com/o/r"), None);
        assert_eq!(parse_github_owner_repo(""), None);
    }

    #[test]
    fn resolve_repo_slug_accepts_owner_slash_name_verbatim_and_bare_name_with_origin_owner() {
        // Regression: an explicit `--repo owner/name` must be used as-is —
        // the origin-derived owner is never re-prepended (the
        // `arbsec/arbsec/orchestraitor` double-owner bug).
        let slug = |owner: Option<&str>, repo: Option<&str>, oo: Option<&str>, or: Option<&str>| {
            let (owner, repo) = resolve_repo_slug(owner, repo, oo, or).unwrap();
            (owner, repo)
        };
        assert_eq!(
            slug(
                None,
                Some("arbsec/orchestraitor"),
                Some("arbsec"),
                Some("orchestraitor")
            ),
            ("arbsec".to_string(), "orchestraitor".to_string())
        );
        // Explicit `--owner` + explicit `--repo owner/name`: the slug wins.
        assert_eq!(
            slug(
                Some("other"),
                Some("arbsec/orchestraitor"),
                Some("arbsec"),
                Some("orchestraitor")
            ),
            ("arbsec".to_string(), "orchestraitor".to_string())
        );
        // Bare `--repo name` combines with the origin-derived owner.
        assert_eq!(
            slug(None, Some("orchestraitor"), Some("arbsec"), None),
            ("arbsec".to_string(), "orchestraitor".to_string())
        );
        // Bare `--repo name` with an explicit `--owner`.
        assert_eq!(
            slug(Some("other"), Some("orchestraitor"), None, None),
            ("other".to_string(), "orchestraitor".to_string())
        );
        // No flags: both from origin.
        assert_eq!(
            slug(None, None, Some("arbsec"), Some("orchestraitor")),
            ("arbsec".to_string(), "orchestraitor".to_string())
        );
        // Bare repo flag with no owner anywhere is a typed error.
        assert!(resolve_repo_slug(None, Some("orchestraitor"), None, None).is_err());
        // No flags and no origin is a typed error.
        assert!(resolve_repo_slug(None, None, None, None).is_err());
    }
}

#[cfg(test)]
mod duplicate_subject_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    // The guard logic (reject_duplicate_subjects) is a pure comparison
    // over (remote_subject, landing_headline) — exercised directly.
    #[test]
    fn duplicate_landing_headline_is_refused() {
        let error = reject_duplicate_subjects(
            Some("feat(orchestration): reference expansion — review fixes"),
            "feat(orchestration): reference expansion — review fixes",
            "feat/614-ref-expansion",
        )
        .expect_err("duplicate landing headline is refused");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("identical to the existing remote head's subject"),
            "{rendered}"
        );
    }

    #[test]
    fn distinct_landing_headline_passes() {
        reject_duplicate_subjects(
            Some("feat(orchestration): reference expansion — round 1"),
            "feat(orchestration): reference expansion — review fixes round 2",
            "feat/614-ref-expansion",
        )
        .expect("distinct headlines pass");
    }

    #[test]
    fn no_remote_head_subject_passes() {
        reject_duplicate_subjects(None, "feat: anything", "feat/614-ref-expansion")
            .expect("no remote head subject (new branch) passes");
    }
}
