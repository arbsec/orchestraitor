//! Retry gate and bounded backoff schedule (spec §9.33.5, §9.26.3).
//!
//! Spec §9.33.5: "NEVER blindly retry side-effecting actions. Resume from
//! checkpoints (§9.24.2) or retry only when the operation is idempotent or
//! its previous effects have been safely rolled back (per §9.26.3)." This
//! module provides the typed idempotency evidence ([`IdempotencyProof`]), the
//! defensive re-assertion gate ([`RetryGate`]) the runner applies *before*
//! executing any retry, and the bounded backoff proposal helper
//! ([`RetrySchedule`]) that feeds the runner's §9.26.2 backoff.
//!
//! Where [`crate::failures::classify`] *proposes* a [`RetryDecision`] from a
//! [`FailureClass`], [`RetryGate::evaluate`] *re-asserts the classification
//! invariants* at the point of use — the safe path is the default: an
//! unproven side-effecting operation is sent to root-cause analysis instead
//! of retried, and policy denials, approval requirements, and non-retriable
//! configuration or security failures can never be laundered into a retry by
//! any idempotency proof or incoming decision. The runner still owns budgets
//! and jitter (§9.26.2); this module proposes and double-checks only.
//!
//! Security boundary (spec §2.2, §9.33.7): the gate re-asserts bookkeeping
//! invariants only. It makes no security decisions (allow/deny/verdict),
//! bypasses no approval requirement, and owns no enforcement — executing the
//! retried command still goes through Arbitraitor. All policy, approval,
//! promotion, and receipt authority belongs to Arbitraitor.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::failures::{FailureClass, RetryDecision};

/// Default backoff base in milliseconds (§9.26.2: base 200 ms, factor 2).
pub const DEFAULT_BASE_DELAY_MS: u64 = crate::failures::DEFAULT_RETRY_DELAY_MS;

/// Default backoff cap in milliseconds for the delivery runner's bounded
/// retries. The runner's §9.26.2 configuration may tighten this per
/// session/domain/role/provider; this module only enforces that it is never
/// exceeded by this schedule.
pub const DEFAULT_MAX_DELAY_MS: u64 = 10_000;

/// Typed evidence that a side-effecting operation satisfies §9.26.3
/// ("Side-effecting operations must prove idempotency").
///
/// The runner attaches a proof to every candidate retry of a
/// [`FailureClass::ToolOrProcess`] failure; [`RetryGate::evaluate`] refuses a
/// blind retry when the proof is absent ([`IdempotencyProof::Unproven`]).
///
/// Serialization is the `snake_case` form persisted in the §9.33.6 durable
/// store. Secrets discipline (§9.23.4): `marker` and `rollback_txn` are
/// display/trace identifiers and MUST NEVER embed secrets, headers, cookies,
/// signed URLs, or approval tokens.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdempotencyProof {
    /// The retry resumes from a §9.24.2 checkpoint instead of re-executing
    /// the side-effecting prefix, so no effect can be applied twice.
    CheckpointResume,
    /// The operation carries a proven-idempotency marker (§9.26.3): an
    /// optimistic-concurrency digest matching at retry time
    /// (`fs.apply_patch` with `expected_digest`, §9.5), an `idempotency-key`
    /// header for `POST`s, or HTTP `PUT`/`DELETE` semantics. `marker` is a
    /// display identifier for the marker — never a secret (§9.23.4).
    ProvenIdempotent {
        /// Display identifier of the idempotency marker (never a secret,
        /// §9.23.4).
        marker: String,
    },
    /// The previous attempt's effects have been safely rolled back (§9.26.3),
    /// so re-execution starts from the pre-attempt state. `rollback_txn`
    /// identifies the rollback transaction in the §9.5 transaction log.
    EffectsRolledBack {
        /// Identifier of the rollback transaction (never a secret, §9.23.4).
        rollback_txn: String,
    },
    /// No idempotency evidence exists. The runner MUST NOT retry the
    /// side-effecting operation; [`RetryGate::evaluate`] overrides any
    /// retry proposal with [`RetryDecision::FixRootCause`].
    Unproven,
}

/// The safety gate the runner applies before executing any proposed retry
/// (§9.33.5 "NEVER blindly retry side-effecting actions").
///
/// Unit marker type: evaluation is a pure, total, deterministic associated
/// function — see [`RetryGate::evaluate`] for the decision table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RetryGate;

impl RetryGate {
    /// Re-asserts the §9.33.5 classification invariants on a proposed
    /// [`RetryDecision`] before the runner executes it.
    ///
    /// Decision table (first matching row wins; the safe path is the
    /// default):
    ///
    /// | Failure class | Proof | Result |
    /// |---|---|---|
    /// | [`FailureClass::PolicyDenial`] | any | [`RetryDecision::Escalate`] |
    /// | [`FailureClass::ApprovalRequired`] | any | [`RetryDecision::AwaitUser`] |
    /// | [`FailureClass::NonRetriableConfigOrSecurity`] | any | [`RetryDecision::Escalate`] |
    /// | [`FailureClass::Verification`] / [`FailureClass::MergeConflict`] | any | [`RetryDecision::FixRootCause`] |
    /// | [`FailureClass::ToolOrProcess`] | [`IdempotencyProof::Unproven`] | [`RetryDecision::FixRootCause`] (overrides every incoming decision variant) |
    /// | [`FailureClass::ToolOrProcess`] | any proof | pass the incoming decision through (checkpoint/proven/rolled-back all satisfy §9.26.3) |
    /// | [`FailureClass::TransientProviderOrNetwork`] / [`FailureClass::RateLimit`] | any | pass through (already retryable; no proof required) |
    /// | other classes | any | pass the incoming decision through |
    ///
    /// Non-retriable-by-spec classes are double-locked: a proof can never
    /// launder a policy denial, approval requirement, or non-retriable
    /// security failure into a retry, regardless of the incoming decision.
    #[must_use]
    pub fn evaluate(
        class: FailureClass,
        proof: &IdempotencyProof,
        decision: &RetryDecision,
    ) -> RetryDecision {
        if matches!(
            class,
            FailureClass::PolicyDenial | FailureClass::NonRetriableConfigOrSecurity
        ) {
            RetryDecision::Escalate
        } else if class == FailureClass::ApprovalRequired {
            RetryDecision::AwaitUser
        } else if matches!(
            class,
            FailureClass::Verification | FailureClass::MergeConflict
        ) || (class == FailureClass::ToolOrProcess
            && *proof == IdempotencyProof::Unproven)
        {
            RetryDecision::FixRootCause
        } else {
            *decision
        }
    }
}

/// Structural validation failure for [`RetrySchedule`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RetryScheduleError {
    /// `base_delay_ms` is zero; a zero base collapses the bounded exponential
    /// backoff (§9.26.2) into a hot retry loop.
    #[error("base_delay_ms must be greater than zero")]
    ZeroBaseDelay,
    /// `max_delay_ms` is below `base_delay_ms`; the cap can never be
    /// reached without violating the base.
    #[error("max_delay_ms must be greater than or equal to base_delay_ms")]
    MaxBelowBase,
}

/// Bounded backoff proposal helper for the runner's §9.26.2 retry timing.
///
/// Doubles `base_delay_ms` per [`RetrySchedule::attempt`], saturating, and
/// caps at `max_delay_ms` ([`RetrySchedule::next_delay`]). Deliberately adds
/// **no jitter**: the runner applies the §9.26.2 jitter (±20%), per-task /
/// per-phase / provider / global retry budgets, and cancellation handling on
/// top of this deterministic proposal. Defaults are
/// [`DEFAULT_BASE_DELAY_MS`] (§9.26.2 base 200 ms) and
/// [`DEFAULT_MAX_DELAY_MS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrySchedule {
    /// First backoff delay in milliseconds; must be non-zero (§9.26.2 base).
    pub base_delay_ms: u64,
    /// Hard cap on any proposed delay in milliseconds; must be at least
    /// `base_delay_ms`.
    pub max_delay_ms: u64,
    /// 0-based number of retries already proposed by this schedule; attempt 0
    /// yields `base_delay_ms`.
    pub attempt: u32,
}

impl Default for RetrySchedule {
    fn default() -> Self {
        Self {
            base_delay_ms: DEFAULT_BASE_DELAY_MS,
            max_delay_ms: DEFAULT_MAX_DELAY_MS,
            attempt: 0,
        }
    }
}

impl RetrySchedule {
    /// Next proposed delay in milliseconds: `base_delay_ms * 2^attempt`,
    /// saturating, capped at `max_delay_ms`. Never panics, even at the `u64`
    /// boundary.
    #[must_use]
    pub fn next_delay(&self) -> u64 {
        let factor = 2u64.checked_pow(self.attempt).unwrap_or(u64::MAX);
        self.base_delay_ms
            .saturating_mul(factor)
            .min(self.max_delay_ms)
    }

    /// Validates the schedule's structural invariants: non-zero base and
    /// `max_delay_ms >= base_delay_ms`.
    ///
    /// # Errors
    ///
    /// Returns [`RetryScheduleError::ZeroBaseDelay`] when `base_delay_ms` is
    /// zero, or [`RetryScheduleError::MaxBelowBase`] when `max_delay_ms` is
    /// below `base_delay_ms`.
    pub const fn validate(&self) -> Result<(), RetryScheduleError> {
        if self.base_delay_ms == 0 {
            return Err(RetryScheduleError::ZeroBaseDelay);
        }
        if self.max_delay_ms < self.base_delay_ms {
            return Err(RetryScheduleError::MaxBelowBase);
        }
        Ok(())
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

    fn proofs() -> [IdempotencyProof; 4] {
        [
            IdempotencyProof::Unproven,
            IdempotencyProof::CheckpointResume,
            IdempotencyProof::ProvenIdempotent {
                marker: "idempotency-key:op-123".to_owned(),
            },
            IdempotencyProof::EffectsRolledBack {
                rollback_txn: "txn-9".to_owned(),
            },
        ]
    }

    fn decisions() -> [RetryDecision; 6] {
        [
            RetryDecision::Retry { delay_ms: 200 },
            RetryDecision::Hold {
                retry_after_ms: 1_500,
            },
            RetryDecision::FixRootCause,
            RetryDecision::Reprompt { attempt: 2 },
            RetryDecision::AwaitUser,
            RetryDecision::Escalate,
        ]
    }

    #[test]
    fn gate_unproven_tool_failure_overrides_any_incoming_retry() {
        // §9.33.5 "NEVER blindly retry side-effecting actions": a ToolOrProcess
        // failure without idempotency evidence must not execute a retry, even
        // when the incoming decision proposes one.
        assert_eq!(
            RetryGate::evaluate(
                FailureClass::ToolOrProcess,
                &IdempotencyProof::Unproven,
                &RetryDecision::Retry { delay_ms: 200 },
            ),
            RetryDecision::FixRootCause
        );
    }

    #[test]
    fn gate_unproven_tool_failure_overrides_every_decision_variant() {
        for decision in decisions() {
            assert_eq!(
                RetryGate::evaluate(
                    FailureClass::ToolOrProcess,
                    &IdempotencyProof::Unproven,
                    &decision,
                ),
                RetryDecision::FixRootCause,
                "Unproven tool failure must override {decision:?}"
            );
        }
    }

    #[test]
    fn gate_tool_failure_with_any_proof_passes_decision_through() {
        // Checkpoint resume, proven idempotency, and rolled-back effects each
        // satisfy §9.26.3, so the incoming decision survives untouched.
        for proof in [
            IdempotencyProof::CheckpointResume,
            IdempotencyProof::ProvenIdempotent {
                marker: "m".to_owned(),
            },
            IdempotencyProof::EffectsRolledBack {
                rollback_txn: "t".to_owned(),
            },
        ] {
            for decision in decisions() {
                assert_eq!(
                    RetryGate::evaluate(FailureClass::ToolOrProcess, &proof, &decision),
                    decision,
                    "proof {proof:?} must pass {decision:?} through"
                );
            }
        }
    }

    #[test]
    fn gate_policy_denial_can_never_be_laundered_by_proof() {
        // Double-lock: no proof variant and no incoming decision may turn a
        // policy denial into anything but escalation (§9.33.5 "Do NOT retry
        // policy denials").
        for proof in proofs() {
            for decision in decisions() {
                assert_eq!(
                    RetryGate::evaluate(FailureClass::PolicyDenial, &proof, &decision),
                    RetryDecision::Escalate,
                    "proof {proof:?} + {decision:?} must still escalate"
                );
            }
        }
    }

    #[test]
    fn gate_approval_required_always_awaits_user() {
        for proof in proofs() {
            for decision in decisions() {
                assert_eq!(
                    RetryGate::evaluate(FailureClass::ApprovalRequired, &proof, &decision),
                    RetryDecision::AwaitUser,
                );
            }
        }
    }

    #[test]
    fn gate_non_retriable_config_or_security_always_escalates() {
        for proof in proofs() {
            for decision in decisions() {
                assert_eq!(
                    RetryGate::evaluate(
                        FailureClass::NonRetriableConfigOrSecurity,
                        &proof,
                        &decision,
                    ),
                    RetryDecision::Escalate,
                );
            }
        }
    }

    #[test]
    fn gate_verification_and_merge_conflict_always_fix_root_cause() {
        for class in [FailureClass::Verification, FailureClass::MergeConflict] {
            for proof in proofs() {
                for decision in decisions() {
                    assert_eq!(
                        RetryGate::evaluate(class, &proof, &decision),
                        RetryDecision::FixRootCause,
                    );
                }
            }
        }
    }

    #[test]
    fn gate_transient_passes_through_regardless_of_proof() {
        for proof in proofs() {
            for decision in decisions() {
                assert_eq!(
                    RetryGate::evaluate(
                        FailureClass::TransientProviderOrNetwork,
                        &proof,
                        &decision,
                    ),
                    decision,
                );
            }
        }
    }

    #[test]
    fn gate_rate_limit_hold_propagates_hint_intact() {
        assert_eq!(
            RetryGate::evaluate(
                FailureClass::RateLimit,
                &IdempotencyProof::Unproven,
                &RetryDecision::Hold {
                    retry_after_ms: 1_500,
                },
            ),
            RetryDecision::Hold {
                retry_after_ms: 1_500,
            }
        );
    }

    #[test]
    fn gate_table_covers_every_class_proof_decision_row() {
        // Exhaustive (class × proof × decision) sweep: every row yields the
        // decision-table outcome and nothing else.
        for class in ALL_CLASSES {
            for proof in proofs() {
                for decision in decisions() {
                    let expected = if matches!(
                        class,
                        FailureClass::PolicyDenial | FailureClass::NonRetriableConfigOrSecurity
                    ) {
                        RetryDecision::Escalate
                    } else if class == FailureClass::ApprovalRequired {
                        RetryDecision::AwaitUser
                    } else if matches!(
                        class,
                        FailureClass::Verification | FailureClass::MergeConflict
                    ) || (class == FailureClass::ToolOrProcess
                        && proof == IdempotencyProof::Unproven)
                    {
                        RetryDecision::FixRootCause
                    } else {
                        decision
                    };
                    assert_eq!(
                        RetryGate::evaluate(class, &proof, &decision),
                        expected,
                        "row {class:?} × {proof:?} × {decision:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn gate_evaluation_is_deterministic() {
        let first = RetryGate::evaluate(
            FailureClass::ToolOrProcess,
            &IdempotencyProof::Unproven,
            &RetryDecision::Retry { delay_ms: 200 },
        );
        for _ in 0..100 {
            assert_eq!(
                first,
                RetryGate::evaluate(
                    FailureClass::ToolOrProcess,
                    &IdempotencyProof::Unproven,
                    &RetryDecision::Retry { delay_ms: 200 },
                )
            );
        }
    }

    #[test]
    fn idempotency_proof_serde_round_trip() -> TestResult {
        for proof in proofs() {
            let json = serde_json::to_string(&proof)?;
            let back: IdempotencyProof = serde_json::from_str(&json)?;
            assert_eq!(back, proof);
        }
        assert_eq!(
            serde_json::to_string(&IdempotencyProof::CheckpointResume)?,
            "\"checkpoint_resume\""
        );
        Ok(())
    }

    #[test]
    fn idempotency_proof_rejects_unknown_strings() {
        for bad in ["idempotent", "unproven_proof", "", "CheckpointResume"] {
            assert!(serde_json::from_str::<IdempotencyProof>(&format!("\"{bad}\"")).is_err());
        }
    }

    #[test]
    fn retry_schedule_default_matches_spec_backoff_base() {
        let schedule = RetrySchedule::default();
        assert_eq!(schedule.base_delay_ms, 200);
        assert_eq!(schedule.max_delay_ms, 10_000);
        assert_eq!(schedule.attempt, 0);
        assert!(schedule.validate().is_ok());
    }

    #[test]
    fn retry_schedule_doubles_and_caps() {
        let expected = [200, 400, 800, 1_600, 3_200, 6_400, 10_000, 10_000];
        for (attempt, want) in expected.into_iter().enumerate() {
            let schedule = RetrySchedule {
                attempt: u32::try_from(attempt).unwrap_or(u32::MAX),
                ..RetrySchedule::default()
            };
            assert_eq!(schedule.next_delay(), want, "attempt {attempt}");
        }
    }

    #[test]
    fn retry_schedule_saturates_at_u64_boundary_without_panicking() {
        let schedule = RetrySchedule {
            base_delay_ms: u64::MAX - 1,
            max_delay_ms: u64::MAX,
            attempt: 5,
        };
        assert!(schedule.validate().is_ok());
        // u64::MAX-1 * 32 would overflow; must saturate and cap, never panic.
        assert_eq!(schedule.next_delay(), u64::MAX);

        let huge_attempt = RetrySchedule {
            base_delay_ms: 200,
            max_delay_ms: 10_000,
            attempt: u32::MAX,
        };
        assert_eq!(huge_attempt.next_delay(), 10_000);
    }

    #[test]
    fn retry_schedule_validate_rejects_zero_base_and_max_below_base() {
        let zero_base = RetrySchedule {
            base_delay_ms: 0,
            max_delay_ms: 10_000,
            attempt: 0,
        };
        assert_eq!(zero_base.validate(), Err(RetryScheduleError::ZeroBaseDelay));

        let max_below_base = RetrySchedule {
            base_delay_ms: 20_000,
            max_delay_ms: 10_000,
            attempt: 0,
        };
        assert_eq!(
            max_below_base.validate(),
            Err(RetryScheduleError::MaxBelowBase)
        );

        // Error rendering stays displayable and stable.
        assert_eq!(
            RetryScheduleError::ZeroBaseDelay.to_string(),
            "base_delay_ms must be greater than zero"
        );
        assert_eq!(
            RetryScheduleError::MaxBelowBase.to_string(),
            "max_delay_ms must be greater than or equal to base_delay_ms"
        );
    }

    #[test]
    fn retry_schedule_serde_round_trip() -> TestResult {
        let json = serde_json::to_string(&RetrySchedule {
            base_delay_ms: 500,
            max_delay_ms: 30_000,
            attempt: 3,
        })?;
        let back: RetrySchedule = serde_json::from_str(&json)?;
        assert_eq!(
            back,
            RetrySchedule {
                base_delay_ms: 500,
                max_delay_ms: 30_000,
                attempt: 3,
            }
        );
        Ok(())
    }
}
