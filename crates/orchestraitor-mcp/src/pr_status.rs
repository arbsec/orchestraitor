//! The shared §9.48 PR-status aggregation engine — the typed verdict and
//! pure classification semantics behind `orc pr status` and the daemon's
//! `pr.status` gateway tool (spec `10-orchestrator.md` §9.39, §9.48;
//! issue #614 slice 2). One engine, three surfaces: the skill scripts (the
//! normative shell surface), `orc pr status` (the CLI), and the `pr.status`
//! MCP tool — the same vocabulary, never a second interpretation.
//!
//! One read-only pass over the SAME facts the pr-lifecycle skill scripts
//! read (`pr-checks`, `review-threads`, `reconcile-checklist`,
//! `convergence-status`, `merge-gate`): one PR view, one checks rollup, one
//! `reviewThreads` GraphQL query, one body fetch, one CLEAN-record file
//! read. The scripts stay the normative definition of every classification
//! rule; this engine consumes their semantics VERBATIM.
//!
//! Read-only: the tool never labels, comments, reviews, or merges.
//!
//! Transport: the engine is transport-agnostic ([`PrStatusTransport`]) and
//! BLOCKING — callers on async runtimes wrap [`Engine::aggregate`] in
//! `tokio::task::spawn_blocking`.

use serde::Serialize;
use serde_json::Value;

/// Typed PR-status failures (spec §9.48 exit-code contract, one variant per
/// exit code): `0` mergeable (BLOCKED is a successful read, not an error);
/// `1` [`PrStatusError::Unrecoverable`] (transport, malformed provider
/// output); `2` [`PrStatusError::Config`] (PR not found, missing project
/// config, service identity unresolved); `3` reserved — never used by this
/// read-only tool; `4` [`PrStatusError::ConcurrentEdit`] (an anchor moved
/// between the pre- and post-reads). The CLI binary maps the variants onto
/// process exit codes; the gateway maps them onto typed structured payloads.
#[derive(Debug, thiserror::Error)]
pub enum PrStatusError {
    /// Unrecoverable failure (`pr status` exit 1): transport, HTTP, or
    /// malformed provider output.
    #[error("pr status: {message}")]
    Unrecoverable {
        /// Typed message naming the failed sub-read; never credential
        /// material.
        message: String,
        /// The typed JSON emitted before this error surfaced (empty when no
        /// partial verdict exists).
        partial_json: String,
    },
    /// Config/state failure (`pr status` exit 2): PR not found, missing
    /// project config, or the service identity is unresolved.
    #[error("pr status: {message}")]
    Config {
        /// Typed message naming the missing/broken configuration.
        message: String,
    },
    /// Concurrent-edit failure (`pr status` exit 4): a convergence anchor
    /// moved between the pre-read and the post-read.
    #[error(
        "pr status: concurrent edit — {anchor} moved between the pre- and post-reads \
         ({before} -> {after}); re-read and retry"
    )]
    ConcurrentEdit {
        /// Which anchor moved: `head` or `base`.
        anchor: &'static str,
        /// The OID observed before the sub-reads.
        before: String,
        /// The OID observed after the sub-reads.
        after: String,
    },
}

/// Executes one GraphQL POST; returns the `data` object plus whether the
/// envelope carried partial-error entries (data present, errors alongside —
/// e.g. FORBIDDEN on a sub-field). Errors from transport/HTTP/malformed
/// bodies are typed [`PrStatusError`]s; config/state conditions (`NOT_FOUND`)
/// surface as [`PrStatusError::Config`].
///
/// Implementations hold the credential; the engine never sees it.
pub trait PrStatusTransport: Send + Sync {
    /// Executes one GraphQL operation.
    ///
    /// # Errors
    /// Returns a typed [`PrStatusError`] on transport/HTTP/malformed-body
    /// failures and on config/state conditions (a NOT_FOUND-typed error
    /// entry is ALWAYS [`PrStatusError::Config`]).
    fn graphql(
        &self,
        operation: &str,
        query: &str,
        variables: &serde_json::Value,
    ) -> Result<(serde_json::Value, bool), PrStatusError>;
}

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

/// The typed §9.48 PR-status verdict. Field names match the spec schema
/// verbatim; the JSON form is the machine contract.
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Checklist {
    /// Items with their orc markers.
    pub items: Vec<ChecklistItem>,
    /// Vacuously true when no markers exist.
    pub all_checked: bool,
    /// Count of unchecked items.
    pub unchecked_count: u64,
}

/// One managed checklist item.
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ChecklistItem {
    /// The orc marker id (the `orc:XXX` in `<!-- orc:XXX -->`).
    pub marker: String,
    /// Whether the box is checked (`x`).
    pub checked: bool,
    /// Description text after the marker.
    pub description: String,
}

/// The CLEAN-record state (convergence-status semantics, verbatim).
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
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
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Convergence {
    /// Whether every gate passes.
    pub mergeable: bool,
    /// Stable machine reason codes, extended only additively.
    pub reasons: Vec<String>,
}

/// The aggregation engine: one transport, one set of sub-reads, one verdict.
/// Transport-agnostic and BLOCKING — async callers wrap [`aggregate`] in
/// `tokio::task::spawn_blocking`.
pub struct Engine {
    /// The GraphQL transport (endpoint + credential owner).
    transport: std::sync::Arc<dyn PrStatusTransport>,
    /// The CLEAN-record root (the caller supplies it: the CLI passes its
    /// checkout toplevel; the daemon records its own workspace root).
    repo_root: std::path::PathBuf,
}

impl Engine {
    /// Builds an engine over a transport and a CLEAN-record root.
    pub fn new(
        transport: std::sync::Arc<dyn PrStatusTransport>,
        repo_root: std::path::PathBuf,
    ) -> Self {
        Self {
            transport,
            repo_root,
        }
    }

    /// Aggregates the full §9.48 verdict for one PR.
    ///
    /// # Errors
    /// Returns [`PrStatusError::Unrecoverable`] when an unread sub-check
    /// degrades the verdict (the partial typed JSON rides the error),
    /// [`PrStatusError::Config`] when the PR is unreadable/not found, and
    /// [`PrStatusError::ConcurrentEdit`] when an anchor moved between the
    /// pre- and post-reads.
    pub fn aggregate(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<PrStatus, PrStatusError> {
        self.aggregate_with_root(owner, repo, number, &self.repo_root)
    }

    /// Aggregates the §9.48 verdict for one PR against an explicit
    /// CLEAN-record root.
    ///
    /// Snapshot discipline (spec §9.48): capture head SHA and base OID
    /// BEFORE the sub-reads, re-read BOTH after them, and refuse (exit 4)
    /// on any anchor movement — never a verdict from a mixed snapshot.
    fn aggregate_with_root(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        repo_root: &std::path::Path,
    ) -> Result<PrStatus, PrStatusError> {
        // 0b. The PR number rides a GraphQL Int — reject anything beyond
        // i32::MAX as a typed config error BEFORE any network call (a
        // number the query cannot carry would fail mid-read, not at the
        // boundary).
        if number > i32::MAX as u64 {
            return Err(PrStatusError::Config {
                message: format!("PR #{number} is outside the GraphQL Int range the query carries"),
            });
        }
        // 1. The pre-read PR view: identity fields + the two anchors.
        let (initial, view_had_errors) = self.pr_view(owner, repo, number)?;
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
            view_had_errors,
        )
    }

    /// Re-reads BOTH convergence anchors from the provider and refuses (exit
    /// 4, a typed [`PrStatusError::ConcurrentEdit`] naming the moved anchor)
    /// on any movement — never a verdict assembled from a mixed snapshot.
    fn check_anchors_stable(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
        head_sha: &str,
        base_oid: &str,
    ) -> Result<(), PrStatusError> {
        let (final_view, _post_errors) = self.pr_view(owner, repo, number)?;
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
        Err(PrStatusError::ConcurrentEdit {
            anchor,
            before,
            after,
        })
    }

    /// The PR view: number, headRefOid, baseRefOid, isDraft, reviewDecision,
    /// mergeStateStatus — the fields pr-state and convergence-status read.
    fn pr_view(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<(Value, bool), PrStatusError> {
        let query = "query($owner:String!,$name:String!,$number:Int!){\
repository(owner:$owner,name:$name){\
pullRequest(number:$number){number state headRefOid baseRefOid isDraft reviewDecision mergeStateStatus}}}\
";
        let (data, had_errors) = self.graphql(
            "pr view",
            query,
            &serde_json::json!({"owner": owner, "name": repo, "number": number}),
        )?;
        let pr = data
            .pointer("/repository/pullRequest")
            .filter(|pr| !pr.is_null());
        let Some(pr) = pr else {
            // A null pullRequest WITHOUT errors is the provider's
            // not-found shape (config/state, exit 2); WITH partial errors
            // it is an unread PR view (unrecoverable, exit 1) — the field
            // error may be scope/transient, never laundered into
            // not-found.
            return Err(if had_errors {
                PrStatusError::Unrecoverable {
                    message: format!(
                        "pr view is unreadable for PR #{number} in {owner}/{repo} (partial \
                         errors on the response)"
                    ),
                    partial_json: String::new(),
                }
            } else {
                PrStatusError::Config {
                    message: format!("PR #{number} not found in {owner}/{repo}"),
                }
            });
        };
        // Whether the response carried partial-error entries: a null field
        // WITH errors is an unread sub-field; a null field WITHOUT errors is
        // a legitimate provider state (e.g. no reviews yet).
        Ok((pr.clone(), had_errors))
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
        // Two query shapes: the full one requests `WorkflowRun.event` (which
        // gh gates behind feature detection — some GHES versions reject the
        // field); the fallback omits it and dedupes on (name, workflow).
        let query_with_event = "query($owner:String!,$name:String!,$oid:GitObjectID!){\
repository(owner:$owner,name:$name){\
object(oid:$oid){... on Commit{statusCheckRollup{contexts(first:100){pageInfo{hasNextPage}nodes{\
__typename\
... on StatusContext{context state targetUrl createdAt}\
... on CheckRun{name checkSuite{workflowRun{event workflow{name}}} status conclusion startedAt completedAt detailsUrl}\
}}}}}}}";
        let query_without_event = "query($owner:String!,$name:String!,$oid:GitObjectID!){\
repository(owner:$owner,name:$name){\
object(oid:$oid){... on Commit{statusCheckRollup{contexts(first:100){pageInfo{hasNextPage}nodes{\
__typename\
... on StatusContext{context state targetUrl createdAt}\
... on CheckRun{name checkSuite{workflowRun{workflow{name}}} status conclusion startedAt completedAt detailsUrl}\
}}}}}}}";
        // Try the event-including shape first; a query-level rejection
        // (unsupported field on older GHES) falls back to the event-less
        // shape once — the same includeEvent detection gh applies.
        let (data, had_errors) = match self.graphql(
            "checks",
            query_with_event,
            &serde_json::json!({"owner": owner, "name": repo, "oid": head_oid}),
        ) {
            Ok(pair) => pair,
            Err(_) => self
                .graphql(
                    "checks",
                    query_without_event,
                    &serde_json::json!({"owner": owner, "name": repo, "oid": head_oid}),
                )
                .map_err(|error| error.to_string())?,
        };
        // A head commit with NO status contexts yet (fresh push, repo without
        // CI) reports `statusCheckRollup: null` — a valid provider state, not
        // a failure: aggregate over an EMPTY checks array (all_passed is then
        // false per pr-checks' `length > 0` rule → `checks_not_passing`,
        // exit 0 BLOCKED), never `unread(checks)`. A missing `object` (no
        // such commit) or a wrong shape stays unread.
        let rollup = data.pointer("/repository/object/statusCheckRollup");
        let nodes: Vec<Value> = match rollup {
            // A null rollup WITH partial errors is an UNREAD checks read
            // (the error may have suppressed the rollup) — fail closed;
            // WITHOUT errors it is the valid "no checks yet" state.
            Some(rollup) if rollup.is_null() && had_errors => {
                return Err(
                    "checks rollup is unreadable (partial errors on the response)".to_string(),
                );
            }
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
        // A missing/malformed pageInfo (when a rollup OBJECT exists) is
        // MALFORMED provider output (a first-page-only response could hide a
        // failing check): fail closed. A null rollup carries no pageInfo —
        // that is the valid "no checks yet" state.
        let has_next = if rollup.is_some_and(Value::is_null) {
            false
        } else {
            data.pointer("/repository/object/statusCheckRollup/contexts/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                .ok_or_else(|| {
                    "checks rollup response is missing pagination metadata".to_string()
                })?
        };
        if has_next {
            // >100 contexts: the first page's aggregate would silently lie.
            return Err(
                "checks rollup exceeds one page (100 contexts); not aggregated".to_string(),
            );
        }
        let deduped = dedupe_check_nodes(&nodes);
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
        let (data, _had_errors) = self
            .graphql(
                "review threads",
                query,
                &serde_json::json!({"owner": owner, "name": repo, "number": number}),
            )
            .map_err(|error| error.to_string())?;
        let nodes = data
            .pointer("/repository/pullRequest/reviewThreads/nodes")
            .and_then(Value::as_array)
            .ok_or_else(|| "review threads response is missing nodes".to_string())?;
        // Same fail-closed rule as the checks rollup.
        let has_next = data
            .pointer("/repository/pullRequest/reviewThreads/pageInfo/hasNextPage")
            .and_then(Value::as_bool)
            .ok_or_else(|| "review threads response is missing pagination metadata".to_string())?;
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
        let (data, _had_errors) = self
            .graphql(
                "pr body",
                query,
                &serde_json::json!({"owner": owner, "name": repo, "number": number}),
            )
            .map_err(|error| error.to_string())?;
        let body = data
            .pointer("/repository/pullRequest/body")
            .and_then(Value::as_str)
            .ok_or_else(|| "pr body response is missing the body string".to_string())?;
        Ok(parse_checklist(body))
    }

    /// Executes one GraphQL operation through the transport.
    fn graphql(
        &self,
        operation: &str,
        query: &str,
        variables: &Value,
    ) -> Result<(Value, bool), PrStatusError> {
        self.transport.graphql(operation, query, variables)
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

/// Applies the review-state gates (`reviewDecision` unreadability,
/// `is_draft`, `pr_not_open`, `CHANGES_REQUESTED`) to the reasons.
fn apply_review_state_gates(
    initial: &Value,
    reasons: &mut Vec<String>,
    unread_subcheck: &mut Option<&'static str>,
    view_had_errors: bool,
) -> bool {
    // An unreadable (null/absent) reviewDecision is an unread sub-field —
    // fail closed (a partial field error must never masquerade as "no
    // review requirement").
    match initial.get("reviewDecision").and_then(Value::as_str) {
        Some("REVIEW_REQUIRED") => reasons.push("review_required".to_string()),
        // A decision is present: no additional reason.
        Some(decision) if !decision.is_empty() => {}
        // A null/empty decision WITH partial errors is an unread sub-field:
        // record the stable reason (forcing mergeable false) AND flag the
        // exit-1 path.
        None | Some("") if view_had_errors => {
            reasons.push("unread(review-decision)".to_string());
            *unread_subcheck = Some("review-decision");
        }
        // A null/empty decision WITHOUT errors is the legitimate "no review
        // required" provider state — no reason, never unread.
        _ => {}
    }
    let is_draft = initial.get("isDraft").and_then(Value::as_bool) == Some(true);
    if is_draft {
        reasons.push("is_draft".to_string());
    }
    // Lifecycle state: a PR that is not OPEN cannot merge, whatever the
    // other gates say (a closed PR's head/base/checks can be unchanged).
    if initial.get("state").and_then(Value::as_str) != Some("OPEN") {
        reasons.push("pr_not_open".to_string());
    }
    // A blocking requested-changes review prevents merging under the branch
    // rules even when every other gate is green.
    if initial.get("reviewDecision").and_then(Value::as_str) == Some("CHANGES_REQUESTED") {
        reasons.push("review_changes_requested".to_string());
    }
    is_draft
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
    view_had_errors: bool,
) -> Result<PrStatus, PrStatusError> {
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
    let is_draft =
        apply_review_state_gates(initial, &mut reasons, &mut unread_subcheck, view_had_errors);

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
        return Err(PrStatusError::Unrecoverable {
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
        });
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

/// gh's `eliminateDuplicates`, verbatim: when a check re-ran, keep ONLY the
/// most recent run per (context) or (name, workflow, event) key (sort by
/// startedAt descending, first seen wins). Without this, an old failed run
/// next to a new passing one would fail the gate forever. Absent startedAt
/// sorts last (oldest), matching an empty instant.
fn dedupe_check_nodes(nodes: &[Value]) -> Vec<&Value> {
    // gh sorts by the run's start instant: CheckRun `startedAt` for check
    // runs, StatusContext `createdAt` for status contexts (an absent value
    // sorts last, matching an empty instant).
    let started_at = |node: &Value| -> String {
        node.get("startedAt")
            .or_else(|| node.get("createdAt"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut sorted: Vec<&Value> = nodes.iter().collect();
    sorted.sort_by_key(|node| std::cmp::Reverse(started_at(node)));
    let mut deduped: Vec<&Value> = Vec::new();
    let mut seen_contexts: Vec<String> = Vec::new();
    let mut seen_checks: Vec<String> = Vec::new();
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
                // gh keys on (name, workflow, event) — include the event so
                // differently-triggered runs of the same job stay distinct,
                // exactly as eliminateDuplicates does.
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
    deduped
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

/// Reads a full-OID field from the PR view; empty/missing values are typed
/// errors (a missing anchor can never pass the snapshot check).
fn oid_field(view: &Value, key: &str, anchor: &'static str) -> Result<String, PrStatusError> {
    let value = view
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PrStatusError::Unrecoverable {
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
