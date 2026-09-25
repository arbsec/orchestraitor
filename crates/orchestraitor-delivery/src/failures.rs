//! Failure classification, retry decisions, and the failure ledger
//! (spec §9.33.5).
//!
//! Spec §9.33.5 requires every delivery error to be persisted with its task
//! and attempt ID, phase, agent/model/provider, normalized error class,
//! retriable status, workspace generation, evidence, partial-results flag,
//! and next retry time — and requires failures to be classified **before**
//! retrying into nine classes (transient provider/network, rate limit,
//! tool/process, verification, merge conflict, invalid agent output, policy
//! denial, approval required, non-retriable configuration or security).
//! This module provides that classification as a pure function
//! ([`classify`]) mapping a [`FailureClass`] plus attempt context to a
//! [`RetryDecision`], the [`FailureRecord`] the durable store (§9.33.6)
//! persists, and the [`FailureLedger`] bookkeeping the runner appends to and
//! queries for retry budgets and escalation history.
//!
//! The dangerous direction is made impossible by construction: policy
//! denials, missing approvals, and non-retriable configuration or security
//! failures ([`FailureClass::PolicyDenial`], [`FailureClass::ApprovalRequired`],
//! [`FailureClass::NonRetriableConfigOrSecurity`]) always classify to
//! non-retry [`RetryDecision`] variants ("Do NOT retry policy denials,
//! missing approvals, or non-retriable security failures as though they were
//! transient errors", §9.33.5). Verification failures and merge conflicts
//! never produce a blind retry either — they demand a root-cause fix or
//! conflict resolution first ([`RetryDecision::FixRootCause`]).
//!
//! Security boundary (spec §9.33.7): classification *proposes*; Arbitraitor
//! owns the policy verdicts (§2.2). Recording a [`FailureClass::PolicyDenial`]
//! is bookkeeping only — this module makes no security decisions
//! (allow/deny/verdict), bypasses no approval requirement, and owns no
//! enforcement. All policy, approval, promotion, and receipt authority
//! belongs to Arbitraitor.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::metadata::BacklogTaskId;

/// Default retry delay in milliseconds when a class is retryable but carries
/// no explicit timing hint. Matches the §9.26.2 backoff base (200 ms); the
/// runner applies the configured bounded exponential backoff with jitter and
/// per-task/per-phase/provider/global retry budgets on top of this proposal.
pub const DEFAULT_RETRY_DELAY_MS: u64 = 200;

/// Normalized delivery-failure class (spec §9.33.5 classification table).
///
/// Exactly the nine classes §9.33.5 mandates; serialization is the
/// `snake_case` form used in the persisted failure log (§9.33.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// Transient provider or network failure (§9.33.5: "retryable, bounded
    /// backoff" — per §9.26.2 base 200 ms, factor 2, cap 30 s, jitter ±20%).
    TransientProviderOrNetwork,
    /// Provider or transport rate limit (§9.33.5: "retryable, honor
    /// retry-after"). A `retry-after` hint from the provider produces
    /// [`RetryDecision::Hold`]; without one it falls back to a bounded
    /// [`RetryDecision::Retry`].
    RateLimit,
    /// Tool or process failure (§9.33.5: "retryable if idempotent").
    /// Callers MUST only classify a tool/process failure with this class when
    /// the operation carries a proven-idempotency marker or its previous
    /// effects have been safely rolled back (§9.26.3, §9.33.5 "NEVER blindly
    /// retry side-effecting actions"); otherwise classify it as
    /// [`FailureClass::NonRetriableConfigOrSecurity`] and escalate.
    ToolOrProcess,
    /// Verification failure (§9.33.5: "NOT retryable blindly — fix the root
    /// cause"). Always classifies to [`RetryDecision::FixRootCause`].
    Verification,
    /// Merge conflict (§9.33.5: "NOT retryable blindly — resolve conflict
    /// first"). Always classifies to [`RetryDecision::FixRootCause`].
    MergeConflict,
    /// Invalid agent output (§9.33.5: "re-prompt with fresh context, limited
    /// retries"). Classifies to [`RetryDecision::Reprompt`] until the
    /// configured reprompt limit, then [`RetryDecision::Escalate`].
    InvalidAgentOutput,
    /// Policy denial (§9.33.5: "NOT retryable — resolve in Arbitraitor").
    /// Always classifies to [`RetryDecision::Escalate`]; the denial itself is
    /// owned by Arbitraitor (§2.2) and recording it here is bookkeeping only.
    PolicyDenial,
    /// Approval required (§9.33.5: "NOT retryable — await user action").
    /// Always classifies to [`RetryDecision::AwaitUser`].
    ApprovalRequired,
    /// Non-retriable configuration or security failure (§9.33.5: "NOT
    /// retryable — escalate"). Always classifies to
    /// [`RetryDecision::Escalate`].
    NonRetriableConfigOrSecurity,
}

/// Delivery phase in which a failure occurred (spec §9.33.5 "phase").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryPhase {
    /// Decompose a spec section into leaf tasks (§9.33.2-§9.33.3).
    Decompose,
    /// Implement a task in its isolated workspace.
    Implement,
    /// Run the task's required verification checks (§9.33.3).
    Verify,
    /// Run the adversarial review loop (§9.33.4).
    Review,
    /// Implement remediation fixes for review findings (§9.33.4).
    Remediate,
}

/// One persisted delivery failure (spec §9.33.5 "Persist every error with:"
/// field list), aligned with the §9.24.2 stable operation/correlation ID for
/// tracing.
///
/// Plain data only — no clock and no I/O: the runner stamps
/// [`FailureRecord::next_retry_ms`] from its own clock when the
/// [`RetryDecision`] schedules a retry. Entries persist in the §9.33.6
/// durable store as serialized [`FailureRecord`] values.
///
/// Secrets discipline (§9.23.4): `agent`, `model`, `provider`, `evidence`,
/// and `correlation_id` are display/trace strings and MUST NEVER embed
/// secrets, headers, cookies, signed URLs, or approval tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureRecord {
    /// Stable task this failure belongs to (spec §9.33.5 "task ID").
    pub task: BacklogTaskId,
    /// 1-based attempt number of the run that failed (spec §9.33.5
    /// "attempt ID").
    pub attempt: u32,
    /// Delivery phase the failure occurred in.
    pub phase: DeliveryPhase,
    /// Display name of the agent that produced the failure. Never a secret:
    /// display strings only (§9.23.4).
    pub agent: String,
    /// Display name of the model in use, when applicable. Never a secret:
    /// display strings only (§9.23.4).
    pub model: String,
    /// Display name of the provider serving the model. Never a secret:
    /// display strings only (§9.23.4).
    pub provider: String,
    /// Normalized failure class assigned before any retry (§9.33.5
    /// "normalized error class").
    pub class: FailureClass,
    /// Whether the runner considers this occurrence retryable given the
    /// operation's idempotency evidence (§9.26.3) — the class decision from
    /// [`classify`] must agree: a record the runner marks retryable for
    /// [`FailureClass::Verification`], [`FailureClass::MergeConflict`],
    /// [`FailureClass::PolicyDenial`], [`FailureClass::ApprovalRequired`], or
    /// [`FailureClass::NonRetriableConfigOrSecurity`] is a runner bug, never
    /// a signal to retry anyway.
    pub retriable: bool,
    /// Workspace generation the failure was observed against (spec §9.33.5
    /// "workspace generation"; §9.24.2 checkpoint ordering).
    pub workspace_generation: u64,
    /// Logs and evidence backing the classification (spec §9.33.5 "relevant
    /// logs and evidence"). Sanitized before persistence: never secrets,
    /// headers, cookies, signed URLs, or approval tokens (§9.23.4).
    pub evidence: String,
    /// Whether the failed attempt left partial results (spec §9.33.5
    /// "partial results"; §9.26.4 partial-stream preservation).
    pub partial_results: bool,
    /// Scheduled wall-clock time of the next retry in milliseconds since the
    /// Unix epoch, stamped by the runner when the [`RetryDecision`] schedules
    /// a retry; `None` for non-retry decisions (spec §9.33.5 "next retry
    /// time").
    pub next_retry_ms: Option<u64>,
    /// Stable correlation ID matching the §9.24.2 operation ID so logs,
    /// receipts, and checkpoints join up across the chain (§9.33.5 tracing;
    /// "even those MUST include a correlation ID"). Never a secret (§9.23.4).
    pub correlation_id: String,
}

/// What the runner should do after a failure (spec §9.33.5 classification
/// outcomes). This is a proposal consumed by the runner loop — never a policy
/// verdict (§9.33.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryDecision {
    /// Retry after `delay_ms` with the configured bounded exponential backoff
    /// with jitter (§9.26.2). Only ever proposed for transient,
    /// rate-limit-without-hint, and idempotent tool/process classes.
    Retry {
        /// Delay before the retry, in milliseconds.
        delay_ms: u64,
    },
    /// Hold until `retry_after_ms` from the provider's `retry-after` hint and
    /// then retry (§9.33.5: rate limit "honor retry-after").
    Hold {
        /// Provider-supplied wait before retry, in milliseconds.
        retry_after_ms: u64,
    },
    /// Do not retry blindly: fix the root cause (verification failure) or
    /// resolve the conflict first (merge conflict) and re-enter the phase.
    FixRootCause,
    /// Re-prompt with a fresh context (invalid agent output), bounded by the
    /// configured reprompt limit. `attempt` is the 1-based number of the
    /// reprompt run that should be launched next.
    Reprompt {
        /// 1-based attempt number of the next (fresh-context) run.
        attempt: u32,
    },
    /// Await user action (approval required). The approval itself is owned by
    /// Arbitraitor (§2.2, §9.9); this decision only parks the task.
    AwaitUser,
    /// Escalate to the §9.33.5 escalation ladder (fresh context → alternate
    /// model → domain expert → revised plan → human). Terminal for policy
    /// denials and non-retriable configuration or security failures.
    Escalate,
}

/// Classifies a delivery failure into the runner's next action (spec
/// §9.33.5 classification table, extending §9.26.1).
///
/// Pure and total over its inputs. `attempt` is the 1-based attempt that
/// just failed; `retry_after_ms` carries the provider's `retry-after` hint
/// for [`FailureClass::RateLimit`]; `max_reprompt_attempts` is the configured
/// reprompt budget for [`FailureClass::InvalidAgentOutput`].
///
/// Non-retriable-by-spec classes never yield a retry regardless of any
/// input: [`FailureClass::Verification`] and [`FailureClass::MergeConflict`]
/// yield [`RetryDecision::FixRootCause`], [`FailureClass::ApprovalRequired`]
/// yields [`RetryDecision::AwaitUser`], and [`FailureClass::PolicyDenial`] /
/// [`FailureClass::NonRetriableConfigOrSecurity`] yield
/// [`RetryDecision::Escalate`] always.
#[must_use]
pub fn classify(
    class: FailureClass,
    attempt: u32,
    retry_after_ms: Option<u64>,
    max_reprompt_attempts: u32,
) -> RetryDecision {
    match class {
        FailureClass::TransientProviderOrNetwork | FailureClass::ToolOrProcess => {
            RetryDecision::Retry {
                delay_ms: DEFAULT_RETRY_DELAY_MS,
            }
        }
        FailureClass::RateLimit => match retry_after_ms {
            Some(retry_after_ms) => RetryDecision::Hold { retry_after_ms },
            None => RetryDecision::Retry {
                delay_ms: DEFAULT_RETRY_DELAY_MS,
            },
        },
        FailureClass::Verification | FailureClass::MergeConflict => RetryDecision::FixRootCause,
        FailureClass::InvalidAgentOutput => {
            if attempt >= max_reprompt_attempts {
                RetryDecision::Escalate
            } else {
                RetryDecision::Reprompt {
                    attempt: attempt.saturating_add(1),
                }
            }
        }
        FailureClass::ApprovalRequired => RetryDecision::AwaitUser,
        FailureClass::PolicyDenial | FailureClass::NonRetriableConfigOrSecurity => {
            RetryDecision::Escalate
        }
    }
}

/// Append-only bookkeeping for every delivery failure (spec §9.33.5
/// "Persist every error").
///
/// The runner appends each classified failure ([`FailureLedger::record`]) and
/// consults the ledger for retry-budget and escalation inputs —
/// [`FailureLedger::count_for_class`], [`FailureLedger::records_for_task`],
/// and [`FailureLedger::retriable_classes`]. These counts are bookkeeping
/// only, not verdicts: the retry action itself is decided per occurrence by
/// [`classify`] and enforced by the runner/policy layer (§9.33.7). Entries
/// persist in the §9.33.6 durable store as serialized [`FailureRecord`]
/// values.
#[derive(Debug, Default)]
pub struct FailureLedger {
    records: Vec<FailureRecord>,
}

impl FailureLedger {
    /// Creates an empty ledger.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    /// Appends one classified failure record, preserving arrival order.
    pub fn record(&mut self, record: FailureRecord) {
        self.records.push(record);
    }

    /// Iterates all recorded failures in arrival order.
    pub fn records(&self) -> impl Iterator<Item = &FailureRecord> {
        self.records.iter()
    }

    /// Iterates the recorded failures for one task, in arrival order.
    pub fn records_for_task<'a>(
        &'a self,
        task: &'a BacklogTaskId,
    ) -> impl Iterator<Item = &'a FailureRecord> {
        self.records.iter().filter(move |r| &r.task == task)
    }

    /// How many occurrences of `class` have been recorded.
    #[must_use]
    pub fn count_for_class(&self, class: FailureClass) -> usize {
        self.records.iter().filter(|r| r.class == class).count()
    }

    /// Per-class occurrence counts for every class observed so far, ordered
    /// by [`FailureClass`].
    pub fn counts(&self) -> impl Iterator<Item = (FailureClass, usize)> {
        let mut counts: BTreeMap<FailureClass, usize> = BTreeMap::new();
        for record in &self.records {
            counts
                .entry(record.class)
                .and_modify(|n| *n = n.saturating_add(1))
                .or_insert(1);
        }
        counts.into_iter()
    }

    /// Distinct classes that have been recorded with
    /// [`FailureRecord::retriable`] set. Bookkeeping only — the runner
    /// consumes [`RetryDecision`] from [`classify`] for the actual retry
    /// action; a class appearing here is never a license to retry a record
    /// whose class is non-retriable by spec (§9.33.5).
    #[must_use]
    pub fn retriable_classes(&self) -> BTreeSet<FailureClass> {
        self.records
            .iter()
            .filter(|r| r.retriable)
            .map(|r| r.class)
            .collect()
    }

    /// Number of recorded failures.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// True when no failure has ever been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const ALL_CLASSES: [FailureClass; 9] = [
        FailureClass::TransientProviderOrNetwork,
        FailureClass::RateLimit,
        FailureClass::ToolOrProcess,
        FailureClass::Verification,
        FailureClass::MergeConflict,
        FailureClass::InvalidAgentOutput,
        FailureClass::PolicyDenial,
        FailureClass::ApprovalRequired,
        FailureClass::NonRetriableConfigOrSecurity,
    ];

    fn record(task: &str, class: FailureClass, retriable: bool) -> FailureRecord {
        FailureRecord {
            task: BacklogTaskId::new(task),
            attempt: 1,
            phase: DeliveryPhase::Implement,
            agent: "coder-1".to_owned(),
            model: "model-x".to_owned(),
            provider: "provider-y".to_owned(),
            class,
            retriable,
            workspace_generation: 7,
            evidence: "command exited 1".to_owned(),
            partial_results: false,
            next_retry_ms: None,
            correlation_id: "op-123".to_owned(),
        }
    }

    #[test]
    fn classify_transient_retries_with_default_backoff() {
        assert_eq!(
            classify(FailureClass::TransientProviderOrNetwork, 1, None, 3),
            RetryDecision::Retry {
                delay_ms: DEFAULT_RETRY_DELAY_MS,
            }
        );
    }

    #[test]
    fn classify_rate_limit_with_retry_after_holds_and_propagates_hint() {
        assert_eq!(
            classify(FailureClass::RateLimit, 2, Some(1_500), 3),
            RetryDecision::Hold {
                retry_after_ms: 1_500,
            }
        );
    }

    #[test]
    fn classify_rate_limit_without_retry_after_retries_with_default() {
        assert_eq!(
            classify(FailureClass::RateLimit, 1, None, 3),
            RetryDecision::Retry {
                delay_ms: DEFAULT_RETRY_DELAY_MS,
            }
        );
    }

    #[test]
    fn classify_tool_or_process_retries() {
        assert_eq!(
            classify(FailureClass::ToolOrProcess, 1, Some(60_000), 3),
            RetryDecision::Retry {
                delay_ms: DEFAULT_RETRY_DELAY_MS,
            }
        );
    }

    #[test]
    fn classify_verification_demands_root_cause_fix_at_any_attempt() {
        for attempt in 1..=5 {
            assert_eq!(
                classify(FailureClass::Verification, attempt, None, 3),
                RetryDecision::FixRootCause
            );
        }
    }

    #[test]
    fn classify_merge_conflict_demands_resolution_first() {
        assert_eq!(
            classify(FailureClass::MergeConflict, 1, Some(0), 3),
            RetryDecision::FixRootCause
        );
    }

    #[test]
    fn classify_invalid_output_reprompts_with_fresh_context_within_limit() {
        assert_eq!(
            classify(FailureClass::InvalidAgentOutput, 1, None, 3),
            RetryDecision::Reprompt { attempt: 2 }
        );
    }

    #[test]
    fn classify_invalid_output_escalates_at_reprompt_limit() {
        // attempt == max and attempt > max both exhaust the budget; a zero
        // budget never reprompts.
        for (attempt, max) in [(3, 3), (4, 3), (1, 0)] {
            assert_eq!(
                classify(FailureClass::InvalidAgentOutput, attempt, None, max),
                RetryDecision::Escalate
            );
        }
    }

    #[test]
    fn classify_policy_denial_escalates_never_retries() {
        assert_eq!(
            classify(FailureClass::PolicyDenial, 1, Some(1_000), 3),
            RetryDecision::Escalate
        );
    }

    #[test]
    fn classify_approval_required_awaits_user() {
        assert_eq!(
            classify(FailureClass::ApprovalRequired, 1, None, 3),
            RetryDecision::AwaitUser
        );
    }

    #[test]
    fn classify_non_retriable_config_or_security_escalates() {
        assert_eq!(
            classify(FailureClass::NonRetriableConfigOrSecurity, 1, Some(5), 3),
            RetryDecision::Escalate
        );
    }

    #[test]
    fn all_nine_classes_classify_to_the_expected_decision() {
        // One row per class from the §9.33.5 table; the array must name each
        // class exactly once (compile would fail on a missing arm via
        // exhaustive match in `classify`, this guards input mapping).
        let table = [
            (
                FailureClass::TransientProviderOrNetwork,
                RetryDecision::Retry {
                    delay_ms: DEFAULT_RETRY_DELAY_MS,
                },
            ),
            (
                FailureClass::RateLimit,
                RetryDecision::Hold { retry_after_ms: 42 },
            ),
            (
                FailureClass::ToolOrProcess,
                RetryDecision::Retry {
                    delay_ms: DEFAULT_RETRY_DELAY_MS,
                },
            ),
            (FailureClass::Verification, RetryDecision::FixRootCause),
            (FailureClass::MergeConflict, RetryDecision::FixRootCause),
            (
                FailureClass::InvalidAgentOutput,
                RetryDecision::Reprompt { attempt: 2 },
            ),
            (FailureClass::PolicyDenial, RetryDecision::Escalate),
            (FailureClass::ApprovalRequired, RetryDecision::AwaitUser),
            (
                FailureClass::NonRetriableConfigOrSecurity,
                RetryDecision::Escalate,
            ),
        ];
        assert_eq!(table.len(), ALL_CLASSES.len());
        for (class, expected) in table {
            assert!(ALL_CLASSES.contains(&class), "table covers {class:?}");
            assert_eq!(classify(class, 1, Some(42), 3), expected);
        }
    }

    #[test]
    fn non_retryable_classes_never_yield_a_retry_variant_for_any_inputs() {
        // The dangerous direction must be unreachable: no attempt number,
        // retry-after hint, or reprompt budget may turn these classes into a
        // retry/reprompt (§9.33.5 "Do NOT retry policy denials, missing
        // approvals, or non-retriable security failures").
        let never_retry = [
            FailureClass::Verification,
            FailureClass::MergeConflict,
            FailureClass::PolicyDenial,
            FailureClass::ApprovalRequired,
            FailureClass::NonRetriableConfigOrSecurity,
        ];
        for class in never_retry {
            for attempt in [0, 1, u32::MAX] {
                for retry_after in [None, Some(0), Some(60_000)] {
                    for max_reprompts in [0, 3, u32::MAX] {
                        let decision = classify(class, attempt, retry_after, max_reprompts);
                        assert!(
                            !matches!(
                                decision,
                                RetryDecision::Retry { .. }
                                    | RetryDecision::Hold { .. }
                                    | RetryDecision::Reprompt { .. }
                            ),
                            "{class:?} attempt={attempt} retry_after={retry_after:?} \
                             max={max_reprompts} classified to {decision:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn failure_class_serde_round_trip_and_names() -> TestResult {
        let expected = [
            (
                FailureClass::TransientProviderOrNetwork,
                "transient_provider_or_network",
            ),
            (FailureClass::RateLimit, "rate_limit"),
            (FailureClass::ToolOrProcess, "tool_or_process"),
            (FailureClass::Verification, "verification"),
            (FailureClass::MergeConflict, "merge_conflict"),
            (FailureClass::InvalidAgentOutput, "invalid_agent_output"),
            (FailureClass::PolicyDenial, "policy_denial"),
            (FailureClass::ApprovalRequired, "approval_required"),
            (
                FailureClass::NonRetriableConfigOrSecurity,
                "non_retriable_config_or_security",
            ),
        ];
        for (class, name) in expected {
            let json = serde_json::to_string(&class)?;
            assert_eq!(json, format!("\"{name}\""));
            let back: FailureClass = serde_json::from_str(&json)?;
            assert_eq!(back, class);
        }
        Ok(())
    }

    #[test]
    fn failure_class_rejects_unknown_strings() {
        // Unknown classes can never deserialize into a retry-safe default.
        for bad in ["transient", "unknown", "", "PolicyDenial"] {
            assert!(serde_json::from_str::<FailureClass>(&format!("\"{bad}\"")).is_err());
        }
    }

    #[test]
    fn delivery_phase_serde_round_trip() -> TestResult {
        let phases = [
            (DeliveryPhase::Decompose, "decompose"),
            (DeliveryPhase::Implement, "implement"),
            (DeliveryPhase::Verify, "verify"),
            (DeliveryPhase::Review, "review"),
            (DeliveryPhase::Remediate, "remediate"),
        ];
        for (phase, name) in phases {
            let json = serde_json::to_string(&phase)?;
            assert_eq!(json, format!("\"{name}\""));
            let back: DeliveryPhase = serde_json::from_str(&json)?;
            assert_eq!(back, phase);
        }
        Ok(())
    }

    #[test]
    fn retry_decision_serde_round_trip() -> TestResult {
        let decisions = [
            RetryDecision::Retry { delay_ms: 200 },
            RetryDecision::Hold {
                retry_after_ms: 1_500,
            },
            RetryDecision::FixRootCause,
            RetryDecision::Reprompt { attempt: 2 },
            RetryDecision::AwaitUser,
            RetryDecision::Escalate,
        ];
        for decision in decisions {
            let json = serde_json::to_string(&decision)?;
            let back: RetryDecision = serde_json::from_str(&json)?;
            assert_eq!(back, decision);
        }
        Ok(())
    }

    #[test]
    fn failure_record_serde_round_trip() -> TestResult {
        let mut sample = record("delivery-failures", FailureClass::RateLimit, true);
        sample.next_retry_ms = Some(1_700_000_000_000);
        let json = serde_json::to_string_pretty(&sample)?;
        let back: FailureRecord = serde_json::from_str(&json)?;
        assert_eq!(back, sample);
        Ok(())
    }

    #[test]
    fn ledger_records_queries_and_counts_per_class() {
        let mut ledger = FailureLedger::new();
        assert!(ledger.is_empty());

        ledger.record(record("task-a", FailureClass::RateLimit, true));
        ledger.record(record("task-a", FailureClass::Verification, false));
        ledger.record(record("task-b", FailureClass::RateLimit, true));

        assert_eq!(ledger.len(), 3);
        assert!(!ledger.is_empty());
        assert_eq!(ledger.count_for_class(FailureClass::RateLimit), 2);
        assert_eq!(ledger.count_for_class(FailureClass::Verification), 1);
        assert_eq!(ledger.count_for_class(FailureClass::PolicyDenial), 0);
        assert_eq!(
            ledger
                .records_for_task(&BacklogTaskId::new("task-a"))
                .count(),
            2
        );
        assert_eq!(
            ledger
                .records_for_task(&BacklogTaskId::new("task-b"))
                .count(),
            1
        );
        let counts: BTreeMap<_, _> = ledger.counts().collect();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts.get(&FailureClass::RateLimit), Some(&2));

        // Arrival order is preserved.
        let order: Vec<_> = ledger.records().map(|r| r.class).collect();
        assert_eq!(
            order,
            [
                FailureClass::RateLimit,
                FailureClass::Verification,
                FailureClass::RateLimit
            ]
        );
    }

    #[test]
    fn ledger_retriable_classes_helper_reports_only_flagged_records() {
        let mut ledger = FailureLedger::new();
        assert!(ledger.retriable_classes().is_empty());

        ledger.record(record("task-a", FailureClass::RateLimit, true));
        ledger.record(record("task-a", FailureClass::PolicyDenial, false));
        ledger.record(record("task-b", FailureClass::ToolOrProcess, true));

        let retriable = ledger.retriable_classes();
        assert_eq!(
            retriable,
            BTreeSet::from([FailureClass::RateLimit, FailureClass::ToolOrProcess])
        );
    }
}
