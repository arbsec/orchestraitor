//! `orc pr status` — the in-process PR-status aggregation engine (spec
//! `10-orchestrator.md` §9.48, issue #614 slice 1).
//!
//! One read-only pass over the SAME facts the pr-lifecycle skill scripts read
//! (`pr-checks`, `review-threads`, `reconcile-checklist`, `convergence-status`,
//! `merge-gate`): one PR view, one checks rollup, one `reviewThreads` GraphQL
//! query, one body fetch, one CLEAN-record file read. The scripts stay the
//! normative definition of every classification rule; this engine consumes
//! their semantics VERBATIM — one vocabulary, never a second interpretation.
//!
//! Read-only: the tool never labels, comments, reviews, or merges.

use std::io::Write;
use std::path::PathBuf;

use miette::{IntoDiagnostic, Result, miette};
use secrecy::{ExposeSecret as _, SecretString};
use serde::Serialize;
use serde_json::Value;

use crate::cli::{ConfigPaths, PrCommand, PrStatusArgs};

/// Known-disabled workflow jobs (pr-checks' `KNOWN_DISABLED_JOBS`): deliberately
/// disabled repo-side via `if: false`, so they report the skipped state and can
/// never pass. A known-disabled job exempts from the all-passed gate ONLY while
/// its bucket is `skipping` — the same job actually failing is a failure, never
/// a pass. Re-enabling a job on PRs means removing its name here AND in the
/// script, in the same change (one vocabulary).
const KNOWN_DISABLED_JOBS: [&str; 2] = ["Docs Consistency", "Documentation"];

/// Size cap for the last-comment excerpt of a review thread (spec §9.48:
/// excerpts are size-capped, never full bodies).
const THREAD_EXCERPT_CAP: usize = 280;

/// Typed exit-code errors (spec §9.48 exit-code contract). Each maps to a
/// distinct process exit code via the binary entry point:
/// `0` mergeable (BLOCKED is a successful read, not an error); `1`
/// unrecoverable (transport, malformed provider output); `2` config/state
/// (PR not found, missing project config, service identity unresolved);
/// `3` reserved — never used by this read-only tool; `4` concurrent edit
/// (an anchor moved between the pre- and post-reads).
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("pr status: {message}")]
#[diagnostic(code("ORC-PR-STATUS-UNRECOVERABLE"))]
pub struct PrStatusUnrecoverable {
    /// Typed message naming the failed sub-read; never credential material.
    pub message: String,
    /// The typed JSON emitted before this error surfaced (empty when no
    /// partial verdict exists).
    pub partial_json: String,
}

/// Config/state failure (`pr status` exit 2): PR not found, missing project
/// config, or the service identity is unresolved.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error("pr status: {message}")]
#[diagnostic(code("ORC-PR-STATUS-CONFIG"))]
pub struct PrStatusConfigError {
    /// Typed message naming the missing/broken configuration.
    pub message: String,
}

/// Concurrent-edit failure (`pr status` exit 4): a convergence anchor moved
/// between the pre-read and the post-read.
#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error(
    "pr status: concurrent edit — {anchor} moved between the pre- and post-reads \
     ({before} -> {after}); re-read and retry"
)]
#[diagnostic(code("ORC-PR-STATUS-CONCURRENT-EDIT"))]
pub struct PrStatusConcurrentEdit {
    /// Which anchor moved: `head` or `base`.
    pub anchor: &'static str,
    /// The OID observed before the sub-reads.
    pub before: String,
    /// The OID observed after the sub-reads.
    pub after: String,
}

/// Runs an `orc pr` subcommand.
///
/// # Errors
/// Returns typed [`PrStatusUnrecoverable`], [`PrStatusConfigError`], or
/// [`PrStatusConcurrentEdit`] diagnostics per the §9.48 exit-code contract;
/// ordinary `miette` failures surface as unrecoverable.
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
            Engine::new(paths).and_then(|engine| {
                engine.aggregate_with_root(&owner, &repo, args.number, &repo_root)
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
        Err(error) if error.is::<PrStatusUnrecoverable>() => {
            // main maps the typed errors WITHOUT its generic miette render,
            // so the diagnostic prints HERE exactly once. The partial typed
            // JSON (an unread aggregate) rides stderr first so a caller
            // needing the degraded state can read it.
            if let Some(unrecoverable) = error.downcast_ref::<PrStatusUnrecoverable>()
                && !unrecoverable.partial_json.is_empty()
            {
                writeln!(std::io::stderr(), "{}", unrecoverable.partial_json).into_diagnostic()?;
            }
            writeln!(std::io::stderr(), "error: {error:#}").into_diagnostic()?;
            Err(error)
        }
        Err(error) if error.is::<PrStatusConfigError>() => {
            writeln!(std::io::stderr(), "error: {error:#}").into_diagnostic()?;
            Err(error)
        }
        Err(error) if error.is::<PrStatusConcurrentEdit>() => {
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
fn resolve_repo(explicit: Option<&str>) -> Result<(String, String)> {
    let slug = if let Some(slug) = explicit {
        slug.to_string()
    } else {
        let (owner, repo) = super::github::origin_owner_repo();
        match (owner, repo) {
            (Some(owner), Some(repo)) => format!("{owner}/{repo}"),
            _ => {
                return Err(PrStatusConfigError {
                    message: "could not resolve the target repository — pass --repo OWNER/REPO \
                              explicitly (no github.com `origin` remote in this checkout)"
                        .to_string(),
                }
                .into());
            }
        }
    };
    let Some((owner, name)) = slug.split_once('/') else {
        return Err(PrStatusConfigError {
            message: format!("--repo must be `owner/name`, got `{slug}`"),
        }
        .into());
    };
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return Err(PrStatusConfigError {
            message: format!("--repo must be `owner/name`, got `{slug}`"),
        }
        .into());
    }
    Ok((owner.to_string(), name.to_string()))
}

/// Renders the human one-pager: verdict first, reason codes, one line per
/// failing/pending check, one line per unresolved actionable thread, the
/// checklist delta — a screen, not a transcript.
fn write_one_pager<W: Write>(writer: &mut W, repository: &str, verdict: &PrStatus) -> Result<()> {
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

// ---------------------------------------------------------------------------
// Typed verdict (the §9.48 schema, one consumer contract)
// ---------------------------------------------------------------------------

/// The typed §9.48 PR-status verdict. Field names match the spec schema
/// verbatim; the JSON form is the machine contract.
#[derive(Debug, Clone, Serialize)]
pub struct PrStatus {
    /// Pull-request number.
    pub number: u64,
    /// `owner/name` slug of the target repository.
    pub repository: String,
    /// Full head OID (convergence anchor one).
    pub head_sha: String,
    /// Full base OID (convergence anchor two).
    pub base_oid: String,
    /// `gh pr view` isDraft, verbatim.
    pub is_draft: bool,
    /// `gh pr view` reviewDecision, verbatim.
    pub review_decision: String,
    /// mergeStateStatus — INFORMATIONAL ONLY, never a convergence input.
    pub merge_state: String,
    /// Classified checks; `None` when the checks sub-read is unread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<Vec<CheckItem>>,
    /// All checks pass (pass or not-applicable) and at least one check exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks_all_passed: Option<bool>,
    /// At least one check is failing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks_any_failing: Option<bool>,
    /// At least one check is pending.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks_any_pending: Option<bool>,
    /// Review threads; `None` when the threads sub-read is unread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threads: Option<ThreadsSummary>,
    /// Checklist reconciliation; `None` when the body sub-read is unread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checklist: Option<Checklist>,
    /// The `.orchestraitor/reviews/` CLEAN-record state.
    pub review_record: ReviewRecord,
    /// The aggregate verdict.
    pub convergence: Convergence,
    /// Provider-clock timestamp of the snapshot (UTC, RFC 3339).
    pub fetched_at: String,
}

/// One classified CI check (pr-checks semantics, verbatim).
#[derive(Debug, Clone, Serialize)]
pub struct CheckItem {
    /// Check name (workflow job name or status context).
    pub name: String,
    /// Raw state string (conclusion, status, or `StatusContext` state).
    pub state: String,
    /// gh's bucket: pass|fail|pending|skipping|cancel.
    pub bucket: String,
    /// pr-checks classification: pass|fail|pending|skip|not-applicable|unknown.
    pub classification: String,
    /// Workflow name (`CheckRun` only; empty for status contexts).
    pub workflow: String,
    /// Details/target URL.
    pub link: String,
}

/// Review-thread aggregate (review-threads semantics, verbatim).
#[derive(Debug, Clone, Serialize)]
pub struct ThreadsSummary {
    /// Total thread count.
    pub total: u64,
    /// Unresolved AND not outdated — the review-threads actionable rule.
    pub unresolved_actionable: u64,
    /// Resolved thread count.
    pub resolved: u64,
    /// Outdated thread count.
    pub outdated: u64,
    /// Size-capped thread items.
    pub items: Vec<ThreadItem>,
}

/// One review-thread item.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadItem {
    /// Thread node id.
    pub id: String,
    /// Resolved flag.
    pub is_resolved: bool,
    /// Outdated flag.
    pub is_outdated: bool,
    /// Whether the thread is actionable (`!is_resolved && !is_outdated`).
    pub actionable: bool,
    /// File path (when known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Line number (when known).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    /// Author logins of the thread's comments, in order, deduplicated.
    pub authors: Vec<String>,
    /// Size-capped excerpt of the last comment body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_comment_excerpt: Option<String>,
}

/// Checklist reconciliation (reconcile-checklist semantics, verbatim).
#[derive(Debug, Clone, Serialize)]
pub struct Checklist {
    /// Items with their orc markers.
    pub items: Vec<ChecklistItem>,
    /// Vacuously true when no markers exist.
    pub all_checked: bool,
    /// Count of unchecked items.
    pub unchecked_count: u64,
}

/// One managed checklist item.
#[derive(Debug, Clone, Serialize)]
pub struct ChecklistItem {
    /// The orc marker id (the `orc:XXX` in `<!-- orc:XXX -->`).
    pub marker: String,
    /// Whether the box is checked (`x`).
    pub checked: bool,
    /// Description text after the marker.
    pub description: String,
}

/// The CLEAN-record state (convergence-status semantics, verbatim).
#[derive(Debug, Clone, Serialize)]
pub struct ReviewRecord {
    /// Whether a record file exists for this PR at the current head.
    pub present: bool,
    /// The record's verdict line (CLEAN or FINDINGS); empty when absent.
    pub verdict: String,
    /// The record's full head SHA equals the PR's current head.
    pub head_sha_match: bool,
    /// The record's base OID equals the PR's current base.
    pub base_oid_match: bool,
}

/// The aggregate convergence verdict.
#[derive(Debug, Clone, Serialize)]
pub struct Convergence {
    /// Whether every gate passes.
    pub mergeable: bool,
    /// Stable machine reason codes, extended only additively.
    pub reasons: Vec<String>,
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The aggregation engine: one minted installation token, one set of
/// sub-reads, one verdict.
struct Engine {
    /// Shared REST/GraphQL transport (endpoint override honored for tests).
    transport: super::github::ApiTransport,
    /// Installation-token bearer for every request of this invocation, held
    /// as a secret (zeroized on drop; never printed or logged).
    bearer: SecretString,
}

impl Engine {
    /// Mints one installation token through the same service-identity route
    /// every `orc github` subcommand uses (the `orc github gh-env` route's
    /// mint path). Missing/broken `github_app` config is a typed CONFIG
    /// error (exit 2) — including a PARTIAL block (the §9.48 contract names
    /// the unresolved service identity a config/state error); only a mint
    /// failure with a complete config (unresolvable key, HTTP error) is
    /// unrecoverable (exit 1).
    fn new(paths: &ConfigPaths) -> Result<Self> {
        let config =
            super::github::resolved_config(paths).map_err(|error| PrStatusConfigError {
                // `resolved_config`'s diagnostic names the failing key/layer and
                // carries no secrets — keep it so an operator can fix the file.
                message: format!("layered configuration could not be resolved: {error:#}"),
            })?;
        let Some(github_app) = config.github_app.as_ref() else {
            return Err(PrStatusConfigError {
                message: "github_app configuration is not set (the §9.48 read route is the \
                          service identity; there is no personal fallback)"
                    .to_string(),
            }
            .into());
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
            return Err(PrStatusConfigError {
                message: format!(
                    "github_app config is incomplete (missing: {}); the §9.48 read route is \
                     the service identity",
                    missing.join(", ")
                ),
            }
            .into());
        }
        let (_, token) = match super::github::mint_installation_token(paths) {
            Ok(minted) => minted,
            Err(error) => {
                return Err(PrStatusUnrecoverable {
                    message: format!("installation token could not be minted: {error}"),
                    partial_json: String::new(),
                }
                .into());
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
        Ok(Self {
            transport,
            bearer: SecretString::from(token.token().expose_secret().to_string()),
        })
    }

    /// Executes one GraphQL operation; transport/HTTP/shape failures are
    /// typed unrecoverable errors.
    fn graphql(&self, operation: &str, query: &str, variables: &Value) -> Result<Value> {
        let response = self
            .transport
            .request_bearer(
                "POST",
                "graphql",
                self.bearer.expose_secret(),
                Some(serde_json::json!({"query": query, "variables": variables}).to_string()),
            )
            .map_err(|error| PrStatusUnrecoverable {
                message: format!("github api transport failed ({operation}): {error}"),
                partial_json: String::new(),
            })?;
        let (status_code, body) = response;
        if !status_code.is_success() {
            return Err(PrStatusUnrecoverable {
                message: format!("github api request returned HTTP {status_code} ({operation})"),
                partial_json: String::new(),
            }
            .into());
        }
        let envelope: Value = serde_json::from_str(&body).map_err(|_| PrStatusUnrecoverable {
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
            return Err(PrStatusConfigError {
                message: format!("github api request failed ({operation}): not found"),
            }
            .into());
        }
        // Partial-error tolerance: GitHub can answer HTTP 200 with `data`
        // populated AND `errors` entries (e.g. FORBIDDEN on a sub-field the
        // installation cannot read, like workflowRun). An empty/absent `data`
        // is a real failure; data WITH errors degrades — the caller's pointer
        // reads produce their own typed misses on the affected sub-fields.
        if envelope.get("data").is_none_or(Value::is_null)
            && let Some(errors) = envelope.get("errors").and_then(Value::as_array)
        {
            // GitHub maps a missing PR/repository to a NOT_FOUND-typed
            // error entry — a config/state condition (exit 2).
            if errors
                .iter()
                .any(|error| error.get("type").and_then(Value::as_str) == Some("NOT_FOUND"))
            {
                return Err(PrStatusConfigError {
                    message: format!("github api request failed ({operation}): not found"),
                }
                .into());
            }
            let rendered: Vec<String> = errors
                .iter()
                .filter_map(|error| error.get("message").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            if !rendered.is_empty() {
                return Err(PrStatusUnrecoverable {
                    message: format!(
                        "github api request failed ({operation}): {}",
                        rendered.join("; ")
                    ),
                    partial_json: String::new(),
                }
                .into());
            }
        }
        envelope.get("data").cloned().ok_or_else(|| {
            PrStatusUnrecoverable {
                message: format!("github api response is malformed: missing data ({operation})"),
                partial_json: String::new(),
            }
            .into()
        })
    }

    /// Aggregates the full §9.48 verdict for one PR.
    ///
    /// Snapshot discipline (spec §9.48): capture head SHA and base OID BEFORE
    /// the sub-reads, re-read BOTH after them, and refuse (exit 4) on any
    /// anchor movement — never a verdict from a mixed snapshot.
    fn aggregate_with_root(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        repo_root: &std::path::Path,
    ) -> Result<PrStatus> {
        // 1. The pre-read PR view: identity fields + the two anchors.
        let initial = self.pr_view(owner, repo, number)?;
        let head_sha = oid_field(&initial, "headRefOid", "head")?;
        let base_oid = oid_field(&initial, "baseRefOid", "base")?;

        // 2. The sub-reads, each failing closed into `unread(<sub-check>)`.
        let checks = self.read_checks(owner, repo, &head_sha);
        let threads = self.read_threads(owner, repo, number);
        let checklist = self.read_body(owner, repo, number);
        let review_record = read_review_record(repo_root, number, &head_sha, &base_oid);

        // 3. The post-read anchor check BEFORE any verdict is emitted.
        self.check_anchors_stable(owner, repo, number, &head_sha, &base_oid)?;

        // 4. Verdict assembly (see `assemble_verdict`).
        assemble_verdict(
            &initial,
            owner,
            repo,
            number,
            SubReads {
                checks,
                threads,
                checklist,
                review_record,
            },
        )
    }

    /// Re-reads BOTH convergence anchors from the provider and refuses (exit
    /// 4, a typed [`PrStatusConcurrentEdit`] naming the moved anchor) on any
    /// movement — never a verdict assembled from a mixed snapshot.
    fn check_anchors_stable(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        head_sha: &str,
        base_oid: &str,
    ) -> Result<()> {
        let final_view = self.pr_view(owner, repo, number)?;
        let final_head = oid_field(&final_view, "headRefOid", "head")?;
        let final_base = oid_field(&final_view, "baseRefOid", "base")?;
        if final_head == head_sha && final_base == base_oid {
            return Ok(());
        }
        let (anchor, before, after) = if final_head == head_sha {
            ("base", base_oid.to_string(), final_base)
        } else {
            ("head", head_sha.to_string(), final_head)
        };
        let _ = number;
        Err(PrStatusConcurrentEdit {
            anchor,
            before,
            after,
        }
        .into())
    }

    /// The PR view: number, headRefOid, baseRefOid, isDraft, reviewDecision,
    /// mergeStateStatus — the fields pr-state and convergence-status read.
    fn pr_view(&self, owner: &str, repo: &str, number: u64) -> Result<Value> {
        let query = "query($owner:String!,$name:String!,$number:Int!){\
repository(owner:$owner,name:$name){\
pullRequest(number:$number){number headRefOid baseRefOid isDraft reviewDecision mergeStateStatus}}}\
";
        let data = self.graphql(
            "pr view",
            query,
            &serde_json::json!({"owner": owner, "name": repo, "number": number}),
        )?;
        let pr = data
            .pointer("/repository/pullRequest")
            .filter(|pr| !pr.is_null())
            .ok_or_else(|| PrStatusConfigError {
                message: format!("PR #{number} not found in {owner}/{repo}"),
            })?;
        Ok(pr.clone())
    }

    /// The checks rollup: the exact GraphQL shape `gh pr checks` reads
    /// (statusCheckRollup contexts: `StatusContext` + `CheckRun`), aggregated
    /// with gh's bucket derivation, then classified with pr-checks' rules.
    fn read_checks(
        &self,
        owner: &str,
        repo: &str,
        head_oid: &str,
    ) -> Result<ChecksAggregate, String> {
        let query = "query($owner:String!,$name:String!,$oid:GitObjectID!){\
repository(owner:$owner,name:$name){\
object(oid:$oid){... on Commit{statusCheckRollup{contexts(first:100){pageInfo{hasNextPage}nodes{\
__typename\
... on StatusContext{context state targetUrl createdAt}\
... on CheckRun{name checkSuite{workflowRun{event workflow{name}}} status conclusion startedAt completedAt detailsUrl}\
}}}}}}}";
        let data = self
            .graphql(
                "checks",
                query,
                &serde_json::json!({"owner": owner, "name": repo, "oid": head_oid}),
            )
            .map_err(|error| format!("{error:#}"))?;
        // A head commit with NO status contexts yet (fresh push, repo without
        // CI) reports `statusCheckRollup: null` — a valid provider state, not
        // a failure: aggregate over an EMPTY checks array (all_passed is then
        // false per pr-checks' `length > 0` rule → `checks_not_passing`,
        // exit 0 BLOCKED), never `unread(checks)`. A missing `object` (no
        // such commit) or a wrong shape stays unread.
        let rollup = data.pointer("/repository/object/statusCheckRollup");
        let nodes: Vec<Value> = match rollup {
            Some(rollup) if rollup.is_null() => Vec::new(),
            Some(rollup) => rollup
                .pointer("/contexts/nodes")
                .and_then(Value::as_array)
                .cloned()
                .ok_or_else(|| "checks rollup response is missing contexts nodes".to_string())?,
            None => {
                return Err(
                    "checks rollup response is missing (no object for the head commit)".to_string(),
                );
            }
        };
        let has_next = data
            .pointer("/repository/object/statusCheckRollup/contexts/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if has_next {
            // >100 contexts: the first page's aggregate would silently lie.
            return Err(
                "checks rollup exceeds one page (100 contexts); not aggregated".to_string(),
            );
        }
        // gh's eliminateDuplicates, verbatim: when a check re-ran, keep ONLY
        // the most recent run (sort by startedAt descending, first seen per
        // (context) or (name, workflow, event) key wins). Without this, an
        // old failed run next to a new passing one would fail the gate
        // forever. startedAt is absent on some StatusContexts — absent sorts
        // last (oldest), matching an empty instant.
        let mut deduped: Vec<&Value> = Vec::new();
        let mut seen_contexts: Vec<String> = Vec::new();
        let mut seen_checks: Vec<String> = Vec::new();
        let started_at = |node: &Value| -> String {
            node.get("startedAt")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let mut sorted: Vec<&Value> = nodes.iter().collect();
        sorted.sort_by_key(|node| std::cmp::Reverse(started_at(node)));
        for node in sorted {
            let context = node
                .get("context")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if context.is_empty() {
                let key = format!(
                    "{}/{}/{}",
                    node.get("name").and_then(Value::as_str).unwrap_or_default(),
                    node.pointer("/checkSuite/workflowRun/workflow/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    // gh keys on (name, workflow, event) — include the event
                    // so differently-triggered runs of the same job stay
                    // distinct, exactly as eliminateDuplicates does.
                    node.pointer("/checkSuite/workflowRun/event")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
                if seen_checks.contains(&key) {
                    continue;
                }
                seen_checks.push(key);
            } else {
                if seen_contexts.contains(&context) {
                    continue;
                }
                seen_contexts.push(context);
            }
            deduped.push(node);
        }
        let mut items: Vec<CheckItem> = Vec::new();
        for node in deduped {
            items.push(classify_check(node)?);
        }
        Ok(aggregate_checks(items))
    }

    /// The reviewThreads GraphQL query (review-threads' query, verbatim shape)
    /// reduced to the typed summary.
    fn read_threads(&self, owner: &str, repo: &str, number: u64) -> Result<ThreadsSummary, String> {
        let query = "query($owner:String!,$name:String!,$number:Int!){\
repository(owner:$owner,name:$name){pullRequest(number:$number){\
reviewThreads(first:100){pageInfo{hasNextPage}nodes{\
id isResolved isOutdated path line\
comments(first:50){totalCount nodes{body author{login}}}\
}}}}}";
        let data = self
            .graphql(
                "review threads",
                query,
                &serde_json::json!({"owner": owner, "name": repo, "number": number}),
            )
            .map_err(|error| format!("{error:#}"))?;
        let nodes = data
            .pointer("/repository/pullRequest/reviewThreads/nodes")
            .and_then(Value::as_array)
            .ok_or_else(|| "review threads response is missing nodes".to_string())?;
        let has_next = data
            .pointer("/repository/pullRequest/reviewThreads/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if has_next {
            return Err("review threads exceed one page (100 threads); not aggregated".to_string());
        }
        let mut items = Vec::new();
        let mut unresolved_actionable = 0_u64;
        let mut resolved = 0_u64;
        let mut outdated = 0_u64;
        for node in nodes {
            let is_resolved = node.get("isResolved").and_then(Value::as_bool) == Some(true);
            let is_outdated = node.get("isOutdated").and_then(Value::as_bool) == Some(true);
            // review-threads rule: actionable = !isResolved && !isOutdated.
            let actionable = !is_resolved && !is_outdated;
            if is_resolved {
                resolved += 1;
            }
            if is_outdated {
                outdated += 1;
            }
            if actionable {
                unresolved_actionable += 1;
            }
            let comments = node
                .pointer("/comments/nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut authors: Vec<String> = Vec::new();
            for comment in &comments {
                if let Some(login) = comment.pointer("/author/login").and_then(Value::as_str)
                    && !authors.iter().any(|known| known == login)
                {
                    authors.push(login.to_string());
                }
            }
            // Bounded read (review-threads shape): the excerpt comes from the
            // last FETCHED comment; with >50 comments the excerpt notes the
            // truncation instead of silently presenting a stale comment as
            // the latest.
            let more_comments = node
                .pointer("/comments/totalCount")
                .and_then(Value::as_i64)
                .is_some_and(|total| total > 50);
            let last_comment_excerpt = comments
                .last()
                .and_then(|comment| comment.get("body"))
                .and_then(Value::as_str)
                .map(|body| {
                    let capped = cap_excerpt(body, THREAD_EXCERPT_CAP);
                    if more_comments {
                        format!("{capped} … (+ more comments)")
                    } else {
                        capped
                    }
                });
            items.push(ThreadItem {
                id: node
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                is_resolved,
                is_outdated,
                actionable,
                path: node.get("path").and_then(Value::as_str).map(str::to_string),
                line: node.get("line").and_then(Value::as_i64),
                authors,
                last_comment_excerpt,
            });
        }
        Ok(ThreadsSummary {
            total: u64::try_from(items.len()).unwrap_or(u64::MAX),
            unresolved_actionable,
            resolved,
            outdated,
            items,
        })
    }

    /// The PR body fetch + orc-marker checklist reconciliation
    /// (reconcile-checklist semantics, verbatim).
    fn read_body(&self, owner: &str, repo: &str, number: u64) -> Result<Checklist, String> {
        let query = "query($owner:String!,$name:String!,$number:Int!){\
repository(owner:$owner,name:$name){pullRequest(number:$number){body}}}";
        let data = self
            .graphql(
                "pr body",
                query,
                &serde_json::json!({"owner": owner, "name": repo, "number": number}),
            )
            .map_err(|error| format!("{error:#}"))?;
        let body = data
            .pointer("/repository/pullRequest/body")
            .and_then(Value::as_str)
            .ok_or_else(|| "pr body response is missing the body string".to_string())?;
        Ok(parse_checklist(body))
    }
}

/// Builds the typed §9.48 verdict object from the reduced sub-reads.
#[allow(clippy::too_many_arguments)]
fn build_verdict(
    initial: &Value,
    owner: &str,
    repo: &str,
    number: u64,
    checks: Option<&ChecksAggregate>,
    threads: Option<&ThreadsSummary>,
    checklist: Option<&Checklist>,
    review_record: &ReviewRecord,
    is_draft: bool,
    reasons: Vec<String>,
) -> PrStatus {
    PrStatus {
        number,
        repository: format!("{owner}/{repo}"),
        head_sha: initial
            .get("headRefOid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        base_oid: initial
            .get("baseRefOid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        is_draft,
        review_decision: initial
            .get("reviewDecision")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        merge_state: initial
            .get("mergeStateStatus")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_string(),
        checks: checks.map(|checks| checks.items.clone()),
        checks_all_passed: checks.map(|checks| checks.all_passed),
        checks_any_failing: checks.map(|checks| checks.any_failing),
        checks_any_pending: checks.map(|checks| checks.any_pending),
        threads: threads.cloned(),
        checklist: checklist.cloned(),
        review_record: review_record.clone(),
        convergence: Convergence {
            mergeable: reasons.is_empty(),
            reasons,
        },
        fetched_at: now_rfc3339(),
    }
}

/// The sub-read results handed to [`assemble_verdict`]: each read is either
/// its typed payload or a failure message. The reason carries the stable
/// `unread(<name>)` code ONLY — the variable provider detail rides the
/// exit-1 diagnostic, never the machine reasons.
struct SubReads {
    /// CI checks.
    checks: Result<ChecksAggregate, String>,
    /// Review threads.
    threads: Result<ThreadsSummary, String>,
    /// Checklist reconciliation.
    checklist: Result<Checklist, String>,
    /// The CLEAN-review record state.
    review_record: ReviewRecord,
}

/// Assembles the §9.48 verdict from the sub-reads. An unread sub-check
/// contributes the stable `unread(<sub-check>)` reason, forces
/// `mergeable: false`, and (via the returned error) the invocation EXITS 1
/// after emitting the typed JSON — an unread aggregate is a provider
/// failure, never a BLOCKED answer. Reason codes follow convergence-status'
/// vocabulary verbatim and in its order.
fn assemble_verdict(
    initial: &Value,
    owner: &str,
    repo: &str,
    number: u64,
    reads: SubReads,
) -> Result<PrStatus> {
    let SubReads {
        checks,
        threads,
        checklist,
        review_record,
    } = reads;
    let mut reasons: Vec<String> = Vec::new();
    let mut unread_subcheck: Option<&'static str> = None;

    let mut unread_detail: Option<String> = None;
    let checks = sub_read(
        checks,
        "checks",
        &mut reasons,
        &mut unread_subcheck,
        &mut unread_detail,
    );
    let threads = sub_read(
        threads,
        "threads",
        &mut reasons,
        &mut unread_subcheck,
        &mut unread_detail,
    );
    let checklist = sub_read(
        checklist,
        "body",
        &mut reasons,
        &mut unread_subcheck,
        &mut unread_detail,
    );

    // convergence-status' reason vocabulary, verbatim and in its order.
    let review_clean = review_record.present
        && review_record.verdict == "CLEAN"
        && review_record.head_sha_match
        && review_record.base_oid_match;
    if !review_clean {
        reasons.push("agent_review_not_clean_at_head".to_string());
    }
    if let Some(checks) = &checks {
        if !checks.all_passed {
            reasons.push("checks_not_passing".to_string());
        }
        if checks.any_pending {
            reasons.push("checks_pending".to_string());
        }
    }
    if let Some(threads) = &threads
        && threads.unresolved_actionable > 0
    {
        reasons.push(format!(
            "unresolved_threads({})",
            threads.unresolved_actionable
        ));
    }
    if let Some(checklist) = &checklist
        && !checklist.all_checked
    {
        reasons.push(format!("unchecked_items({})", checklist.unchecked_count));
    }
    if initial.get("reviewDecision").and_then(Value::as_str) == Some("REVIEW_REQUIRED") {
        reasons.push("review_required".to_string());
    }
    let is_draft = initial.get("isDraft").and_then(Value::as_bool) == Some(true);
    if is_draft {
        reasons.push("is_draft".to_string());
    }

    let verdict = build_verdict(
        initial,
        owner,
        repo,
        number,
        checks.as_ref(),
        threads.as_ref(),
        checklist.as_ref(),
        &review_record,
        is_draft,
        reasons,
    );
    if let Some(name) = unread_subcheck {
        return Err(PrStatusUnrecoverable {
            message: match unread_detail {
                Some(detail) => format!(
                    "sub-check unread({name}) could not be read ({detail}); mergeable is \
                     reported false with unread(<sub-check>) reasons in the emitted JSON"
                ),
                None => format!(
                    "sub-check unread({name}) could not be read; mergeable is reported false \
                     with unread(<sub-check>) reasons in the emitted JSON"
                ),
            },
            partial_json: serde_json::to_string(&verdict).unwrap_or_else(|_| String::new()),
        }
        .into());
    }
    Ok(verdict)
}

/// Reduces one sub-read result: `Ok` passes through; `Err(message)` records
/// the stable `unread(<name>)` code in the reasons, keeps the variable detail
/// for the diagnostic, and flags the unread marker.
fn sub_read<T: Clone>(
    result: Result<T, String>,
    name: &'static str,
    reasons: &mut Vec<String>,
    unread_subcheck: &mut Option<&'static str>,
    unread_detail: &mut Option<String>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(message) => {
            *unread_subcheck = Some(name);
            reasons.push(format!("unread({name})"));
            // The message may be a rendered typed error carrying its own
            // `pr status: ` prefix — strip it so the diagnostic never
            // repeats the prefix.
            let detail = message.trim_start_matches("pr status: ");
            *unread_detail = Some(format!("{name}: {detail}"));
            None
        }
    }
}

/// pr-checks' aggregate flags over the classified array: `all_passed` =
/// (length > 0) AND every classification is `pass` or `not-applicable`; the
/// genuinely skipped-but-optioned checks are NOT known-disabled and fail
/// the gate.
fn aggregate_checks(items: Vec<CheckItem>) -> ChecksAggregate {
    let all_passed = !items.is_empty()
        && items.iter().all(|check| {
            check.classification == "pass" || check.classification == "not-applicable"
        });
    let any_failing = items.iter().any(|check| check.classification == "fail");
    let any_pending = items.iter().any(|check| check.classification == "pending");
    ChecksAggregate {
        items,
        all_passed,
        any_failing,
        any_pending,
    }
}

/// Aggregated checks verdict (pr-checks' output object shape).
#[derive(Clone)]
struct ChecksAggregate {
    items: Vec<CheckItem>,
    all_passed: bool,
    any_failing: bool,
    any_pending: bool,
}

/// Classifies one statusCheckRollup context node with gh's bucket derivation
/// followed by pr-checks' classification — the two-stage semantics, verbatim.
fn classify_check(node: &Value) -> Result<CheckItem, String> {
    let type_name = node
        .get("__typename")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // gh's aggregateChecks: state = conclusion when COMPLETED, else status;
    // StatusContext carries `state` directly. Names: CheckRun `name`,
    // StatusContext `context`. Links: detailsUrl / targetUrl.
    let (name, raw_state, link) = if type_name == "StatusContext" {
        (
            node.get("context")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            node.get("state")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            node.get("targetUrl")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    } else {
        let status = node
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let conclusion = node
            .get("conclusion")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let state = if status == "COMPLETED" {
            conclusion
        } else {
            status
        };
        (
            node.get("name").and_then(Value::as_str).unwrap_or_default(),
            state,
            node.get("detailsUrl")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    };
    if name.is_empty() {
        return Err("check context is missing its name/context field".to_string());
    }
    let bucket = gh_bucket(raw_state);
    let workflow = node
        .pointer("/checkSuite/workflowRun/workflow/name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // pr-checks' classification: the not-applicable exemption requires BOTH
    // the known-disabled name AND bucket == "skipping" — a state-blind
    // exemption would launder real failures. `cancel` classifies as fail.
    let exempt = KNOWN_DISABLED_JOBS.contains(&name) && bucket == "skipping";
    let classification = if exempt {
        "not-applicable"
    } else {
        match bucket.as_str() {
            "pass" => "pass",
            "fail" | "cancel" => "fail",
            "pending" => "pending",
            "skipping" => "skip",
            _ => "unknown",
        }
    };
    Ok(CheckItem {
        name: name.to_string(),
        state: raw_state.to_string(),
        bucket,
        classification: classification.to_string(),
        workflow,
        link: link.to_string(),
    })
}

/// gh's bucket mapping (cli/cli `aggregateChecks`), verbatim: SUCCESS -> pass;
/// `SKIPPED`/`NEUTRAL` -> skipping; `ERROR`/`FAILURE`/`TIMED_OUT`/`ACTION_REQUIRED` ->
/// fail; `CANCELLED` -> cancel; everything else (`EXPECTED`, `REQUESTED`,
/// `WAITING`, `QUEUED`, `PENDING`, `IN_PROGRESS`, `STALE`,
/// `STARTUP_FAILURE`, `""`) -> pending.
fn gh_bucket(state: &str) -> String {
    match state {
        "SUCCESS" => "pass",
        "SKIPPED" | "NEUTRAL" => "skipping",
        "ERROR" | "FAILURE" | "TIMED_OUT" | "ACTION_REQUIRED" => "fail",
        "CANCELLED" => "cancel",
        _ => "pending",
    }
    .to_string()
}

/// Parses the PR body for `<!-- orc:MARKER -->` checklist lines
/// (reconcile-checklist's grep pattern, verbatim):
/// `- [ ] <!-- orc:XXX --> description` / `- [x] <!-- orc:XXX --> description`.
/// A body with zero markers is a legitimate empty checklist (`all_checked`
/// is vacuously true).
fn parse_checklist(body: &str) -> Checklist {
    let mut items = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("- [") else {
            continue;
        };
        let Some((checked_char, after_box)) = rest.split_once(']') else {
            continue;
        };
        if checked_char.len() != 1 || !(checked_char == "x" || checked_char == " ") {
            continue;
        }
        let after_box = after_box.trim_start();
        let Some(rest) = after_box.strip_prefix("<!--") else {
            continue;
        };
        let Some((marker, after_marker)) = rest.split_once("-->") else {
            continue;
        };
        let marker = marker.trim();
        if marker.is_empty()
            || !marker.starts_with("orc:")
            || !marker["orc:".len()..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            continue;
        }
        let marker = marker["orc:".len()..].to_string();
        let description = after_marker.trim().to_string();
        items.push(ChecklistItem {
            marker,
            checked: checked_char == "x",
            description,
        });
    }
    let unchecked_count =
        u64::try_from(items.iter().filter(|item| !item.checked).count()).unwrap_or(u64::MAX);
    let all_checked = items.iter().all(|item| item.checked);
    Checklist {
        items,
        all_checked,
        unchecked_count,
    }
}

/// Reads the CLEAN-review record for this PR at the current head
/// (convergence-status' record semantics, verbatim): the fixed metadata
/// header is authoritative; finding text below it is never interpreted.
/// `head_sha`/`base_oid` must be full 40-hex OIDs; a malformed OID means no
/// record (never a guess).
fn read_review_record(
    repo_root: &std::path::Path,
    number: u64,
    head_sha: &str,
    base_oid: &str,
) -> ReviewRecord {
    let hex40 = |value: &str| value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit());
    if !hex40(head_sha) || !hex40(base_oid) {
        return ReviewRecord {
            present: false,
            verdict: String::new(),
            head_sha_match: false,
            base_oid_match: false,
        };
    }
    let record_path = repo_root
        .join(".orchestraitor")
        .join("reviews")
        .join(format!("{number}-{}.md", &head_sha[..7]));
    let Ok(content) = std::fs::read_to_string(record_path) else {
        return ReviewRecord {
            present: false,
            verdict: String::new(),
            head_sha_match: false,
            base_oid_match: false,
        };
    };
    let lines: Vec<&str> = content.lines().collect();
    // The script reads lines 3, 5, 6 (1-based): verdict, head SHA, base OID.
    let verdict = lines
        .get(2)
        .and_then(|line| line.strip_prefix("- verdict: "))
        .unwrap_or_default()
        .to_string();
    let recorded_head = lines
        .get(4)
        .and_then(|line| line.strip_prefix("- head SHA: "))
        .unwrap_or_default();
    let recorded_base = lines
        .get(5)
        .and_then(|line| line.strip_prefix("- base OID: "))
        .unwrap_or_default();
    ReviewRecord {
        present: true,
        verdict,
        head_sha_match: recorded_head == head_sha,
        base_oid_match: recorded_base == base_oid,
    }
}

/// Resolves the enclosing git repository's working-tree root (the scripts'
/// `git rev-parse --show-toplevel`).
fn repo_root() -> Result<PathBuf> {
    // The shared scrubbed-git helper: ambient GIT_DIR/GIT_WORK_TREE (hook
    // exports) must never redirect the record lookup away from the intended
    // checkout (#553/#529 corruption class — same scrub every other
    // production git call in this crate uses). Running outside a checkout is
    // a config/state condition (exit 2), not a transport failure.
    let root = super::github::git(None, &["rev-parse", "--show-toplevel"]).map_err(|error| {
        PrStatusConfigError {
            message: format!(
                "pr status requires running from the target git checkout (the review records \
                 live in <repo-root>/.orchestraitor/reviews/): {error:#}"
            ),
        }
    })?;
    if root.is_empty() {
        return Err(PrStatusConfigError {
            message: "`git rev-parse --show-toplevel` returned an empty path".to_string(),
        }
        .into());
    }
    Ok(PathBuf::from(root))
}

/// Reads a full-OID field from the PR view; empty/missing values are typed
/// errors (a missing anchor can never pass the snapshot check).
fn oid_field(view: &Value, key: &str, anchor: &'static str) -> Result<String> {
    let value = view
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PrStatusUnrecoverable {
            message: format!("pr view response is missing the {anchor} OID ({key})"),
            partial_json: String::new(),
        })?;
    Ok(value.to_string())
}

/// Size-caps an excerpt on a char boundary (never mid-codepoint).
fn cap_excerpt(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let truncated: String = text.chars().take(cap).collect();
    format!("{truncated}…")
}

/// Current UTC time in RFC 3339 (the provider-clock timestamp of the
/// snapshot; wall-clock, fail-open to epoch like the repo's other clocks).
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    time::OffsetDateTime::from_unix_timestamp(i64::try_from(secs).unwrap_or(0))
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

    use super::*;
    use serde_json::json;

    // --- gh bucket derivation (cli/cli aggregateChecks, verbatim) -----------

    #[test]
    fn gh_bucket_mapping_matches_gh_aggregate_checks() {
        // pass / skipping / fail / cancel / pending buckets.
        for (state, expected) in [
            ("SUCCESS", "pass"),
            ("SKIPPED", "skipping"),
            ("NEUTRAL", "skipping"),
            ("ERROR", "fail"),
            ("FAILURE", "fail"),
            ("TIMED_OUT", "fail"),
            ("ACTION_REQUIRED", "fail"),
            ("CANCELLED", "cancel"),
            // Everything else is pending, per gh's `default:` arm.
            ("EXPECTED", "pending"),
            ("REQUESTED", "pending"),
            ("WAITING", "pending"),
            ("QUEUED", "pending"),
            ("PENDING", "pending"),
            ("IN_PROGRESS", "pending"),
            ("STALE", "pending"),
            ("STARTUP_FAILURE", "pending"),
            ("", "pending"),
            ("SOMETHING_NEW", "pending"),
        ] {
            assert_eq!(gh_bucket(state), expected, "state {state}");
        }
    }

    // --- pr-checks classification, including known-N/A SKIP semantics -------

    #[test]
    fn known_disabled_skipping_job_is_not_applicable() {
        let item = classify_check(&json!({
            "__typename": "CheckRun",
            "name": "Docs Consistency",
            "status": "COMPLETED",
            "conclusion": "SKIPPED",
        }))
        .expect("well-formed CheckRun");
        assert_eq!(item.bucket, "skipping");
        assert_eq!(item.classification, "not-applicable");
    }

    #[test]
    fn known_disabled_failing_job_is_a_failure_never_a_pass() {
        // A state-blind exemption would launder real failures (pr-checks:
        // "the not-applicable exemption requires BOTH the known-disabled
        // name AND the skipped state").
        for conclusion in ["FAILURE", "ERROR", "TIMED_OUT", "ACTION_REQUIRED"] {
            let item = classify_check(&json!({
                "__typename": "CheckRun",
                "name": "Documentation",
                "status": "COMPLETED",
                "conclusion": conclusion,
            }))
            .expect("well-formed CheckRun");
            assert_eq!(item.bucket, "fail", "conclusion {conclusion}");
            assert_eq!(item.classification, "fail", "conclusion {conclusion}");
        }
    }

    #[test]
    fn pending_status_context_classifies_pending() {
        // StatusContext state PENDING → bucket pending (gh's default arm).
        let item = classify_check(&json!({
            "__typename": "StatusContext",
            "context": "a status",
            "state": "PENDING",
        }))
        .expect("well-formed StatusContext");
        assert_eq!(item.bucket, "pending");
        assert_eq!(item.classification, "pending");
    }

    #[test]
    fn cancel_bucket_classifies_as_fail_like_the_script() {
        let item = classify_check(&json!({
            "__typename": "CheckRun",
            "name": "cool tests",
            "status": "COMPLETED",
            "conclusion": "CANCELLED",
        }))
        .expect("well-formed CheckRun");
        assert_eq!(item.bucket, "cancel");
        assert_eq!(item.classification, "fail");
    }

    #[test]
    fn status_context_uses_state_and_target_url() {
        let item = classify_check(&json!({
            "__typename": "StatusContext",
            "context": "a status",
            "state": "FAILURE",
            "targetUrl": "sweet link",
        }))
        .expect("well-formed StatusContext");
        assert_eq!(item.name, "a status");
        assert_eq!(item.state, "FAILURE");
        assert_eq!(item.bucket, "fail");
        assert_eq!(item.link, "sweet link");
        assert_eq!(item.workflow, "");
    }

    #[test]
    fn check_run_state_is_conclusion_when_completed_else_status() {
        let completed = classify_check(&json!({
            "__typename": "CheckRun",
            "name": "cool tests",
            "status": "COMPLETED",
            "conclusion": "SUCCESS",
            "detailsUrl": "sweet link",
            "checkSuite": {"workflowRun": {"workflow": {"name": "code.yml"}}},
        }))
        .expect("completed run");
        assert_eq!(completed.state, "SUCCESS");
        assert_eq!(completed.workflow, "code.yml");
        let in_progress = classify_check(&json!({
            "__typename": "CheckRun",
            "name": "slow tests",
            "status": "IN_PROGRESS",
            "conclusion": "",
        }))
        .expect("in-progress run");
        assert_eq!(in_progress.state, "IN_PROGRESS");
        assert_eq!(in_progress.bucket, "pending");
    }

    #[test]
    fn missing_check_name_is_a_typed_error() {
        let error = classify_check(&json!({"__typename": "CheckRun", "status": "COMPLETED"}))
            .expect_err("nameless check is an error");
        assert!(error.contains("name/context"), "{error}");
    }

    // --- aggregate flags over the classified array ---------------------------

    #[test]
    fn all_passed_excludes_not_applicable_and_requires_at_least_one_check() {
        // Calls the PRODUCTION reducer (pr-checks' aggregate flags).
        let passing = |name: &str, classification: &str| CheckItem {
            name: name.to_string(),
            state: "SUCCESS".to_string(),
            bucket: "pass".to_string(),
            classification: classification.to_string(),
            workflow: String::new(),
            link: String::new(),
        };
        // Empty checks array: all_passed is FALSE (the script's length > 0).
        assert!(!aggregate_checks(Vec::new()).all_passed);
        let with_na = aggregate_checks(vec![
            passing("cool tests", "pass"),
            passing("Docs Consistency", "not-applicable"),
        ]);
        assert!(with_na.all_passed);
        assert!(!with_na.any_failing);
        // A genuinely skipped-but-optioned check is NOT known-disabled and
        // still fails the gate (all_passed == false).
        let skipped = aggregate_checks(vec![passing("rad tests", "skip")]);
        assert!(!skipped.all_passed);
        assert!(!skipped.any_failing, "a skip is not a failure");
    }

    // --- checklist parsing (reconcile-checklist semantics) -------------------

    #[test]
    fn checklist_parses_markers_checked_and_descriptions() {
        let checklist = parse_checklist(
            "- [x] <!-- orc:convergence --> evidence recorded\n\
             - [ ] <!-- orc:docs --> docs updated\n\
             - plain bullet\n\
             - [x] <!-- orc:weird-marker_1 --> trailing\n",
        );
        assert_eq!(checklist.items.len(), 3);
        assert!(checklist.items[0].checked);
        assert_eq!(checklist.items[0].marker, "convergence");
        assert_eq!(checklist.items[0].description, "evidence recorded");
        assert!(!checklist.items[1].checked);
        assert_eq!(checklist.items[1].marker, "docs");
        assert!(!checklist.all_checked);
        assert_eq!(checklist.unchecked_count, 1);
        assert_eq!(checklist.items[2].marker, "weird-marker_1");
    }

    #[test]
    fn empty_body_is_a_vacuously_checked_empty_checklist() {
        let checklist = parse_checklist("Just a custom PR body with no markers.");
        assert!(checklist.items.is_empty());
        assert!(checklist.all_checked);
        assert_eq!(checklist.unchecked_count, 0);
    }

    #[test]
    fn checklist_rejects_malformed_markers() {
        // Not an orc marker, wrong charset, missing the box: none are items.
        let checklist = parse_checklist(
            "- [x] <!-- other:foo --> nope\n\
             - [x] <!-- orc:bad marker! --> charset\n\
             - [x] plain text\n\
             - [q] <!-- orc:nope --> invalid box char\n",
        );
        assert!(checklist.items.is_empty(), "{:?}", checklist.items);
    }

    // --- review record (convergence-status record semantics) ----------------

    fn write_record(
        dir: &std::path::Path,
        pr: u64,
        head7: &str,
        verdict: &str,
        head: &str,
        base: &str,
    ) {
        let records = dir.join(".orchestraitor").join("reviews");
        std::fs::create_dir_all(&records).expect("records dir");
        std::fs::write(
            records.join(format!("{pr}-{head7}.md")),
            format!(
                "# Agent review record: PR #{pr} @ {head7}\n\n\
                 - verdict: {verdict}\n\
                 - reviewer-agent: test\n\
                 - head SHA: {head}\n\
                 - base OID: {base}\n\
                 - findings: 0\n\nNo findings.\n"
            ),
        )
        .expect("record file");
    }

    const HEAD: &str = "cb7cf776939f65981f9fb7c9e5263ba0b20b7c7c";
    const BASE: &str = "9cc67bf0db85a7248a9b9718974f4460d5fb3759";

    #[test]
    fn clean_record_matching_both_anchors_converges() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_record(temp.path(), 614, &HEAD[..7], "CLEAN", HEAD, BASE);
        let record = read_review_record(temp.path(), 614, HEAD, BASE);
        assert!(record.present);
        assert_eq!(record.verdict, "CLEAN");
        assert!(record.head_sha_match);
        assert!(record.base_oid_match);
    }

    #[test]
    fn base_advance_makes_the_record_stale_not_clean() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_record(temp.path(), 614, &HEAD[..7], "CLEAN", HEAD, BASE);
        let advanced_base = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let record = read_review_record(temp.path(), 614, HEAD, advanced_base);
        assert!(record.present);
        assert_eq!(record.verdict, "CLEAN");
        assert!(!record.base_oid_match);
        // The verdict alone is not convergence: the caller requires BOTH
        // matches, so a stale record blocks (the script's && chain).
        assert!(
            !(record.present
                && record.verdict == "CLEAN"
                && record.head_sha_match
                && record.base_oid_match)
        );
    }

    #[test]
    fn findings_record_is_present_but_not_clean() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_record(temp.path(), 614, &HEAD[..7], "FINDINGS", HEAD, BASE);
        let record = read_review_record(temp.path(), 614, HEAD, BASE);
        assert!(record.present);
        assert_eq!(record.verdict, "FINDINGS");
    }

    #[test]
    fn abbreviated_oid_refuses_to_read_a_record() {
        // The script guards `^[0-9a-f]{40}$`; a truncated OID means no
        // record — never a guess.
        let temp = tempfile::tempdir().expect("tempdir");
        write_record(temp.path(), 614, &HEAD[..7], "CLEAN", HEAD, BASE);
        let record = read_review_record(temp.path(), 614, &HEAD[..20], BASE);
        assert!(!record.present);
        assert!(!record.head_sha_match);
    }

    #[test]
    fn missing_record_file_is_absent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = read_review_record(temp.path(), 999, HEAD, BASE);
        assert!(!record.present);
        assert_eq!(record.verdict, "");
    }

    #[test]
    fn mergeable_requires_zero_reasons() {
        let verdict = PrStatus {
            number: 1,
            repository: "o/r".to_string(),
            head_sha: HEAD.to_string(),
            base_oid: BASE.to_string(),
            is_draft: false,
            review_decision: "APPROVED".to_string(),
            merge_state: "CLEAN".to_string(),
            checks: None,
            checks_all_passed: None,
            checks_any_failing: None,
            checks_any_pending: None,
            threads: None,
            checklist: None,
            review_record: ReviewRecord {
                present: true,
                verdict: "CLEAN".to_string(),
                head_sha_match: true,
                base_oid_match: true,
            },
            convergence: Convergence {
                mergeable: true,
                reasons: Vec::new(),
            },
            fetched_at: now_rfc3339(),
        };
        assert!(verdict.convergence.mergeable);
        assert!(verdict.convergence.reasons.is_empty());
        // JSON round-trip carries the §9.48 field names.
        let json = serde_json::to_value(&verdict).expect("serializable");
        for key in [
            "number",
            "repository",
            "head_sha",
            "base_oid",
            "is_draft",
            "review_decision",
            "merge_state",
            "review_record",
            "convergence",
            "fetched_at",
        ] {
            assert!(json.get(key).is_some(), "missing {key}");
        }
        // Optional sub-check fields stay absent (not null) when unread.
        assert!(json.get("checks").is_none());
    }
}
