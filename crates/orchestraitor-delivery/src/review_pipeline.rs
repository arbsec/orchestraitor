//! Change-set review pipeline (spec §9.33.4).
//!
//! Completion of an implementation change set triggers a configurable review
//! pipeline. This module composes the sibling leaf modules into that loop:
//! [`select_reviewers`] (#195) builds the reviewer set from the trigger-time
//! [`ChangeSetProfile`], reviewer findings flow into the [`FindingLedger`]
//! (#196) so identical findings deduplicate and resolve across loops, and
//! [`convergence::evaluate`] (#197) renders the stop/continue/blocked decision
//! after every completed generation against the [`ReviewLoopConfig`] (#198).
//!
//! The pipeline is deterministic, synchronous, and I/O-free — the same pure
//! state-machine discipline as the backlog runner. Every effect (spawning
//! reviewer agents, collecting their reports, moving the change-set head
//! during remediation) belongs to the invocation site, which executes each
//! review generation and hands its result back through the injectable `review`
//! executor of [`ReviewPipeline::trigger`] as a plain-data [`ReviewOutcome`].
//! There is no clock, no provider call, and no agent spawn here; BTree-keyed
//! bookkeeping and append-only journaling make identical inputs produce an
//! identical event sequence.
//!
//! Trigger semantics ([`ReviewPipeline::trigger`]): a change set with no
//! changed files is not an implementation change set, so the pipeline never
//! opens a review loop for it — the journal records a single
//! [`ReviewPipelineEvent::Terminated`] with
//! [`ReviewVerdict::SkippedEmptyChangeSet`]. §9.33.4 names the trigger as
//! completion of an *implementation* change set, and §9.33.8 outlaws looping
//! without work. Any non-empty change set runs generation 0 plus further
//! generations as convergence dictates. Reviewer selection runs once, at
//! trigger time, over the change set's classification; when remediation
//! changes the classification enough to warrant a different roster, the
//! runtime re-triggers a fresh pipeline. Each generation:
//!
//! 1. journals [`ReviewPipelineEvent::GenerationStarted`];
//! 2. hands the generation number and the trigger-time [`ReviewerSet`] to the
//!    injectable executor, receiving back the generation's head (when git
//!    tracking is in use) and findings;
//! 3. notes the head (when provided), records every finding into the ledger
//!    (deduplicating by `(path, line, rule)`), and closes the generation by
//!    resolving every finding it did not re-report;
//! 4. journals [`ReviewPipelineEvent::GenerationRecorded`] with the
//!    per-generation summary;
//! 5. evaluates [`convergence::evaluate`] for the completed loop and journals
//!    [`ReviewPipelineEvent::ConvergenceEvaluated`] with the verbatim verdict;
//! 6. terminates on `Converged` ([`ReviewVerdict::StopNoBlockingFindings`])
//!    or `Blocked` ([`ReviewVerdict::MaxLoopsBlocked`]), or continues into
//!    the next generation on `Continue`.
//!
//! A reviewer report that misses the spec-mandated finding payload (§9.33.4:
//! severity, evidence, affected paths, violated requirement or rule, proposed
//! remediation) terminates the pipeline with [`ReviewVerdict::Failed`] —
//! malformed reviewer output is a §9.33.5 `invalid agent output` condition,
//! so the runtime re-prompts and re-triggers instead of the pipeline ever
//! ignoring it.
//!
//! Never silent approval (the §9.33.4 hard rule): reaching `max_review_loops`
//! or the hard loop ceiling yields [`ReviewVerdict::MaxLoopsBlocked`], an
//! explicit blocked/needs-human state per §9.24 — and there is deliberately
//! no approve/pass verdict. A stopped loop means the caller and the policy
//! layer decide promotion and Arbitraitor approves (§2.2, §9.33.7).
//!
//! Fresh context per review generation (§9.33.1; §9.33.4 "Reviewers MUST use
//! fresh contexts"): constructing each generation's reviewer contexts is the
//! invocation site's duty. The pipeline accounts for it by journaling exactly
//! one [`ReviewPipelineEvent::GenerationStarted`] per generation boundary and
//! by threading no reviewer state between executor invocations; reviewers are
//! never the session that implemented the change (§9.33.4).
//!
//! Configuration honesty: the pipeline honors exactly the knobs the underlying
//! modules expose — `max_review_loops`, `max_reviewers`,
//! `required_reviewer_domains`, `minimum_severity_to_block`, and
//! `stop_when_no_blocking_findings` — through their public APIs, never by
//! re-implementing them. `require_human_review` is surfaced verbatim through
//! [`ReviewPipeline::config`] for the caller's promotion gate (`true` for
//! security-sensitive change sets per §21.1); the pipeline does not gate on
//! it because no pipeline verdict ever grants anything. `allow_same_model`
//! and `require_provider_diversity` are not enforceable at this layer: no
//! reviewer-identity or provider information crosses this module's boundary
//! yet, so the pipeline neither pretends to enforce those knobs nor erases
//! them from the surfaced configuration.
//!
//! Security boundary (spec §2.2, §9.33.7): the pipeline composes selection,
//! bookkeeping, and loop control. It MUST NOT make security decisions
//! (allow/deny/verdict) — policy, approval, promotion, and receipt authority
//! belongs to Arbitraitor.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::convergence::{
    self, BlockedReason, ConvergenceError, ConvergenceInput, ConvergenceVerdict,
};
use crate::findings::{FindingId, FindingLedger, RecordOutcome, ReviewFinding};
use crate::review_loop::{ReviewLoopConfig, ReviewLoopConfigError};
use crate::reviewer_selection::{
    ChangeSetProfile, ReviewerSelectionError, ReviewerSet, select_reviewers,
};

/// Plain-data result of one executed review generation, injected by the
/// runtime (the runner-pattern injectable-outcome contract).
///
/// The runtime spawns the generation's reviewers in fresh contexts
/// (§9.33.1, §9.33.4), consolidates their reports, and returns this record;
/// the pipeline records it verbatim into the ledger and the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReviewOutcome {
    /// Head (e.g. git SHA) the generation ran at. `None` means a local
    /// change-set review without git (the documented choice in
    /// [`crate::convergence`]): head tracking and stale flags stay off. A
    /// non-blank head is noted before recording so a remediation-moved head
    /// flags still-open findings stale until re-confirmed (§9.33.4).
    pub head: Option<String>,
    /// Findings reported by the generation's reviewers. Duplicate reports of
    /// the same `(path, line, rule)` key within one generation collapse in the
    /// ledger like any re-report (§9.33.4 cross-loop deduplication).
    pub findings: Vec<ReviewFinding>,
}

/// Per-generation review execution request handed to the injected executor.
///
/// Carries no accumulated reviewer state: the executor answers with a fresh
/// review of the current change set for this generation (§9.33.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRequest {
    /// 0-based review generation being executed (generation 0 is the trigger
    /// generation). The ledger and convergence inputs count completed loops
    /// 1-based as `generation + 1`.
    pub generation: u32,
    /// Reviewers selected at trigger time for the whole loop; the executor
    /// spawns one fresh-context `(domain, role)` reviewer per slot (§9.19.1,
    /// §9.33.4). Cloned per generation so no borrow crosses the executor
    /// boundary.
    pub reviewers: ReviewerSet,
}

/// Why the pipeline terminated in [`ReviewVerdict::Failed`]. Never an
/// approval or a block — a failed review loop renders no judgment on the
/// change set at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewFailureReason {
    /// A reviewer-reported finding missed a spec-mandated payload element
    /// (§9.33.4: severity, evidence, affected paths, violated rule, proposed
    /// remediation); `detail` carries the ledger's structural error. This is
    /// a §9.33.5 invalid-agent-output condition: the runtime re-prompts and
    /// re-triggers instead of the pipeline dropping the finding.
    MalformedFinding {
        /// The ledger's structural rejection reason (secrets-free, §9.23.4).
        detail: String,
    },
    /// The runtime reported a blank generation head — caller misuse, failed
    /// closed rather than recorded.
    BlankGenerationHead,
}

/// Terminal state of a review pipeline run (spec §9.33.4 stop conditions).
///
/// There is deliberately no approve/pass variant (§2.2, §9.33.7): stopping
/// the loop is the pipeline's only authority. Promotion decisions belong to
/// the caller and the policy layer; approval belongs to Arbitraitor.
/// Serialization is the `snake_case` form persisted in the §9.33.6 durable
/// store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    /// The trigger named no changed files: not an implementation change set,
    /// so no review generation was opened and no reviewer was selected
    /// (§9.33.4, §9.33.8 — never loop without work).
    SkippedEmptyChangeSet,
    /// §9.33.4 "Stop when no blocking findings remain": a full review
    /// generation confirmed at the current head left no open non-stale finding
    /// at or above `minimum_severity_to_block`, and
    /// `stop_when_no_blocking_findings` honored the early stop (verbatim
    /// [`ConvergenceVerdict::Converged`]). Not an approval — the caller and
    /// the policy layer decide promotion.
    StopNoBlockingFindings,
    /// A configured limit was reached before convergence: the explicit
    /// `blocked`/`needs-human` state §9.33.4 and §9.24 mandate — never a
    /// silent approval and never another silently scheduled loop.
    MaxLoopsBlocked {
        /// Which limit forced the stop (`max_review_loops` or the hard loop
        /// ceiling), verbatim [`BlockedReason`].
        reason: BlockedReason,
    },
    /// Reviewer output could not be consumed (see [`ReviewFailureReason`]).
    /// The loop produced no judgment; remediation of the runtime fault and a
    /// re-trigger belong to the invocation site.
    Failed {
        /// Why the run failed.
        reason: ReviewFailureReason,
    },
}

/// One entry of the pipeline's durable decision journal (§9.33.6), analogous
/// to the runner's `RunnerEvent` journal. `snake_case` serde, externally
/// tagged, matching every other delivery-crate record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewPipelineEvent {
    /// The change-set-completion trigger opened a review loop and reviewer
    /// selection ran once over the trigger-time [`ChangeSetProfile`]. The
    /// selected roster is recorded here in full for the durable record
    /// (§9.33.4 reviewer-selection decision).
    Triggered {
        /// The trigger-time reviewer selection for the whole loop.
        reviewers: ReviewerSet,
    },
    /// A new review generation boundary passed; the invocation site spawns
    /// this generation's reviewers in fresh, minimal contexts (§9.33.1,
    /// §9.33.4), never as the session that implemented the change. Exactly
    /// one per generation — the fresh-context accounting record.
    GenerationStarted {
        /// 0-based generation being started.
        generation: u32,
    },
    /// One generation's findings were recorded into the ledger and the
    /// generation was closed (unreported findings resolved, §9.33.4).
    GenerationRecorded {
        /// 0-based generation recorded.
        generation: u32,
        /// Finding reports consumed this generation (pre-dedup).
        reported: usize,
        /// Reports whose `(path, line, rule)` key was seen for the first
        /// time.
        new_findings: usize,
        /// Reports collapsed into existing ledger entries (re-reports,
        /// including re-opened findings; §9.33.4 cross-loop deduplication).
        deduplicated: usize,
        /// Open findings resolved because this completed generation did not
        /// re-report them.
        resolved: usize,
        /// Open, non-stale findings at or above `minimum_severity_to_block`
        /// after recording — the convergence aggregation input.
        blocking_open: usize,
    },
    /// The convergence decision for the just-recorded generation, verbatim
    /// [`ConvergenceVerdict`] (§9.33.4 stop/continue/blocked decision record).
    ConvergenceEvaluated {
        /// 0-based generation the verdict decides.
        generation: u32,
        /// The verbatim convergence verdict (a `continue` verdict journals
        /// the next 1-based loop number).
        verdict: ConvergenceVerdict,
    },
    /// The pipeline reached a terminal state. Final event of every run;
    /// [`ReviewPipeline::verdict`] mirrors its verdict.
    Terminated {
        /// The terminal verdict.
        verdict: ReviewVerdict,
    },
}

/// Construction-time or trigger-time pipeline failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReviewPipelineError {
    /// The review-loop configuration failed structural validation (§9.33.4).
    #[error("invalid review-loop configuration: {0}")]
    InvalidConfig(#[from] ReviewLoopConfigError),
    /// Reviewer selection is unsatisfiable under the configuration (e.g. the
    /// required domains alone exceed `max_reviewers`). Surfaced at
    /// construction — the required-domain/cap arithmetic is
    /// configuration-only — and re-checked fail-closed at trigger time
    /// through the same public API.
    #[error("reviewer selection failed: {0}")]
    Selection(#[from] ReviewerSelectionError),
    /// The pipeline already terminated; one pipeline instance reviews exactly
    /// one change-set trigger. Re-review means a new pipeline (fresh context,
    /// §9.33.1).
    #[error("review pipeline already terminated")]
    AlreadyTerminated,
    /// The per-generation convergence input failed structural validation.
    /// Unreachable when the config validated at construction, generations are
    /// 1-based positive, and blank heads failed closed first — kept fail-closed
    /// rather than unwrapped.
    #[error("convergence input invalid: {0}")]
    Convergence(#[from] ConvergenceError),
}

/// Deterministic review loop over an injectable per-generation executor
/// (spec §9.33.4).
///
/// Construct with [`ReviewPipeline::new`] (fail-closed validation of the
/// [`ReviewLoopConfig`], matching the runner convention), then trigger once
/// with a non-empty [`ChangeSetProfile`] and an executor. The append-only
/// [`ReviewPipelineEvent`] journal is the §9.33.6 durable decision record of
/// everything the pipeline decided.
#[derive(Debug)]
pub struct ReviewPipeline<'a> {
    /// Review-loop configuration in force (borrowed; validated at
    /// construction §9.33.4).
    config: &'a ReviewLoopConfig,
    /// Trigger-time reviewer selection; `None` until a non-empty trigger.
    reviewers: Option<ReviewerSet>,
    /// Deduplicated finding ledger accumulated across generations (#196).
    ledger: FindingLedger,
    /// Append-only durable journal of pipeline events (§9.33.6).
    journal: Vec<ReviewPipelineEvent>,
    /// Terminal verdict once reached.
    verdict: Option<ReviewVerdict>,
}

impl<'a> ReviewPipeline<'a> {
    /// Creates a pipeline bound to `config`, validating the configuration and
    /// the reviewer-selection satisfiability up front (fail-closed, matching
    /// the construction-time validation rule of the other delivery modules).
    ///
    /// # Errors
    ///
    /// Returns [`ReviewPipelineError::InvalidConfig`] when the configuration
    /// fails [`ReviewLoopConfig::validate`], or
    /// [`ReviewPipelineError::Selection`] when the required reviewer domains
    /// alone exceed `max_reviewers` (checked through
    /// [`select_reviewers`], never re-implemented here).
    pub fn new(config: &'a ReviewLoopConfig) -> Result<Self, ReviewPipelineError> {
        config.validate()?;
        // Required-domain/cap satisfiability is configuration-only, so check
        // it now through the selection module's own public API.
        select_reviewers(&ChangeSetProfile::new(), config)?;
        Ok(Self {
            config,
            reviewers: None,
            ledger: FindingLedger::new(),
            journal: Vec::new(),
            verdict: None,
        })
    }

    /// The configuration this pipeline was constructed with (§9.33.4 knob
    /// surface, e.g. `require_human_review` for the caller's promotion gate).
    #[must_use]
    pub const fn config(&self) -> &ReviewLoopConfig {
        self.config
    }

    /// The trigger-time reviewer selection, once a non-empty change set
    /// triggered the loop.
    #[must_use]
    pub const fn reviewers(&self) -> Option<&ReviewerSet> {
        self.reviewers.as_ref()
    }

    /// The deduplicated finding ledger accumulated across generations (§9.33.4).
    #[must_use]
    pub const fn ledger(&self) -> &FindingLedger {
        &self.ledger
    }

    /// The append-only ordered journal of everything the pipeline did and
    /// decided (§9.33.6).
    #[must_use]
    pub fn journal(&self) -> &[ReviewPipelineEvent] {
        &self.journal
    }

    /// The terminal verdict once the pipeline terminated.
    #[must_use]
    pub const fn verdict(&self) -> Option<&ReviewVerdict> {
        self.verdict.as_ref()
    }

    /// Triggers the review loop for a completed change set.
    ///
    /// An empty change set (no changed files) never opens a loop: the journal
    /// records one [`ReviewPipelineEvent::Terminated`] with
    /// [`ReviewVerdict::SkippedEmptyChangeSet`]. Otherwise reviewer selection
    /// runs once over `change_set`, generation 0 starts, and each generation
    /// hands a [`ReviewRequest`] to `review`, whose returned [`ReviewOutcome`]
    /// is recorded into the ledger (deduplicating across generations) before
    /// [`convergence::evaluate`] decides stop/continue/blocked. The loop runs
    /// until convergence, a configured limit
    /// ([`ReviewVerdict::MaxLoopsBlocked`] — never silent approval, §9.33.4),
    /// or an unconsumable reviewer report ([`ReviewVerdict::Failed`]).
    ///
    /// # Errors
    ///
    /// Returns [`ReviewPipelineError::AlreadyTerminated`] when this instance
    /// already completed a trigger, [`ReviewPipelineError::Selection`] when
    /// trigger-time selection fails despite construction-time validation
    /// (fail-closed), or [`ReviewPipelineError::Convergence`] when the
    /// per-generation convergence input fails validation (fail-closed).
    pub fn trigger(
        &mut self,
        change_set: &ChangeSetProfile,
        review: &mut dyn FnMut(&ReviewRequest) -> ReviewOutcome,
    ) -> Result<ReviewVerdict, ReviewPipelineError> {
        if self.verdict.is_some() {
            return Err(ReviewPipelineError::AlreadyTerminated);
        }
        if change_set.changed_files.is_empty() {
            // Not an implementation change set: recording the decline is the
            // durable decision; no loop opens and selection never runs.
            return Ok(self.terminate(ReviewVerdict::SkippedEmptyChangeSet));
        }
        let reviewers = select_reviewers(change_set, self.config)?;
        self.reviewers = Some(reviewers.clone());
        self.push_event(ReviewPipelineEvent::Triggered {
            reviewers: reviewers.clone(),
        });

        let mut generation: u32 = 0;
        loop {
            self.push_event(ReviewPipelineEvent::GenerationStarted { generation });
            let request = ReviewRequest {
                generation,
                reviewers: reviewers.clone(),
            };
            let outcome = review(&request);

            if let Some(head) = outcome.head.as_deref()
                && self.ledger.note_head(head).is_err()
            {
                return Ok(self.terminate(ReviewVerdict::Failed {
                    reason: ReviewFailureReason::BlankGenerationHead,
                }));
            }
            let completed_loop = generation.saturating_add(1);
            let reported_count = outcome.findings.len();
            let mut reported: BTreeSet<FindingId> = BTreeSet::new();
            let mut new_findings = 0usize;
            for finding in outcome.findings {
                match self.ledger.record(finding, completed_loop) {
                    Ok((id, record_outcome)) => {
                        reported.insert(id);
                        if record_outcome == RecordOutcome::New {
                            new_findings = new_findings.saturating_add(1);
                        }
                    }
                    Err(err) => {
                        return Ok(self.terminate(ReviewVerdict::Failed {
                            reason: ReviewFailureReason::MalformedFinding {
                                detail: err.to_string(),
                            },
                        }));
                    }
                }
            }
            let resolved = self.ledger.resolve_unreported(&reported);
            let blocking_open = self
                .ledger
                .blocking_open_count(&self.config.minimum_severity_to_block);
            self.push_event(ReviewPipelineEvent::GenerationRecorded {
                generation,
                reported: reported_count,
                new_findings,
                deduplicated: reported_count - new_findings,
                resolved,
                blocking_open,
            });

            let input = ConvergenceInput::new(&self.ledger, self.config, completed_loop);
            let input = match outcome.head.as_deref() {
                Some(head) => input.with_generation_head(head),
                None => input,
            };
            input.validate()?;
            let verdict = convergence::evaluate(&input);
            self.push_event(ReviewPipelineEvent::ConvergenceEvaluated {
                generation,
                verdict: verdict.clone(),
            });
            match verdict {
                ConvergenceVerdict::Converged => {
                    return Ok(self.terminate(ReviewVerdict::StopNoBlockingFindings));
                }
                ConvergenceVerdict::Blocked { reason } => {
                    return Ok(self.terminate(ReviewVerdict::MaxLoopsBlocked { reason }));
                }
                ConvergenceVerdict::Continue { .. } => {
                    generation = generation.saturating_add(1);
                }
            }
        }
    }

    /// Journals the terminal event and stores the verdict.
    fn terminate(&mut self, verdict: ReviewVerdict) -> ReviewVerdict {
        self.push_event(ReviewPipelineEvent::Terminated {
            verdict: verdict.clone(),
        });
        self.verdict = Some(verdict.clone());
        verdict
    }

    /// Appends one event to the durable journal.
    fn push_event(&mut self, event: ReviewPipelineEvent) {
        self.journal.push(event);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::convergence::BlockedReason;
    use crate::findings::FindingError;
    use crate::metadata::{DomainId, RiskClass};
    use crate::review_loop::Severity;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A non-empty change set: the trigger condition (§9.33.4).
    fn change_set() -> ChangeSetProfile {
        ChangeSetProfile::new().with_changed_files([PathBuf::from("src/lib.rs")])
    }

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

    /// Runs the pipeline with a scripted executor: per-generation outcomes in
    /// order, repeating no findings once the script is exhausted.
    fn scripted(
        pipeline: &mut ReviewPipeline<'_>,
        profile: &ChangeSetProfile,
        outcomes: Vec<ReviewOutcome>,
    ) -> Result<ReviewVerdict, ReviewPipelineError> {
        let mut iter = outcomes.into_iter();
        pipeline.trigger(profile, &mut move |_: &ReviewRequest| {
            iter.next().unwrap_or_default()
        })
    }

    fn quiet_pipeline(
        config: &ReviewLoopConfig,
    ) -> Result<ReviewPipeline<'_>, ReviewPipelineError> {
        ReviewPipeline::new(config)
    }

    #[test]
    fn empty_change_set_is_not_opened() -> TestResult {
        // §9.33.4's trigger is completion of an *implementation* change set;
        // with no changed files no review generation opens and no reviewer is
        // selected. The decline is journaled, not silent.
        let config = ReviewLoopConfig::default();
        let mut pipeline = quiet_pipeline(&config)?;
        let mut invoked = false;
        let verdict = pipeline.trigger(&ChangeSetProfile::new(), &mut |_: &ReviewRequest| {
            invoked = true;
            ReviewOutcome::default()
        })?;
        assert_eq!(verdict, ReviewVerdict::SkippedEmptyChangeSet);
        assert!(!invoked, "executor must never run for an empty change set");
        assert_eq!(
            pipeline.journal(),
            &[ReviewPipelineEvent::Terminated {
                verdict: ReviewVerdict::SkippedEmptyChangeSet,
            }],
            "exactly one journaled event: the decline decision"
        );
        assert!(pipeline.reviewers().is_none());
        assert!(pipeline.ledger().is_empty());
        assert_eq!(
            pipeline.verdict(),
            Some(&ReviewVerdict::SkippedEmptyChangeSet)
        );
        Ok(())
    }

    #[test]
    fn trigger_opens_generation_zero_and_stops_on_no_blocking_findings() -> TestResult {
        // Happy path (todo-31 acceptance 3): a non-empty change set runs
        // generation 0; a clean generation converges immediately under
        // stop_when_no_blocking_findings.
        let config = ReviewLoopConfig::default();
        let mut pipeline = quiet_pipeline(&config)?;
        let profile = change_set();
        let verdict = scripted(&mut pipeline, &profile, Vec::new())?;
        assert_eq!(verdict, ReviewVerdict::StopNoBlockingFindings);
        let selected = select_reviewers(&profile, &config)?;
        assert_eq!(
            pipeline.journal(),
            &[
                ReviewPipelineEvent::Triggered {
                    reviewers: selected,
                },
                ReviewPipelineEvent::GenerationStarted { generation: 0 },
                ReviewPipelineEvent::GenerationRecorded {
                    generation: 0,
                    reported: 0,
                    new_findings: 0,
                    deduplicated: 0,
                    resolved: 0,
                    blocking_open: 0,
                },
                ReviewPipelineEvent::ConvergenceEvaluated {
                    generation: 0,
                    verdict: ConvergenceVerdict::Converged,
                },
                ReviewPipelineEvent::Terminated {
                    verdict: ReviewVerdict::StopNoBlockingFindings,
                },
            ]
        );
        assert_eq!(
            pipeline.verdict(),
            Some(&ReviewVerdict::StopNoBlockingFindings)
        );
        Ok(())
    }

    #[test]
    fn selection_at_trigger_respects_changed_areas_and_required_domains() -> TestResult {
        // Changed-area selection (todo-31 acceptance 3): task domain, coverage,
        // and the security-sensitive trigger flags drive the roster; the
        // default config's required `security` domain is satisfied by the
        // auth/permissions/scripts/execution trigger itself (#195 semantics).
        let config = ReviewLoopConfig::default();
        let profile = change_set()
            .with_task_domain(DomainId::new("backend"))
            .with_coverage_or_verification(true)
            .with_auth_permissions_scripts_execution(true);
        let mut pipeline = quiet_pipeline(&config)?;
        let verdict = scripted(&mut pipeline, &profile, Vec::new())?;
        assert_eq!(verdict, ReviewVerdict::StopNoBlockingFindings);
        let reviewers = pipeline.reviewers().ok_or("reviewers selected")?;
        let domains: Vec<&str> = reviewers.domains().map(DomainId::as_str).collect();
        assert_eq!(domains, ["general", "security", "backend", "testing"]);
        assert!(reviewers.contains_domain(&DomainId::new("security")));
        assert_eq!(
            reviewers.slots()[1].reason,
            crate::reviewer_selection::SelectionReason::SecurityTriggered
        );
        // Selection runs once, at trigger time, and is what every generation
        // executes under.
        assert_eq!(pipeline.journal().len(), 5);
        Ok(())
    }

    #[test]
    fn findings_dedup_across_generations_and_new_ones_append() -> TestResult {
        // Dedup across loops (todo-31 acceptance 3): the same finding reported
        // in generation 0 (twice) and generation 1 collapses to one ledger
        // entry; a new finding appends separately; resolving happens when a
        // completed generation stops reporting.
        let config = ReviewLoopConfig::default();
        let mut pipeline = quiet_pipeline(&config)?;
        let a = || finding(Severity::High, "src/lib.rs", Some(10), "9.33.4");
        let b = || finding(Severity::Medium, "src/main.rs", Some(4), "rule/b");
        let outcomes = vec![
            // Generation 0: A reported twice (same-generation dupes collapse too).
            ReviewOutcome {
                head: None,
                findings: vec![a(), a()],
            },
            // Generation 1: A re-reported (blocking), B reported (non-blocking).
            ReviewOutcome {
                head: None,
                findings: vec![a(), b()],
            },
            // Generation 2: nothing reported; A and B resolve, loop converges.
            ReviewOutcome::default(),
        ];
        let verdict = scripted(&mut pipeline, &change_set(), outcomes)?;
        assert_eq!(verdict, ReviewVerdict::StopNoBlockingFindings);
        let ledger = pipeline.ledger();
        assert_eq!(ledger.len(), 2, "A collapses into one entry; B appends");
        let id_a = FindingId::of(&a())?;
        let entry_a = ledger.entry(&id_a).ok_or("entry A exists")?;
        assert_eq!(
            entry_a.occurrences, 3,
            "gen0 reported twice into one entry, gen1 re-reported"
        );
        assert_eq!(entry_a.first_seen_loop, 1);
        assert_eq!(entry_a.last_seen_loop, 2);
        assert_eq!(entry_a.status, crate::findings::FindingStatus::Resolved);
        // Per-generation summaries pin the dedup accounting.
        let recordings: Vec<(u32, usize, usize, usize)> = pipeline
            .journal()
            .iter()
            .filter_map(|e| match e {
                ReviewPipelineEvent::GenerationRecorded {
                    generation,
                    reported,
                    new_findings,
                    deduplicated,
                    ..
                } => Some((*generation, *reported, *new_findings, *deduplicated)),
                _ => None,
            })
            .collect();
        assert_eq!(
            recordings,
            vec![
                (0, 2, 1, 1), // A twice: one new entry, one same-gen dupe
                (1, 2, 1, 1), // A re-report (dedup) + B (new)
                (2, 0, 0, 0),
            ]
        );
        Ok(())
    }

    #[test]
    fn max_loops_produces_blocked_never_approval() -> TestResult {
        // The §9.33.4 centerpiece (todo-31 QA failure 1): a pipeline that
        // auto-approved after max_review_loops, or silently retried past it,
        // MUST fail this test. The injected reviewer always reports one
        // blocking finding, so the loop can never converge.
        let config = ReviewLoopConfig::default(); // max_review_loops: 3
        let mut pipeline = quiet_pipeline(&config)?;
        let blocking = || ReviewOutcome {
            head: None,
            findings: vec![finding(
                Severity::High,
                "src/lib.rs",
                Some(1),
                "rule/blocker",
            )],
        };
        let verdict = scripted(
            &mut pipeline,
            &change_set(),
            vec![blocking(), blocking(), blocking(), blocking()],
        )?;
        // Reaching the limit is an explicit blocked/needs-human state (§9.24):
        assert_eq!(
            verdict,
            ReviewVerdict::MaxLoopsBlocked {
                reason: BlockedReason::MaxReviewLoops,
            }
        );
        // ... and it is the ONLY way this run terminated: no stop-through-
        // convergence and no verdict outside the never-approve taxonomy could
        // follow without the journal saying so.
        assert!(!matches!(
            verdict,
            ReviewVerdict::StopNoBlockingFindings | ReviewVerdict::SkippedEmptyChangeSet
        ));
        assert_eq!(
            pipeline.journal().last(),
            Some(&ReviewPipelineEvent::Terminated {
                verdict: ReviewVerdict::MaxLoopsBlocked {
                    reason: BlockedReason::MaxReviewLoops,
                },
            })
        );
        // Exactly max_review_loops generations ran — a silently retried extra
        // loop would append a fourth GenerationStarted and fail this pin.
        let started = pipeline
            .journal()
            .iter()
            .filter(|e| matches!(e, ReviewPipelineEvent::GenerationStarted { .. }))
            .count();
        assert_eq!(started, 3);
        // The blocking finding is still open at termination: nothing resolved
        // it into a false convergence.
        assert_eq!(
            pipeline
                .ledger()
                .blocking_open_count(&config.minimum_severity_to_block),
            1
        );
        Ok(())
    }

    #[test]
    fn disabled_stop_flag_runs_full_budget_then_blocks() -> TestResult {
        // With stop_when_no_blocking_findings: false a clean ledger does NOT
        // converge early — and reaching the limit with a clean ledger is still
        // the explicit blocked state, never a silent approval (§9.33.4).
        let config = ReviewLoopConfig {
            max_review_loops: 2,
            stop_when_no_blocking_findings: false,
            ..ReviewLoopConfig::default()
        };
        let mut pipeline = quiet_pipeline(&config)?;
        let verdict = scripted(&mut pipeline, &change_set(), Vec::new())?;
        assert_eq!(
            verdict,
            ReviewVerdict::MaxLoopsBlocked {
                reason: BlockedReason::MaxReviewLoops,
            }
        );
        assert!(
            pipeline.ledger().is_empty(),
            "ledger stayed clean throughout"
        );
        Ok(())
    }

    #[test]
    fn convergence_verdicts_are_recorded_verbatim() -> TestResult {
        // Loop control comes from convergence.rs (#197) verbatim: the journal
        // carries the exact ConvergenceVerdict of each generation.
        let config = ReviewLoopConfig::default();
        let mut pipeline = quiet_pipeline(&config)?;
        let blocking = || ReviewOutcome {
            head: None,
            findings: vec![finding(Severity::High, "src/lib.rs", Some(1), "rule/a")],
        };
        let verdict = scripted(
            &mut pipeline,
            &change_set(),
            vec![blocking(), ReviewOutcome::default()],
        )?;
        assert_eq!(verdict, ReviewVerdict::StopNoBlockingFindings);
        let verdicts: Vec<&ConvergenceVerdict> = pipeline
            .journal()
            .iter()
            .filter_map(|e| match e {
                ReviewPipelineEvent::ConvergenceEvaluated { verdict, .. } => Some(verdict),
                _ => None,
            })
            .collect();
        assert_eq!(
            verdicts,
            [
                &ConvergenceVerdict::Continue { next_loop: 2 },
                &ConvergenceVerdict::Converged,
            ]
        );
        Ok(())
    }

    #[test]
    fn journal_accounts_one_fresh_generation_per_loop() -> TestResult {
        // Fresh-context accounting (§9.33.1, todo-31 acceptance): the journal
        // carries exactly one GenerationStarted per generation boundary, in
        // order, always recorded before its generation's outcome lands.
        let config = ReviewLoopConfig::default();
        let mut pipeline = quiet_pipeline(&config)?;
        let blocking = || ReviewOutcome {
            head: None,
            findings: vec![finding(Severity::High, "src/lib.rs", Some(1), "rule/a")],
        };
        let _ = scripted(&mut pipeline, &change_set(), vec![blocking(), blocking()])?;
        // The third scripted slot is the default empty outcome: gen 0 and 1
        // continue on the blocking finding; gen 2 resolves it and converges.
        let started: Vec<u32> = pipeline
            .journal()
            .iter()
            .filter_map(|e| match e {
                ReviewPipelineEvent::GenerationStarted { generation } => Some(*generation),
                _ => None,
            })
            .collect();
        assert_eq!(started, vec![0, 1, 2]);
        // One trigger-time selection only; per-generation events pair up.
        let sequence: Vec<&str> = pipeline
            .journal()
            .iter()
            .map(|e| match e {
                ReviewPipelineEvent::Triggered { .. } => "triggered",
                ReviewPipelineEvent::GenerationStarted { .. } => "started",
                ReviewPipelineEvent::GenerationRecorded { .. } => "recorded",
                ReviewPipelineEvent::ConvergenceEvaluated { .. } => "evaluated",
                ReviewPipelineEvent::Terminated { .. } => "terminated",
            })
            .collect();
        assert_eq!(
            sequence,
            [
                "triggered",
                "started",
                "recorded",
                "evaluated",
                "started",
                "recorded",
                "evaluated",
                "started",
                "recorded",
                "evaluated",
                "terminated",
            ]
        );
        Ok(())
    }

    #[test]
    fn head_movement_is_tracked_across_generations() -> TestResult {
        // The ledger's note-then-record ordering contract holds: a moved head
        // flags unconfirmed findings stale until re-reported; the pipeline
        // re-notes before recording each generation.
        let config = ReviewLoopConfig::default();
        let mut pipeline = quiet_pipeline(&config)?;
        let a = || finding(Severity::High, "src/lib.rs", Some(9), "rule/a");
        let outcomes = vec![
            ReviewOutcome {
                head: Some("aaaaaaaa".to_owned()),
                findings: vec![a()],
            },
            // Remediation moved the head, but the finding re-reports there.
            ReviewOutcome {
                head: Some("bbbbbbbb".to_owned()),
                findings: vec![a()],
            },
            // The next generation at the same head reports nothing: resolved.
            ReviewOutcome {
                head: Some("bbbbbbbb".to_owned()),
                findings: Vec::new(),
            },
        ];
        let verdict = scripted(&mut pipeline, &change_set(), outcomes)?;
        assert_eq!(verdict, ReviewVerdict::StopNoBlockingFindings);
        let id = FindingId::of(&a())?;
        let entry = pipeline.ledger().entry(&id).ok_or("entry exists")?;
        assert_eq!(
            entry.heads_seen,
            vec!["aaaaaaaa".to_owned(), "bbbbbbbb".to_owned()]
        );
        assert_eq!(entry.status, crate::findings::FindingStatus::Resolved);
        assert_eq!(pipeline.ledger().current_head(), Some("bbbbbbbb"));
        Ok(())
    }

    #[test]
    fn below_threshold_findings_do_not_block() -> TestResult {
        // minimum_severity_to_block is honored through the ledger aggregation:
        // a Medium finding under the default High threshold stops the loop on
        // generation 0.
        let config = ReviewLoopConfig::default();
        let mut pipeline = quiet_pipeline(&config)?;
        let outcomes = vec![ReviewOutcome {
            head: None,
            findings: vec![finding(Severity::Medium, "src/lib.rs", Some(2), "rule/b")],
        }];
        let verdict = scripted(&mut pipeline, &change_set(), outcomes)?;
        assert_eq!(verdict, ReviewVerdict::StopNoBlockingFindings);
        let recorded = pipeline.journal().iter().find_map(|e| match e {
            ReviewPipelineEvent::GenerationRecorded { blocking_open, .. } => Some(*blocking_open),
            _ => None,
        });
        assert_eq!(recorded, Some(0));
        // The finding is still tracked (ledger bookkeeping), just non-blocking.
        assert_eq!(pipeline.ledger().len(), 1);
        Ok(())
    }

    #[test]
    fn malformed_reviewer_finding_fails_closed_never_approves() -> TestResult {
        // A finding missing its spec-mandated payload (§9.33.4) terminates the
        // run as Failed — invalid-agent-output is re-prompted and re-triggered
        // by the runtime, never ignored into an implicit pass.
        let config = ReviewLoopConfig::default();
        let mut pipeline = quiet_pipeline(&config)?;
        let malformed = ReviewFinding {
            evidence: "   ".to_owned(),
            ..finding(Severity::High, "src/lib.rs", Some(1), "rule/a")
        };
        let outcomes = vec![ReviewOutcome {
            head: None,
            findings: vec![malformed],
        }];
        let verdict = scripted(&mut pipeline, &change_set(), outcomes)?;
        assert_eq!(
            verdict,
            ReviewVerdict::Failed {
                reason: ReviewFailureReason::MalformedFinding {
                    detail: FindingError::BlankEvidence.to_string(),
                },
            }
        );
        assert!(!matches!(
            verdict,
            ReviewVerdict::StopNoBlockingFindings | ReviewVerdict::SkippedEmptyChangeSet
        ));
        assert!(
            pipeline.ledger().is_empty(),
            "rejected findings are not recorded"
        );
        assert_eq!(
            pipeline.journal().last(),
            Some(&ReviewPipelineEvent::Terminated {
                verdict: ReviewVerdict::Failed {
                    reason: ReviewFailureReason::MalformedFinding {
                        detail: FindingError::BlankEvidence.to_string(),
                    },
                },
            })
        );
        Ok(())
    }

    #[test]
    fn blank_generation_head_fails_closed() -> TestResult {
        let config = ReviewLoopConfig::default();
        let mut pipeline = quiet_pipeline(&config)?;
        let outcomes = vec![ReviewOutcome {
            head: Some("   ".to_owned()),
            findings: Vec::new(),
        }];
        let verdict = scripted(&mut pipeline, &change_set(), outcomes)?;
        assert_eq!(
            verdict,
            ReviewVerdict::Failed {
                reason: ReviewFailureReason::BlankGenerationHead,
            }
        );
        assert_eq!(pipeline.ledger().current_head(), None);
        Ok(())
    }

    #[test]
    fn construction_rejects_invalid_config() -> TestResult {
        let zero_loops = ReviewLoopConfig {
            max_review_loops: 0,
            ..ReviewLoopConfig::default()
        };
        let Err(err) = ReviewPipeline::new(&zero_loops) else {
            return Err("zero max_review_loops must fail construction".into());
        };
        assert_eq!(
            err,
            ReviewPipelineError::InvalidConfig(ReviewLoopConfigError::ZeroMaxReviewLoops)
        );
        // Required domains exceeding the reviewer cap are unsatisfiable.
        let unsatisfiable = ReviewLoopConfig {
            max_reviewers: 2,
            required_reviewer_domains: vec![
                DomainId::new("security"),
                DomainId::new("backend"),
                DomainId::new("qa"),
            ],
            ..ReviewLoopConfig::default()
        };
        let Err(err) = ReviewPipeline::new(&unsatisfiable) else {
            return Err("unsatisfiable selection must fail construction".into());
        };
        assert_eq!(
            err,
            ReviewPipelineError::Selection(ReviewerSelectionError::ConfigUnsatisfiable {
                required: 3,
                max_reviewers: 2,
            })
        );
        Ok(())
    }

    #[test]
    fn trigger_after_termination_is_rejected() -> TestResult {
        // One pipeline instance reviews exactly one trigger; re-review means a
        // fresh pipeline (fresh-context discipline, §9.33.1).
        let config = ReviewLoopConfig::default();
        let mut pipeline = quiet_pipeline(&config)?;
        let verdict = scripted(&mut pipeline, &change_set(), Vec::new())?;
        assert_eq!(verdict, ReviewVerdict::StopNoBlockingFindings);
        let mut no_op = |_: &ReviewRequest| ReviewOutcome::default();
        let result = pipeline.trigger(&change_set(), &mut no_op);
        assert_eq!(result, Err(ReviewPipelineError::AlreadyTerminated));
        // A terminated pipeline's journal is untouched by the rejected trigger.
        let journal_len = pipeline.journal().len();
        let result = pipeline.trigger(&ChangeSetProfile::new(), &mut no_op);
        assert_eq!(result, Err(ReviewPipelineError::AlreadyTerminated));
        assert_eq!(pipeline.journal().len(), journal_len);
        Ok(())
    }

    #[test]
    fn human_review_requirement_is_surfaced_not_auto_approved() -> TestResult {
        // Security-sensitive change sets (§21.1): require_human_review is
        // surfaced through config for the caller's promotion gate; the
        // pipeline's terminal state is still only ever a loop-control verdict,
        // never an approval.
        let config = ReviewLoopConfig::for_security_sensitive();
        let mut pipeline = quiet_pipeline(&config)?;
        let profile = change_set()
            .with_risk(RiskClass::Critical)
            .with_auth_permissions_scripts_execution(true);
        let verdict = scripted(&mut pipeline, &profile, Vec::new())?;
        assert_eq!(verdict, ReviewVerdict::StopNoBlockingFindings);
        assert!(
            pipeline.config().require_human_review,
            "the §21.1 human-review requirement reaches the caller unmodified"
        );
        assert!(
            pipeline
                .reviewers()
                .ok_or("reviewers")?
                .contains_domain(&DomainId::new("security")),
            "security reviewer is required for security-sensitive change sets"
        );
        Ok(())
    }

    #[test]
    fn same_inputs_produce_identical_journals() -> TestResult {
        // Determinism: identical change set, config, and scripted outcomes
        // yield byte-identical journals across runs (BTree-keyed bookkeeping,
        // no clocks, no randomness).
        let config = ReviewLoopConfig::default();
        let outcomes = || {
            vec![
                ReviewOutcome {
                    head: Some("aaaaaaaa".to_owned()),
                    findings: vec![finding(Severity::High, "src/lib.rs", Some(1), "rule/a")],
                },
                ReviewOutcome {
                    head: Some("bbbbbbbb".to_owned()),
                    findings: Vec::new(),
                },
            ]
        };
        let mut first = quiet_pipeline(&config)?;
        let mut second = quiet_pipeline(&config)?;
        let verdict_first = scripted(&mut first, &change_set(), outcomes())?;
        let verdict_second = scripted(&mut second, &change_set(), outcomes())?;
        assert_eq!(verdict_first, verdict_second);
        assert_eq!(first.journal(), second.journal());
        let json_first = serde_json::to_string(first.journal())?;
        let json_second = serde_json::to_string(second.journal())?;
        assert_eq!(json_first, json_second);
        Ok(())
    }

    #[test]
    fn events_and_verdicts_round_trip_as_snake_case() -> TestResult {
        // Durable-store surface (§9.33.6): every journal event and verdict
        // round-trips through JSON with pinned snake_case tags, matching the
        // runner/convergence serde convention.
        let reviewers = select_reviewers(&change_set(), &ReviewLoopConfig::default())?;
        let events = [
            ReviewPipelineEvent::Triggered { reviewers },
            ReviewPipelineEvent::GenerationStarted { generation: 0 },
            ReviewPipelineEvent::GenerationRecorded {
                generation: 0,
                reported: 2,
                new_findings: 1,
                deduplicated: 1,
                resolved: 0,
                blocking_open: 1,
            },
            ReviewPipelineEvent::ConvergenceEvaluated {
                generation: 0,
                verdict: ConvergenceVerdict::Continue { next_loop: 2 },
            },
            ReviewPipelineEvent::Terminated {
                verdict: ReviewVerdict::MaxLoopsBlocked {
                    reason: BlockedReason::MaxReviewLoops,
                },
            },
            ReviewPipelineEvent::Terminated {
                verdict: ReviewVerdict::StopNoBlockingFindings,
            },
            ReviewPipelineEvent::Terminated {
                verdict: ReviewVerdict::SkippedEmptyChangeSet,
            },
            ReviewPipelineEvent::Terminated {
                verdict: ReviewVerdict::Failed {
                    reason: ReviewFailureReason::BlankGenerationHead,
                },
            },
        ];
        let tags = [
            "triggered",
            "generation_started",
            "generation_recorded",
            "convergence_evaluated",
            "terminated",
            "terminated",
            "terminated",
            "terminated",
        ];
        for (event, tag) in events.iter().zip(tags) {
            let json = serde_json::to_string(event)?;
            assert!(
                json.starts_with(&format!("{{\"{tag}\":")),
                "{tag} tag pins the serde shape, got {json}"
            );
            let back: ReviewPipelineEvent = serde_json::from_str(&json)?;
            assert_eq!(&back, event);
        }
        // Verdict payloads pin too.
        let blocked = ReviewVerdict::MaxLoopsBlocked {
            reason: BlockedReason::MaxReviewLoops,
        };
        assert_eq!(
            serde_json::to_string(&blocked)?,
            r#"{"max_loops_blocked":{"reason":"max_review_loops"}}"#
        );
        assert_eq!(
            serde_json::to_string(&ReviewVerdict::StopNoBlockingFindings)?,
            r#""stop_no_blocking_findings""#
        );
        assert_eq!(
            serde_json::to_string(&ReviewVerdict::SkippedEmptyChangeSet)?,
            r#""skipped_empty_change_set""#
        );
        assert_eq!(
            serde_json::to_string(&ReviewVerdict::Failed {
                reason: ReviewFailureReason::MalformedFinding {
                    detail: "d".to_owned(),
                },
            })?,
            r#"{"failed":{"reason":{"malformed_finding":{"detail":"d"}}}}"#
        );
        let outcome = ReviewOutcome {
            head: Some("aaaaaaaa".to_owned()),
            findings: vec![finding(Severity::Low, "src/lib.rs", Some(3), "rule/c")],
        };
        let back: ReviewOutcome = serde_json::from_str(&serde_json::to_string(&outcome)?)?;
        assert_eq!(back, outcome);
        // Missing keys fall back to the empty outcome; drift fails visibly.
        assert_eq!(
            serde_json::from_str::<ReviewOutcome>("{}")?,
            ReviewOutcome::default()
        );
        assert!(serde_json::from_str::<ReviewOutcome>(r#"{"surprise": true}"#).is_err());
        Ok(())
    }
}
