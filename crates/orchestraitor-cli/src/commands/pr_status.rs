//! `orc pr status` — the CLI surface of the shared §9.48 PR-status
//! aggregation engine (spec `10-orchestrator.md` §9.48, issue #614 slices
//! 1–2). The typed verdict, the pure classification semantics, and the
//! engine live in [`orchestraitor_mcp::pr_status`] ("one engine, three
//! surfaces"); this module owns the CLI-specific shell: repository
//! resolution, credential minting, the blocking GitHub transport, the human
//! one-pager, and the diagnostic printing for the exit-code contract.
//!
//! Read-only: the tool never labels, comments, reviews, or merges.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use miette::{IntoDiagnostic, Result, miette};
use secrecy::{ExposeSecret as _, SecretString};
use serde_json::Value;

use crate::cli::{ConfigPaths, PrCommand, PrStatusArgs};
use orchestraitor_mcp::pr_status::{Engine, PrStatus, PrStatusError, PrStatusTransport};

/// A miette `Diagnostic` wrapper around the shared [`PrStatusError`]. The
/// mcp crate has no miette dependency (the shared engine is transport- and
/// diagnostics-agnostic), and miette's `Report::from_err` boxes std errors
/// into a wrapper whose downcast CANNOT reach the inner type — so the CLI
/// wraps the typed error itself: `Report::new(PrStatusDiagnostic(error))`
/// keeps `downcast_ref::<PrStatusDiagnostic>()` working for the
/// exit-code mapping in `main` and the diagnostic printing here.
///
/// The `Diagnostic` impl is manual (not `#[diagnostic(transparent)]`,
/// which requires the inner type to implement `Diagnostic` itself) and
/// forwards the display message verbatim; the diagnostic code is named
/// per-variant so `ORC-PR-STATUS-*` codes survive the move to the shared
/// crate.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct PrStatusDiagnostic(pub PrStatusError);

impl PrStatusDiagnostic {
    /// The wrapped typed error.
    #[must_use]
    pub fn error(&self) -> &PrStatusError {
        &self.0
    }
}

/// The diagnostic code for one wrapped [`PrStatusError`] variant (the
/// `ORC-PR-STATUS-*` codes the typed errors carried when they lived in
/// the CLI; one per §9.48 exit-code class).
fn pr_status_diagnostic_code(error: &PrStatusError) -> &'static str {
    match error {
        PrStatusError::Unrecoverable { .. } => "ORC-PR-STATUS-UNRECOVERABLE",
        PrStatusError::Config { .. } => "ORC-PR-STATUS-CONFIG",
        PrStatusError::ConcurrentEdit { .. } => "ORC-PR-STATUS-CONCURRENT-EDIT",
    }
}

impl miette::Diagnostic for PrStatusDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        Some(Box::new(pr_status_diagnostic_code(&self.0)))
    }
}

/// Wraps a typed [`PrStatusError`] into a downcastable miette report.
pub(crate) fn typed_report(error: PrStatusError) -> miette::Report {
    miette::Report::new(PrStatusDiagnostic(error))
}

/// Runs an `orc pr` subcommand.
///
/// # Errors
/// Returns typed [`PrStatusError::Unrecoverable`], [`PrStatusError::Config`],
/// or [`PrStatusError::ConcurrentEdit`] diagnostics per the §9.48 exit-code
/// contract; ordinary `miette` failures surface as unrecoverable.
pub fn run<W: Write>(paths: &ConfigPaths, command: PrCommand, writer: &mut W) -> Result<()> {
    match command {
        PrCommand::Status(args) => status(paths, &args, writer),
    }
}

/// Runs `orc pr status <N>`.
fn status<W: Write>(paths: &ConfigPaths, args: &PrStatusArgs, writer: &mut W) -> Result<()> {
    // All typed failures — slug validation, engine construction, the
    // aggregation — funnel through ONE outcome so the diagnostic-printing
    // branch below covers every error (a typed error that bypasses it would
    // exit mapped-but-silent).
    let outcome = resolve_repo(args.repo.as_deref()).and_then(|(owner, repo)| {
        // The review-record root resolves BEFORE the token mint: a caller
        // outside a git checkout gets the typed config error (exit 2)
        // immediately, never after a wasted mint and network call.
        repo_root().and_then(|repo_root| {
            build_engine(paths, repo_root).and_then(|engine| {
                // `PrStatusError` is a plain std error (the mcp crate has no
                // miette dependency): `Report::new` requires `Diagnostic`, so
                // the typed error is wrapped in the [`PrStatusDiagnostic`]
                // newtype — the exit-code mapping in `main.rs` and the
                // diagnostic-printing branch below match THAT wrapper
                // (`is::<PrStatusDiagnostic>()` / `downcast_ref`), never the
                // bare `PrStatusError`.
                engine
                    .aggregate(&owner, &repo, args.number)
                    .map_err(typed_report)
            })
        })
    });
    match outcome {
        Ok(verdict) => {
            if args.json {
                let json = serde_json::to_string_pretty(&verdict)
                    .map_err(|error| miette!("verdict is not serializable: {error}"))?;
                writeln!(writer, "{json}").into_diagnostic()?;
            } else {
                write_one_pager(writer, &verdict.repository, &verdict)?;
            }
            Ok(())
        }
        Err(error) if error.is::<PrStatusDiagnostic>() => {
            // main maps the typed errors WITHOUT its generic miette render,
            // so the diagnostic prints HERE exactly once. The partial typed
            // JSON (an unread aggregate) rides stderr first so a caller
            // needing the degraded state can read it.
            if let Some(PrStatusError::Unrecoverable { partial_json, .. }) = error
                .downcast_ref::<PrStatusDiagnostic>()
                .map(PrStatusDiagnostic::error)
                && !partial_json.is_empty()
            {
                writeln!(std::io::stderr(), "{partial_json}").into_diagnostic()?;
            }
            writeln!(std::io::stderr(), "error: {error:#}").into_diagnostic()?;
            Err(error)
        }
        // Untyped errors (transport construction, writer failures): main
        // renders them via miette — print nothing here (no double output).
        Err(error) => Err(error),
    }
}

/// Resolves the target repository slug: an explicit `--repo owner/name` wins;
/// otherwise the `origin` remote of the current checkout (the scripts'
/// `orc_lib_resolve_repo` default). Never guesses beyond that.
pub(crate) fn resolve_repo(explicit: Option<&str>) -> Result<(String, String)> {
    let slug = if let Some(slug) = explicit {
        slug.to_string()
    } else {
        let (owner, repo) = super::github::origin_owner_repo();
        match (owner, repo) {
            (Some(owner), Some(repo)) => format!("{owner}/{repo}"),
            _ => {
                return Err(typed_report(PrStatusError::Config {
                    message: "could not resolve the target repository — pass --repo OWNER/REPO \
                              explicitly (no github.com `origin` remote in this checkout)"
                        .to_string(),
                }));
            }
        }
    };
    let Some((owner, name)) = slug.split_once('/') else {
        return Err(typed_report(PrStatusError::Config {
            message: format!("--repo must be `owner/name`, got `{slug}`"),
        }));
    };
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(typed_report(PrStatusError::Config {
            message: format!("--repo must be `owner/name`, got `{slug}`"),
        }));
    }
    Ok((owner.to_string(), name.to_string()))
}

/// Resolves the enclosing git repository's working-tree root (the scripts'
/// `git rev-parse --show-toplevel`).
pub(crate) fn repo_root() -> Result<PathBuf> {
    // The shared scrubbed-git helper: ambient GIT_DIR/GIT_WORK_TREE (hook
    // exports) must never redirect the record lookup away from the intended
    // checkout (#553/#529 corruption class — same scrub every other
    // production git call in this crate uses). Running outside a checkout is
    // a config/state condition (exit 2), not a transport failure.
    let root = super::github::git(None, &["rev-parse", "--show-toplevel"]).map_err(|error| {
        typed_report(PrStatusError::Config {
            message: format!(
                "pr status requires running from the target git checkout (the review records \
                     live in <repo-root>/.orchestraitor/reviews/): {error:#}"
            ),
        })
    })?;
    if root.is_empty() {
        return Err(typed_report(PrStatusError::Config {
            message: "`git rev-parse --show-toplevel` returned an empty path".to_string(),
        }));
    }
    Ok(PathBuf::from(root))
}

/// Builds the shared engine over the CLI transport: mints one installation
/// token through the same service-identity route every `orc github`
/// subcommand uses (the `orc github gh-env` route's mint path) and resolves
/// the GraphQL endpoint. Missing/broken `github_app` config is a typed
/// CONFIG error (exit 2) — including a PARTIAL block (the §9.48 contract
/// names the unresolved service identity a config/state error); only a mint
/// failure with a complete config (unresolvable key, HTTP error) is
/// unrecoverable (exit 1).
pub(crate) fn build_engine(paths: &ConfigPaths, repo_root: PathBuf) -> Result<Engine> {
    let config = super::github::resolved_config(paths).map_err(|error| {
        typed_report(PrStatusError::Config {
            // `resolved_config`'s diagnostic names the failing key/layer and
            // carries no secrets — keep it so an operator can fix the file.
            message: format!("layered configuration could not be resolved: {error:#}"),
        })
    })?;
    let Some(github_app) = config.github_app.as_ref() else {
        return Err(typed_report(PrStatusError::Config {
            message: "github_app configuration is not set (the §9.48 read route is the \
                      service identity; there is no personal fallback)"
                .to_string(),
        }));
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
        return Err(typed_report(PrStatusError::Config {
            message: format!(
                "github_app config is incomplete (missing: {}); the §9.48 read route is \
                 the service identity",
                missing.join(", ")
            ),
        }));
    }
    let (_, token) = match super::github::mint_installation_token(paths) {
        Ok(minted) => minted,
        Err(error) => {
            return Err(typed_report(PrStatusError::Unrecoverable {
                message: format!("installation token could not be minted: {error}"),
                partial_json: String::new(),
            }));
        }
    };
    let graphql_base: Option<String> = match paths.github_graphql_endpoint.as_deref() {
        // Endpoint already names `/graphql`: use its API-root prefix so
        // ApiTransport (which appends "graphql") lands on the same URL.
        Some(endpoint) if endpoint.ends_with("/graphql") => {
            let base = endpoint.trim_end_matches("/graphql").trim_end_matches('/');
            Some(base.to_string())
        }
        // Explicit GraphQL override that does NOT end in /graphql: an
        // API root — ApiTransport appends "graphql" itself.
        Some(endpoint) => Some(endpoint.trim_end_matches('/').to_string()),
        // No GraphQL override: fall back to the REST override so GHES
        // stays on one host (REST /api/v3, GraphQL /api/graphql — strip
        // /v3).
        None => paths.github_api_endpoint.as_deref().map(|endpoint| {
            endpoint
                .trim_end_matches('/')
                .trim_end_matches("/v3")
                .to_string()
        }),
    };
    let transport = super::github::ApiTransport::new(graphql_base)?;
    let cli_transport = CliTransport {
        transport,
        bearer: SecretString::from(token.token().expose_secret().to_string()),
    };
    Ok(Engine::new(Arc::new(cli_transport), repo_root))
}

/// The CLI's blocking GitHub GraphQL transport: wraps the shared
/// `ApiTransport` with the installation token bearer. The HTTP/status/parse
/// logic (partial-error tolerance, `NOT_FOUND` → config) is the transport's
/// contract side of [`PrStatusTransport`].
struct CliTransport {
    /// Shared REST/GraphQL transport (endpoint override honored for tests).
    transport: super::github::ApiTransport,
    /// Installation-token bearer for every request of this invocation, held
    /// as a secret (zeroized on drop; never printed or logged).
    bearer: SecretString,
}

impl PrStatusTransport for CliTransport {
    /// Executes one GraphQL operation; transport/HTTP/shape failures are
    /// typed unrecoverable errors. Returns the `data` object plus whether
    /// the envelope carried partial-error entries (data present, errors
    /// alongside — e.g. FORBIDDEN on a sub-field).
    fn graphql(
        &self,
        operation: &str,
        query: &str,
        variables: &Value,
    ) -> std::result::Result<(Value, bool), PrStatusError> {
        let response = self
            .transport
            .request_bearer(
                "POST",
                "graphql",
                self.bearer.expose_secret(),
                Some(serde_json::json!({"query": query, "variables": variables}).to_string()),
            )
            .map_err(|error| PrStatusError::Unrecoverable {
                message: format!("github api transport failed ({operation}): {error}"),
                partial_json: String::new(),
            })?;
        let (status_code, body) = response;
        if !status_code.is_success() {
            return Err(PrStatusError::Unrecoverable {
                message: format!("github api request returned HTTP {status_code} ({operation})"),
                partial_json: String::new(),
            });
        }
        let envelope: Value =
            serde_json::from_str(&body).map_err(|_| PrStatusError::Unrecoverable {
                message: format!("github api response is malformed ({operation})"),
                partial_json: String::new(),
            })?;
        // A NOT_FOUND-typed error is ALWAYS a config/state condition (exit 2),
        // even alongside partial data.
        let not_found = envelope
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| {
                errors
                    .iter()
                    .any(|error| error.get("type").and_then(Value::as_str) == Some("NOT_FOUND"))
            });
        if not_found {
            return Err(PrStatusError::Config {
                message: format!("github api request failed ({operation}): not found"),
            });
        }
        // Partial-error tolerance: GitHub can answer HTTP 200 with `data`
        // populated AND `errors` entries (e.g. FORBIDDEN on a sub-field the
        // installation cannot read, like workflowRun). An empty/absent `data`
        // is a real failure; data WITH errors degrades — the caller's pointer
        // reads produce their own typed misses on the affected sub-fields.
        // (A NOT_FOUND-typed error was already handled above.)
        if envelope.get("data").is_none_or(Value::is_null)
            && let Some(errors) = envelope.get("errors").and_then(Value::as_array)
        {
            let rendered: Vec<String> = errors
                .iter()
                .filter_map(|error| error.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            if !rendered.is_empty() {
                return Err(PrStatusError::Unrecoverable {
                    message: format!(
                        "github api request failed ({operation}): {}",
                        rendered.join("; ")
                    ),
                    partial_json: String::new(),
                });
            }
        }
        // `data: null` (explicit null, no errors) is malformed for every
        // operation this engine issues — same exit-1 path as an absent key.
        let data = envelope
            .get("data")
            .filter(|data| !data.is_null())
            .cloned()
            .ok_or_else(|| PrStatusError::Unrecoverable {
                message: format!("github api response is malformed: missing data ({operation})"),
                partial_json: String::new(),
            })?;
        let had_errors = envelope
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty());
        Ok((data, had_errors))
    }
}

/// Renders the human one-pager: verdict first, reason codes, one line per
/// failing/pending check, one line per unresolved actionable thread, the
/// checklist delta — a screen, not a transcript.
pub(crate) fn write_one_pager<W: Write>(
    writer: &mut W,
    repository: &str,
    verdict: &PrStatus,
) -> Result<()> {
    let convergence = &verdict.convergence;
    let headline = if convergence.mergeable {
        "MERGEABLE"
    } else {
        "BLOCKED"
    };
    writeln!(
        writer,
        "{headline}: PR #{} ({}) is {}right now.",
        verdict.number,
        repository,
        if convergence.mergeable {
            "mergeable "
        } else {
            "NOT mergeable "
        }
    )
    .into_diagnostic()?;
    writeln!(
        writer,
        "head {}  base {}",
        verdict.head_sha, verdict.base_oid
    )
    .into_diagnostic()?;
    if convergence.reasons.is_empty() {
        writeln!(writer, "reasons: (none — all gates pass)").into_diagnostic()?;
    } else {
        writeln!(writer, "reasons: {}", convergence.reasons.join(", ")).into_diagnostic()?;
    }
    if let Some(checks) = &verdict.checks {
        for check in checks {
            if matches!(check.classification.as_str(), "fail" | "pending") {
                writeln!(
                    writer,
                    "check [{}] {}: {}",
                    check.classification, check.name, check.state
                )
                .into_diagnostic()?;
            }
        }
    }
    if let Some(threads) = &verdict.threads {
        for thread in &threads.items {
            if thread.actionable {
                writeln!(
                    writer,
                    "thread {}: {}:{} — {}",
                    thread.id,
                    thread.path.as_deref().unwrap_or("(no path)"),
                    thread
                        .line
                        .map_or_else(|| "-".to_string(), |line| line.to_string()),
                    thread
                        .last_comment_excerpt
                        .as_deref()
                        .unwrap_or("(no comments)")
                )
                .into_diagnostic()?;
            }
        }
    }
    if let Some(checklist) = &verdict.checklist
        && !checklist.all_checked
    {
        writeln!(
            writer,
            "checklist: {} unchecked item(s)",
            checklist.unchecked_count
        )
        .into_diagnostic()?;
        for item in &checklist.items {
            if !item.checked {
                writeln!(writer, "  [ ] {}: {}", item.marker, item.description)
                    .into_diagnostic()?;
            }
        }
    }
    writeln!(writer, "fetched_at: {}", verdict.fetched_at).into_diagnostic()?;
    Ok(())
}
