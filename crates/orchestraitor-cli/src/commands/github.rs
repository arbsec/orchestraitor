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

use crate::cli::{ApiArgs, ConfigPaths, GitHubCommand, PushBranchArgs};
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
        GitHubCommand::CommitAuthor => commit_author(paths, writer),
        GitHubCommand::PushBranch(args) => push_branch(paths, &args),
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
fn resolved_config(paths: &ConfigPaths) -> Result<OrchestraitorConfig> {
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
fn mint_installation_token(paths: &ConfigPaths) -> Result<(u64, InstallationToken)> {
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
    // weaker layered value. Invalid env values fail closed.
    let config = resolved_config(paths)?;
    let mut required = enforcement_mode(&config) == ServiceIdentityEnforcement::Required;
    if let Ok(pinned) = std::env::var("ORC_GITHUB_APP_ENFORCEMENT") {
        match pinned.as_str() {
            "required" => required = true,
            "recommended" => {}
            other => bail!(
                "invalid ORC_GITHUB_APP_ENFORCEMENT value `{other}` (expected `recommended` \
                 or `required`); refusing to delegate under an ambiguous enforcement \
                 declaration"
            ),
        }
    }
    let mut bot_identity: Option<(String, String)> = None;
    if required {
        require_service_identity(&config, "delegate a gh-env child command")?;
        // Exact-identity check before delegating: the repo git identity must
        // equal the service-identity bot's canonical noreply email (resolved
        // from the live App) so any agent-driven `git commit` path cannot
        // stamp personal or look-alike attribution onto commits.
        bot_identity = Some(require_bot_git_identity(paths, program)?);
    }
    let (_, token) = mint_installation_token(paths)?;
    let Some(executable) = which else {
        bail!("child command `{program}` not found on PATH");
    };
    let mut child = Command::new(executable);
    child
        .args(&args.command[1..])
        .env("GH_TOKEN", token.token().expose_secret());
    // Pin the git identity for the child: `git -c user.email=… commit` beats
    // repo config but NOT the GIT_AUTHOR_*/GIT_COMMITTER_* environment
    // variables, so env-pinning guarantees bot attribution even for a child
    // that rewrites its identity per invocation. Identity values are
    // non-secret (public bot login + noreply email).
    if let Some((name, email)) = bot_identity {
        child
            .env("GIT_AUTHOR_NAME", &name)
            .env("GIT_AUTHOR_EMAIL", &email)
            .env("GIT_COMMITTER_NAME", &name)
            .env("GIT_COMMITTER_EMAIL", &email);
    }
    let status = child
        .status()
        .map_err(|error| miette!("failed to spawn child command `{program}`: {error}"))?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
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
fn push_branch(paths: &ConfigPaths, args: &PushBranchArgs) -> Result<()> {
    let remote_branch = args.remote_branch.as_ref().unwrap_or(&args.branch);

    // Fail-closed gate: identical to the other mutating subcommands — the
    // landing path exists for the App service identity, so `required`
    // enforcement refuses to run it without a complete `github_app` config.
    let config = resolved_config(paths)?;
    if enforcement_mode(&config) == ServiceIdentityEnforcement::Required {
        require_service_identity(&config, "land an App-signed branch commit")?;
    }

    // 1. Compute the intended tree from the local branch. `rev-parse
    //    <branch>^{tree}` reads the committed tree object — no index state,
    //    no textconv filters, exactly the object identity the remote must
    //    match (the same check `pr-convergence.md` prescribes).
    let local_tree = git(&["rev-parse", &format!("{}^{{tree}}", args.branch)])?;
    let base = resolve_base(args.base.as_deref())?;
    // `git diff-tree -r -z --no-renames --name-status <base-tree> <tree>`
    // emits NUL-separated `STATUS\0path\0` records (no quoting, no renames);
    // with `--no-commit-id` against two trees there is no leading commit oid.
    let base_tree = git(&["rev-parse", &format!("{base}^{{tree}}")])?;
    let raw = git_raw(&[
        "diff-tree",
        "-r",
        "-z",
        "--no-renames",
        "--no-commit-id",
        "--name-status",
        &base_tree,
        &local_tree,
    ])?;
    let changes = parse_diff_tree_z(&raw);
    if changes.is_empty() {
        writeln!(
            std::io::stderr(),
            "branch `{}` tree {local_tree} is identical to base `{base}` — nothing to land",
            args.branch
        )
        .into_diagnostic()?;
        return Ok(());
    }

    // 2. Build the fileChanges payload from the worktree files. Additions
    //    carry base64 contents read from disk; deletions carry the path only.
    let file_changes = build_file_changes(&changes)?;

    // 3. Mint and drive the remote: one GraphQL endpoint, installation-token
    //    bearer, every mutation's result validated. The token rides only the
    //    Authorization header and never enters a diagnostic.
    let (_, token) = mint_installation_token(paths)?;
    let transport = ApiTransport::new(paths.github_graphql_endpoint.clone())?;
    let graphql = GraphqlSession {
        transport: &transport,
        bearer: token.token().expose_secret(),
    };

    // Read the remote branch's current head (empty for a new branch).
    let existing = graphql.branch_ref(&args.owner, &args.repo, remote_branch)?;
    let new_head = if let Some(ref_payload) = existing {
        let head = ref_payload
            .pointer("/target/oid")
            .and_then(Value::as_str)
            .ok_or_else(|| miette!("branch ref response is malformed: missing target.oid"))?
            .to_string();
        let ref_id = ref_payload
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| miette!("branch ref response is malformed: missing ref.id"))?
            .to_string();
        // Append the squashed commit ON TOP of the current remote head
        // with an expectedHeadOid compare-and-swap, then force-move the
        // ref onto the commit whose tree is the local tree. Landing on
        // top (instead of force-moving first) keeps the PR head from
        // ever regressing to an ancestor of main — the auto-close
        // pitfall in verified-commit-path.md — and the expectedHeadOid
        // gate refuses a concurrent move in the window.
        let commit = graphql.create_commit_on_branch(
            &args.owner,
            &args.repo,
            remote_branch,
            &head,
            &args.message,
            args.body.as_deref().unwrap_or_default(),
            &file_changes,
        )?;
        verify_landed_commit(&commit, &local_tree, &args.branch)?;
        let oid = commit_oid(&commit)?.to_string();
        // Force-move: updateRef with force=true swings the branch onto
        // the verified commit. The earlier expectedHeadOid CAS plus the
        // immediate window make a concurrent move extremely unlikely;
        // the tree and verification gates ran against the landed commit
        // itself.
        graphql.update_ref(&ref_id, &oid)?;
        oid
    } else {
        // New branch: bootstrap the ref at the base commit, then append
        // the commit (createCommitOnBranch does NOT auto-create branches
        // — verified empirically, gh-aw#22564).
        let repository_id = graphql.repository_id(&args.owner, &args.repo)?;
        let created = graphql.create_ref(
            &repository_id,
            &format!("refs/heads/{remote_branch}"),
            &base,
        )?;
        let created_head = created
            .pointer("/target/oid")
            .and_then(Value::as_str)
            .ok_or_else(|| miette!("createRef response is malformed: missing target.oid"))?
            .to_string();
        let commit = graphql.create_commit_on_branch(
            &args.owner,
            &args.repo,
            remote_branch,
            &created_head,
            &args.message,
            args.body.as_deref().unwrap_or_default(),
            &file_changes,
        )?;
        verify_landed_commit(&commit, &local_tree, &args.branch)?;
        commit_oid(&commit)?.to_string()
    };

    writeln!(
        std::io::stderr(),
        "landed {remote_branch} at {new_head} (App-signed, tree {local_tree})"
    )
    .into_diagnostic()?;
    Ok(())
}

/// Resolves the diff base commit: an explicit `--base` ref, else
/// `origin/main`, else `main`. Typed error when nothing resolves.
fn resolve_base(base: Option<&str>) -> Result<String> {
    if let Some(explicit) = base {
        return git(&["rev-parse", "--verify", explicit]);
    }
    let from_origin = git(&["rev-parse", "--verify", "--quiet", "origin/main"]).ok();
    from_origin
        .or_else(|| git(&["rev-parse", "--verify", "--quiet", "main"]).ok())
        .ok_or_else(|| {
            miette!(
                "cannot resolve the base branch: no --base given and neither origin/main nor \
                 main exists — pass --base <ref>"
            )
        })
}

/// Builds the GraphQL `FileChanges` input from the parsed diff records:
/// additions/modifications carry base64 file contents read from the worktree,
/// deletions carry the path. An unreadable file or an unknown diff status is
/// a typed error — nothing is landed on a partial change set.
fn build_file_changes(changes: &[(String, std::path::PathBuf)]) -> Result<Value> {
    let mut additions = Vec::new();
    let mut deletions = Vec::new();
    for (status, path) in changes {
        match status.as_str() {
            "A" | "M" => {
                let bytes = std::fs::read(path).map_err(|error| {
                    miette!(
                        "failed to read `{}` for the landing commit: {error}",
                        path.display()
                    )
                })?;
                additions.push(serde_json::json!({
                    "path": path.display().to_string(),
                    "contents": BASE64_ENGINE.encode(&bytes),
                }));
            }
            "D" => {
                deletions.push(serde_json::json!(path.display().to_string()));
            }
            other => bail!(
                "unexpected diff-tree status `{other}` for `{}` — refusing to land an ambiguous \
                 change set",
                path.display()
            ),
        }
    }
    Ok(serde_json::json!({"additions": additions, "deletions": deletions}))
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

/// Runs `git` with the given arguments and returns trimmed stdout; a non-zero
/// exit is a typed error carrying stderr (never credential material).
fn git(args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| miette!("failed to run `git {}`: {error}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Like [`git`], but returns raw bytes (for NUL-delimited output).
fn git_raw(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| miette!("failed to run `git {}`: {error}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
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

    /// Force-moves the branch ref onto `oid` (updateRef with force=true).
    fn update_ref(&self, ref_id: &str, oid: &str) -> Result<()> {
        let data = self.execute(
            "updateRef",
            "mutation($input:UpdateRefInput!){updateRef(input:$input){ref{id}}}",
            &serde_json::json!({"input": {"refId": ref_id, "oid": oid, "force": true}}),
        )?;
        graphql_mutation_field(&data, "updateRef", "ref").map(|_| ())
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
struct ApiTransport {
    client: reqwest::blocking::Client,
    base_url: String,
}

impl ApiTransport {
    fn new(endpoint_override: Option<String>) -> Result<Self> {
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
    fn request_bearer(
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
        )
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
        assert!(parse_diff_tree_z(b"").is_empty());
        assert!(parse_diff_tree_z(b"\0").is_empty());
    }

    #[test]
    fn base64_engine_encodes_standard_alphabet_with_padding() {
        assert_eq!(BASE64_ENGINE.encode("hello"), "aGVsbG8=");
    }
}
