//! Reference expansion at the delegation boundary (spec
//! `10-orchestrator.md` §9.49, issue #614 slice 4).
//!
//! Chat and delegation flows carry entity references — `pr://<N>`,
//! `issue://<N>` — in worker-bound task payloads. At the §9.25.2 delegation
//! boundary the ORCHESTRATION LAYER expands those references pre-send: the
//! worker receives the bounded expanded block and performs ZERO fetches of
//! its own.
//!
//! Semantics (§9.49, verbatim):
//! - **Grammar.** `pr://<number>` and `issue://<number>` — bare integer
//!   keys. Unrecognized schemes (`issue:/`, `pr:/x`, `fixes://`,
//!   `cr://…`) are NOT references: they pass through as literal text,
//!   never best-effort expanded.
//! - **Bounded payload.** At most 2 KiB per expanded reference and 8 KiB
//!   per expansion pass; titles and labels are size-capped; full bodies,
//!   comments, and review text are NEVER inlined.
//! - **Data, never instructions (§6.1).** Everything expanded is UNTRUSTED
//!   PROVIDER CONTENT in a fenced, labelled block. The layer performs no
//!   interpretation of body content. Secret-shaped strings are REDACTED
//!   with an announcement — never a silent pass, never the secret.
//! - **Failure semantics (typed, never silent).** `not-found`,
//!   `forbidden`, `rate-limited`, `provider-error` leave the reference as
//!   literal text with a typed notice; the payload is still delivered.
//!   The layer does NOT retry synchronously (§9.26.1).
//! - **One pass, no rescan.** The expansion runs once over the payload;
//!   expanded block CONTENT is never re-scanned for references.
//!
//! Read-only: expansion fetches provider state; it never labels, comments,
//! reviews, or merges.

use std::fmt::Write as _;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Maximum characters of one expanded reference block (2 KiB per
/// reference, §9.49).
pub const MAX_REFERENCE_CHARS: usize = 2 * 1024;
/// Maximum characters of the whole expansion pass (8 KiB per pass, §9.49).
pub const MAX_PASS_CHARS: usize = 8 * 1024;
/// Maximum characters of a size-capped title or label value.
pub const MAX_TITLE_CHARS: usize = 200;
/// Static suffix appended to truncated values.
pub const TRUNCATION_SUFFIX: &str = "…[truncated]";
/// The exact fence used for expanded untrusted content (§6.1: quoted as
/// data, no instruction-bearing framing).
pub const FENCE: &str = "```";

/// How the expansion layer classifies one reference lookup failure.
/// Typed, log-safe (static labels; never provider content, §9.23.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ExpansionFailure {
    /// The reference does not exist (or is outside the configured
    /// repository scope).
    NotFound,
    /// The service identity is not authorized for the reference.
    Forbidden,
    /// The provider rate-limited the read; no synchronous retry (§9.26.1).
    RateLimited,
    /// Any other provider failure.
    ProviderError,
}

impl ExpansionFailure {
    /// The stable machine label for the notice.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NotFound => "not-found",
            Self::Forbidden => "forbidden",
            Self::RateLimited => "rate-limited",
            Self::ProviderError => "provider-error",
        }
    }
}

/// A failure carrying its typed reason for one reference lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceError {
    /// Typed lookup failure; the reference stays literal text.
    Failure(ExpansionFailure),
}

/// The typed fields the expansion layer needs for one `pr://` reference
/// (the §9.48 engine's output, bounded).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PrReferenceFields {
    /// PR number.
    pub number: u64,
    /// Size-capped title.
    pub title: String,
    /// State (OPEN/CLOSED/MERGED).
    pub state: String,
    /// Author login.
    pub author: String,
    /// Size-capped labels, comma-joined.
    pub labels: String,
    /// One-line URL.
    pub url: String,
    /// Full head OID.
    pub head_sha: String,
    /// Full base OID.
    pub base_oid: String,
    /// §9.48 `checks_all_passed`.
    pub checks_all_passed: bool,
    /// §9.48 `checks_any_pending`.
    pub checks_any_pending: bool,
    /// §9.48 `checks_any_failing`.
    pub checks_any_failing: bool,
    /// Failing-check names (never the full check array).
    pub failing_checks: Vec<String>,
    /// Unresolved-thread count.
    pub unresolved_threads: u64,
    /// §9.48 convergence reasons, verbatim.
    pub convergence_reasons: Vec<String>,
}

/// The typed fields the expansion layer needs for one `issue://` reference.
/// NO PR-derived fields (§9.49: issue references never carry them).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IssueReferenceFields {
    /// Issue number.
    pub number: u64,
    /// Size-capped title.
    pub title: String,
    /// State (OPEN/CLOSED).
    pub state: String,
    /// Author login.
    pub author: String,
    /// Size-capped labels, comma-joined.
    pub labels: String,
    /// One-line URL.
    pub url: String,
}

/// One classified reference found in a payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The scheme: `pr` or `issue`.
    pub kind: ReferenceKind,
    /// The bare integer key.
    pub number: u64,
    /// The full literal text as it appeared (`pr://614`).
    pub literal: String,
}

/// The two reference kinds specified for MVP (§9.49 grammar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceKind {
    /// `pr://<number>`.
    Pr,
    /// `issue://<number>`.
    Issue,
}

impl ReferenceKind {
    /// The scheme label.
    #[must_use]
    pub fn scheme(self) -> &'static str {
        match self {
            Self::Pr => "pr",
            Self::Issue => "issue",
        }
    }
}

/// Parses the reference grammar (§9.49, fail-closed no-fuzzy-matching):
/// only the EXACT shapes `pr://<digits>` and `issue://<digits>` at word
/// boundaries are references. Returns (reference, start, end) spans into
/// `payload`. Unrecognized schemes pass through as literal text.
#[must_use]
pub fn parse_reference_spans(payload: &str) -> Vec<(Reference, usize, usize)> {
    let mut refs = Vec::new();
    let mut rest = payload;
    let mut offset = 0usize;
    while let Some(pos) = rest.find("://") {
        // Classify the scheme immediately before the separator and require
        // it to START at a word boundary: the character before the scheme
        // must not be alphanumeric or one of `_`, `-`, `.`, `+` (§9.49
        // fail-closed grammar — `xpr://614`, `subissue://5`, `cr-pr://1`
        // are NOT references). At the payload start the scheme is valid.
        let before = &rest[..pos];
        let (scheme_start, kind) = if before.ends_with("pr") {
            (pos.saturating_sub(2), Some(ReferenceKind::Pr))
        } else if before.ends_with("issue") {
            (pos.saturating_sub(5), Some(ReferenceKind::Issue))
        } else {
            (usize::MAX, None)
        };
        let boundary_ok = scheme_start == 0
            || (scheme_start < pos
                && before[..scheme_start].chars().next_back().is_none_or(|c| {
                    !(c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '+')
                }));
        let after_scheme = &rest[pos + 3..];
        let key: String = after_scheme
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        // The digits must end at a word boundary too: `pr://614abc` is not
        // a reference.
        let key_ended = after_scheme[key.len()..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric());
        if boundary_ok
            && let Some(kind) = kind
            && !key.is_empty()
            && key_ended
            && let Ok(number) = key.parse::<u64>()
        {
            let literal = format!("{}://{}", kind.scheme(), key);
            let start = offset + scheme_start;
            let end = offset + pos + 3 + key.len();
            refs.push((
                Reference {
                    kind,
                    number,
                    literal,
                },
                start,
                end,
            ));
        }
        offset += pos + 3;
        rest = after_scheme;
    }
    refs
}

/// Parses the reference grammar (§9.49, fail-closed no-fuzzy-matching):
/// only the EXACT shapes `pr://<digits>` and `issue://<digits>` are
/// references. Unrecognized schemes pass through as literal text.
#[must_use]
pub fn parse_references(payload: &str) -> Vec<Reference> {
    parse_reference_spans(payload)
        .into_iter()
        .map(|(reference, _, _)| reference)
        .collect()
}

/// The transport the expansion layer reads provider state through (the
/// same service-identity GraphQL route the §9.48 engine uses).
pub trait ExpansionProvider: Send + Sync {
    /// Fetches the bounded fields for one `pr://` reference.
    ///
    /// # Errors
    /// Returns [`ReferenceError::Failure`] typed per §9.49; never retries.
    fn fetch_pr(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<PrReferenceFields, ReferenceError>;

    /// Fetches the bounded fields for one `issue://` reference.
    ///
    /// # Errors
    /// Returns [`ReferenceError::Failure`] typed per §9.49; never retries.
    fn fetch_issue(
        &self,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<IssueReferenceFields, ReferenceError>;
}

/// True when `candidate` is a secret-shaped token (GitHub App/user token
/// prefixes or an `sk-` API key with a long alphanumeric tail).
fn is_secret_token(candidate: &str) -> bool {
    const PREFIXES: [&str; 7] = ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "sk-"];
    PREFIXES.iter().any(|prefix| {
        candidate.starts_with(prefix)
            && candidate.len() >= prefix.len() + 16
            && candidate[prefix.len()..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// True when `candidate` is a `secret://` URI (§9.23).
fn is_secret_uri(candidate: &str) -> bool {
    candidate.starts_with("secret://")
}

/// True when `candidate` looks like a Bearer/credential header value or an
/// `key=value` credential assignment (case-insensitive key, long value).
fn is_credential_assignment(candidate: &str) -> bool {
    let lower = candidate.to_ascii_lowercase();
    (lower.starts_with("api_key=")
        || lower.starts_with("apikey=")
        || lower.starts_with("api-key=")
        || lower.starts_with("token=")
        || lower.starts_with("password="))
        && candidate.len() >= 8 + 8
}

/// True when the word starting at `start` in `text` is secret-shaped
/// (a token, a `secret://` URI, or a credential assignment).
fn is_secret_shaped(text: &str, start: usize) -> bool {
    let rest = &text[start..];
    let word: String = rest
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '`')
        .collect();
    is_secret_token(&word) || is_secret_uri(&word) || is_credential_assignment(&word)
}

/// Redacts secret-shaped strings per the §9.23 secret-detection posture:
/// each secret-shaped word is replaced with `[REDACTED]`; the count is
/// announced by the caller — never a silent pass, never the secret.
#[must_use]
pub fn redact_secrets(text: &str) -> (String, usize) {
    let mut redacted = String::with_capacity(text.len());
    let mut count = 0usize;
    let mut rest = text;
    while !rest.is_empty() {
        let word: String = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '`')
            .collect();
        // `Bearer <value>`: the scheme word AND the following value word
        // are both consumed — a bare scheme word alone carries no secret.
        if word.eq_ignore_ascii_case("bearer") {
            let after = &rest[word.len()..];
            let value: String = after
                .chars()
                .skip_while(|c| c.is_whitespace())
                .take_while(|c| !c.is_whitespace() && *c != '"' && *c != '`')
                .collect();
            if !value.is_empty() {
                redacted.push_str("[REDACTED]");
                count += 1;
                let skipped = word.len() + (after.len() - value.len()) + value.len();
                rest = &rest[skipped..];
                continue;
            }
        }
        if is_secret_shaped(rest, 0) {
            redacted.push_str("[REDACTED]");
            count += 1;
            rest = &rest[word.len()..];
        } else {
            let ch = rest.chars().next().unwrap_or('\0');
            redacted.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    (redacted, count)
}

/// Cap a value with the static truncation marker.
#[must_use]
pub fn cap_value(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let truncated: String = text.chars().take(cap).collect();
    format!("{truncated}{TRUNCATION_SUFFIX}")
}

/// Builds the fenced expanded block for one reference (§6.1: untrusted
/// provider content, quoted as data in a labelled fence; secrets redacted
/// with the announcement). Returns (content, fence) — the fence is
/// `max(longest backtick run in the content + 1, 3)` backticks, so
/// provider text can never close the fence early (§6.1 containment).
#[must_use]
fn build_block(scheme: &str, fields: &ExpandedFields) -> (String, String) {
    let raw = build_block_unfenced(fields);
    let mut max_run = 0usize;
    let mut cur = 0usize;
    for c in raw.chars() {
        if c == '`' {
            cur += 1;
            max_run = max_run.max(cur);
        } else {
            cur = 0;
        }
    }
    let fence = "`".repeat((max_run + 1).max(3));
    let _ = scheme; // scheme label is applied by the caller's header line
    (raw, fence)
}

/// The unfenced block content (typed fields + redaction).
#[must_use]
fn build_block_unfenced(fields: &ExpandedFields) -> String {
    let mut block = String::new();
    match fields {
        ExpandedFields::Pr(pr) => {
            let _ = writeln!(block, "number: {}", pr.number);
            let _ = writeln!(block, "title: {}", cap_value(&pr.title, MAX_TITLE_CHARS));
            let _ = writeln!(block, "state: {}", pr.state);
            let _ = writeln!(block, "author: {}", pr.author);
            let _ = writeln!(block, "labels: {}", cap_value(&pr.labels, MAX_TITLE_CHARS));
            let _ = writeln!(block, "url: {}", pr.url);
            let _ = writeln!(block, "head_sha: {}", pr.head_sha);
            let _ = writeln!(block, "base_oid: {}", pr.base_oid);
            let _ = writeln!(
                block,
                "ci: checks_all_passed={} checks_any_pending={} checks_any_failing={}",
                pr.checks_all_passed, pr.checks_any_pending, pr.checks_any_failing
            );
            if !pr.failing_checks.is_empty() {
                let names: Vec<String> = pr
                    .failing_checks
                    .iter()
                    .map(|name| cap_value(name, MAX_TITLE_CHARS))
                    .collect();
                let _ = writeln!(block, "failing_checks: {}", names.join(", "));
            }
            let _ = writeln!(block, "unresolved_threads: {}", pr.unresolved_threads);
            if !pr.convergence_reasons.is_empty() {
                let _ = writeln!(
                    block,
                    "convergence_reasons: {}",
                    pr.convergence_reasons.join(", ")
                );
            }
        }
        ExpandedFields::Issue(issue) => {
            let _ = writeln!(block, "number: {}", issue.number);
            let _ = writeln!(block, "title: {}", cap_value(&issue.title, MAX_TITLE_CHARS));
            let _ = writeln!(block, "state: {}", issue.state);
            let _ = writeln!(block, "author: {}", issue.author);
            let _ = writeln!(
                block,
                "labels: {}",
                cap_value(&issue.labels, MAX_TITLE_CHARS)
            );
            let _ = writeln!(block, "url: {}", issue.url);
        }
    }
    let (mut redacted, count) = redact_secrets(&block);
    if count > 0 {
        // §9.49: the redaction is ANNOUNCED — never a silent pass.
        writeln!(redacted, "redacted ({count} instance(s))").ok();
    }
    redacted
}

/// The resolved fields for one reference (PR or issue flavor).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExpandedFields {
    /// `pr://` fields (bounded §9.48 output).
    Pr(PrReferenceFields),
    /// `issue://` fields (no PR-derived data).
    Issue(IssueReferenceFields),
}

/// One expansion outcome for the result report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExpansionOutcome {
    /// The reference literal (`pr://614`).
    pub reference: String,
    /// Whether the expansion succeeded.
    pub expanded: bool,
    /// The typed failure label when it did not (§9.49).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

/// The full expansion-pass report: the expanded payload plus the typed
/// outcomes for every reference found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExpansionReport {
    /// The payload with references replaced by bounded expanded blocks
    /// (failures left as literal text with a typed notice).
    pub payload: String,
    /// One outcome per reference found, in first-appearance order.
    pub outcomes: Vec<ExpansionOutcome>,
    /// Whether the pass hit its 8 KiB budget (later references stay
    /// literal, §9.49).
    pub budget_exceeded: bool,
}

/// Expands every reference in `payload` at the delegation boundary (§9.49).
///
/// The expansion is ONE pass over the payload; expanded block content is
/// never re-scanned. References are processed in first-appearance order;
/// the pass stops inlining at the 8 KiB budget (later references stay
/// literal with a typed budget notice). Failures leave the reference as
/// literal text with a typed notice and the payload is still delivered —
/// the layer never retries synchronously (§9.26.1).
///
/// # Errors
///
/// Never returns an error for a lookup failure (those are typed notices in
/// the report); returns [`ReferenceError`] only if the payload itself
/// cannot be processed, which cannot occur for text input.
/// The typed budget-exceeded notice for one literal (§9.49: the payload
/// is still delivered; the reference stays literal with the notice).
/// The typed budget-exceeded notice for one literal (§9.49: the payload
/// is still delivered; the reference stays literal with the notice).
fn budget_exceeded_notice(literal: &str) -> String {
    format!(
        "{literal} [budget-exceeded; the payload is still delivered — the pass budget \
         was reached before this reference could be expanded]"
    )
}

/// Expands every reference in `payload` at the delegation boundary (§9.49).
///
/// The expansion is ONE pass over the payload; expanded block content is
/// never re-scanned. References are processed in first-appearance order;
/// the pass stops inlining at the 8 KiB budget (later references stay
/// literal with a typed budget notice). Failures leave the reference as
/// literal text with a typed notice and the payload is still delivered —
/// the layer never retries synchronously (§9.26.1).
///
/// # Errors
///
/// Only propagated transport-level construction failures; lookup failures
/// are typed notices inside the returned report, never an error return.
/// Resolves each unique reference ONCE, in first-appearance order, and
/// records the substitutions at every parsed span carrying that literal.
/// Budget-exceeded references keep their literal text with a typed notice.
#[allow(clippy::type_complexity)]
fn resolve_references(
    unique: &[Reference],
    spans: &[(Reference, usize, usize)],
    owner: &str,
    repo: &str,
    provider: &dyn ExpansionProvider,
) -> (Vec<ExpansionOutcome>, Vec<(usize, usize, String)>, bool) {
    let mut outcomes: Vec<ExpansionOutcome> = Vec::new();
    let mut substitutions: Vec<(usize, usize, String)> = Vec::new();
    let mut pass_chars = 0usize;
    let mut budget_exceeded = false;
    for reference in unique {
        if budget_exceeded {
            let notice = budget_exceeded_notice(&reference.literal);
            for (_, start, end) in spans
                .iter()
                .filter(|(candidate, _, _)| candidate.literal == reference.literal)
            {
                substitutions.push((*start, *end, notice.clone()));
            }
            outcomes.push(ExpansionOutcome {
                reference: reference.literal.clone(),
                expanded: false,
                failure: Some(String::from("budget-exceeded")),
            });
            continue;
        }
        let fields = match reference.kind {
            ReferenceKind::Pr => provider
                .fetch_pr(owner, repo, reference.number)
                .map(ExpandedFields::Pr),
            ReferenceKind::Issue => provider
                .fetch_issue(owner, repo, reference.number)
                .map(ExpandedFields::Issue),
        };
        match fields {
            Ok(resolved) => {
                let (raw, fence) = build_block(reference.kind.scheme(), &resolved);
                // §9.49: 2 KiB per expanded reference — truncate the
                // content at a CHAR boundary in BYTES, leaving room for
                // the suffix and the real closing fence, so the result
                // never exceeds MAX_REFERENCE_CHARS and the fence always
                // closes.
                let mut raw = raw;
                if raw.len() + fence.len() * 2 + 32 > MAX_REFERENCE_CHARS {
                    let byte_cap = MAX_REFERENCE_CHARS.saturating_sub(fence.len() * 2 + 32);
                    let mut cut = byte_cap;
                    while cut > 0 && !raw.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    raw = format!("{}…[truncated]", &raw[..cut]);
                }
                let block = format!(
                    "{fence}expanded {}://{} (untrusted content below)\n{raw}\n{fence}",
                    reference.kind.scheme(),
                    reference.number
                );
                if pass_chars + block.len() > MAX_PASS_CHARS {
                    budget_exceeded = true;
                    let notice = budget_exceeded_notice(&reference.literal);
                    for (_, start, end) in spans
                        .iter()
                        .filter(|(candidate, _, _)| candidate.literal == reference.literal)
                    {
                        substitutions.push((*start, *end, notice.clone()));
                    }
                    outcomes.push(ExpansionOutcome {
                        reference: reference.literal.clone(),
                        expanded: false,
                        failure: Some(String::from("budget-exceeded")),
                    });
                    continue;
                }
                pass_chars += block.len();
                for (_, start, end) in spans
                    .iter()
                    .filter(|(candidate, _, _)| candidate.literal == reference.literal)
                {
                    substitutions.push((*start, *end, block.clone()));
                }
                outcomes.push(ExpansionOutcome {
                    reference: reference.literal.clone(),
                    expanded: true,
                    failure: None,
                });
            }
            Err(ReferenceError::Failure(failure)) => {
                let label = failure.label();
                // The reference STAYS literal text with the typed notice
                // attached after it (§9.49).
                let notice = format!(
                    "{literal} [{label}; the payload is still delivered — the reference \
                     could not be expanded]",
                    literal = reference.literal
                );
                for (_, start, end) in spans
                    .iter()
                    .filter(|(candidate, _, _)| candidate.literal == reference.literal)
                {
                    substitutions.push((*start, *end, notice.clone()));
                }
                outcomes.push(ExpansionOutcome {
                    reference: reference.literal.clone(),
                    expanded: false,
                    failure: Some(String::from(label)),
                });
            }
        }
    }
    (outcomes, substitutions, budget_exceeded)
}

/// Expands every reference in `payload` at the delegation boundary (§9.49).
///
/// The expansion is ONE pass over the payload; expanded block content is
/// never re-scanned. References are processed in first-appearance order;
/// the pass stops inlining at the 8 KiB budget (later references stay
/// literal text with a typed budget notice). Failures leave the reference as
/// literal text with a typed notice and the payload is still delivered —
/// the layer never retries synchronously (§9.26.1).
///
/// # Errors
///
/// Only propagated transport-level construction failures; lookup failures
/// are typed notices inside the returned report, never an error return.
pub fn expand_payload(
    payload: &str,
    owner: &str,
    repo: &str,
    provider: &dyn ExpansionProvider,
) -> Result<ExpansionReport, ReferenceError> {
    let spans = parse_reference_spans(payload);

    // Dedupe by literal, preserving first-appearance order.
    let references = spans.clone();
    let mut unique: Vec<Reference> = Vec::new();
    for (reference, _, _) in &references {
        if !unique
            .iter()
            .any(|known| known.literal == reference.literal)
        {
            unique.push(reference.clone());
        }
    }
    let (outcomes, substitutions, budget_exceeded) =
        resolve_references(&unique, &references, owner, repo, provider);

    // Assemble: substitute at the PARSED spans (exact boundaries — never a
    // substring collision between literals of different lengths), applied
    // right-to-left so earlier spans stay valid. The payload's literal
    // text passes through verbatim; expanded block content is never
    // re-scanned (one pass, §9.49).
    let mut out = payload.to_string();
    let mut ordered = substitutions;
    ordered.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    for (start, end, replacement) in &ordered {
        out.replace_range(*start..*end, replacement);
    }

    Ok(ExpansionReport {
        payload: out,
        outcomes,
        budget_exceeded,
    })
}
