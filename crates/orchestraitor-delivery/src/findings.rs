//! Review-finding deduplication and cross-loop tracking (spec §9.33.4).
//!
//! Every review finding MUST include severity, evidence, affected paths, the
//! violated requirement or rule, and a proposed remediation, and findings MUST
//! be deduplicated and tracked across review loops (§9.33.4). This module
//! provides that bookkeeping: a stable dedup key (`path + line + rule`), a
//! ledger that counts reoccurrences per loop, reopens findings that resurface
//! after resolution, resolves findings absent from a completed generation,
//! and flags open findings as stale when the reviewed HEAD moves — so the
//! convergence checker (leaf task #197) can discount findings produced
//! against an older generation of the change set. The convergence verdict
//! itself ("one full review generation at the current HEAD with no new
//! noteworthy findings and all earlier blocking findings resolved") is owned
//! by that checker, not this module.
//!
//! Security boundary (spec §9.33.7): this module only records and organizes
//! review findings. It MUST NOT make security decisions (allow/deny/verdict) —
//! blocking decisions are made by the runner and policy layer, and all policy,
//! approval, promotion, and receipt authority belongs to Arbitraitor (§2.2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::review_loop::Severity;

/// One review finding as mandated by spec §9.33.4 ("Each finding MUST
/// include: severity, evidence, affected paths, violated requirement or rule,
/// and proposed remediation"), plus an optional line span used for dedup
/// keying.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewFinding {
    /// Severity assigned by the reporting reviewer.
    pub severity: Severity,
    /// Concrete evidence backing the finding (quote, diff hunk, command
    /// output). Must not be blank.
    pub evidence: String,
    /// Repository-relative paths the finding applies to. Must contain at
    /// least one non-blank path; the first non-blank path (after
    /// normalization) is the dedup-key path — see [`FindingId::of`].
    pub affected_paths: Vec<PathBuf>,
    /// Violated requirement or rule key (e.g. a spec-reference anchor such as
    /// `9.33.4` or a policy-rule identifier). Trimmed for keying; must not be
    /// blank.
    pub rule: String,
    /// 1-based line at which the finding starts. When present it is always
    /// part of the dedup key; a finding without a pinned line keys on
    /// path+rule only (§9.33.4 path+line+rule keying).
    pub line: Option<u32>,
    /// Inclusive 1-based end of the flagged range. Informational only; not
    /// part of the dedup key.
    pub line_end: Option<u32>,
    /// Proposed remediation for the finding.
    pub remediation: String,
}

/// Stable deduplication key for a review finding (spec §9.33.4): the
/// normalized primary affected path, the 1-based line when pinned, and the
/// trimmed rule.
///
/// Keying rules: the primary path is the first non-blank entry of
/// [`ReviewFinding::affected_paths`] after lexical normalization
/// (whitespace-trimmed, leading `./` segments stripped, trailing slashes
/// stripped). Normalization is lexical only — path case and interior
/// separators are not folded. A finding without a pinned [`ReviewFinding::line`]
/// keys on path+rule only; two findings sharing path+rule but pinning
/// different lines are distinct.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FindingId {
    /// Normalized primary affected path.
    pub path: String,
    /// 1-based pinned line, when the reviewer provided one.
    pub line: Option<u32>,
    /// Trimmed violated requirement or rule key.
    pub rule: String,
}

impl FindingId {
    /// Derives the dedup key for a finding, enforcing the spec §9.33.4
    /// payload minimums (non-blank evidence, a proposed remediation, at least
    /// one non-blank affected path, non-blank rule).
    ///
    /// # Errors
    ///
    /// Returns [`FindingError::NoAffectedPaths`], [`FindingError::BlankRule`],
    /// [`FindingError::BlankEvidence`], or [`FindingError::BlankRemediation`]
    /// when the finding misses a required payload element.
    pub fn of(finding: &ReviewFinding) -> Result<Self, FindingError> {
        let path = finding
            .affected_paths
            .iter()
            .map(|p| normalize_path(p))
            .find(|p| !p.is_empty())
            .ok_or(FindingError::NoAffectedPaths)?;
        let rule = finding.rule.trim();
        if rule.is_empty() {
            return Err(FindingError::BlankRule);
        }
        if finding.evidence.trim().is_empty() {
            return Err(FindingError::BlankEvidence);
        }
        if finding.remediation.trim().is_empty() {
            return Err(FindingError::BlankRemediation);
        }
        Ok(Self {
            path,
            line: finding.line,
            rule: rule.to_owned(),
        })
    }
}

/// Lexically normalizes an affected path for dedup keying: trims surrounding
/// whitespace, strips leading `./` segments, and strips trailing slashes.
fn normalize_path(path: &Path) -> String {
    let mut s = path.to_string_lossy().trim().to_owned();
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_owned();
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// Validation failure for a §9.33.4 review finding or ledger operation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FindingError {
    /// The finding names no usable affected path, but spec §9.33.4 mandates
    /// one.
    #[error("finding must name at least one non-blank affected path")]
    NoAffectedPaths,
    /// The finding's violated rule is empty or whitespace.
    #[error("finding must name the violated requirement or rule")]
    BlankRule,
    /// The finding's evidence is empty or whitespace.
    #[error("finding must carry non-blank evidence")]
    BlankEvidence,
    /// The finding carries no proposed remediation, but spec §9.33.4 mandates
    /// that each finding include one.
    #[error("finding must carry a proposed remediation")]
    BlankRemediation,
    /// A head SHA passed to [`FindingLedger::note_head`] was empty or
    /// whitespace.
    #[error("reviewed head must be a non-blank identifier")]
    BlankHead,
}

/// Cross-loop status of a tracked finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    /// Reported in the most recent completed review generation.
    Open,
    /// Absent from a later completed generation
    /// ([`FindingLedger::resolve_unreported`]).
    Resolved,
}

/// What [`FindingLedger::record`] concluded about a reported finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// First time this `(path, line, rule)` key was ever reported.
    New,
    /// Re-report of an already-open finding.
    ReReported,
    /// The finding resurfaced after being resolved in an earlier generation;
    /// the entry transitions back to open.
    Reopened,
}

/// Cross-loop bookkeeping for one deduplicated finding.
///
/// Severity policy: on re-report the stored [`LedgerEntry::finding`] is
/// replaced, so `finding.severity` always reflects the latest report, while
/// [`LedgerEntry::max_severity`] tracks the highest severity ever seen. The
/// blocking aggregation [`FindingLedger::blocking_open_count`] deliberately
/// uses `max_severity`: a finding that once met the blocking threshold keeps
/// blocking until a full review generation stops reporting it, so severity
/// can never be silently downgraded away (engineering priorities: security
/// and correctness over convenience).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Stable dedup key of the finding.
    pub id: FindingId,
    /// Most recent report of the finding.
    pub finding: ReviewFinding,
    /// Review-loop number at which the finding was first recorded.
    pub first_seen_loop: u32,
    /// Review-loop number of the most recent report.
    pub last_seen_loop: u32,
    /// Total number of times this finding has been reported.
    pub occurrences: u32,
    /// Highest severity ever reported for this finding (see the type-level
    /// severity policy).
    pub max_severity: Severity,
    /// Ordered reviewed-head identifiers this finding has been reported
    /// against (newest last).
    pub heads_seen: Vec<String>,
    /// Current cross-loop status.
    pub status: FindingStatus,
    /// True when the reviewed head moved after this finding's last report and
    /// the finding has not been re-reported against the current head. A stale
    /// finding is discounted by the blocking aggregation so the convergence
    /// checker (#197) only counts findings confirmed against the current
    /// generation.
    pub stale: bool,
}

/// Deduplicated ledger of review findings across loops (spec §9.33.4).
///
/// The runner records every finding of each review generation
/// ([`FindingLedger::record`]), notes the reviewed head whenever it moves
/// ([`FindingLedger::note_head`]), and after a full generation has been
/// recorded resolves everything it did not re-report
/// ([`FindingLedger::resolve_unreported`]). Entries persist in the §9.33.6
/// durable store as serialized [`LedgerEntry`] values.
#[derive(Debug, Default)]
pub struct FindingLedger {
    entries: BTreeMap<FindingId, LedgerEntry>,
    current_head: Option<String>,
}

impl FindingLedger {
    /// Creates an empty ledger with no reviewed head recorded.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            current_head: None,
        }
    }

    /// Records the head (e.g. git SHA) the current review generation targets.
    ///
    /// When the head moves, every open entry that has not yet been
    /// re-reported against the new head is flagged stale (§9.33.4 stale
    /// detection on HEAD movement): findings produced against an older
    /// generation of the change set must not count toward blocking until
    /// confirmed again. Calling this with the same head is a no-op.
    ///
    /// # Ordering contract
    ///
    /// Call this when a new generation's head is known — **before** recording
    /// that generation's findings. Entries recorded before a head change are
    /// attributed to the previous head and will be flagged stale by the next
    /// [`FindingLedger::note_head`]; a runner that records against a new head
    /// without noting it first discounts its own results in the blocking
    /// aggregation (fail-safe, but pointless). [`FindingLedger::record`]
    /// clears the stale flag, so the note-then-record order is what keeps a
    /// just-completed generation's own findings counted.
    ///
    /// # Errors
    ///
    /// Returns [`FindingError::BlankHead`] when `head` is empty or whitespace.
    pub fn note_head(&mut self, head: &str) -> Result<(), FindingError> {
        let head = head.trim();
        if head.is_empty() {
            return Err(FindingError::BlankHead);
        }
        if self.current_head.as_deref() == Some(head) {
            return Ok(());
        }
        self.current_head = Some(head.to_owned());
        for entry in self.entries.values_mut() {
            if entry.status == FindingStatus::Open {
                entry.stale = true;
            }
        }
        Ok(())
    }

    /// Records one review finding reported during loop `loop_number`.
    ///
    /// Deduplicates on [`FindingId`]: a re-report updates `last_seen_loop`,
    /// bumps `occurrences`, replaces the stored finding (latest severity
    /// wins for reporting), keeps [`LedgerEntry::max_severity`] at the
    /// highest severity ever seen, appends the current head to
    /// `heads_seen` if not already recorded, clears the stale flag (the
    /// finding was just confirmed against the current generation), and
    /// reopens a resolved entry.
    ///
    /// # Errors
    ///
    /// Returns the [`FindingError`] from [`FindingId::of`] when the finding
    /// misses a spec-mandated payload element.
    pub fn record(
        &mut self,
        finding: ReviewFinding,
        loop_number: u32,
    ) -> Result<(FindingId, RecordOutcome), FindingError> {
        let id = FindingId::of(&finding)?;
        if let Some(entry) = self.entries.get_mut(&id) {
            let outcome = if entry.status == FindingStatus::Resolved {
                RecordOutcome::Reopened
            } else {
                RecordOutcome::ReReported
            };
            entry.status = FindingStatus::Open;
            entry.last_seen_loop = entry.last_seen_loop.max(loop_number);
            entry.occurrences = entry.occurrences.saturating_add(1);
            if finding.severity > entry.max_severity {
                entry.max_severity = finding.severity;
            }
            if let Some(head) = &self.current_head
                && entry.heads_seen.last().map(String::as_str) != Some(head.as_str())
            {
                entry.heads_seen.push(head.clone());
            }
            entry.stale = false;
            entry.finding = finding;
            Ok((id, outcome))
        } else {
            let heads_seen = match &self.current_head {
                Some(head) => vec![head.clone()],
                None => Vec::new(),
            };
            let entry = LedgerEntry {
                id: id.clone(),
                max_severity: finding.severity,
                finding,
                first_seen_loop: loop_number,
                last_seen_loop: loop_number,
                occurrences: 1,
                heads_seen,
                status: FindingStatus::Open,
                stale: false,
            };
            self.entries.insert(id.clone(), entry);
            Ok((id, RecordOutcome::New))
        }
    }

    /// Closes a completed review generation: every open entry whose key is
    /// absent from `reported` transitions to [`FindingStatus::Resolved`]
    /// ("a finding not re-reported in a later full generation transitions to
    /// resolved", §9.33.4). Returns how many entries transitioned.
    pub fn resolve_unreported(&mut self, reported: &BTreeSet<FindingId>) -> usize {
        let mut resolved = 0;
        for (id, entry) in &mut self.entries {
            if entry.status == FindingStatus::Open && !reported.contains(id) {
                entry.status = FindingStatus::Resolved;
                resolved += 1;
            }
        }
        resolved
    }

    /// Counts findings that currently block convergence: open, not stale, and
    /// with [`LedgerEntry::max_severity`] at or above `minimum_to_block`
    /// ([`Severity::blocks`]). The blocking verdict itself belongs to the
    /// runner/policy layer (§9.33.7); this is only the aggregation input.
    #[must_use]
    pub fn blocking_open_count(&self, minimum_to_block: &Severity) -> usize {
        self.entries
            .values()
            .filter(|e| {
                e.status == FindingStatus::Open
                    && !e.stale
                    && e.max_severity.blocks(minimum_to_block)
            })
            .count()
    }

    /// Returns the entry for a dedup key, when tracked.
    #[must_use]
    pub fn entry(&self, id: &FindingId) -> Option<&LedgerEntry> {
        self.entries.get(id)
    }

    /// Iterates all tracked entries, ordered by dedup key.
    pub fn entries(&self) -> impl Iterator<Item = &LedgerEntry> {
        self.entries.values()
    }

    /// Number of tracked entries (open and resolved).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no finding has ever been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The head the current review generation targets, when one was noted.
    #[must_use]
    pub fn current_head(&self) -> Option<&str> {
        self.current_head.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn finding(severity: Severity, path: &str, line: Option<u32>, rule: &str) -> ReviewFinding {
        ReviewFinding {
            severity,
            evidence: format!("evidence for {rule}"),
            affected_paths: vec![PathBuf::from(path)],
            rule: rule.to_owned(),
            line,
            line_end: None,
            remediation: format!("remediate {rule}"),
        }
    }

    #[test]
    fn new_finding_creates_single_open_entry() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (id, outcome) =
            ledger.record(finding(Severity::High, "src/lib.rs", Some(10), "9.33.4"), 1)?;
        assert_eq!(outcome, RecordOutcome::New);
        assert_eq!(id.path, "src/lib.rs");
        assert_eq!(id.line, Some(10));
        assert_eq!(id.rule, "9.33.4");
        let entry = ledger.entry(&id).ok_or("entry must exist")?;
        assert_eq!(entry.status, FindingStatus::Open);
        assert_eq!(entry.first_seen_loop, 1);
        assert_eq!(entry.last_seen_loop, 1);
        assert_eq!(entry.occurrences, 1);
        assert_eq!(entry.max_severity, Severity::High);
        assert!(!entry.stale);
        assert_eq!(ledger.len(), 1);
        Ok(())
    }

    #[test]
    fn identical_finding_across_loops_deduplicates() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (id, _) =
            ledger.record(finding(Severity::High, "src/lib.rs", Some(10), "rule/a"), 1)?;
        let (id2, outcome2) =
            ledger.record(finding(Severity::High, "src/lib.rs", Some(10), "rule/a"), 2)?;
        let (_id3, outcome3) =
            ledger.record(finding(Severity::High, "src/lib.rs", Some(10), "rule/a"), 3)?;
        assert_eq!(id, id2);
        assert_eq!(outcome2, RecordOutcome::ReReported);
        assert_eq!(outcome3, RecordOutcome::ReReported);
        assert_eq!(ledger.len(), 1);
        let entry = ledger.entry(&id).ok_or("entry must exist")?;
        assert_eq!(entry.first_seen_loop, 1);
        assert_eq!(entry.last_seen_loop, 3);
        assert_eq!(entry.occurrences, 3);
        Ok(())
    }

    #[test]
    fn same_path_and_rule_with_different_line_is_distinct() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (a, _) = ledger.record(finding(Severity::High, "src/lib.rs", Some(10), "rule/a"), 1)?;
        let (b, _) = ledger.record(finding(Severity::High, "src/lib.rs", Some(42), "rule/a"), 1)?;
        assert_ne!(a, b);
        assert_eq!(ledger.len(), 2);
        Ok(())
    }

    #[test]
    fn same_path_and_line_with_different_rule_is_distinct() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (a, _) = ledger.record(finding(Severity::High, "src/lib.rs", Some(10), "rule/a"), 1)?;
        let (b, _) = ledger.record(finding(Severity::High, "src/lib.rs", Some(10), "rule/b"), 1)?;
        assert_ne!(a, b);
        assert_eq!(ledger.len(), 2);
        Ok(())
    }

    #[test]
    fn key_normalizes_trailing_slash_dot_prefix_and_whitespace() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (plain, _) =
            ledger.record(finding(Severity::Low, "src/lib.rs", Some(1), "rule/a"), 1)?;
        let (dot, _) =
            ledger.record(finding(Severity::Low, "./src/lib.rs", Some(1), "rule/a"), 2)?;
        let (slash, _) =
            ledger.record(finding(Severity::Low, "src/lib.rs/", Some(1), "rule/a"), 3)?;
        let (padded, _) = ledger.record(
            finding(Severity::Low, "  src/lib.rs  ", Some(1), "  rule/a  "),
            4,
        )?;
        assert_eq!(plain, dot);
        assert_eq!(plain, slash);
        assert_eq!(plain, padded);
        assert_eq!(ledger.len(), 1);
        let entry = ledger.entry(&plain).ok_or("entry must exist")?;
        assert_eq!(entry.occurrences, 4);
        assert_eq!(entry.last_seen_loop, 4);
        Ok(())
    }

    #[test]
    fn finding_without_line_keys_on_path_and_rule() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (id, _) = ledger.record(
            finding(Severity::Medium, "docs/spec/spec.md", None, "9.33.4"),
            1,
        )?;
        let (id2, outcome) = ledger.record(
            finding(Severity::Medium, "docs/spec/spec.md", None, "9.33.4"),
            2,
        )?;
        assert_eq!(id, id2);
        assert_eq!(id.line, None);
        assert_eq!(outcome, RecordOutcome::ReReported);
        assert_eq!(ledger.len(), 1);
        Ok(())
    }

    #[test]
    fn severity_rereport_updates_last_and_keeps_max() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (id, _) = ledger.record(finding(Severity::High, "src/lib.rs", Some(7), "rule/a"), 1)?;
        ledger.record(finding(Severity::Low, "src/lib.rs", Some(7), "rule/a"), 2)?;
        let entry = ledger.entry(&id).ok_or("entry must exist")?;
        assert_eq!(entry.finding.severity, Severity::Low);
        assert_eq!(entry.max_severity, Severity::High);
        ledger.record(
            finding(Severity::Critical, "src/lib.rs", Some(7), "rule/a"),
            3,
        )?;
        let entry = ledger.entry(&id).ok_or("entry must exist")?;
        assert_eq!(entry.finding.severity, Severity::Critical);
        assert_eq!(entry.max_severity, Severity::Critical);
        Ok(())
    }

    #[test]
    fn unreported_finding_resolves_on_completed_generation() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (a, _) = ledger.record(finding(Severity::High, "src/a.rs", Some(1), "rule/a"), 1)?;
        let (b, _) = ledger.record(finding(Severity::High, "src/b.rs", Some(2), "rule/b"), 1)?;
        // Generation 2 re-reports only finding a.
        ledger.record(finding(Severity::High, "src/a.rs", Some(1), "rule/a"), 2)?;
        let reported: BTreeSet<FindingId> = [a.clone()].into_iter().collect();
        assert_eq!(ledger.resolve_unreported(&reported), 1);
        assert_eq!(
            ledger.entry(&a).ok_or("a exists")?.status,
            FindingStatus::Open
        );
        assert_eq!(
            ledger.entry(&b).ok_or("b exists")?.status,
            FindingStatus::Resolved
        );
        // Resolving again with the same reported set is a no-op.
        assert_eq!(ledger.resolve_unreported(&reported), 0);
        Ok(())
    }

    #[test]
    fn rereport_after_resolved_reopens_entry() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (id, _) = ledger.record(finding(Severity::High, "src/lib.rs", Some(5), "rule/a"), 1)?;
        assert_eq!(ledger.resolve_unreported(&BTreeSet::new()), 1);
        assert_eq!(
            ledger.entry(&id).ok_or("entry exists")?.status,
            FindingStatus::Resolved
        );
        let (id2, outcome) =
            ledger.record(finding(Severity::High, "src/lib.rs", Some(5), "rule/a"), 3)?;
        assert_eq!(id, id2);
        assert_eq!(outcome, RecordOutcome::Reopened);
        let entry = ledger.entry(&id).ok_or("entry exists")?;
        assert_eq!(entry.status, FindingStatus::Open);
        assert_eq!(entry.first_seen_loop, 1);
        assert_eq!(entry.last_seen_loop, 3);
        assert_eq!(entry.occurrences, 2);
        Ok(())
    }

    #[test]
    fn head_movement_flags_open_findings_stale() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.note_head("aaaaaaaa")?;
        let (id, _) = ledger.record(finding(Severity::High, "src/lib.rs", Some(9), "rule/a"), 1)?;
        assert!(!ledger.entry(&id).ok_or("entry exists")?.stale);
        assert_eq!(ledger.current_head(), Some("aaaaaaaa"));
        ledger.note_head("bbbbbbbb")?;
        let entry = ledger.entry(&id).ok_or("entry exists")?;
        assert!(entry.stale);
        assert_eq!(entry.heads_seen, vec!["aaaaaaaa".to_owned()]);
        // Re-report at the new head clears stale and records the head.
        ledger.record(finding(Severity::High, "src/lib.rs", Some(9), "rule/a"), 2)?;
        let entry = ledger.entry(&id).ok_or("entry exists")?;
        assert!(!entry.stale);
        assert_eq!(
            entry.heads_seen,
            vec!["aaaaaaaa".to_owned(), "bbbbbbbb".to_owned()]
        );
        // Noting the same head again is a no-op.
        ledger.note_head("bbbbbbbb")?;
        assert!(!ledger.entry(&id).ok_or("entry exists")?.stale);
        Ok(())
    }

    #[test]
    fn blank_head_is_rejected_and_leaves_state_unchanged() {
        let mut ledger = FindingLedger::new();
        assert_eq!(ledger.note_head("   "), Err(FindingError::BlankHead));
        assert_eq!(ledger.current_head(), None);
    }

    #[test]
    fn blocking_count_excludes_stale_resolved_and_below_threshold() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.note_head("aaaaaaaa")?;
        let (high, _) = ledger.record(finding(Severity::High, "src/a.rs", Some(1), "rule/a"), 1)?;
        let (_medium, _) =
            ledger.record(finding(Severity::Medium, "src/b.rs", Some(2), "rule/b"), 1)?;
        let (resolved, _) = ledger.record(
            finding(Severity::Critical, "src/c.rs", Some(3), "rule/c"),
            1,
        )?;
        assert_eq!(ledger.blocking_open_count(&Severity::High), 2);
        // Resolve the critical one: no longer blocks.
        let reported: BTreeSet<FindingId> = [high.clone()].into_iter().collect::<BTreeSet<_>>();
        ledger.resolve_unreported(&reported);
        assert_eq!(
            ledger.entry(&resolved).ok_or("entry exists")?.status,
            FindingStatus::Resolved
        );
        assert_eq!(ledger.blocking_open_count(&Severity::High), 1);
        // Head moves: the remaining high finding goes stale and stops blocking.
        ledger.note_head("bbbbbbbb")?;
        assert_eq!(ledger.blocking_open_count(&Severity::High), 0);
        // Re-report at the new head: blocks again.
        ledger.record(finding(Severity::High, "src/a.rs", Some(1), "rule/a"), 2)?;
        assert_eq!(ledger.blocking_open_count(&Severity::High), 1);
        // At the Medium threshold only the re-reported high finding counts:
        // the medium finding was resolved by the completed generation above.
        assert_eq!(ledger.blocking_open_count(&Severity::Medium), 1);
        Ok(())
    }

    #[test]
    fn finding_missing_required_payload_is_rejected() {
        let mut ledger = FindingLedger::new();
        let no_paths = ReviewFinding {
            affected_paths: vec![],
            ..finding(Severity::High, "src/lib.rs", Some(1), "rule/a")
        };
        assert_eq!(
            ledger.record(no_paths, 1).map(|(id, _)| id),
            Err(FindingError::NoAffectedPaths)
        );
        let blank_paths = ReviewFinding {
            affected_paths: vec![PathBuf::from("   ")],
            ..finding(Severity::High, "src/lib.rs", Some(1), "rule/a")
        };
        assert_eq!(
            ledger.record(blank_paths, 1).map(|(id, _)| id),
            Err(FindingError::NoAffectedPaths)
        );
        let blank_rule = ReviewFinding {
            rule: "   ".to_owned(),
            ..finding(Severity::High, "src/lib.rs", Some(1), "rule/a")
        };
        assert_eq!(
            ledger.record(blank_rule, 1).map(|(id, _)| id),
            Err(FindingError::BlankRule)
        );
        let blank_evidence = ReviewFinding {
            evidence: " ".to_owned(),
            ..finding(Severity::High, "src/lib.rs", Some(1), "rule/a")
        };
        assert_eq!(
            ledger.record(blank_evidence, 1).map(|(id, _)| id),
            Err(FindingError::BlankEvidence)
        );
        let blank_remediation = ReviewFinding {
            remediation: "  ".to_owned(),
            ..finding(Severity::High, "src/lib.rs", Some(1), "rule/a")
        };
        assert_eq!(
            ledger.record(blank_remediation, 1).map(|(id, _)| id),
            Err(FindingError::BlankRemediation)
        );
        assert!(ledger.is_empty(), "rejected findings must not be recorded");
    }

    #[test]
    fn mid_generation_note_head_flags_just_recorded_findings_stale() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (id, _) = ledger.record(finding(Severity::High, "src/lib.rs", Some(1), "rule/a"), 1)?;
        assert_eq!(ledger.blocking_open_count(&Severity::High), 1);
        let entry = ledger.entry(&id).ok_or("entry exists")?;
        assert_eq!(entry.heads_seen, Vec::<String>::new());
        ledger.note_head("aaaaaaaa")?;
        assert!(
            ledger.entry(&id).ok_or("entry exists")?.stale,
            "entries recorded before the head was noted are attributed to no \
             head and must not count toward blocking until re-confirmed"
        );
        assert_eq!(ledger.blocking_open_count(&Severity::High), 0);
        ledger.record(finding(Severity::High, "src/lib.rs", Some(1), "rule/a"), 2)?;
        let entry = ledger.entry(&id).ok_or("entry exists")?;
        assert!(!entry.stale);
        assert_eq!(entry.heads_seen, vec!["aaaaaaaa".to_owned()]);
        assert_eq!(ledger.blocking_open_count(&Severity::High), 1);
        Ok(())
    }

    #[test]
    fn backdated_loop_numbers_keep_last_seen_monotonic() -> TestResult {
        let mut ledger = FindingLedger::new();
        let (id, _) = ledger.record(finding(Severity::High, "src/lib.rs", Some(1), "rule/a"), 3)?;
        ledger.record(finding(Severity::High, "src/lib.rs", Some(1), "rule/a"), 1)?;
        let entry = ledger.entry(&id).ok_or("entry exists")?;
        assert_eq!(entry.first_seen_loop, 3);
        assert_eq!(
            entry.last_seen_loop, 3,
            "first_seen_loop <= last_seen_loop must hold even when a caller \
             re-reports with a lower loop number"
        );
        Ok(())
    }

    #[test]
    fn ledger_entry_round_trips_through_json() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.note_head("aaaaaaaa")?;
        let (id, _) =
            ledger.record(finding(Severity::High, "src/lib.rs", Some(10), "9.33.4"), 1)?;
        ledger.note_head("bbbbbbbb")?;
        ledger.record(
            finding(Severity::Medium, "src/lib.rs", Some(10), "9.33.4"),
            2,
        )?;
        let entry = ledger.entry(&id).ok_or("entry exists")?.clone();
        assert_eq!(entry.max_severity, Severity::High);
        assert!(!entry.stale);
        let json = serde_json::to_string_pretty(&entry)?;
        let back: LedgerEntry = serde_json::from_str(&json)?;
        assert_eq!(entry, back);
        Ok(())
    }

    #[test]
    fn empty_ledger_edge_cases() -> TestResult {
        let mut ledger = FindingLedger::new();
        assert!(ledger.is_empty());
        assert_eq!(ledger.len(), 0);
        assert_eq!(ledger.blocking_open_count(&Severity::Minimal), 0);
        assert_eq!(ledger.current_head(), None);
        assert_eq!(ledger.resolve_unreported(&BTreeSet::new()), 0);
        // Noting a head on an empty ledger only records the head.
        ledger.note_head("aaaaaaaa")?;
        assert_eq!(ledger.current_head(), Some("aaaaaaaa"));
        assert_eq!(ledger.entries().count(), 0);
        Ok(())
    }
}
