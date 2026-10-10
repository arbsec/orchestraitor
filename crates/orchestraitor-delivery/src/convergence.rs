//! Review-loop convergence evaluation (spec §9.33.4).
//!
//! The §9.33.4 review loop "[s]top[s] when no blocking findings remain or a
//! configured limit is reached", and "[r]eaching the limit MUST produce an
//! explicit `blocked` or `needs-human` state (per §9.24), never silently
//! approve the result". This module owns that stop/continue decision as a
//! pure, deterministic function over per-generation facts: the findings
//! ledger (#196), the review-loop configuration (#198), the just-completed
//! loop number, and the head the generation ran at.
//!
//! Convergence follows the merge invariant: one full review generation at the
//! current HEAD found no new noteworthy findings and all earlier blocking
//! findings are resolved — "a blocked backlog is a visible state requiring
//! resolution, not an excuse to loop forever" (§9.33.8).
//!
//! Security boundary (spec §9.33.7): this module only proposes loop control
//! (converge / continue / blocked). It MUST NOT make security decisions
//! (allow/deny/verdict) — blocking decisions are made by the runner and
//! policy layer, and all policy, approval, promotion, and receipt authority
//! belongs to Arbitraitor (§2.2).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::findings::{FindingLedger, FindingStatus, LedgerEntry};
use crate::review_loop::{ReviewLoopConfig, ReviewLoopConfigError, Severity};

/// Default hard review-loop ceiling (project `[review] hard_loop_ceiling`:
/// 5, spec §9.33.4): the absolute loop number at or above which only explicit
/// human escalation is allowed — hitting it produces `blocked`/`needs-human`,
/// never silent approval and never another silently scheduled loop.
pub const DEFAULT_HARD_LOOP_CEILING: u32 = 5;

/// Structural validation failure for [`ConvergenceInput`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConvergenceError {
    /// `completed_loop` is zero: convergence requires at least one completed
    /// review generation (§9.33.4: "one full review generation").
    #[error(
        "completed_loop must be at least 1: convergence requires one full completed review generation"
    )]
    NoCompletedLoop,
    /// `generation_head` was provided but is empty or whitespace.
    #[error("generation head, when provided, must be a non-blank identifier")]
    BlankGenerationHead,
    /// `hard_loop_ceiling` is zero; the loop could never run.
    #[error("hard_loop_ceiling must be at least 1")]
    ZeroHardLoopCeiling,
    /// The referenced [`ReviewLoopConfig`] is structurally invalid.
    #[error(transparent)]
    InvalidReviewLoopConfig(#[from] ReviewLoopConfigError),
}

/// Per-generation facts the convergence decision is evaluated from.
///
/// Construct with [`ConvergenceInput::new`] and refine with the `with_*`
/// builders; call [`ConvergenceInput::validate`] before handing the input to
/// a runner. [`evaluate`] itself is total and fails safe: an unvalidated
/// input can refuse convergence but can never amplify into approval.
#[derive(Debug, Clone)]
pub struct ConvergenceInput<'a> {
    /// Findings ledger accumulated across review loops (#196).
    pub finding_ledger: &'a FindingLedger,
    /// Review-loop configuration in force for this change set (#198).
    pub config: &'a ReviewLoopConfig,
    /// Number of the just-completed review loop (1-based). Zero means no
    /// review generation has completed yet and can never converge.
    pub completed_loop: u32,
    /// Head (e.g. git SHA) the just-completed generation ran at, when head
    /// tracking is in use. `None` means a local change-set review without git
    /// (documented choice, see [`evaluate`]): the ledger never sets stale
    /// flags without [`FindingLedger::note_head`], so a completed local
    /// generation with a clean ledger is allowed to converge.
    pub generation_head: Option<String>,
    /// Severity at or above which findings block convergence. Defaults to
    /// [`ReviewLoopConfig::minimum_severity_to_block`] via
    /// [`ConvergenceInput::new`].
    pub blocking_threshold: &'a Severity,
    /// Hard loop ceiling (project `[review] hard_loop_ceiling`). Defaults to
    /// [`DEFAULT_HARD_LOOP_CEILING`] via [`ConvergenceInput::new`].
    pub hard_loop_ceiling: u32,
}

impl<'a> ConvergenceInput<'a> {
    /// Creates an evaluation input from the ledger, configuration, and the
    /// just-completed loop number. `generation_head` starts unset (local
    /// review without git), `blocking_threshold` defaults to
    /// `config.minimum_severity_to_block`, and `hard_loop_ceiling` defaults
    /// to [`DEFAULT_HARD_LOOP_CEILING`].
    #[must_use]
    pub fn new(
        finding_ledger: &'a FindingLedger,
        config: &'a ReviewLoopConfig,
        completed_loop: u32,
    ) -> Self {
        Self {
            finding_ledger,
            config,
            completed_loop,
            generation_head: None,
            blocking_threshold: &config.minimum_severity_to_block,
            hard_loop_ceiling: DEFAULT_HARD_LOOP_CEILING,
        }
    }

    /// Sets the head the just-completed generation ran at.
    #[must_use]
    pub fn with_generation_head(mut self, generation_head: impl Into<String>) -> Self {
        self.generation_head = Some(generation_head.into());
        self
    }

    /// Overrides the severity at or above which findings block convergence.
    #[must_use]
    pub const fn with_blocking_threshold(mut self, blocking_threshold: &'a Severity) -> Self {
        self.blocking_threshold = blocking_threshold;
        self
    }

    /// Overrides the hard loop ceiling.
    #[must_use]
    pub const fn with_hard_loop_ceiling(mut self, hard_loop_ceiling: u32) -> Self {
        self.hard_loop_ceiling = hard_loop_ceiling;
        self
    }

    /// Validates structural invariants: at least one review generation must
    /// have completed, a provided generation head must be non-blank, the hard
    /// ceiling must be at least 1, and the referenced review-loop
    /// configuration must itself be structurally valid.
    ///
    /// # Errors
    ///
    /// Returns the first [`ConvergenceError`] encountered.
    pub fn validate(&self) -> Result<(), ConvergenceError> {
        self.config.validate()?;
        if self.completed_loop == 0 {
            return Err(ConvergenceError::NoCompletedLoop);
        }
        if self.hard_loop_ceiling == 0 {
            return Err(ConvergenceError::ZeroHardLoopCeiling);
        }
        if matches!(&self.generation_head, Some(head) if head.trim().is_empty()) {
            return Err(ConvergenceError::BlankGenerationHead);
        }
        Ok(())
    }
}

/// Why the review loop budget forced an explicit `blocked`/`needs-human`
/// state (§9.24, §9.33.4 — never silent approval).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockedReason {
    /// [`ReviewLoopConfig::max_review_loops`] was reached before the loop
    /// converged.
    MaxReviewLoops,
    /// The hard loop ceiling ([`DEFAULT_HARD_LOOP_CEILING`] unless
    /// overridden) was reached; at or above it only explicit human escalation
    /// is allowed.
    ReviewBudgetHardCeiling,
}

/// Loop-control proposal produced by [`evaluate`].
///
/// A converged verdict means "stop the loop; the runner/policy decides
/// promotion" (§2.2 + §9.33.7). There is deliberately no approve/pass
/// variant: promotion authority stays with the runner, the policy layer, and
/// Arbitraitor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvergenceVerdict {
    /// The just-completed generation is confirmed at the current head, no
    /// open non-stale finding meets the blocking threshold, and no stale
    /// finding would block if re-confirmed. Honors
    /// [`ReviewLoopConfig::stop_when_no_blocking_findings`].
    Converged,
    /// Blocking findings remain, or the configuration demands additional
    /// review rounds, and loop budget remains for another remediation round.
    Continue {
        /// The review loop number to run next (`completed_loop + 1`).
        next_loop: u32,
    },
    /// A configured limit was reached: the loop stops in an explicit
    /// `blocked`/`needs-human` state (§9.24, §9.33.4), never a silent
    /// approval.
    Blocked {
        /// Which limit forced the stop.
        reason: BlockedReason,
    },
}

/// Evaluates the convergence decision for a completed review generation.
///
/// Pure and deterministic: same input, same verdict, no I/O, no security
/// decision (§9.33.7 — the verdict proposes loop control; the runner and
/// policy layer decide promotion, Arbitraitor owns approval).
///
/// Decision order:
///
/// 1. **Hard ceiling with unremediated blocking findings** — blocking
///    findings remain and `completed_loop >= hard_loop_ceiling` →
///    [`BlockedReason::ReviewBudgetHardCeiling`]. The ceiling is the absolute
///    bound above which only explicit human escalation is allowed; it takes
///    precedence over `max_review_loops`.
/// 2. **Converged** — [`blocking_findings`] is empty (so
///    [`FindingLedger::blocking_open_count`] at the threshold is zero), no
///    stale finding *would block* if re-confirmed (earlier blocking findings
///    must actually be resolved — "no new noteworthy findings, and all
///    earlier blocking findings are resolved"), at least one review
///    generation has completed, the generation is confirmed at the current
///    head (see below), and
///    [`ReviewLoopConfig::stop_when_no_blocking_findings`] is set →
///    [`ConvergenceVerdict::Converged`].
/// 3. **Configured budget exhausted** — `completed_loop >=
///    config.max_review_loops` → [`BlockedReason::MaxReviewLoops`].
/// 4. **Absolute hard-ceiling guard** — `completed_loop >=
///    hard_loop_ceiling` without convergence →
///    [`BlockedReason::ReviewBudgetHardCeiling`]. A loop must never be
///    *scheduled* past the ceiling, even under configurations that set
///    `max_review_loops` higher or disable
///    `stop_when_no_blocking_findings`.
/// 5. Otherwise → [`ConvergenceVerdict::Continue`] with `completed_loop + 1`.
///
/// Current-generation confirmation: when `generation_head` is `Some(head)`,
/// it must equal the ledger's current head ([`FindingLedger::current_head`])
/// — otherwise the facts came from an older (or untracked) generation and
/// cannot confirm convergence at `head`. An open entry is *current-generation
/// confirmed* when the last element of its
/// [`LedgerEntry::heads_seen`] equals the generation head;
/// [`FindingLedger::record`] clears the stale flag exactly on such
/// re-reports, so a stale blocking entry signals a blocking state observed
/// against an older head that a new generation has not yet cleared. A `None`
/// generation head means a local change-set review without git (documented
/// choice): head tracking is off, stale flags are never set, and a completed
/// local generation with a clean ledger may converge.
///
/// With `stop_when_no_blocking_findings: false`, a clean ledger does NOT
/// converge early: the loop continues until `max_review_loops` (some
/// organizations require a full extra clean loop), at which point rule 3
/// yields the explicit blocked state (§9.33.4: reaching the limit is
/// `blocked`/`needs-human`, never silent approval).
#[must_use]
pub fn evaluate(input: &ConvergenceInput<'_>) -> ConvergenceVerdict {
    let blocking = blocking_findings(input).len();
    // Rule 1: hard ceiling with unremediated blocking findings.
    if blocking > 0 && input.completed_loop >= input.hard_loop_ceiling {
        return ConvergenceVerdict::Blocked {
            reason: BlockedReason::ReviewBudgetHardCeiling,
        };
    }
    // Rule 2: converged — a clean generation, confirmed at the current head.
    if blocking == 0
        && input.config.stop_when_no_blocking_findings
        && input.completed_loop >= 1
        && stale_blocking_count(input) == 0
        && current_generation_confirmed(input)
    {
        return ConvergenceVerdict::Converged;
    }
    // Rule 3: configured loop budget exhausted.
    if input.completed_loop >= input.config.max_review_loops {
        return ConvergenceVerdict::Blocked {
            reason: BlockedReason::MaxReviewLoops,
        };
    }
    // Rule 4: the hard ceiling is absolute — never schedule a loop past it.
    if input.completed_loop >= input.hard_loop_ceiling {
        return ConvergenceVerdict::Blocked {
            reason: BlockedReason::ReviewBudgetHardCeiling,
        };
    }
    // Rule 5: another remediation round remains.
    ConvergenceVerdict::Continue {
        next_loop: input.completed_loop.saturating_add(1),
    }
}

/// Open, non-stale entries whose *maximum-ever* severity meets the blocking
/// threshold — the findings currently blocking convergence.
///
/// Sorted by [`LedgerEntry::max_severity`] descending, then by dedup key
/// ascending, so the result is deterministic across runs. Filtering uses
/// `max_severity` (mirroring [`FindingLedger::blocking_open_count`]): a
/// finding that once met the threshold keeps blocking until a full review
/// generation stops reporting it, so severity can never be silently
/// downgraded away.
#[must_use]
pub fn blocking_findings<'a>(input: &ConvergenceInput<'a>) -> Vec<&'a LedgerEntry> {
    let mut blocking: Vec<&LedgerEntry> = input
        .finding_ledger
        .entries()
        .filter(|e| {
            e.status == FindingStatus::Open
                && !e.stale
                && e.max_severity.blocks(input.blocking_threshold)
        })
        .collect();
    blocking.sort_by(|a, b| {
        b.max_severity
            .cmp(&a.max_severity)
            .then_with(|| a.id.cmp(&b.id))
    });
    blocking
}

/// Counts open findings that are stale but *would block* if re-confirmed at
/// the current head: a blocking state observed against an older generation of
/// the change set that no generation at the current head has re-reported or
/// resolved yet.
fn stale_blocking_count(input: &ConvergenceInput<'_>) -> usize {
    input
        .finding_ledger
        .entries()
        .filter(|e| {
            e.status == FindingStatus::Open
                && e.stale
                && e.max_severity.blocks(input.blocking_threshold)
        })
        .count()
}

/// Whether the just-completed generation is confirmed to have run at the
/// ledger's current head. See [`evaluate`] for the documented `None`-head
/// (local review without git) choice.
fn current_generation_confirmed(input: &ConvergenceInput<'_>) -> bool {
    match input.generation_head.as_deref().map(str::trim) {
        None => true,
        Some(head) => input.finding_ledger.current_head() == Some(head),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::*;
    use crate::findings::{FindingId, ReviewFinding};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn finding(severity: Severity, path: &str, rule: &str) -> ReviewFinding {
        ReviewFinding {
            severity,
            evidence: format!("evidence for {rule}"),
            affected_paths: vec![PathBuf::from(path)],
            rule: rule.to_owned(),
            line: Some(1),
            line_end: None,
            remediation: format!("remediate {rule}"),
        }
    }

    #[test]
    fn clean_ledger_converges_with_stop_flag() {
        let ledger = FindingLedger::new();
        let config = ReviewLoopConfig::default();
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 1));
        assert_eq!(verdict, ConvergenceVerdict::Converged);
    }

    #[test]
    fn blocking_findings_continue_to_next_loop() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        let config = ReviewLoopConfig::default();
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 1));
        assert_eq!(verdict, ConvergenceVerdict::Continue { next_loop: 2 });
        Ok(())
    }

    #[test]
    fn hard_ceiling_blocks_with_unremediated_blocking() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        // Configured budget above the ceiling: the ceiling must intervene first.
        let config = ReviewLoopConfig {
            max_review_loops: 10,
            ..ReviewLoopConfig::default()
        };
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 5));
        assert_eq!(
            verdict,
            ConvergenceVerdict::Blocked {
                reason: BlockedReason::ReviewBudgetHardCeiling
            }
        );
        Ok(())
    }

    #[test]
    fn hard_ceiling_takes_precedence_over_max_review_loops() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        // Both limits reached: the hard ceiling reason must win.
        let config = ReviewLoopConfig {
            max_review_loops: 5,
            ..ReviewLoopConfig::default()
        };
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 6));
        assert_eq!(
            verdict,
            ConvergenceVerdict::Blocked {
                reason: BlockedReason::ReviewBudgetHardCeiling
            }
        );
        Ok(())
    }

    #[test]
    fn max_review_loops_blocks_when_budget_exhausted() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        let config = ReviewLoopConfig::default();
        // Default max = 3, default ceiling = 5: loop 3 exhausts the budget.
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 3));
        assert_eq!(
            verdict,
            ConvergenceVerdict::Blocked {
                reason: BlockedReason::MaxReviewLoops
            }
        );
        Ok(())
    }

    #[test]
    fn max_review_loops_one_blocks_immediately_with_blocking() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        let config = ReviewLoopConfig {
            max_review_loops: 1,
            ..ReviewLoopConfig::default()
        };
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 1));
        assert_eq!(
            verdict,
            ConvergenceVerdict::Blocked {
                reason: BlockedReason::MaxReviewLoops
            }
        );
        Ok(())
    }

    #[test]
    fn stop_disabled_clean_ledger_continues() {
        let ledger = FindingLedger::new();
        let config = ReviewLoopConfig {
            stop_when_no_blocking_findings: false,
            ..ReviewLoopConfig::default()
        };
        // Clean ledger, but the org requires the full loop budget.
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 1));
        assert_eq!(verdict, ConvergenceVerdict::Continue { next_loop: 2 });
    }

    #[test]
    fn stop_disabled_blocks_at_max_review_loops() {
        let ledger = FindingLedger::new();
        let config = ReviewLoopConfig {
            stop_when_no_blocking_findings: false,
            ..ReviewLoopConfig::default()
        };
        // Reaching the limit with a clean ledger is still an explicit blocked
        // state, never silent approval (§9.33.4).
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 3));
        assert_eq!(
            verdict,
            ConvergenceVerdict::Blocked {
                reason: BlockedReason::MaxReviewLoops
            }
        );
    }

    #[test]
    fn hard_ceiling_guard_blocks_past_ceiling_even_when_clean() {
        let ledger = FindingLedger::new();
        let config = ReviewLoopConfig {
            max_review_loops: 10,
            stop_when_no_blocking_findings: false,
            ..ReviewLoopConfig::default()
        };
        // Rule 4: even a clean ledger must not schedule loop 6 past ceiling 5.
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 5));
        assert_eq!(
            verdict,
            ConvergenceVerdict::Blocked {
                reason: BlockedReason::ReviewBudgetHardCeiling
            }
        );
    }

    #[test]
    fn completed_loop_zero_never_converges() {
        let ledger = FindingLedger::new();
        let config = ReviewLoopConfig::default();
        // No full review generation has run: convergence is impossible.
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 0));
        assert_eq!(verdict, ConvergenceVerdict::Continue { next_loop: 1 });
    }

    #[test]
    fn head_mismatch_prevents_convergence() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.note_head("aaaaaaaa")?;
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        assert_eq!(ledger.resolve_unreported(&BTreeSet::new()), 1);
        ledger.note_head("bbbbbbbb")?;
        let config = ReviewLoopConfig::default();
        // The just-completed generation claims head "aaaaaaaa", but the ledger
        // tracks "bbbbbbbb": facts from an older generation cannot confirm
        // convergence at that head.
        let verdict =
            evaluate(&ConvergenceInput::new(&ledger, &config, 2).with_generation_head("aaaaaaaa"));
        assert_eq!(verdict, ConvergenceVerdict::Continue { next_loop: 3 });
        Ok(())
    }

    #[test]
    fn stale_blocking_findings_prevent_convergence() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.note_head("aaaaaaaa")?;
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        // Head moved: the blocking finding is stale and no longer counts, but
        // must not converge-confirm the older blocking state either.
        ledger.note_head("bbbbbbbb")?;
        assert_eq!(ledger.blocking_open_count(&Severity::High), 0);
        let config = ReviewLoopConfig::default();
        let verdict =
            evaluate(&ConvergenceInput::new(&ledger, &config, 2).with_generation_head("bbbbbbbb"));
        assert_eq!(verdict, ConvergenceVerdict::Continue { next_loop: 3 });
        Ok(())
    }

    #[test]
    fn no_head_tracking_allows_convergence_for_local_review() -> TestResult {
        // Local change-set review without git: no note_head, so stale flags
        // are never set and a completed clean generation converges.
        let mut ledger = FindingLedger::new();
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        let config = ReviewLoopConfig::default();
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 1));
        assert_eq!(verdict, ConvergenceVerdict::Continue { next_loop: 2 });
        assert_eq!(ledger.resolve_unreported(&BTreeSet::new()), 1);
        let verdict = evaluate(&ConvergenceInput::new(&ledger, &config, 2));
        assert_eq!(verdict, ConvergenceVerdict::Converged);
        Ok(())
    }

    #[test]
    fn pipeline_record_then_resolve_converges() -> TestResult {
        // Full loop composition: generation 1 finds a blocking finding;
        // generation 2 at the same head reports nothing and is closed, so the
        // ledger resolves the finding and converges.
        let mut ledger = FindingLedger::new();
        ledger.note_head("aaaaaaaa")?;
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        let config = ReviewLoopConfig::default();
        let loop1 =
            evaluate(&ConvergenceInput::new(&ledger, &config, 1).with_generation_head("aaaaaaaa"));
        assert_eq!(loop1, ConvergenceVerdict::Continue { next_loop: 2 });
        assert_eq!(ledger.resolve_unreported(&BTreeSet::new()), 1);
        let loop2 =
            evaluate(&ConvergenceInput::new(&ledger, &config, 2).with_generation_head("aaaaaaaa"));
        assert_eq!(loop2, ConvergenceVerdict::Converged);
        Ok(())
    }

    #[test]
    fn blocking_findings_are_filtered_and_deterministically_sorted() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.note_head("aaaaaaaa")?;
        ledger.record(finding(Severity::Medium, "src/m.rs", "rule/m"), 1)?;
        ledger.record(finding(Severity::High, "src/b.rs", "rule/b"), 1)?;
        ledger.record(finding(Severity::Critical, "src/c.rs", "rule/c"), 1)?;
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        // Below the threshold: never listed.
        ledger.record(finding(Severity::Low, "src/low.rs", "rule/low"), 1)?;
        // Resolved: never listed.
        let (stale_id, _) = ledger.record(finding(Severity::High, "src/s.rs", "rule/s"), 1)?;
        assert!(
            ledger.resolve_unreported(
                &[
                    FindingId {
                        path: "src/m.rs".to_owned(),
                        line: Some(1),
                        rule: "rule/m".to_owned()
                    },
                    FindingId {
                        path: "src/b.rs".to_owned(),
                        line: Some(1),
                        rule: "rule/b".to_owned()
                    },
                    FindingId {
                        path: "src/c.rs".to_owned(),
                        line: Some(1),
                        rule: "rule/c".to_owned()
                    },
                    FindingId {
                        path: "src/a.rs".to_owned(),
                        line: Some(1),
                        rule: "rule/a".to_owned()
                    },
                    FindingId {
                        path: "src/s.rs".to_owned(),
                        line: Some(1),
                        rule: "rule/s".to_owned()
                    },
                ]
                .into_iter()
                .collect::<BTreeSet<_>>()
            ) == 1,
            "only the low finding resolves"
        );
        // Stale: head moved after its last report, so it is discounted.
        ledger.note_head("bbbbbbbb")?;
        assert!(ledger.entry(&stale_id).ok_or("entry exists")?.stale);
        // Re-report the four current-head findings at the new head.
        ledger.record(finding(Severity::Medium, "src/m.rs", "rule/m"), 2)?;
        ledger.record(finding(Severity::High, "src/b.rs", "rule/b"), 2)?;
        ledger.record(finding(Severity::Critical, "src/c.rs", "rule/c"), 2)?;
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 2)?;

        let config = ReviewLoopConfig::default();
        let threshold = Severity::Medium;
        let input = ConvergenceInput::new(&ledger, &config, 2)
            .with_generation_head("bbbbbbbb")
            .with_blocking_threshold(&threshold);
        let blocking = blocking_findings(&input);
        let order: Vec<(&str, Severity)> = blocking
            .iter()
            .map(|e| (e.id.path.as_str(), e.max_severity))
            .collect();
        assert_eq!(
            order,
            [
                ("src/c.rs", Severity::Critical),
                ("src/a.rs", Severity::High),
                ("src/b.rs", Severity::High),
                ("src/m.rs", Severity::Medium),
            ]
        );
        // evaluate uses the same list: blocking > 0 at loop 2 of 3.
        assert_eq!(
            evaluate(&input),
            ConvergenceVerdict::Continue { next_loop: 3 }
        );
        Ok(())
    }

    #[test]
    fn custom_blocking_threshold_shifts_convergence() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        let config = ReviewLoopConfig::default();
        // With the threshold raised to Critical, the high finding no longer
        // blocks and the clean generation converges.
        let threshold = Severity::Critical;
        let verdict = evaluate(
            &ConvergenceInput::new(&ledger, &config, 1).with_blocking_threshold(&threshold),
        );
        assert_eq!(verdict, ConvergenceVerdict::Converged);
        Ok(())
    }

    #[test]
    fn custom_hard_ceiling_override_blocks_at_override() -> TestResult {
        let mut ledger = FindingLedger::new();
        ledger.record(finding(Severity::High, "src/a.rs", "rule/a"), 1)?;
        let config = ReviewLoopConfig {
            max_review_loops: 10,
            ..ReviewLoopConfig::default()
        };
        let verdict =
            evaluate(&ConvergenceInput::new(&ledger, &config, 2).with_hard_loop_ceiling(2));
        assert_eq!(
            verdict,
            ConvergenceVerdict::Blocked {
                reason: BlockedReason::ReviewBudgetHardCeiling
            }
        );
        Ok(())
    }

    #[test]
    fn verdict_and_blocked_reason_round_trip_as_snake_case() -> TestResult {
        let cases = [
            (ConvergenceVerdict::Converged, r#""converged""#),
            (
                ConvergenceVerdict::Continue { next_loop: 2 },
                r#"{"continue":{"next_loop":2}}"#,
            ),
            (
                ConvergenceVerdict::Blocked {
                    reason: BlockedReason::MaxReviewLoops,
                },
                r#"{"blocked":{"reason":"max_review_loops"}}"#,
            ),
            (
                ConvergenceVerdict::Blocked {
                    reason: BlockedReason::ReviewBudgetHardCeiling,
                },
                r#"{"blocked":{"reason":"review_budget_hard_ceiling"}}"#,
            ),
        ];
        for (verdict, expected) in cases {
            assert_eq!(serde_json::to_string(&verdict)?, expected);
            let back: ConvergenceVerdict = serde_json::from_str(expected)?;
            assert_eq!(back, verdict);
        }
        Ok(())
    }

    #[test]
    fn validate_rejects_unusable_input() -> TestResult {
        let ledger = FindingLedger::new();
        let config = ReviewLoopConfig::default();
        assert_eq!(
            ConvergenceInput::new(&ledger, &config, 0).validate(),
            Err(ConvergenceError::NoCompletedLoop)
        );
        assert_eq!(
            ConvergenceInput::new(&ledger, &config, 1)
                .with_hard_loop_ceiling(0)
                .validate(),
            Err(ConvergenceError::ZeroHardLoopCeiling)
        );
        assert_eq!(
            ConvergenceInput::new(&ledger, &config, 1)
                .with_generation_head("   ")
                .validate(),
            Err(ConvergenceError::BlankGenerationHead)
        );
        // An invalid review-loop config propagates.
        let bad_config = ReviewLoopConfig {
            max_reviewers: 0,
            ..ReviewLoopConfig::default()
        };
        assert_eq!(
            ConvergenceInput::new(&ledger, &bad_config, 1).validate(),
            Err(ConvergenceError::InvalidReviewLoopConfig(
                ReviewLoopConfigError::ZeroMaxReviewers
            ))
        );
        // A fully valid input passes.
        ConvergenceInput::new(&ledger, &config, 1)
            .with_generation_head("aaaaaaaa")
            .validate()?;
        Ok(())
    }
}
