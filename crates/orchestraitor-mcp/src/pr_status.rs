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
/// disabled or event-gated repo-side, so they report the skipped state and can
/// never pass. A known-disabled job exempts from the all-passed gate ONLY while
/// its bucket is `skipping` — the same job actually failing is a failure, never
/// a pass. Re-enabling a job on PRs means removing its name here AND in the
/// script, in the same change (one vocabulary): a re-enabled job that still
/// reports `skipping` must never be silently exempted.
///
/// `Docs Consistency` was removed when `markdown.yml` re-enabled it
/// (arbsec/orchestraitor#741): it now runs `xtask docs-check` on every
/// markdown/Rust/YAML/TOML pull request, so a skip is no longer tolerated.
/// `Documentation` stays — `code.yml` gates it on
/// `github.event_name == 'push'`, so it never runs on a pull request.
const KNOWN_DISABLED_JOBS: [&str; 1] = ["Documentation"];

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
    /// pr-checks' `skip_reason`: `known-disabled-job` or `bot-author` when the
    /// check is structurally unavailable (classified `not-applicable`), absent
    /// otherwise — a skip is never widened silently.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
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

/// The nine MANAGED marker names.
///
/// A managed marker has ONE canonical form and its `<NAME>` is one of these.
/// The list is the SINGLE source of truth for the shell engine too (the
/// `_lib.sh` fallback, which the drift guard in `test-reconcile-checklist`
/// asserts equals `.github/PULL_REQUEST_TEMPLATE.md` verbatim); the Rust copy
/// is guarded by [`managed_marker_names_match_the_template`], so the two cannot
/// drift.
const MANAGED_MARKER_NAMES: [&str; 9] = [
    "issue",
    "service-identity",
    "tests",
    "security",
    "docs",
    "rollback",
    "checks",
    "review",
    "threads",
];

/// The construct that made a checklist reading INDETERMINATE.
///
/// The verdict reason is the same for all four (`checklist_unparseable`), but
/// the explanatory message names the construct, so the reader knows WHICH rule
/// refused the reading and what remedy applies.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum UnparseableConstruct {
    /// A fence OPENER indented 1-3 spaces: its container scope (top level, list
    /// item, block quote) cannot be determined without a container model
    /// (arbsec/orchestraitor#809). The remedy is to dedent the opener to
    /// column 0.
    IndentedFenceOpener,
    /// A RAW HTML CONSTRUCT: a line outside every fenced code region whose first
    /// non-whitespace character is `<`, indented 0-3 spaces (arbsec/orchestraitor#799
    /// R1). The scanner does NOT model HTML — it cannot tell what such a
    /// construct hides — so the reading is REFUSED rather than guessed, which is
    /// deliberately stricter than `CommonMark`; arbsec/orchestraitor#809 tracks
    /// real modelling. The remedies are: put the line inside a fenced code
    /// block, indent it by 4 spaces (literal indented code), or remove it.
    HtmlConstruct,
    /// A NEAR-MARKER: a line that presents a managed marker — a task-item box
    /// token plus an `orc:NAME` reference — but not in the canonical form, so
    /// the gate cannot tell what the author asserted (arbsec/orchestraitor#799
    /// R1). The remedy is to write the marker canonically.
    NearMarker,
    /// A MANAGED MARKER indented FOUR OR MORE spaces, or one led by a TAB
    /// (arbsec/orchestraitor#802). When such a line is not inside a list item it
    /// renders as an INDENTED CODE BLOCK, where it is not a checkbox at all, and
    /// neither the canonical grammar (its leading `[ \t]*` class) nor this
    /// scanner has a container model to tell that apart from a marker nested in
    /// a list item. The reading is therefore REFUSED rather than guessed. The
    /// remedy is to dedent the marker line to column 0 (0-3 leading spaces stays
    /// accepted).
    IndentedMarker,
}

/// Checklist reconciliation (reconcile-checklist semantics, verbatim).
#[derive(Debug, Clone, Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Checklist {
    /// Items with their orc markers.
    pub items: Vec<ChecklistItem>,
    /// Number of managed markers parsed from the body.
    pub marker_count: u64,
    /// Whether the body carried at least one managed marker.
    pub markers_present: bool,
    /// True only when at least one marker exists AND every marker is checked.
    /// A marker-less body is MISSING evidence, never "all checked" (arbsec/orchestraitor#788),
    /// and a body whose checklist reading is indeterminate is never "all checked"
    /// either (arbsec/orchestraitor#809, arbsec/orchestraitor#799 R1).
    pub all_checked: bool,
    /// Count of unchecked items.
    pub unchecked_count: u64,
    /// True when the body carries a RAW HTML CONSTRUCT (a line-start `<`
    /// outside every fenced region), opens a fenced code block indented 1-3
    /// spaces, carries a NEAR-MARKER, or carries a MANAGED MARKER indented 4+
    /// spaces (or led by a tab): the reading is indeterminate — the container
    /// scope of the opener cannot be determined without a container model, HTML
    /// is not modelled at all, a near-marker asserts something the canonical
    /// grammar cannot read, and an indented marker is a code block rather than a
    /// checkbox whenever it is not inside a list item — so NO checklist reading
    /// from this body is trusted and the verdict blocks with
    /// `checklist_unparseable` (arbsec/orchestraitor#809,
    /// arbsec/orchestraitor#799 R1, arbsec/orchestraitor#802). STICKY: a closer
    /// does not clear it.
    pub unparseable: bool,
    /// 1-based line number of the first such construct, or `None`.
    pub unparseable_line: Option<u64>,
    /// WHICH construct made the reading indeterminate, or `None`.
    pub unparseable_construct: Option<UnparseableConstruct>,
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
        // The PR author discriminates the structural bot-author CodeRabbit
        // skip (pr-checks' `bot_author_pr`); an unread/absent author is ""
        // and never treated as a bot.
        let pr_author = gh_author_login(&initial);
        let checks = self.read_checks(owner, repo, &head_sha, &pr_author);
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
    /// mergeStateStatus — the fields pr-state and convergence-status read —
    /// plus the author login (the discriminator for the structural
    /// bot-author `CodeRabbit` skip).
    fn pr_view(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<(Value, bool), PrStatusError> {
        let query = "query($owner:String!,$name:String!,$number:Int!){\
repository(owner:$owner,name:$name){\
pullRequest(number:$number){number state headRefOid baseRefOid isDraft reviewDecision mergeStateStatus author{__typename login}}}}\
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
        pr_author: &str,
    ) -> Result<ChecksAggregate, String> {
        // Two query shapes: the full one requests `WorkflowRun.event` (which
        // gh gates behind feature detection — some GHES versions reject the
        // field); the fallback omits it and dedupes on (name, workflow).
        let query_with_event = "query($owner:String!,$name:String!,$oid:GitObjectID!){\
repository(owner:$owner,name:$name){\
object(oid:$oid){... on Commit{statusCheckRollup{contexts(first:100){pageInfo{hasNextPage}nodes{\
__typename\
... on StatusContext{context state targetUrl createdAt}\
... on CheckRun{name title checkSuite{workflowRun{event workflow{name}}} status conclusion startedAt completedAt detailsUrl}\
}}}}}}}";
        let query_without_event = "query($owner:String!,$name:String!,$oid:GitObjectID!){\
repository(owner:$owner,name:$name){\
object(oid:$oid){... on Commit{statusCheckRollup{contexts(first:100){pageInfo{hasNextPage}nodes{\
__typename\
... on StatusContext{context state targetUrl createdAt}\
... on CheckRun{name title checkSuite{workflowRun{workflow{name}}} status conclusion startedAt completedAt detailsUrl}\
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
            items.push(classify_check(node, pr_author)?);
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
    unread_subchecks: &mut Vec<&'static str>,
    view_had_errors: bool,
) {
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
            unread_subchecks.push("review-decision");
        }
        // A null/empty decision WITHOUT errors is the legitimate "no review
        // required" provider state — no reason, never unread.
        _ => {}
    }
    // An unreadable (null/absent) isDraft WITH partial errors is an unread
    // sub-field — fail closed (a partial field error must never silently
    // downgrade a draft to non-draft). Without errors, absent/null is the
    // provider's false shape.
    match initial.get("isDraft").and_then(Value::as_bool) {
        Some(is_draft) => {
            if is_draft {
                reasons.push("is_draft".to_string());
            }
        }
        None if view_had_errors => {
            reasons.push("unread(is-draft)".to_string());
            unread_subchecks.push("is-draft");
        }
        None => {}
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
    let mut unread_subchecks: Vec<&'static str> = Vec::new();
    let mut unread_details: Vec<String> = Vec::new();
    let checks = sub_read(
        checks,
        "checks",
        &mut reasons,
        &mut unread_subchecks,
        &mut unread_details,
    );
    let threads = sub_read(
        threads,
        "threads",
        &mut reasons,
        &mut unread_subchecks,
        &mut unread_details,
    );
    let checklist = sub_read(
        checklist,
        "body",
        &mut reasons,
        &mut unread_subchecks,
        &mut unread_details,
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
    if let Some(checklist) = &checklist {
        // Three DISTINCT checklist failures (arbsec/orchestraitor#788, #809): a
        // body with no managed markers is missing evidence, a marker-bearing
        // body with unticked boxes is the item-count reason, and a body whose
        // fenced scope is INDETERMINATE (a fence opener indented 1-3 spaces) is
        // unreadable — the scanner cannot say what is code, so it refuses
        // rather than guessing. There is NO author-granted opt-out: a
        // marker-less body always blocks, because the author cannot grant
        // themselves the exemption (an exemption needs an authority they lack —
        // arbsec/orchestraitor#801).
        if checklist.unparseable {
            reasons.push("checklist_unparseable".to_string());
        } else if !checklist.markers_present {
            reasons.push("checklist_markers_missing".to_string());
        } else if !checklist.all_checked {
            reasons.push(format!("unchecked_items({})", checklist.unchecked_count));
        }
    }
    apply_review_state_gates(
        initial,
        &mut reasons,
        &mut unread_subchecks,
        view_had_errors,
    );
    // is_draft for the verdict struct: true only when explicitly reported
    // true (an unread isDraft rides the exit-1 path above, so the struct
    // value is never load-bearing for a mergeable verdict).
    let is_draft = initial.get("isDraft").and_then(Value::as_bool) == Some(true);

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
    if !unread_subchecks.is_empty() {
        // EVERY unread sub-check and its detail stays in the diagnostic —
        // the caller sees the full degraded surface, never just the first
        // failure.
        let details = if unread_details.is_empty() {
            String::new()
        } else {
            format!(" ({})", unread_details.join("; "))
        };
        return Err(PrStatusError::Unrecoverable {
            message: format!(
                "sub-checks [{}] could not be read{}; mergeable is reported false with \
                 unread(<sub-check>) reasons in the emitted JSON",
                unread_subchecks.join(", "),
                details
            ),
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
    unread_subchecks: &mut Vec<&'static str>,
    unread_details: &mut Vec<String>,
) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(message) => {
            unread_subchecks.push(name);
            reasons.push(format!("unread({name})"));
            // The message may be a rendered typed error carrying its own
            // `pr status: ` prefix — strip it so the diagnostic never
            // repeats the prefix.
            let detail = message.trim_start_matches("pr status: ");
            unread_details.push(format!("{name}: {detail}"));
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
/// `pr_author` is the PR author login, read only to decide whether a
/// `CodeRabbit` skip is the structural bot-author no-op.
fn classify_check(node: &Value, pr_author: &str) -> Result<CheckItem, String> {
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
    // gh's `pr checks --json description` is the check run's `title`; it is
    // "" for the CodeRabbit bot-author no-op in practice, and the shell's
    // narrowing only refuses a non-empty description that does NOT name the
    // bot-user reason. Read it so the engine is never LOOSER than the script.
    let description = node
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // pr-checks' classification: a structural exemption requires BOTH the
    // exempting name/reason AND bucket == "skipping" — a state-blind
    // exemption would launder real failures. `cancel` classifies as fail.
    let skip_reason = skip_exemption(name, &bucket, pr_author, description);
    let classification = if skip_reason.is_some() {
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
        skip_reason: skip_reason.map(str::to_string),
        workflow,
        link: link.to_string(),
    })
}

/// pr-checks' `skip_reason` derivation: the structural exemptions that widen a
/// skipped bucket to `not-applicable`. `known-disabled-job` requires the name
/// AND the skipping bucket; `bot-author` additionally requires the PR to be
/// authored by a bot/App login and no contradicting check description. An
/// unknown/empty author or an unexplained skip yields `None` — fail closed,
/// the skip still blocks the gate.
fn skip_exemption(
    name: &str,
    bucket: &str,
    pr_author: &str,
    description: &str,
) -> Option<&'static str> {
    if KNOWN_DISABLED_JOBS.contains(&name) && bucket == "skipping" {
        return Some("known-disabled-job");
    }
    if name == "CodeRabbit"
        && bucket == "skipping"
        && is_bot_author_login(pr_author)
        && (description.is_empty() || is_bot_user_skip_description(description))
    {
        return Some("bot-author");
    }
    None
}

/// The PR author login in the shape `gh pr view --json author` reports it.
/// GitHub's GraphQL `Bot` actor is a GitHub App/bot identity: gh surfaces it
/// as `app/<login>` when the actor has no user id, and as the raw
/// `<slug>[bot]` login otherwise. Either shape satisfies
/// [`is_bot_author_login`], so every `Bot` actor is normalised to a shape the
/// script's predicate recognises. An absent/empty author is "" (fail closed).
fn gh_author_login(initial: &Value) -> String {
    let Some(author) = initial.get("author") else {
        return String::new();
    };
    let login = author
        .get("login")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let is_bot_actor = author.get("__typename").and_then(Value::as_str) == Some("Bot");
    if is_bot_actor && !login.is_empty() && !is_bot_author_login(login) {
        format!("app/{login}")
    } else {
        login.to_string()
    }
}

/// pr-checks' `bot_author_pr` predicate: a bot/App login either ends with
/// `[bot]` or starts with `app/`. An empty/unknown login is never a bot.
fn is_bot_author_login(login: &str) -> bool {
    login.ends_with("[bot]") || login.starts_with("app/")
}

/// pr-checks' `bot_author_description` narrowing, `(?i)^review skipped.*bot
/// user`, implemented without a regex dependency. A non-matching non-empty
/// description (rate limit, config error, out of credits) is NOT exempted.
fn is_bot_user_skip_description(description: &str) -> bool {
    let lowered = description.to_lowercase();
    lowered.starts_with("review skipped") && lowered.contains("bot user")
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
/// (reconcile-checklist's canonical form, verbatim):
/// `- [ ] <!-- orc:XXX --> description` / `- [x] <!-- orc:XXX --> description`.
///
/// A managed marker has ONE CANONICAL form:
///
/// ```text
/// [ \t]*- [BOX] <!-- orc:NAME --> <description>
/// ```
///
/// - the indent is SPACES and TABS and nothing else;
/// - every separator is EXACTLY ONE ASCII SPACE: between `-` and `[`, between
///   `]` and `<!--`, and inside `<!-- orc:NAME -->`;
/// - `<BOX>` is exactly one of `x`, `X` or a single SPACE;
/// - `<NAME>` is one of [`MANAGED_MARKER_NAMES`].
///
/// A line that is not canonical but carries BOTH a task-item BOX TOKEN (`[` +
/// exactly one character + `]`) AND a reference to one of the nine managed
/// names (`orc:NAME`) is a NEAR-MARKER: the author presented a managed marker
/// and the gate cannot tell what they asserted, so the reading is
/// INDETERMINATE — `unparseable` is true and the verdict blocks with
/// `checklist_unparseable` (arbsec/orchestraitor#799 R1). A line that only
/// mentions `orc:NAME` (prose) and a line that carries only a box token (a
/// plain task) are NOT near-markers and never block.
///
/// Markers inside a FENCED code region are EXAMPLES, not evidence: they are
/// excluded before the scan (see [`scan_fences`]).
///
/// A body with zero markers is an EMPTY checklist and, per
/// arbsec/orchestraitor#788, MISSING evidence: `markers_present` is false and
/// `all_checked` is false (an empty `all()` used to make it vacuously true and
/// merge a PR with no checklist evidence at all). There is NO author-granted
/// opt-out.
///
/// A body whose checklist reading is INDETERMINATE — a fence OPENER indented
/// 1-3 spaces, whether or not its region closes; a RAW HTML CONSTRUCT (a
/// line-start `<`, which this scanner does not model); a NEAR-MARKER; or a
/// MANAGED MARKER indented 4+ spaces (or led by a tab), which is a code block
/// rather than a checkbox whenever it is not inside a list item — is never "all
/// checked": the reading is refused rather than guessed
/// (arbsec/orchestraitor#809, arbsec/orchestraitor#799 R1, #802).
fn parse_checklist(body: &str) -> Checklist {
    let scan = scan_fences(body);
    let mut items = Vec::new();
    for (_line_number, line) in &scan.lines {
        let Some(item) = parse_canonical_marker(line) else {
            continue;
        };
        items.push(item);
    }
    let marker_count = u64::try_from(items.len()).unwrap_or(u64::MAX);
    let markers_present = !items.is_empty();
    let unchecked_count =
        u64::try_from(items.iter().filter(|item| !item.checked).count()).unwrap_or(u64::MAX);
    // An indeterminate reading is never "all checked": the scan cannot say what
    // the body asserts, so the reading is refused (fail closed). The EARLIEST
    // of the constructs wins — every one of them makes the reading undecidable.
    let mut unparseable_line: Option<u64> = None;
    let mut unparseable_construct: Option<UnparseableConstruct> = None;
    for (line, construct) in [
        (
            scan.indented_opener,
            UnparseableConstruct::IndentedFenceOpener,
        ),
        (scan.html_construct, UnparseableConstruct::HtmlConstruct),
        (scan.indented_marker, UnparseableConstruct::IndentedMarker),
        (scan.near_marker, UnparseableConstruct::NearMarker),
    ] {
        let Some(line) = line else {
            continue;
        };
        if unparseable_line.is_none_or(|earliest| line < earliest) {
            unparseable_line = Some(line);
            unparseable_construct = Some(construct);
        }
    }
    let unparseable = unparseable_line.is_some();
    let all_checked = !unparseable && markers_present && items.iter().all(|item| item.checked);
    Checklist {
        items,
        marker_count,
        markers_present,
        all_checked,
        unchecked_count,
        unparseable,
        unparseable_line,
        unparseable_construct,
    }
}

/// Parses one line as a CANONICAL managed marker.
///
/// The grammar is exact and contains NO whitespace class: the indent is
/// spaces/tabs, every separator is exactly one ASCII space, the box is `x`,
/// `X` or a single space, and the name is one of [`MANAGED_MARKER_NAMES`].
/// Returns `None` for anything else — including a NEAR-MARKER, which is
/// reported by the scan, not counted as an item.
fn parse_canonical_marker(line: &str) -> Option<ChecklistItem> {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let rest = trimmed.strip_prefix("- [")?;
    let (checked_char, after_box) = rest.split_once(']')?;
    if !matches!(checked_char, "x" | "X" | " ") {
        return None;
    }
    // Exactly one ASCII space between `]` and `<!--`.
    let after_box = after_box.strip_prefix(' ')?;
    let rest = after_box.strip_prefix("<!--")?;
    // Exactly one ASCII space after `<!--`, then `orc:NAME`.
    let rest = rest.strip_prefix(' ')?;
    let rest = rest.strip_prefix("orc:")?;
    let (name, rest) = split_managed_marker_name(rest)?;
    // Exactly one ASCII space before `-->`.
    let rest = rest.strip_prefix(' ')?;
    let rest = rest.strip_prefix("-->")?;
    Some(ChecklistItem {
        marker: name.to_string(),
        checked: checked_char != " ",
        // The description is the rest of the line, trimmed of SPACES and TABS
        // only — never of a Unicode whitespace class, which is the divergence
        // this grammar removes.
        description: rest.trim_matches([' ', '\t']).to_string(),
    })
}

/// Splits a managed marker NAME out of `orc:NAME -->`, requiring the name to be
/// one of [`MANAGED_MARKER_NAMES`] and to be followed by exactly one ASCII
/// space and `-->`.
fn split_managed_marker_name(rest: &str) -> Option<(&'static str, &str)> {
    for name in MANAGED_MARKER_NAMES {
        if let Some(tail) = rest.strip_prefix(name) {
            return Some((name, tail));
        }
    }
    None
}

/// True when the line carries a task-item BOX TOKEN — `[` + exactly one
/// character + `]` — and a reference to one of the nine managed names
/// (`orc:NAME`), but is NOT in the canonical form.
///
/// The reference test is a plain substring search and the canonical test is an
/// exact ASCII comparison: no whitespace class is involved, so the two engines
/// cannot disagree about it (arbsec/orchestraitor#799 R1).
fn is_near_marker(line: &str) -> bool {
    if parse_canonical_marker(line).is_some() {
        return false;
    }
    if !has_box_token(line) {
        return false;
    }
    MANAGED_MARKER_NAMES
        .iter()
        .any(|name| line.contains(&format!("orc:{name}")))
}

/// True when the line contains a `[` + exactly one character + `]` box token.
fn has_box_token(line: &str) -> bool {
    line.char_indices().any(|(index, character)| {
        if character != '[' {
            return false;
        }
        // One character, then `]`. A multi-byte character counts as ONE
        // character, so the scan is over CHARACTERS, not bytes.
        let Some(rest) = line.get(index + 1..) else {
            return false;
        };
        let mut characters = rest.chars();
        let Some(middle) = characters.next() else {
            return false;
        };
        if middle == ']' {
            return false;
        }
        characters.next() == Some(']')
    })
}

/// Normalises line endings ONCE, before any scan (`CommonMark` §2.1: a lone CR
/// is a line ending and so is CRLF; parsers normalise both to LF before
/// parsing). Every `\r\n` becomes `\n`, then every remaining lone `\r` becomes
/// `\n`, so no CR byte reaches the scanners and the closing-fence tail rule is
/// exactly "only spaces and tabs". The shell engine normalises identically
/// (`orc_lib_normalize_line_endings`); the two used to disagree on
/// `` ```\r\r\n `` because `str::lines` strips one CR and the scanners stripped
/// a second by hand (arbsec/orchestraitor#799 gen-7).
fn normalize_line_endings(body: &str) -> String {
    body.replace("\r\n", "\n").replace('\r', "\n")
}

/// One fence scan over the normalised body.
struct FenceScan {
    /// The VISIBLE lines with their 1-based line numbers in the ORIGINAL body.
    /// Every line inside a fenced code region is absent: it is an example, not
    /// evidence.
    lines: Vec<(u64, String)>,
    /// 1-based line number of the first fence OPENER indented 1-3 spaces.
    /// Such an opener may belong to a list item or a block quote, and this
    /// scanner has no container model, so every checklist reading from the body
    /// is REFUSED rather than guessed (arbsec/orchestraitor#809). The condition
    /// is STICKY: closing the region does not clear it.
    indented_opener: Option<u64>,
    /// 1-based line number of the first RAW HTML CONSTRUCT: a line outside every
    /// fenced region whose first non-whitespace character is `<`, indented 0-3
    /// spaces. The scanner does not model HTML, so it cannot tell what the
    /// construct hides and REFUSES the reading (arbsec/orchestraitor#799 R1).
    html_construct: Option<u64>,
    /// 1-based line number of the first NEAR-MARKER: a line that presents a
    /// managed marker (a task-item box token plus an `orc:NAME` reference) but
    /// not in the canonical form, so the gate cannot tell what the author
    /// asserted (arbsec/orchestraitor#799 R1). Every checklist reading from
    /// the body is REFUSED rather than guessed.
    near_marker: Option<u64>,
    /// 1-based line number of the first MANAGED MARKER indented too far: a
    /// canonical managed marker, or a near-marker, indented FOUR OR MORE spaces
    /// or led by a TAB (arbsec/orchestraitor#802). Such a line renders as an
    /// INDENTED CODE BLOCK whenever it is not inside a list item, and neither
    /// this scanner nor the canonical grammar has a container model to tell that
    /// apart from a marker nested in a list item, so every checklist reading
    /// from the body is REFUSED rather than guessed.
    indented_marker: Option<u64>,
}

/// Removes every FENCED code region from the body and reports the constructs
/// that make a reading INDETERMINATE (arbsec/orchestraitor#788, #809).
///
/// A managed checklist marker inside a fenced region is an EXAMPLE, not
/// evidence. The shell engine (`orc_lib_strip_fenced_code` in
/// `.agents/skills/github-pr-lifecycle/scripts/_lib.sh`) applies exactly these
/// rules, so creation and reconciliation agree on what a marker is:
///
/// - LINE ENDINGS are normalised once, before the scan (see
///   [`normalize_line_endings`]), so no CR byte is special-cased below;
/// - a fence OPENS on a line with at most THREE leading spaces whose next
///   characters are 3+ backticks OR 3+ tildes (four or more spaces — or a tab,
///   which reaches the 4-column tab stop — is an INDENTED code block, where
///   fence characters are literal text; arbsec/orchestraitor#799 F16). A 1-3
///   space indent makes the opener's CONTAINER indeterminate: the line number
///   is recorded in [`FenceScan::indented_opener`] and the verdict blocks. The
///   condition is STICKY — a closer does NOT clear it, because a closer at a
///   different container level ends the region EARLIER for the renderer than
///   for this scanner, so a marker swallowed by the scanner's region can still
///   be visible to a human;
/// - a backtick fence's info string MUST NOT contain a backtick, which makes
///   ```` ```example``` ```` a CLOSED inline code span rather than an opener; a
///   tilde fence MAY carry backticks in its info string (F17);
/// - the region CLOSES at the first later line whose first 0-3 characters are
///   spaces and whose next characters are the SAME fence character repeated at
///   least as many times as the opener, followed by nothing but SPACES and TABS
///   — `CommonMark` 0.31: a closing code fence "may be followed only by spaces or
///   tabs, which are ignored." A form feed, a vertical tab, an info string or
///   any other character leaves the region OPEN (a DIFFERENT fence character
///   never closes it either);
/// - an unclosed fence runs to the end of the body;
/// - every line inside the region — INCLUDING the opening and the closing
///   fence lines — is excluded from marker scanning;
/// - HTML IS NOT MODELLED, and the scanner says so instead of guessing
///   (arbsec/orchestraitor#799 R1). A line OUTSIDE every fenced region whose
///   FIRST non-whitespace character is `<`, indented 0-3 spaces, is recorded in
///   [`FenceScan::html_construct`] and REFUSES the reading
///   ([`UnparseableConstruct::HtmlConstruct`]): the scanner cannot tell whether
///   such a line opens a comment, a raw-text block, a declaration, an inline
///   span or a literal autolink, and a wrong guess is a false pass. This is
///   deliberately STRICTER than `CommonMark` — `<https://example.com>` and
///   `<em>x</em> prose` ARE ordinary text there — and arbsec/orchestraitor#809
///   tracks real modelling. The line itself is otherwise ordinary text: nothing
///   is hidden, because the whole reading is refused. A line indented by FOUR OR
///   MORE spaces, or led by a tab, is literal indented code and hides nothing,
///   so it is not a refusal; a line that is JUST `<` opens no construct and is
///   not one either; and a `<` anywhere else (`x < y`, `<` mid-line) is
///   ordinary text;
/// - the same applies to a managed marker line: its `<!--` is never the line's
///   first content, so a marker is never an HTML construct;
/// - a MANAGED MARKER (canonical, or a NEAR-MARKER) indented FOUR OR MORE
///   spaces, or led by a tab, is INDETERMINATE (arbsec/orchestraitor#802): when
///   it is not inside a list item it renders as an INDENTED CODE BLOCK, where it
///   is not a checkbox, and this scanner has no container model to tell that
///   apart from a marker nested in a list item. The line number is recorded in
///   [`FenceScan::indented_marker`] and the reading is REFUSED
///   ([`UnparseableConstruct::IndentedMarker`]). 0-3 leading spaces stays
///   accepted; the marker still COUNTS (`marker_count` is a pure function of the
///   canonical grammar) — it is the reading that is refused, so no verdict is
///   derived from that count. A marker inside a fenced region is inert, not a
///   refusal. The remedy is to dedent the marker line to column 0.
fn scan_fences(body: &str) -> FenceScan {
    let normalized = normalize_line_endings(body);
    let mut lines: Vec<(u64, String)> = Vec::new();
    // The open fence: its character and how many times it was repeated.
    let mut fence: Option<(char, usize)> = None;
    let mut indented_opener = None;
    let mut html_construct = None;
    let mut near_marker = None;
    let mut indented_marker = None;
    for (index, line) in normalized.lines().enumerate() {
        let candidate = fence_line(line);
        let line_number = u64::try_from(index + 1).ok();
        if let Some((fence_char, fence_len)) = fence {
            // Only the SAME character, repeated at least as many times as the
            // opener, with nothing but a blank tail after it, closes the
            // region. CommonMark 0.31 permits only spaces and tabs there, so a
            // form feed or a vertical tab leaves the fence OPEN — GitHub
            // renders the marker behind it as code (#799 F18). Rust's
            // `char::is_ascii_whitespace` is deliberately NOT used: it also
            // admits `\f`, `\n` and `\r`, which would close a region the
            // renderer keeps open.
            if let Some((_, trimmed)) = candidate
                && trimmed.starts_with(fence_char)
            {
                let run = trimmed.chars().take_while(|c| *c == fence_char).count();
                // The fence characters are ASCII, so `run` is also the byte
                // offset of the tail; `get` keeps a non-boundary from panicking.
                let tail = trimmed.get(run..).unwrap_or_default();
                let blank = tail.chars().all(|c| c == ' ' || c == '\t');
                if run >= fence_len && blank {
                    fence = None;
                }
            }
            continue;
        }
        // A RAW HTML CONSTRUCT is REFUSED, not modelled (#799 R1): a line
        // OUTSIDE every fenced region whose first non-whitespace character is
        // `<` and which carries something after it, indented 0-3 spaces. The
        // scanner cannot tell whether it opens a comment, a raw-text block, a
        // declaration or nothing at all (an autolink and inline HTML are
        // ordinary text in CommonMark), and a wrong guess is a false pass — so
        // the reading is refused instead. The line is otherwise ordinary text:
        // nothing is hidden, because the whole reading is. A 4+-space or
        // tab-led line is literal indented code and hides nothing, and a line
        // that is JUST `<` opens no construct at all: neither is a refusal.
        if candidate.is_some_and(|(_, trimmed)| trimmed.len() > 1 && trimmed.starts_with('<'))
            && html_construct.is_none()
        {
            html_construct = line_number;
        }
        // Outside a region: a line whose first 0-3 characters are spaces and
        // whose next characters are 3+ backticks or 3+ tildes OPENS a fence.
        if let Some((indent, trimmed)) = candidate
            && let Some(first) = trimmed.chars().next()
            && (first == '`' || first == '~')
        {
            let run = trimmed.chars().take_while(|c| *c == first).count();
            let info_has_backtick = first == '`' && trimmed.chars().skip(run).any(|c| c == '`');
            if run >= 3 && !info_has_backtick {
                // A 1-3-space indent makes the opener's CONTAINER indeterminate:
                // it may equally belong to a list item or a block quote. The
                // condition is STICKY and is NOT cleared when the region closes:
                // a closer at a different container level ends the region
                // EARLIER for the renderer than for this scanner, so a marker
                // that closes the region here can still be visible to a human —
                // reading such a body as "all checked" is the false pass this
                // rule exists to stop (#809). The remedy is to DEDENT the opener
                // to column 0.
                if indent > 0 && indented_opener.is_none() {
                    indented_opener = line_number;
                }
                fence = Some((first, run));
                continue;
            }
        }
        // A VISIBLE line: it is evidence, and it may be a NEAR-MARKER — a line
        // that presents a managed marker but not in the canonical form, so the
        // gate cannot tell what the author asserted (#799 R1). It may also be a
        // MANAGED MARKER indented too far (#802): `fence_line` is `None` for
        // FOUR OR MORE leading spaces, or for a tab after 0-3 spaces (a tab
        // reaches the 4-column tab stop). Such a line renders as an INDENTED
        // CODE BLOCK whenever it is not inside a list item, where it is not a
        // checkbox, and this scanner has no container model to tell that apart
        // from a marker nested in a list item — so the reading is REFUSED, never
        // counted as evidence. 0-3 leading spaces stays accepted; a line inside
        // a fenced region never reaches here (it is inert, not a refusal).
        if indented_marker.is_none()
            && fence_line(line).is_none()
            && (parse_canonical_marker(line).is_some() || is_near_marker(line))
        {
            indented_marker = line_number;
        }
        if near_marker.is_none() && is_near_marker(line) {
            near_marker = line_number;
        }
        if let Some(number) = line_number {
            lines.push((number, line.to_string()));
        }
    }
    FenceScan {
        lines,
        indented_opener,
        html_construct,
        near_marker,
        indented_marker,
    }
}

/// The part of `line` at which a fence may open or close, with its leading
/// SPACE indentation, or `None` when the line is indented too far to be a fence
/// line.
///
/// CommonMark/GFM allow at most THREE spaces of indentation before a top-level
/// fence. Four or more spaces — or a tab, which reaches the 4-column tab stop —
/// is an INDENTED code block, where fence characters are literal text
/// (arbsec/orchestraitor#799 review, finding F16). A form feed or a vertical tab
/// is likewise not indentation: it is content, so a `\f`- or `\v`-led fence line
/// opens nothing (F18). Both engines apply the rule identically: a
/// 4-space-indented run neither opens nor closes a region.
fn fence_line(line: &str) -> Option<(usize, &str)> {
    let indent = line.chars().take_while(|c| *c == ' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    if rest.starts_with('\t') {
        return None;
    }
    Some((indent, rest))
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
    fn known_disabled_jobs_holds_exactly_the_still_disabled_jobs() {
        // The vocabulary must not keep a job that is now enabled on pull
        // requests: the exemption matches on name AND `skipping` only, so a
        // stale entry silently excuses a re-enabled job that reports SKIPPED
        // (arbsec/orchestraitor#741 — the #750 review's re-enablement
        // condition). Adding a name here means it is disabled again.
        assert_eq!(KNOWN_DISABLED_JOBS, ["Documentation"]);
        assert!(
            !KNOWN_DISABLED_JOBS.contains(&"Docs Consistency"),
            "Docs Consistency is enabled on PRs by markdown.yml"
        );
    }

    #[test]
    fn known_disabled_skipping_job_is_not_applicable() {
        let item = classify_check(
            &json!({
                "__typename": "CheckRun",
                "name": "Documentation",
                "status": "COMPLETED",
                "conclusion": "SKIPPED",
            }),
            "",
        )
        .expect("well-formed CheckRun");
        assert_eq!(item.bucket, "skipping");
        assert_eq!(item.classification, "not-applicable");
        assert_eq!(item.skip_reason.as_deref(), Some("known-disabled-job"));
    }

    #[test]
    fn re_enabled_docs_consistency_skip_is_never_exempt() {
        // `Docs Consistency` left the vocabulary when markdown.yml re-enabled
        // it (arbsec/orchestraitor#741): its skip is now an UNEXPLAINED skip, so
        // it fails closed instead of laundering to not-applicable.
        let item = classify_check(
            &json!({
                "__typename": "CheckRun",
                "name": "Docs Consistency",
                "status": "COMPLETED",
                "conclusion": "SKIPPED",
            }),
            "",
        )
        .expect("well-formed CheckRun");
        assert_eq!(item.bucket, "skipping");
        assert_eq!(item.classification, "skip");
        assert_eq!(item.skip_reason, None);
        let aggregate = aggregate_checks(vec![
            classify_check(&passing_check("Test"), "").expect("passing check"),
            item,
        ]);
        assert!(!aggregate.all_passed, "an unexplained skip must block");
        assert!(!aggregate.any_failing, "a skip is not a failure");
    }

    #[test]
    fn known_disabled_failing_job_is_a_failure_never_a_pass() {
        // A state-blind exemption would launder real failures (pr-checks:
        // "the not-applicable exemption requires BOTH the known-disabled
        // name AND the skipped state").
        for conclusion in ["FAILURE", "ERROR", "TIMED_OUT", "ACTION_REQUIRED"] {
            let item = classify_check(
                &json!({
                    "__typename": "CheckRun",
                    "name": "Documentation",
                    "status": "COMPLETED",
                    "conclusion": conclusion,
                }),
                "",
            )
            .expect("well-formed CheckRun");
            assert_eq!(item.bucket, "fail", "conclusion {conclusion}");
            assert_eq!(item.classification, "fail", "conclusion {conclusion}");
        }
    }

    #[test]
    fn pending_status_context_classifies_pending() {
        // StatusContext state PENDING → bucket pending (gh's default arm).
        let item = classify_check(
            &json!({
                "__typename": "StatusContext",
                "context": "a status",
                "state": "PENDING",
            }),
            "",
        )
        .expect("well-formed StatusContext");
        assert_eq!(item.bucket, "pending");
        assert_eq!(item.classification, "pending");
    }

    #[test]
    fn cancel_bucket_classifies_as_fail_like_the_script() {
        let item = classify_check(
            &json!({
                "__typename": "CheckRun",
                "name": "cool tests",
                "status": "COMPLETED",
                "conclusion": "CANCELLED",
            }),
            "",
        )
        .expect("well-formed CheckRun");
        assert_eq!(item.bucket, "cancel");
        assert_eq!(item.classification, "fail");
    }

    #[test]
    fn status_context_uses_state_and_target_url() {
        let item = classify_check(
            &json!({
                "__typename": "StatusContext",
                "context": "a status",
                "state": "FAILURE",
                "targetUrl": "sweet link",
            }),
            "",
        )
        .expect("well-formed StatusContext");
        assert_eq!(item.name, "a status");
        assert_eq!(item.state, "FAILURE");
        assert_eq!(item.bucket, "fail");
        assert_eq!(item.link, "sweet link");
        assert_eq!(item.workflow, "");
    }

    #[test]
    fn check_run_state_is_conclusion_when_completed_else_status() {
        let completed = classify_check(
            &json!({
                "__typename": "CheckRun",
                "name": "cool tests",
                "status": "COMPLETED",
                "conclusion": "SUCCESS",
                "detailsUrl": "sweet link",
                "checkSuite": {"workflowRun": {"workflow": {"name": "code.yml"}}},
            }),
            "",
        )
        .expect("completed run");
        assert_eq!(completed.state, "SUCCESS");
        assert_eq!(completed.workflow, "code.yml");
        let in_progress = classify_check(
            &json!({
                "__typename": "CheckRun",
                "name": "slow tests",
                "status": "IN_PROGRESS",
                "conclusion": "",
            }),
            "",
        )
        .expect("in-progress run");
        assert_eq!(in_progress.state, "IN_PROGRESS");
        assert_eq!(in_progress.bucket, "pending");
    }

    #[test]
    fn missing_check_name_is_a_typed_error() {
        let error = classify_check(
            &json!({"__typename": "CheckRun", "status": "COMPLETED"}),
            "",
        )
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
            skip_reason: None,
            workflow: String::new(),
            link: String::new(),
        };
        // Empty checks array: all_passed is FALSE (the script's length > 0).
        assert!(!aggregate_checks(Vec::new()).all_passed);
        let with_na = aggregate_checks(vec![
            passing("cool tests", "pass"),
            passing("Documentation", "not-applicable"),
        ]);
        assert!(with_na.all_passed);
        assert!(!with_na.any_failing);
        // A genuinely skipped-but-optioned check is NOT known-disabled and
        // still fails the gate (all_passed == false).
        let skipped = aggregate_checks(vec![passing("rad tests", "skip")]);
        assert!(!skipped.all_passed);
        assert!(!skipped.any_failing, "a skip is not a failure");
    }

    // --- the structural bot-author CodeRabbit skip (pr-checks #706/#707) ----

    #[test]
    fn graphql_bot_actor_normalises_to_the_gh_app_login() {
        // GraphQL returns `__typename: "Bot"` with the bare slug; gh's
        // `pr view --json author` surfaces it as `app/<slug>` — the shape
        // pr-checks' predicate recognises.
        assert_eq!(
            gh_author_login(&json!({"author": {"__typename": "Bot", "login": "arbsec-agent"}})),
            "app/arbsec-agent"
        );
        // A bot user's `[bot]` login is already bot-shaped.
        assert_eq!(
            gh_author_login(&json!({"author": {"__typename": "Bot", "login": "dependabot[bot]"}})),
            "dependabot[bot]"
        );
        // Humans, and an absent/empty author, are never bots (fail closed).
        assert_eq!(
            gh_author_login(&json!({"author": {"__typename": "User", "login": "mekwall"}})),
            "mekwall"
        );
        assert_eq!(gh_author_login(&json!({})), "");
    }

    /// A `CodeRabbit` SKIPPED check run authored by a bot/App PR login: the
    /// pr-checks `bot_author_skip` case. `title` is the check run description
    /// gh exposes as `--json description` ("" for these runs in practice).
    fn code_rabbit_skip(title: &str) -> serde_json::Value {
        json!({
            "__typename": "CheckRun",
            "name": "CodeRabbit",
            "title": title,
            "status": "COMPLETED",
            "conclusion": "SKIPPED",
        })
    }

    fn passing_check(name: &str) -> serde_json::Value {
        json!({
            "__typename": "CheckRun",
            "name": name,
            "status": "COMPLETED",
            "conclusion": "SUCCESS",
        })
    }

    #[test]
    fn bot_authored_code_rabbit_skip_is_not_applicable_and_does_not_block() {
        // `<>[bot]` and `app/<>` are both bot/App logins (pr-checks'
        // `bot_author_pr`). A green PR with this structural skip is MERGEABLE
        // on checks: all_passed true, any_failing false.
        for author in ["arbsec-agent[bot]", "app/arbsec-agent"] {
            let rabbit = classify_check(&code_rabbit_skip(""), author)
                .expect("well-formed CodeRabbit CheckRun");
            assert_eq!(rabbit.bucket, "skipping", "author {author}");
            assert_eq!(rabbit.classification, "not-applicable", "author {author}");
            assert_eq!(
                rabbit.skip_reason.as_deref(),
                Some("bot-author"),
                "author {author}"
            );
            let aggregate = aggregate_checks(vec![
                classify_check(&passing_check("Check & Lint"), author).expect("passing check"),
                classify_check(&passing_check("Test"), author).expect("passing check"),
                rabbit,
            ]);
            assert!(aggregate.all_passed, "author {author}");
            assert!(!aggregate.any_failing, "author {author}");
            assert!(!aggregate.any_pending, "author {author}");
        }
    }

    #[test]
    fn human_authored_code_rabbit_skip_stays_a_skip_and_blocks() {
        // A human-authored PR's CodeRabbit skip is NOT the structural no-op:
        // it stays `skip` with no exemption, so all_passed is false (fail
        // closed, exactly like pr-checks).
        for author in ["mekwall", "some-human", ""] {
            let rabbit = classify_check(&code_rabbit_skip(""), author)
                .expect("well-formed CodeRabbit CheckRun");
            assert_eq!(rabbit.classification, "skip", "author {author:?}");
            assert_eq!(rabbit.skip_reason, None, "author {author:?}");
            let aggregate = aggregate_checks(vec![
                classify_check(&passing_check("Test"), author).expect("passing check"),
                rabbit,
            ]);
            assert!(!aggregate.all_passed, "author {author:?}");
            assert!(!aggregate.any_failing, "author {author:?}");
        }
    }

    #[test]
    fn bot_author_skip_with_a_non_bot_reason_description_stays_a_skip() {
        // pr-checks narrows on a non-empty description that does not name the
        // bot-user reason (rate limit, config error, out of credits): the
        // engine must not be looser than the script.
        let narrowed = classify_check(
            &code_rabbit_skip("Review skipped: rate limited"),
            "arbsec-agent[bot]",
        )
        .expect("well-formed CodeRabbit CheckRun");
        assert_eq!(narrowed.classification, "skip");
        assert_eq!(narrowed.skip_reason, None);
        // The real bot-user reason IS exempted.
        let exempted = classify_check(
            &code_rabbit_skip("Review skipped: bot user not eligible for review"),
            "arbsec-agent[bot]",
        )
        .expect("well-formed CodeRabbit CheckRun");
        assert_eq!(exempted.classification, "not-applicable");
        assert_eq!(exempted.skip_reason.as_deref(), Some("bot-author"));
    }

    #[test]
    fn bot_authored_code_rabbit_that_actually_failed_stays_a_failure() {
        // The exemption is state-blind when keyed on the author alone; a real
        // failure must never be laundered into not-applicable.
        let failed = classify_check(
            &json!({
                "__typename": "CheckRun",
                "name": "CodeRabbit",
                "status": "COMPLETED",
                "conclusion": "FAILURE",
            }),
            "arbsec-agent[bot]",
        )
        .expect("well-formed CodeRabbit CheckRun");
        assert_eq!(failed.bucket, "fail");
        assert_eq!(failed.classification, "fail");
        assert_eq!(failed.skip_reason, None);
    }

    #[test]
    fn genuine_failure_still_blocks_a_green_pr_with_a_bot_skip() {
        let aggregate = aggregate_checks(vec![
            classify_check(&passing_check("Test"), "arbsec-agent[bot]").expect("passing check"),
            classify_check(&code_rabbit_skip(""), "arbsec-agent[bot]").expect("bot skip"),
            classify_check(
                &json!({
                    "__typename": "CheckRun",
                    "name": "Check & Lint",
                    "status": "COMPLETED",
                    "conclusion": "CANCELLED",
                }),
                "arbsec-agent[bot]",
            )
            .expect("cancelled check"),
        ]);
        assert!(!aggregate.all_passed);
        assert!(aggregate.any_failing);
    }

    #[test]
    fn known_disabled_skip_keeps_a_green_pr_all_passed() {
        let aggregate = aggregate_checks(vec![
            classify_check(&passing_check("Test"), "").expect("passing check"),
            classify_check(
                &json!({
                    "__typename": "CheckRun",
                    "name": "Documentation",
                    "status": "COMPLETED",
                    "conclusion": "SKIPPED",
                }),
                "",
            )
            .expect("known-disabled skip"),
        ]);
        assert!(aggregate.all_passed);
        assert!(!aggregate.any_failing);
    }

    #[test]
    fn unclassifiable_check_is_never_a_pass() {
        // Fail closed: a classification the engine cannot place is not pass
        // and not not-applicable, so the gate stays closed.
        let unknown = CheckItem {
            name: "mystery".to_string(),
            state: "SOMETHING_NEW".to_string(),
            bucket: "pending".to_string(),
            classification: "unknown".to_string(),
            skip_reason: None,
            workflow: String::new(),
            link: String::new(),
        };
        let aggregate = aggregate_checks(vec![unknown]);
        assert!(!aggregate.all_passed);
    }

    // --- checklist parsing (reconcile-checklist semantics) -------------------

    #[test]
    fn checklist_parses_markers_checked_and_descriptions() {
        // A managed marker names one of the NINE managed items — a custom name
        // is not a managed marker (arbsec/orchestraitor#799 R1).
        let checklist = parse_checklist(
            "- [x] <!-- orc:issue --> evidence recorded\n\
             - [ ] <!-- orc:docs --> docs updated\n\
             - plain bullet\n\
             - [x] <!-- orc:tests --> trailing\n",
        );
        assert_eq!(checklist.items.len(), 3);
        assert!(checklist.items[0].checked);
        assert_eq!(checklist.items[0].marker, "issue");
        assert_eq!(checklist.items[0].description, "evidence recorded");
        assert!(!checklist.items[1].checked);
        assert_eq!(checklist.items[1].marker, "docs");
        assert!(!checklist.all_checked);
        assert_eq!(checklist.unchecked_count, 1);
        assert_eq!(checklist.items[2].marker, "tests");
        // A CUSTOM name is neither a marker nor a near-marker: it names no
        // managed item, so it is prose like any other.
        let custom = parse_checklist("- [x] <!-- orc:convergence --> evidence recorded\n");
        assert!(custom.items.is_empty(), "{:?}", custom.items);
        assert!(!custom.unparseable);
        assert!(!custom.markers_present);
    }

    #[test]
    fn marker_less_body_is_missing_not_all_checked() {
        // arbsec/orchestraitor#788: an empty `all()` used to report a
        // marker-less body as fully checked, which reached merge-gate's
        // `.mergeable` and merged a PR with no checklist evidence at all.
        let checklist = parse_checklist("Just a custom PR body with no markers.");
        assert!(checklist.items.is_empty());
        assert_eq!(checklist.marker_count, 0);
        assert!(!checklist.markers_present);
        assert!(!checklist.all_checked);
        assert_eq!(checklist.unchecked_count, 0);
    }

    #[test]
    fn marker_count_and_markers_present_track_the_parsed_items() {
        let checklist = parse_checklist(
            "- [x] <!-- orc:issue --> linked\n\
             - [x] <!-- orc:tests --> tested\n",
        );
        assert_eq!(checklist.marker_count, 2);
        assert!(checklist.markers_present);
        assert!(checklist.all_checked);
    }

    #[test]
    fn a_body_sentinel_is_not_an_opt_out() {
        // arbsec/orchestraitor#788 revision: the `<!-- orc:none -->` waiver was
        // REMOVED — an author cannot grant themselves the exemption. A body
        // that carries the sentinel and no marker is still MISSING evidence.
        //
        // arbsec/orchestraitor#799 R1: on its own LINE the sentinel is also a
        // raw HTML construct, so the reading is REFUSED (`html_construct`).
        // Stricter, and still no waiver.
        for body in [
            "A body.\n\n<!-- orc:none -->\n",
            "A body.\n\n<!-- orc:none --> machine-authored delivery summary\n",
        ] {
            let sentinel = parse_checklist(body);
            assert!(!sentinel.markers_present, "the sentinel is not a marker");
            assert!(!sentinel.all_checked);
            assert_eq!(sentinel.marker_count, 0);
            assert!(sentinel.unparseable, "a line-start sentinel is refused");
            assert_eq!(
                sentinel.unparseable_construct,
                Some(UnparseableConstruct::HtmlConstruct)
            );
        }

        // MID-LINE the sentinel is inert prose: no refusal, no evidence, no
        // waiver.
        let midline = parse_checklist("A body.\nnote: <!-- orc:none --> is inert prose\n");
        assert!(!midline.markers_present);
        assert!(!midline.all_checked);
        assert!(!midline.unparseable);
    }

    #[test]
    fn a_sentinel_never_masks_a_partially_checked_checklist() {
        // MID-LINE sentinel: the readable shape, where the real item count
        // decides the verdict.
        let checklist = parse_checklist(
            "note: <!-- orc:none --> sneaky waiver\n\
             - [x] <!-- orc:issue --> linked\n\
             - [ ] <!-- orc:tests --> tested\n",
        );
        assert!(checklist.markers_present);
        assert_eq!(checklist.marker_count, 2);
        assert!(!checklist.all_checked);
        assert_eq!(checklist.unchecked_count, 1);
        assert!(!checklist.unparseable);

        // LINE-START sentinel: refused outright, so the unticked box can never
        // be laundered into a pass by the sentinel either.
        let line_start = parse_checklist(
            "<!-- orc:none --> sneaky waiver\n- [x] <!-- orc:issue --> linked\n- [ ] <!-- orc:tests --> tested\n",
        );
        assert_eq!(line_start.marker_count, 2);
        assert!(!line_start.all_checked);
        assert!(line_start.unparseable);
    }

    #[test]
    fn checkbox_characters_are_case_insensitive_and_exactly_three() {
        // arbsec/orchestraitor#788 review finding: the shell regex accepted
        // ANY single character in the box while the Rust parser accepted only
        // `x` and a space. Both engines now accept exactly `x`, `X` and a
        // space: `x`/`X` are checked, a space is unchecked, and any other
        // character is not a checklist line at all.
        let upper = parse_checklist("- [X] <!-- orc:tests --> tested\n");
        assert_eq!(upper.marker_count, 1);
        assert!(upper.items[0].checked, "`X` is a checked box");
        assert!(upper.all_checked);

        let space = parse_checklist("- [ ] <!-- orc:tests --> untested\n");
        assert_eq!(space.marker_count, 1);
        assert!(!space.items[0].checked, "a space is an unchecked box");

        let nonsense = parse_checklist("- [q] <!-- orc:tests --> nonsense box\n");
        assert_eq!(nonsense.marker_count, 0, "a nonsense box is not an item");
        assert!(!nonsense.markers_present);
        assert!(!nonsense.all_checked);
    }

    #[test]
    fn fenced_code_is_not_checklist_state() {
        // A marker inside a fenced code region is an EXAMPLE, not evidence:
        // both engines exclude the fenced region before scanning.
        let fenced = parse_checklist(
            "Here is the shape:\n\
             ```markdown\n\
             - [x] <!-- orc:issue --> example only\n\
             ```\n\
             \n\
             No real checklist here.\n",
        );
        assert_eq!(fenced.marker_count, 0);
        assert!(!fenced.markers_present);
        assert!(!fenced.all_checked);

        let tilde_fenced = parse_checklist(
            "~~~text\n\
             - [x] <!-- orc:issue --> example only\n\
             ~~~\n",
        );
        assert_eq!(tilde_fenced.marker_count, 0);

        // A marker AFTER a closed fence IS a marker.
        let after = parse_checklist(
            "```\n\
             - [x] <!-- orc:issue --> example only\n\
             ```\n\
             - [x] <!-- orc:tests --> real item\n",
        );
        assert_eq!(after.marker_count, 1);
        assert_eq!(after.items[0].marker, "tests");
        assert!(after.all_checked);
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end_of_the_body() {
        let body = parse_checklist(
            "```\n\
             - [x] <!-- orc:issue --> example only\n\
             - [x] <!-- orc:tests --> also example\n",
        );
        assert_eq!(body.marker_count, 0);
        assert!(!body.markers_present);
    }

    /// The delivery lane's PR body: the nine TRUSTED markers lead, the untrusted
    /// model summary follows (arbsec/orchestraitor#788 gen-3 review).
    ///
    /// The summary is adversarial — a checked managed marker plus a code fence
    /// that never closes. Appended BEFORE the checklist, all nine markers land
    /// inside that fence, `strip_fenced_code` drops them, and the parse returns
    /// the summary's single checked marker: `markers_present: true`,
    /// `unchecked_count: 0`, `all_checked: true` — a merge gate satisfied by
    /// untrusted model output alone. Leading with the trusted block keeps every
    /// one of the nine markers unscanned-by-fence, all unticked.
    #[test]
    fn untrusted_summary_cannot_enclose_the_delivery_checklist() {
        let summary = "- [x] <!-- orc:issue --> Linked issue and specification requirements are satisfied\n```\nunclosed\n";
        let trusted = "## Merge checklist\n\n\
             - [ ] <!-- orc:issue --> Linked issue and specification requirements are satisfied\n\
             - [ ] <!-- orc:service-identity --> All GitHub writes attributed to arbsec-agent\n\
             - [ ] <!-- orc:tests --> Tests are added or updated\n\
             - [ ] <!-- orc:security --> Security impact is reviewed\n\
             - [ ] <!-- orc:docs --> Human-facing documentation is updated\n\
             - [ ] <!-- orc:rollback --> Rollback or recovery behavior is verified\n\
             - [ ] <!-- orc:checks --> All required and non-optional checks pass\n\
             - [ ] <!-- orc:review --> All noteworthy findings are resolved\n\
             - [ ] <!-- orc:threads --> All actionable review threads are resolved\n\
             \n";

        // The delivered order: trusted block first.
        let delivered = parse_checklist(&format!("{trusted}{summary}"));
        assert_eq!(
            delivered.marker_count, 10,
            "nine trusted + the summary's one"
        );
        assert!(delivered.markers_present);
        assert_eq!(delivered.unchecked_count, 9, "no trusted box is ticked");
        assert!(
            !delivered.all_checked,
            "nine unchecked trusted markers must block the gate"
        );
        let trusted_items: Vec<_> = delivered
            .items
            .iter()
            .filter(|item| !item.checked)
            .map(|item| item.marker.as_str())
            .collect();
        assert_eq!(
            trusted_items,
            [
                "issue",
                "service-identity",
                "tests",
                "security",
                "docs",
                "rollback",
                "checks",
                "review",
                "threads",
            ]
        );

        // The defect this ordering removes: the same two pieces the other way
        // round let the summary certify itself.
        let bypassed = parse_checklist(&format!("{summary}{trusted}"));
        assert_eq!(
            bypassed.marker_count, 1,
            "the fence swallowed the checklist"
        );
        assert!(bypassed.markers_present);
        assert_eq!(bypassed.unchecked_count, 0);
        assert!(
            bypassed.all_checked,
            "the old ordering is exactly the bypass: reproduce it here so the regression is visible"
        );
    }

    #[test]
    fn a_different_fence_character_never_closes_a_region() {
        // The opener is a backtick fence; a tilde line does not close it.
        let body = parse_checklist(
            "```\n\
             ~~~\n\
             - [x] <!-- orc:issue --> still inside the fence\n\
             ```\n\
             - [x] <!-- orc:tests --> real item\n",
        );
        assert_eq!(body.marker_count, 1);
        assert_eq!(body.items[0].marker, "tests");
    }

    /// The nine managed marker names are the SINGLE list: the Rust copy must
    /// equal the names in `.github/PULL_REQUEST_TEMPLATE.md` AND the names in
    /// the `_lib.sh` fallback (which the shell suite's drift guard asserts
    /// equals the template verbatim). Neither engine may carry a second copy.
    #[test]
    fn managed_marker_names_match_the_template() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let template = std::fs::read_to_string(root.join(".github/PULL_REQUEST_TEMPLATE.md"))
            .expect("the PR template");
        let lib = std::fs::read_to_string(
            root.join(".agents/skills/github-pr-lifecycle/scripts/_lib.sh"),
        )
        .expect("the shared helper");
        let names = |text: &str| -> Vec<String> {
            text.lines()
                .filter_map(|line| {
                    let rest = line.split("<!-- orc:").nth(1)?;
                    let name = rest.split(" -->").next()?;
                    if name.is_empty()
                        || !name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    {
                        return None;
                    }
                    Some(name.to_string())
                })
                .collect()
        };
        let template_names = names(&template);
        assert_eq!(
            template_names,
            MANAGED_MARKER_NAMES
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<String>>(),
            "the Rust managed-marker names must equal the template's"
        );
        let lib_names = {
            // Only the FALLBACK constant carries the list; the file's own
            // comments mention `<!-- orc:NAME -->` as examples.
            let start = lib
                .find("ORC_LIB_CHECKLIST_FALLBACK='")
                .expect("the fallback constant");
            let rest = &lib[start + "ORC_LIB_CHECKLIST_FALLBACK='".len()..];
            let end = rest.find("'\n").expect("the end of the fallback constant");
            names(&rest[..end])
        };
        assert_eq!(
            lib_names, template_names,
            "the _lib.sh fallback must carry the same managed names as the template"
        );
    }

    /// The CANONICAL managed-marker form, and the NEAR-MARKER rule that
    /// refuses everything else (arbsec/orchestraitor#799 R1).
    ///
    /// The canonical form is `[ \t]*- [BOX] <!-- orc:NAME -->` where every
    /// separator is EXACTLY one ASCII space, `<BOX>` is `x`, `X` or a single
    /// space, and `<NAME>` is one of the nine managed names. A line that is not
    /// canonical but carries BOTH a task-item box token and an `orc:NAME`
    /// reference is a NEAR-MARKER: the gate cannot tell what the author
    /// asserted, so the reading is REFUSED (`unparseable`).
    #[test]
    fn a_managed_marker_has_one_canonical_form_and_near_markers_are_refused() {
        // The canonical form is counted and ticked.
        let canonical = parse_checklist("- [x] <!-- orc:issue --> x\n");
        assert_eq!(canonical.marker_count, 1);
        assert!(canonical.items[0].checked);
        assert_eq!(canonical.items[0].marker, "issue");
        assert_eq!(canonical.items[0].description, "x");
        assert!(!canonical.unparseable);
        assert!(canonical.all_checked);

        // Every near-marker form: not canonical, but a box token AND an
        // `orc:NAME` reference. Each is refused, with the construct named.
        let near_markers = [
            // A NO-BREAK SPACE after the box.
            "- [ ]\u{a0}<!-- orc:security --> visible unchecked\n",
            // An EM SPACE after the box.
            "- [ ]\u{2003}<!-- orc:security --> visible unchecked\n",
            // A NARROW NO-BREAK SPACE after the box.
            "- [ ]\u{202f}<!-- orc:security --> visible unchecked\n",
            // A NO-BREAK SPACE right after `<!--`.
            "- [ ] <!--\u{a0}orc:security --> visible unchecked\n",
            // A NO-BREAK SPACE right before `-->`.
            "- [ ] <!-- orc:security\u{a0}--> visible unchecked\n",
            // A TAB after the box.
            "- [ ]\t<!-- orc:tests --> visible unchecked\n",
            // TWO spaces after the box.
            "- [ ]  <!-- orc:tests --> visible unchecked\n",
            // A box character that is not `x`, `X` or a space.
            "- [y] <!-- orc:tests --> a nonsense box\n",
            // A managed marker behind a block-quote prefix.
            "> - [ ] <!-- orc:tests --> quoted task\n",
        ];
        for body in near_markers {
            let parsed = parse_checklist(body);
            assert!(
                parsed.unparseable,
                "a near-marker must be refused: {body:?} -> {:?}",
                parsed.items
            );
            assert_eq!(
                parsed.unparseable_construct,
                Some(UnparseableConstruct::NearMarker),
                "{body:?}"
            );
            assert!(!parsed.all_checked, "{body:?}");
            assert_eq!(parsed.marker_count, 0, "{body:?}");
        }

        // PROSE that mentions a managed name (no box token) is not a
        // near-marker: it must never block.
        let prose = parse_checklist(
            "The managed markers look like `<!-- orc:issue -->` and name one of the nine items.\n",
        );
        assert_eq!(prose.marker_count, 0);
        assert!(!prose.unparseable);

        // A PLAIN TASK (a box token, no managed name) is not a near-marker
        // either.
        let plain = parse_checklist("- [ ] plain task\n");
        assert_eq!(plain.marker_count, 0);
        assert!(!plain.unparseable);

        // NO opt-out: `orc:none` is not one of the nine managed names, so a
        // body carrying only that sentinel has NO checklist evidence — and it
        // is not a NEAR-MARKER either (it names no managed marker), so it never
        // blocks as one. On its own line it is a raw HTML construct instead
        // (#799 R1), which is a different refusal.
        let sentinel = parse_checklist("<!-- orc:none -->\n");
        assert_eq!(sentinel.marker_count, 0);
        assert!(!sentinel.markers_present);
        assert_eq!(
            sentinel.unparseable_construct,
            Some(UnparseableConstruct::HtmlConstruct)
        );
        // A BOX TOKEN plus `orc:none` is still not a near-marker: `none` is not
        // one of the nine managed names.
        let sentinel_with_box = parse_checklist("- [x] <!-- orc:none -->\n");
        assert_eq!(sentinel_with_box.marker_count, 0);
        assert!(!sentinel_with_box.unparseable);
    }

    /// A MANAGED MARKER indented FOUR OR MORE spaces — or led by a TAB — makes
    /// the reading INDETERMINATE (arbsec/orchestraitor#802): when such a line is
    /// not inside a list item it renders as an INDENTED CODE BLOCK, where it is
    /// not a checkbox at all, and neither the canonical grammar (its leading
    /// `[ \t]*` class) nor this scanner has a container model to tell that apart
    /// from a marker nested in a list item. The confirmed repro is the first
    /// case below: nine canonical ticked markers, each preceded by four spaces,
    /// reported `marker_count=9, all_checked=true` and the merge lane merged it.
    ///
    /// The markers still COUNT — `marker_count` is a pure function of the
    /// canonical grammar — and the verdict blocks on the refusal, so no decision
    /// is derived from that count. 0-3 leading spaces stays ACCEPTED, and a
    /// marker inside a fenced region is inert rather than a refusal.
    #[test]
    fn a_marker_indented_four_spaces_is_refused() {
        let nine = |indent: &str| -> String {
            MANAGED_MARKER_NAMES
                .iter()
                .map(|name| format!("{indent}- [x] <!-- orc:{name} --> d"))
                .collect::<Vec<String>>()
                .join("\n")
                + "\n"
        };

        // The confirmed repro: nine 4-space-indented ticked markers, alone.
        let parsed = parse_checklist(&nine("    "));
        assert_eq!(parsed.marker_count, 9, "the grammar still counts them");
        assert!(parsed.unparseable, "an indented code block is not evidence");
        assert_eq!(parsed.unparseable_line, Some(1));
        assert_eq!(
            parsed.unparseable_construct,
            Some(UnparseableConstruct::IndentedMarker)
        );
        assert!(
            !parsed.all_checked,
            "a refused reading is never all checked"
        );

        // Blocked next to nine DETERMINATE markers too: the indented line is
        // refused wherever it sits.
        let parsed = parse_checklist(&format!("{}{}", nine("    "), nine("")));
        assert_eq!(parsed.marker_count, 18);
        assert!(parsed.unparseable);
        assert_eq!(
            parsed.unparseable_construct,
            Some(UnparseableConstruct::IndentedMarker)
        );
        assert!(!parsed.all_checked);

        // 0-3 leading spaces is a marker nested in a list item: ACCEPTED, and
        // unchanged.
        let parsed = parse_checklist(&nine("  "));
        assert_eq!(parsed.marker_count, 9);
        assert!(parsed.all_checked);
        assert!(!parsed.unparseable);

        // A TAB reaches the 4-column tab stop, so it is the same refusal — and
        // the line is still counted, never silently dropped.
        let parsed = parse_checklist("\t- [x] <!-- orc:tests --> tab\n");
        assert_eq!(parsed.marker_count, 1);
        assert_eq!(
            parsed.unparseable_construct,
            Some(UnparseableConstruct::IndentedMarker)
        );

        // A NEAR-MARKER indented four spaces is indeterminate for BOTH reasons;
        // the named construct is the more specific `indented_marker`, because
        // the indentation is what makes the line render as code.
        let parsed = parse_checklist(&format!("{}    - [x]  <!-- orc:tests --> near\n", nine("")));
        assert_eq!(parsed.marker_count, 9);
        assert_eq!(parsed.unparseable_line, Some(10));
        assert_eq!(
            parsed.unparseable_construct,
            Some(UnparseableConstruct::IndentedMarker)
        );

        // A marker inside a FENCED region is inert, not a refusal: the fence
        // already removes it from the scan.
        let parsed = parse_checklist("```\n    - [x] <!-- orc:tests --> fenced\n```\n");
        assert_eq!(parsed.marker_count, 0);
        assert!(!parsed.unparseable);
    }

    /// HTML IS NOT MODELLED: a line-start `<` REFUSES the reading
    /// (arbsec/orchestraitor#799 R1).
    ///
    /// A merge gate must not depend on a hand-rolled Markdown parser being
    /// spec-exact, so the scanner applies the repo's rule for a construct it
    /// cannot model — refuse and block. A line whose FIRST non-whitespace
    /// character is `<`, indented 0-3 spaces and outside every fenced region,
    /// makes the reading indeterminate: `unparseable` is true, `all_checked` is
    /// false, and every verdict blocks with `checklist_unparseable` and
    /// `unparseable_construct: html_construct`. This is deliberately STRICTER
    /// than `CommonMark` (an autolink and inline HTML are ordinary text there);
    /// arbsec/orchestraitor#809 tracks real modelling.
    #[test]
    fn a_line_start_html_construct_refuses_the_reading() {
        // Every one of these hid (or could hide) a marker under SOME markdown
        // model, so none may be guessed at.
        for text in [
            "<pre></pre>",
            "<!-->",
            "<!--->",
            "<!-- orc:none -->",
            "<https://example.com>",
            "<em>emphasis</em> prose",
            "<notatag nonsense",
            "<span>",
            "<span//>",
            "<div>",
            "</div>",
            "<?xml",
            "<![CDATA[",
            "   <div>",
        ] {
            let body = parse_checklist(&format!("{text}\n- [ ] <!-- orc:tests --> v\n"));
            assert!(body.unparseable, "line {text:?} must refuse the reading");
            assert_eq!(body.unparseable_line, Some(1), "line {text:?}");
            assert_eq!(
                body.unparseable_construct,
                Some(UnparseableConstruct::HtmlConstruct),
                "line {text:?}"
            );
            assert!(
                !body.all_checked,
                "line {text:?} must never read as all checked"
            );
        }

        // A `<` that is NOT the line's first non-whitespace character is
        // ordinary text — `x < y`, `<` mid-line, a marker's own `<!--` — and so
        // is a line that is JUST `<`: there is no construct there to hide.
        for text in ["x < y", "<", "prose <div> prose", "- [x] <!-- orc:x --> f"] {
            let body = parse_checklist(&format!("{text}\n- [x] <!-- orc:tests --> v\n"));
            assert!(!body.unparseable, "line {text:?} is ordinary text");
            assert_eq!(body.marker_count, 1, "line {text:?}");
        }

        // FOUR OR MORE leading spaces — or a leading tab — is literal indented
        // code: it hides nothing, so it is not a refusal and the marker below is
        // counted normally.
        for text in ["    <div>", "\t<div>", "    <!--"] {
            let body = parse_checklist(&format!("{text}\n- [x] <!-- orc:tests --> v\n"));
            assert!(!body.unparseable, "line {text:?} is literal indented code");
            assert_eq!(body.marker_count, 1, "line {text:?}");
        }

        // Inside a FENCED code region the line is inert: not a refusal.
        let fenced = parse_checklist("```\n<div>\n```\n- [x] <!-- orc:tests --> v\n");
        assert!(!fenced.unparseable);
        assert_eq!(fenced.marker_count, 1);
        assert!(fenced.all_checked);
    }

    /// The description is the text AFTER the delimiter, for a checked and an
    /// unchecked item alike (arbsec/orchestraitor#799 R2): the shell engine used
    /// to emit the BOX character here while this engine returned the text.
    #[test]
    fn descriptions_are_the_text_after_the_delimiter() {
        let body =
            parse_checklist("- [x] <!-- orc:issue --> alpha\n- [ ] <!-- orc:tests --> beta\n");
        assert_eq!(body.marker_count, 2);
        assert_eq!(body.items[0].marker, "issue");
        assert!(body.items[0].checked);
        assert_eq!(body.items[0].description, "alpha");
        assert_eq!(body.items[1].marker, "tests");
        assert!(!body.items[1].checked);
        assert_eq!(body.items[1].description, "beta");

        // The empty description stays empty, and spaces/tabs are trimmed from
        // both ends only (never a Unicode whitespace class).
        let empty =
            parse_checklist("- [ ] <!-- orc:tests -->\n- [x] <!-- orc:issue -->  \tgamma\t \n");
        assert_eq!(empty.items[0].description, "");
        assert_eq!(empty.items[1].description, "gamma");
    }

    /// The EXACT size of the shared corpus. A deleted case must FAIL, so this
    /// is a floor that is also a ceiling: bump it whenever a case is added and
    /// never lower it to make a red suite green. `CORPUS_FLOOR` in
    /// `.agents/skills/github-pr-lifecycle/scripts/test-reconcile-checklist`
    /// carries the same number; keep the two in step.
    const CORPUS_FLOOR: u32 = 82;

    /// The SHARED cross-engine verdict corpus
    /// (`.agents/skills/github-pr-lifecycle/scripts/tests/checklist-corpus/`).
    ///
    /// Every case is a PR body (`<nn>-<slug>.md`) plus the verdict both engines
    /// must derive from it, recorded in ONE `expectations.tsv` row keyed by the
    /// file stem: `case`, `marker_count`, `all_checked`, and — only when the
    /// case pins them — `unparseable` and `unparseable_construct`. The shell
    /// suite (`scripts/test-reconcile-checklist`) reads the SAME table through
    /// `reconcile-checklist`, so the corpus is the CONTRACT between the two
    /// implementations: the fence scanner drifted between them three review
    /// rounds in a row (arbsec/orchestraitor#799 F16/F17), and a divergence in
    /// EITHER engine now fails CI.
    ///
    /// Three failure modes are pinned here, not just a value mismatch: a case
    /// file with NO row fails, a row with NO case file fails, and the case count
    /// must equal [`CORPUS_FLOOR`] exactly — a deleted case must FAIL.
    #[test]
    fn checklist_corpus_matches_both_engines() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.agents/skills/github-pr-lifecycle/scripts/tests/checklist-corpus");
        let (cases, mismatches) = compare_corpus(&dir);
        assert_eq!(
            cases, CORPUS_FLOOR,
            "corpus is {CORPUS_FLOOR} cases: a deleted case must fail, and an added case must raise CORPUS_FLOOR in this test AND in test-reconcile-checklist"
        );
        assert!(
            mismatches.is_empty(),
            "the Rust engine disagrees with the shared corpus:\n{}",
            mismatches.join("\n")
        );
    }

    /// One row of `expectations.tsv`: `(marker_count, all_checked,
    /// unparseable, unparseable_construct)`, keyed by case stem.
    type Expectations = std::collections::BTreeMap<String, (u64, bool, bool, Option<String>)>;

    /// Parses `expectations.tsv`: `#` comment and blank lines skipped, five
    /// TAB-separated columns, columns 4-5 optional. Returns the map plus every
    /// malformed line as a mismatch (a broken table must fail, not silently
    /// shrink the corpus).
    fn read_expectations(dir: &std::path::Path) -> (Expectations, Vec<String>) {
        let table = std::fs::read_to_string(dir.join("expectations.tsv")).expect("expectations");
        let mut expectations = Expectations::new();
        let mut malformed = Vec::new();
        for line in table.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let columns: Vec<&str> = line.split('\t').collect();
            if columns.len() < 3 {
                malformed.push(format!("too few columns: {line}"));
                continue;
            }
            let Some(marker_count) = columns[1].parse::<u64>().ok() else {
                malformed.push(format!("marker_count is not a number: {line}"));
                continue;
            };
            let all_checked = match columns[2] {
                "true" => true,
                "false" => false,
                other => {
                    malformed.push(format!("all_checked is not true/false: {other}"));
                    continue;
                }
            };
            // `unparseable` defaults to false and `unparseable_construct` to
            // "no construct": a case states them only when they are the rule it
            // pins.
            let unparseable = columns.get(3).is_some_and(|value| *value == "true");
            let construct = columns
                .get(4)
                .filter(|value| !value.is_empty())
                .map(|value| (*value).to_string());
            expectations.insert(
                columns[0].to_string(),
                (marker_count, all_checked, unparseable, construct),
            );
        }
        (expectations, malformed)
    }

    /// Compares every row against the engine, in both directions: a case file
    /// with no row and a row with no case file are BOTH mismatches. Returns
    /// `(case_count, mismatches)`.
    fn compare_corpus(dir: &std::path::Path) -> (u32, Vec<String>) {
        let (expectations, mut mismatches) = read_expectations(dir);
        let mut cases = 0_u32;
        let mut seen = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(dir).expect("corpus directory") {
            let path = entry.expect("corpus entry").path();
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("md") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
                mismatches.push(format!("{}: unreadable file stem", path.display()));
                continue;
            };
            let Some((expected_count, expected_all, expected_unparseable, expected_construct)) =
                expectations.get(stem)
            else {
                mismatches.push(format!(
                    "{}: no expectations.tsv row for this case (a case with no row must fail)",
                    path.display()
                ));
                continue;
            };
            seen.insert(stem.to_string());
            let body = std::fs::read_to_string(&path).expect("corpus body");
            let parsed = parse_checklist(&body);
            if parsed.marker_count != *expected_count {
                mismatches.push(format!(
                    "{}: marker_count actual={} expected={expected_count}",
                    path.display(),
                    parsed.marker_count
                ));
            }
            if parsed.all_checked != *expected_all {
                mismatches.push(format!(
                    "{}: all_checked actual={} expected={expected_all}",
                    path.display(),
                    parsed.all_checked
                ));
            }
            if parsed.unparseable != *expected_unparseable {
                mismatches.push(format!(
                    "{}: unparseable actual={} expected={expected_unparseable}",
                    path.display(),
                    parsed.unparseable
                ));
            }
            let actual_construct = parsed
                .unparseable_construct
                .map(|construct| match construct {
                    UnparseableConstruct::IndentedFenceOpener => "indented_fence_opener",
                    UnparseableConstruct::HtmlConstruct => "html_construct",
                    UnparseableConstruct::NearMarker => "near_marker",
                    UnparseableConstruct::IndentedMarker => "indented_marker",
                });
            if let Some(expected_construct) = expected_construct
                && actual_construct != Some(expected_construct.as_str())
            {
                mismatches.push(format!(
                    "{}: unparseable_construct actual={actual_construct:?} expected={expected_construct:?}",
                    path.display()
                ));
            }
            cases += 1;
        }
        for orphan in expectations.keys() {
            if !seen.contains(orphan) {
                mismatches.push(format!(
                    "expectations.tsv row `{orphan}` has NO case file (a deleted case must fail)"
                ));
            }
        }
        (cases, mismatches)
    }

    /// A form feed is neither leading fence INDENTATION nor a legal closing
    /// tail (arbsec/orchestraitor#799 F18).
    ///
    /// `CommonMark` 0.31 allows 0-3 SPACES before a fence, and a closing fence
    /// "may be followed only by spaces or tabs, which are ignored." Rust's
    /// `char::is_ascii_whitespace` admits `\f`, so the earlier implementation
    /// closed a region the renderer keeps open — the shell and Rust engines
    /// disagreed. Both halves are pinned here and by corpus cases 37 (a form
    /// feed after the closer) and 38 (a vertical-tab-led opener).
    #[test]
    fn a_form_feed_is_neither_fence_indentation_nor_a_closing_tail() {
        // A \f-led fence line opens nothing: the marker below is a real item.
        let opener = parse_checklist("\u{c}```\n- [x] <!-- orc:tests --> real item\n");
        assert_eq!(opener.marker_count, 1, "a form feed is content, not indent");
        assert_eq!(opener.items[0].marker, "tests");

        // The closing-tail boundary table: only spaces and tabs are ignored
        // after the closing run. Every other character is CONTENT, so the
        // region stays OPEN and the checked marker behind it is an example (0
        // items). A carriage return is no longer a special case at all: the body
        // is normalised to LF first (see `normalize_line_endings`).
        for (tail, expected) in [
            ("", 1_u64),
            ("   ", 1),
            ("\t", 1),
            ("\r", 1),
            ("\u{c}", 0),
            ("\u{b}", 0),
        ] {
            let body = parse_checklist(&format!(
                "```\n- [x] <!-- orc:issue --> example only\n```{tail}\n- [x] <!-- orc:tests --> behind the closer\n"
            ));
            assert_eq!(
                body.marker_count, expected,
                "closer tail {tail:?} must yield {expected} marker(s)"
            );
        }
    }

    /// A fence OPENER indented 1-3 spaces is INDETERMINATE scope
    /// (arbsec/orchestraitor#809): it may belong to a list item or a block
    /// quote, and the scanner has no container model, so the reading is REFUSED
    /// rather than guessed — `all_checked` is false and `unparseable` names the
    /// opener's line.
    #[test]
    fn an_indented_fence_opener_is_refused_not_guessed_809() {
        let list_item = parse_checklist(
            r"- [x] <!-- orc:issue --> linked
- Example:
  ```
  sample

- [ ] <!-- orc:tests --> not tested
",
        );
        assert!(
            list_item.unparseable,
            "a 2-space-indented opener is indeterminate scope"
        );
        assert_eq!(list_item.unparseable_line, Some(3));
        assert!(
            !list_item.all_checked,
            "an indeterminate scan is never 'all checked'"
        );

        // The same body with the fence DEDENTED to column 0 is determinate, and
        // the visible unchecked item blocks on its own count.
        let dedented = parse_checklist(
            r"- [x] <!-- orc:issue --> linked
- Example:
```
sample
```
- [ ] <!-- orc:tests --> not tested
",
        );
        assert!(!dedented.unparseable);
        assert_eq!(dedented.marker_count, 2);
        assert!(!dedented.all_checked);
        assert_eq!(dedented.unchecked_count, 1);

        // 0-space openers and 4+ space openers are unchanged: the former is a
        // fence, the latter is indented code (literal text).
        let top_level = parse_checklist("```\nx\n```\n- [x] <!-- orc:tests --> fine\n");
        assert!(!top_level.unparseable);
        assert!(top_level.all_checked);
        let literal = parse_checklist("    ```\n- [x] <!-- orc:tests --> fine\n");
        assert!(!literal.unparseable);
        assert!(literal.all_checked);

        // A CLOSED indented region does NOT clear the condition (the rule is
        // STICKY): a closer at a different container level ends the region
        // EARLIER for the renderer than for this scanner, so the scanner can
        // swallow a marker a human still sees. `CommonMark` ends this fence with
        // the list item, so the unticked item is VISIBLE — a "closing clears"
        // rule reported it as no evidence at all.
        let closed = parse_checklist(
            r"- [x] <!-- orc:issue --> linked
- Example:
  ```
  sample
- [ ] <!-- orc:tests --> not tested
  ```
",
        );
        assert!(
            closed.unparseable,
            "a CLOSED 1-3-space opener is still indeterminate (sticky)"
        );
        assert_eq!(closed.unparseable_line, Some(3));
        assert_eq!(
            closed.marker_count, 1,
            "the scanner's region swallows the visible item — which is exactly why the reading is refused"
        );
        assert!(!closed.all_checked, "the sticky condition must block");
    }

    /// `CommonMark` §2.1 line endings: CRLF and a lone CR are both line endings
    /// and are normalised to LF ONCE, before any scan. A closing fence followed
    /// by a CRLF-CRLF terminator closes (the first CR ends the fence line, the
    /// CRLF ends an empty line), which is the case the two engines used to
    /// disagree on (arbsec/orchestraitor#799 gen-7).
    #[test]
    fn line_endings_are_normalised_once_before_the_scan() {
        let cr_crlf = parse_checklist(
            "```\r\r\n- [x] <!-- orc:issue --> example only\r\r\n```\r\r\n- [x] <!-- orc:tests --> real item\r\r\n",
        );
        assert_eq!(cr_crlf.marker_count, 1, "```\\r\\r\\n closes the fence");
        assert!(cr_crlf.all_checked);

        let lone_cr = parse_checklist(
            "```\r- [x] <!-- orc:issue --> example only\r```\r- [x] <!-- orc:tests --> real item\r",
        );
        assert_eq!(
            lone_cr.marker_count, 1,
            "a lone CR is a line ending, so the fence closes"
        );
        assert!(lone_cr.all_checked);

        // A form feed or a vertical tab is CONTENT after the run, so the region
        // stays OPEN in both engines (no normalisation of those characters).
        let form_feed = parse_checklist("```\u{c}x\n```\u{c}\n- [x] <!-- orc:tests --> x\n");
        assert_eq!(form_feed.marker_count, 0);
        let vertical_tab = parse_checklist("```\u{b}\n- [x] <!-- orc:tests --> x\n");
        assert_eq!(vertical_tab.marker_count, 0);
    }

    #[test]
    fn one_ticked_marker_satisfies_the_gate_today() {
        // DOCUMENTED GAP — arbsec/orchestraitor#800 (deferred from #788).
        //
        // The gate asserts that every marker the body CARRIES is checked; it
        // does NOT assert that the body carries the managed marker SET. A body
        // with exactly ONE ticked marker therefore reports `all_checked: true`
        // and produces no checklist reason, so the other eight managed markers
        // are silently absent.
        //
        // This is PRE-EXISTING (the base gate at 1297cb07 had the same
        // property) and independently testable, so it is tracked in its own
        // issue rather than folded into the #788 bug-fix. This test exists so
        // the gap is VISIBLE and cannot be mistaken for intended behaviour.
        let checklist = parse_checklist("- [x] <!-- orc:issue --> linked\n");
        assert_eq!(checklist.marker_count, 1);
        assert!(checklist.markers_present);
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
