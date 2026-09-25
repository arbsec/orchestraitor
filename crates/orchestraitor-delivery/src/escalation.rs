//! Escalation chain for repeated implementation or verification failures
//! (spec §9.33.5).
//!
//! Spec §9.33.5 permits escalation when retries keep failing, as an ordered
//! ladder: same agent with fresh context → alternate model → domain expert →
//! revised task plan → human escalation. This module provides the ladder
//! ([`EscalationStep`]), its configurable ordering policy
//! ([`EscalationPolicy`]), the per-task progress tracker
//! ([`EscalationState`]), and the pure [`next_escalation`] proposal mapping a
//! state to the runner's next escalation target ([`EscalationOutcome`]).
//!
//! Fresh-context rule: every ladder step spawns a **fresh, minimal context**
//! per §9.33.1 — the escalation target never receives the accumulated
//! conversation of the failed attempt ("Do NOT pass accumulated conversations
//! between agents", §9.33.1). Context hand-over across steps is forbidden:
//! each spawn derives its context from the spec, task metadata, workspace
//! state, and dependency outputs only, preventing accidental authority
//! leakage and context poisoning (§7.3). This module records *which* step is
//! next; the runner performs the fresh-context spawn.
//!
//! Boundary with retry budgets: this module only **walks the ladder**. It
//! deliberately owns no per-step retry budget — how many failures a step
//! tolerates before the runner advances is retry-policy configuration
//! (§9.26, §9.22.1) owned by the runner. [`EscalationState::record_failure`]
//! counts attempts at the current step; [`EscalationState::advance`] moves
//! the ladder on; nothing here decides *when* to advance.
//!
//! Security boundary (spec §9.33.7): escalation *proposes*; the runner/policy
//! layer executes and Arbitraitor approves (§2.2). There are no auto-approve
//! semantics anywhere in this module: reaching
//! [`EscalationOutcome::HumanEscalation`] is a terminal proposal to stop and
//! hand off, never a permission grant. This module makes no security
//! decisions (allow/deny/verdict), bypasses no approval requirement, and owns
//! no enforcement — all policy, approval, promotion, and receipt authority
//! belongs to Arbitraitor.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// One step of the §9.33.5 escalation ladder.
///
/// Exactly the five mandated steps; `PartialOrd`/`Ord` follow the spec ladder
/// precedence (declaration order), so `SameAgentFreshContext <
/// AlternateModel < DomainExpert < RevisedPlan < HumanEscalation`.
/// Serialization is the `snake_case` form used in persisted escalation
/// history (§9.33.6).
///
/// Every step spawns a **fresh context** (§9.33.1): the new attempt MUST NOT
/// inherit the failed attempt's conversation, so no accumulated-conversation
/// state leaks across or within ladder steps (§7.3 context-poisoning rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationStep {
    /// Retry the same agent with a fresh context (§9.33.5 ladder step 1).
    /// The fresh spawn carries no conversation from the failed attempts
    /// (§9.33.1, §7.3).
    SameAgentFreshContext,
    /// Retry with an alternate model, fresh context (§9.33.5 ladder step 2).
    AlternateModel,
    /// Retry with a domain-expert agent or profile, fresh context (§9.33.5
    /// ladder step 3).
    DomainExpert,
    /// Hand the task back to planning for a revised task plan before any
    /// further implementation attempt, fresh context (§9.33.5 ladder step 4).
    RevisedPlan,
    /// Terminal step: escalate to a human (§9.33.5 ladder step 5). Never a
    /// permission grant — the task stops and awaits human direction.
    HumanEscalation,
}

/// Structural validation failure for [`EscalationPolicy`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EscalationPolicyError {
    /// `steps` is empty; the ladder could never start. Every policy must
    /// contain at least the terminal step.
    #[error("escalation steps must not be empty")]
    EmptyLadder,
    /// `steps` is not in spec ladder precedence order (duplicates count as
    /// out of order). The §9.33.5 ladder is an ordered chain; a policy may
    /// shorten it but MUST NOT reorder it (§9.22.4).
    #[error("escalation steps must follow the spec ladder order without duplicates")]
    StepsOutOfOrder,
    /// `steps` does not end at [`EscalationStep::HumanEscalation`]. Human
    /// escalation is the mandated terminal step of the §9.33.5 ladder; a
    /// policy without it could strand a task with no terminal proposal.
    #[error("escalation steps must end at human_escalation")]
    MissingHumanEscalation,
}

/// Configurable ordering of the §9.33.5 escalation ladder (§9.22.4: no
/// hardcoded routing beyond spec defaults).
///
/// This is the struct a `[delivery.escalation]`-style configuration section
/// deserializes into. A missing section falls back to the full five-step
/// ladder in spec order; unknown keys are rejected so configuration drift
/// fails visibly.
///
/// The spec ladder is an **ordered chain**: a policy may shorten it (any
/// subsequence) but MUST follow the spec precedence order and MUST end at
/// [`EscalationStep::HumanEscalation`] — see
/// [`EscalationPolicy::validate`]. The number of steps before human
/// escalation is therefore `steps.len() - 1` by construction; there is no
/// separate step-count knob that could disagree with the ladder itself.
/// Per-step attempt budgets are *not* configured here: they are retry-policy
/// configuration (§9.26, §9.22.1) owned by the runner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EscalationPolicy {
    /// Ordered escalation steps. Default: the full §9.33.5 ladder
    /// ([`EscalationStep::SameAgentFreshContext`] →
    /// [`EscalationStep::AlternateModel`] → [`EscalationStep::DomainExpert`]
    /// → [`EscalationStep::RevisedPlan`] →
    /// [`EscalationStep::HumanEscalation`]).
    pub steps: Vec<EscalationStep>,
}

impl Default for EscalationPolicy {
    fn default() -> Self {
        Self {
            steps: vec![
                EscalationStep::SameAgentFreshContext,
                EscalationStep::AlternateModel,
                EscalationStep::DomainExpert,
                EscalationStep::RevisedPlan,
                EscalationStep::HumanEscalation,
            ],
        }
    }
}

impl EscalationPolicy {
    /// Validates structural invariants: the ladder is non-empty, follows the
    /// spec precedence order with no duplicates (a policy may shorten the
    /// ladder but never reorder it), and ends at
    /// [`EscalationStep::HumanEscalation`].
    ///
    /// # Errors
    ///
    /// Returns the first [`EscalationPolicyError`] encountered.
    pub fn validate(&self) -> Result<(), EscalationPolicyError> {
        if self.steps.is_empty() {
            return Err(EscalationPolicyError::EmptyLadder);
        }
        if !self.steps.windows(2).all(|pair| pair[0] < pair[1]) {
            return Err(EscalationPolicyError::StepsOutOfOrder);
        }
        if self.steps.last() != Some(&EscalationStep::HumanEscalation) {
            return Err(EscalationPolicyError::MissingHumanEscalation);
        }
        Ok(())
    }
}

/// Per-task escalation progress against an [`EscalationPolicy`]
/// (§9.33.5).
///
/// Tracks the current ladder step, how many failures have been recorded at
/// it, and an append-only ordered history of the steps already left behind.
/// The runner drives the state machine explicitly:
/// [`EscalationState::record_failure`] counts another failed attempt at the
/// current step (saturating, never advancing on its own — the runner owns
/// per-step attempt budgets, §9.26, §9.22.1) and
/// [`EscalationState::advance`] moves to the next step when the runner
/// decides the current step is spent.
///
/// Plain bookkeeping only: no clocks, no I/O, no security decisions
/// (§9.33.7). Durable persistence (§9.33.6) is left to the runner/store
/// layer, which can reconstruct an equivalent state from its persisted
/// failure/escalation records by replaying `record_failure`/`advance` over a
/// validated policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationState {
    /// The policy ladder this state walks.
    policy: EscalationPolicy,
    /// Current ladder step. Starts at the policy's first step.
    current_step: EscalationStep,
    /// Failures recorded at the current step (saturating counter).
    attempts_at_current_step: u32,
    /// Append-only, ordered record of steps the state has advanced away
    /// from. Never contains the current step.
    history: Vec<EscalationStep>,
}

impl EscalationState {
    /// Creates a state at the first step of `policy`, after validating it.
    ///
    /// # Errors
    ///
    /// Returns the first [`EscalationPolicyError`] from
    /// [`EscalationPolicy::validate`].
    pub fn new(policy: EscalationPolicy) -> Result<Self, EscalationPolicyError> {
        policy.validate()?;
        let current_step = policy
            .steps
            .first()
            .copied()
            .ok_or(EscalationPolicyError::EmptyLadder)?;
        Ok(Self {
            policy,
            current_step,
            attempts_at_current_step: 0,
            history: Vec::new(),
        })
    }

    /// The policy ladder this state walks.
    #[must_use]
    pub const fn policy(&self) -> &EscalationPolicy {
        &self.policy
    }

    /// The current ladder step.
    #[must_use]
    pub const fn current_step(&self) -> EscalationStep {
        self.current_step
    }

    /// How many failures have been recorded at the current step.
    #[must_use]
    pub const fn attempts_at_current_step(&self) -> u32 {
        self.attempts_at_current_step
    }

    /// Ordered record of the steps this state has advanced away from, in the
    /// order they were left. Append-only; never contains the current step.
    #[must_use]
    pub fn history(&self) -> &[EscalationStep] {
        &self.history
    }

    /// Records one more failure at the current step.
    ///
    /// Saturating counter only — this deliberately does **not** advance the
    /// ladder: whether a step's failures exhaust it is the runner's
    /// retry-policy decision (§9.26, §9.22.1), not this module's. Call
    /// [`EscalationState::advance`] to move on.
    pub fn record_failure(&mut self) {
        self.attempts_at_current_step = self.attempts_at_current_step.saturating_add(1);
    }

    /// The step that follows the current one in the policy ladder, or `None`
    /// when the current step is the terminal
    /// [`EscalationStep::HumanEscalation`] (ladder exhausted).
    #[must_use]
    pub fn next_step(&self) -> Option<EscalationStep> {
        let position = self
            .policy
            .steps
            .iter()
            .position(|&step| step == self.current_step)?;
        self.policy.steps.get(position.saturating_add(1)).copied()
    }

    /// Advances to the next ladder step: appends the current step to the
    /// history, moves to [`EscalationState::next_step`], and resets the
    /// attempt counter.
    ///
    /// Returns the new current step, or `None` when already terminal —
    /// advancing past [`EscalationStep::HumanEscalation`] is idempotent and
    /// never panics: the state stays at the terminal step with history and
    /// counter untouched.
    pub fn advance(&mut self) -> Option<EscalationStep> {
        let next = self.next_step()?;
        self.history.push(self.current_step);
        self.current_step = next;
        self.attempts_at_current_step = 0;
        Some(next)
    }

    /// True when the ladder is exhausted: the current step is the terminal
    /// [`EscalationStep::HumanEscalation`]. Human escalation is always the
    /// last step of any validated policy, so this coincides with
    /// [`EscalationState::next_step`] returning `None`.
    ///
    /// Deliberately independent of attempt counts: this module owns no
    /// per-step retry budget (§9.26, §9.22.1), so no number of recorded
    /// failures at a non-terminal step makes the state exhausted.
    #[must_use]
    pub fn exhausted(&self) -> bool {
        self.current_step == EscalationStep::HumanEscalation
    }
}

/// The runner's next escalation target after a failure (§9.33.5).
///
/// A proposal consumed by the runner loop — never a policy verdict and never
/// an approval (§2.2, §9.33.7). Reaching
/// [`EscalationOutcome::HumanEscalation`] carries no auto-approve semantics:
/// the task stops and awaits human direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationOutcome {
    /// Run the next attempt at `step` with a fresh context (§9.33.1 — no
    /// accumulated conversation from the failed attempts, §7.3).
    RetryAt {
        /// The ladder step whose agent/model/profile should be spawned next.
        step: EscalationStep,
    },
    /// Terminal proposal: the ladder is exhausted, escalate to a human.
    HumanEscalation,
}

/// Maps an [`EscalationState`] to the runner's next escalation target
/// (§9.33.5).
///
/// Pure and total: [`EscalationOutcome::HumanEscalation`] when the state's
/// ladder is exhausted, otherwise [`EscalationOutcome::RetryAt`] at the
/// state's current step. This function does not advance the state and owns no
/// attempt budget — the runner decides when to call
/// [`EscalationState::record_failure`] and [`EscalationState::advance`].
#[must_use]
pub fn next_escalation(state: &EscalationState) -> EscalationOutcome {
    if state.exhausted() {
        EscalationOutcome::HumanEscalation
    } else {
        EscalationOutcome::RetryAt {
            step: state.current_step(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const FULL_LADDER: [EscalationStep; 5] = [
        EscalationStep::SameAgentFreshContext,
        EscalationStep::AlternateModel,
        EscalationStep::DomainExpert,
        EscalationStep::RevisedPlan,
        EscalationStep::HumanEscalation,
    ];

    #[test]
    fn default_policy_is_the_full_ladder_in_spec_order() -> TestResult {
        let policy = EscalationPolicy::default();
        assert_eq!(policy.steps, FULL_LADDER);
        policy.validate()?;
        Ok(())
    }

    #[test]
    fn escalation_step_serde_round_trip_and_names() -> TestResult {
        let expected = [
            (
                EscalationStep::SameAgentFreshContext,
                "same_agent_fresh_context",
            ),
            (EscalationStep::AlternateModel, "alternate_model"),
            (EscalationStep::DomainExpert, "domain_expert"),
            (EscalationStep::RevisedPlan, "revised_plan"),
            (EscalationStep::HumanEscalation, "human_escalation"),
        ];
        for (step, name) in expected {
            let json = serde_json::to_string(&step)?;
            assert_eq!(json, format!("\"{name}\""));
            let back: EscalationStep = serde_json::from_str(&json)?;
            assert_eq!(back, step);
        }
        Ok(())
    }

    #[test]
    fn escalation_step_rejects_unknown_strings() {
        for bad in ["same_agent", "human", "", "HumanEscalation"] {
            assert!(serde_json::from_str::<EscalationStep>(&format!("\"{bad}\"")).is_err());
        }
    }

    #[test]
    fn escalation_step_order_matches_spec_ladder() {
        for pair in FULL_LADDER.windows(2) {
            assert!(pair[0] < pair[1]);
        }
    }

    #[test]
    fn empty_object_deserializes_to_default_ladder() -> TestResult {
        let policy: EscalationPolicy = serde_json::from_str("{}")?;
        assert_eq!(policy, EscalationPolicy::default());
        policy.validate()?;
        Ok(())
    }

    #[test]
    fn policy_round_trips_through_json() -> TestResult {
        let policy = EscalationPolicy {
            steps: vec![
                EscalationStep::SameAgentFreshContext,
                EscalationStep::HumanEscalation,
            ],
        };
        let json = serde_json::to_string_pretty(&policy)?;
        let back: EscalationPolicy = serde_json::from_str(&json)?;
        assert_eq!(back, policy);
        back.validate()?;
        Ok(())
    }

    #[test]
    fn policy_unknown_fields_are_rejected() {
        let json = r#"{"steps": ["human_escalation"], "surprise_key": true}"#;
        assert!(serde_json::from_str::<EscalationPolicy>(json).is_err());
    }

    #[test]
    fn empty_ladder_is_rejected() {
        let policy = EscalationPolicy { steps: vec![] };
        assert_eq!(policy.validate(), Err(EscalationPolicyError::EmptyLadder));
    }

    #[test]
    fn reordered_ladder_is_rejected() {
        let policy = EscalationPolicy {
            steps: vec![
                EscalationStep::AlternateModel,
                EscalationStep::SameAgentFreshContext,
                EscalationStep::HumanEscalation,
            ],
        };
        assert_eq!(
            policy.validate(),
            Err(EscalationPolicyError::StepsOutOfOrder)
        );
    }

    #[test]
    fn duplicated_step_is_rejected_as_out_of_order() {
        // Duplicates break strict ladder precedence.
        let policy = EscalationPolicy {
            steps: vec![
                EscalationStep::DomainExpert,
                EscalationStep::DomainExpert,
                EscalationStep::HumanEscalation,
            ],
        };
        assert_eq!(
            policy.validate(),
            Err(EscalationPolicyError::StepsOutOfOrder)
        );
    }

    #[test]
    fn ladder_without_terminal_human_escalation_is_rejected() {
        let policy = EscalationPolicy {
            steps: vec![
                EscalationStep::SameAgentFreshContext,
                EscalationStep::RevisedPlan,
            ],
        };
        assert_eq!(
            policy.validate(),
            Err(EscalationPolicyError::MissingHumanEscalation)
        );
    }

    #[test]
    fn policy_error_display_messages_are_actionable() {
        assert_eq!(
            EscalationPolicyError::EmptyLadder.to_string(),
            "escalation steps must not be empty"
        );
        assert_eq!(
            EscalationPolicyError::StepsOutOfOrder.to_string(),
            "escalation steps must follow the spec ladder order without duplicates"
        );
        assert_eq!(
            EscalationPolicyError::MissingHumanEscalation.to_string(),
            "escalation steps must end at human_escalation"
        );
    }

    #[test]
    fn new_state_rejects_an_invalid_policy() {
        let policy = EscalationPolicy { steps: vec![] };
        assert_eq!(
            EscalationState::new(policy).err(),
            Some(EscalationPolicyError::EmptyLadder)
        );
    }

    #[test]
    fn sequential_failures_walk_the_ladder_to_human_escalation() -> TestResult {
        let mut state = EscalationState::new(EscalationPolicy::default())?;
        let mut walked = vec![state.current_step()];
        while let Some(next) = state.next_step() {
            // The runner records failures, decides the step is spent, then
            // advances; the module itself never advances on its own.
            state.record_failure();
            state.record_failure();
            assert_eq!(state.advance(), Some(next));
            walked.push(state.current_step());
        }
        assert_eq!(walked, FULL_LADDER);
        assert!(state.exhausted());
        assert_eq!(state.next_step(), None);
        Ok(())
    }

    #[test]
    fn advance_past_human_escalation_is_terminal_and_idempotent() -> TestResult {
        // Single-step policy: straight to human escalation.
        let policy = EscalationPolicy {
            steps: vec![EscalationStep::HumanEscalation],
        };
        let mut state = EscalationState::new(policy)?;
        assert!(state.exhausted());
        assert_eq!(next_escalation(&state), EscalationOutcome::HumanEscalation);
        assert_eq!(state.advance(), None);
        assert_eq!(state.advance(), None);
        assert_eq!(state.current_step(), EscalationStep::HumanEscalation);
        assert!(state.history().is_empty());
        assert_eq!(state.attempts_at_current_step(), 0);
        Ok(())
    }

    #[test]
    fn history_is_an_append_only_ordered_record_of_left_steps() -> TestResult {
        let mut state = EscalationState::new(EscalationPolicy::default())?;
        assert!(state.history().is_empty());

        assert_eq!(state.advance(), Some(EscalationStep::AlternateModel));
        assert_eq!(state.history(), [EscalationStep::SameAgentFreshContext]);

        assert_eq!(state.advance(), Some(EscalationStep::DomainExpert));
        assert_eq!(
            state.history(),
            [
                EscalationStep::SameAgentFreshContext,
                EscalationStep::AlternateModel
            ]
        );

        // The current step is never part of the history.
        assert!(!state.history().contains(&state.current_step()));
        Ok(())
    }

    #[test]
    fn shortened_policy_skips_middle_steps() -> TestResult {
        let policy = EscalationPolicy {
            steps: vec![
                EscalationStep::SameAgentFreshContext,
                EscalationStep::HumanEscalation,
            ],
        };
        policy.validate()?;
        let mut state = EscalationState::new(policy)?;
        assert_eq!(state.next_step(), Some(EscalationStep::HumanEscalation));
        assert_eq!(state.advance(), Some(EscalationStep::HumanEscalation));
        assert!(state.exhausted());
        assert_eq!(state.next_step(), None);
        assert_eq!(next_escalation(&state), EscalationOutcome::HumanEscalation);
        Ok(())
    }

    #[test]
    fn attempts_counter_saturates_without_overflow() -> TestResult {
        let mut state = EscalationState::new(EscalationPolicy::default())?;
        for _ in 0..3 {
            state.record_failure();
        }
        assert_eq!(state.attempts_at_current_step(), 3);

        state.attempts_at_current_step = u32::MAX;
        state.record_failure();
        assert_eq!(state.attempts_at_current_step(), u32::MAX);
        state.record_failure();
        assert_eq!(state.attempts_at_current_step(), u32::MAX);
        // Saturation never advances the ladder or exhausts the state.
        assert_eq!(state.current_step(), EscalationStep::SameAgentFreshContext);
        assert!(!state.exhausted());
        Ok(())
    }

    #[test]
    fn record_failure_counts_attempts_without_advancing() -> TestResult {
        // Boundary with retry-policy config (§9.26, §9.22.1): attempt budgets
        // live with the runner, so failures alone never move the ladder.
        let mut state = EscalationState::new(EscalationPolicy::default())?;
        for _ in 0..10 {
            state.record_failure();
        }
        assert_eq!(state.attempts_at_current_step(), 10);
        assert_eq!(state.current_step(), EscalationStep::SameAgentFreshContext);
        assert!(state.history().is_empty());
        assert!(!state.exhausted());
        assert_eq!(
            next_escalation(&state),
            EscalationOutcome::RetryAt {
                step: EscalationStep::SameAgentFreshContext,
            }
        );

        // Only an explicit advance moves the ladder, and resets the counter.
        assert_eq!(state.advance(), Some(EscalationStep::AlternateModel));
        assert_eq!(state.attempts_at_current_step(), 0);
        assert_eq!(
            next_escalation(&state),
            EscalationOutcome::RetryAt {
                step: EscalationStep::AlternateModel,
            }
        );
        Ok(())
    }

    #[test]
    fn next_escalation_proposes_only_current_step_or_human_terminal() -> TestResult {
        // Full sweep: at every non-terminal step the outcome is `RetryAt` the
        // current step; at the terminal step it is `HumanEscalation`. No
        // outcome ever carries an approval or skip ahead of the ladder.
        let mut state = EscalationState::new(EscalationPolicy::default())?;
        for step in FULL_LADDER {
            assert_eq!(state.current_step(), step);
            let expected = if step == EscalationStep::HumanEscalation {
                EscalationOutcome::HumanEscalation
            } else {
                EscalationOutcome::RetryAt { step }
            };
            assert_eq!(next_escalation(&state), expected);
            state.advance();
        }
        Ok(())
    }

    #[test]
    fn escalation_outcome_serde_round_trip() -> TestResult {
        let outcomes = [
            EscalationOutcome::RetryAt {
                step: EscalationStep::DomainExpert,
            },
            EscalationOutcome::HumanEscalation,
        ];
        for outcome in outcomes {
            let json = serde_json::to_string(&outcome)?;
            let back: EscalationOutcome = serde_json::from_str(&json)?;
            assert_eq!(back, outcome);
        }
        Ok(())
    }
}
