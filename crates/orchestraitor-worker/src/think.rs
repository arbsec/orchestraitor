//! The `think` tool's session thought state and reasoning records (spec
//! `20-harness-worker.md` §9.48).
//!
//! One [`ThoughtChain`] per run tracks the step counters the §9.48.3 budgets
//! and refusal semantics need: the highest accepted `thought_number`, the
//! accepted-thought count, whether the current sequence is closed (a
//! `thought_number = 1` after a closed sequence starts a fresh one), and the
//! run-total thought budget across sequences.

/// Static refusal reason codes for the `think` tool (§9.48.1). Typed,
/// receipted, and never carrying thought content.
pub const REASON_THOUGHT_EMPTY: &str = "thought-empty";
/// Per-thought size cap exceeded (§9.48.3).
pub const REASON_THOUGHT_TOO_LARGE: &str = "thought-too-large";

/// `thought_number` did not advance past the previous accepted number
/// without `is_revision` (§9.48.1).
pub const REASON_THOUGHT_NUMBER_REGRESSED: &str = "thought-number-regressed";
/// An `is_revision` call named a `revises_thought` that was never accepted
/// (§9.48.1).
pub const REASON_REVISION_TARGET_UNKNOWN: &str = "revision-target-unknown";
/// Sequence ceiling or run-total thought budget exhausted (§9.48.3).
pub const REASON_THOUGHT_BUDGET_EXHAUSTED: &str = "thought-budget-exhausted";
/// The thought text matched the §9.23.4 secret-shape heuristic (§9.48.4).
pub const REASON_SECRET_SHAPED_CONTENT: &str = "secret-shaped-content";
/// Reserved v2 branching fields were supplied (§9.48.5).
pub const REASON_BRANCHING_NOT_SUPPORTED: &str = "branching-not-supported-in-v1";

/// Sequence ceiling: thoughts per sequence (§9.48.3).
pub const MAX_THOUGHTS_PER_SEQUENCE: u32 = 256;
/// Run-total budget: thoughts per run across sequences (§9.48.3).
pub const MAX_THOUGHTS_PER_RUN: u32 = 512;

/// One accepted (or refused-but-receipted) reasoning step's audit metadata.
/// The payload shape of an `agent reasoning` event (§9.48.8): structure is
/// always carried; `thought_text` rides only under payload-recording consent.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ReasoningRecord {
    /// Position in the sequence (1-based).
    pub thought_number: u32,
    /// Current step-count estimate.
    pub total_thoughts: u32,
    /// Whether more steps are coming.
    pub next_thought_needed: bool,
    /// Whether this call revises earlier thinking.
    pub is_revision: bool,
    /// Which thought is reconsidered; `None` when `is_revision` is `false`.
    pub revises_thought: Option<u32>,
    /// Char count of the thought text at validation time (also carried on
    /// refusals, §9.48.4).
    pub thought_chars: usize,
    /// Sequence this step belongs to (1-based; a fresh sequence starts when
    /// a closed sequence is followed by a new `thought_number = 1`).
    pub sequence_id: u32,
    /// The step's deliberation text. `None` unless payload recording is
    /// enabled AND the thought was accepted — a refused thought's text is
    /// never written (§9.48.4).
    pub thought_text: Option<String>,
}

/// Per-run thought-chain state backing the §9.48.3 budgets.
#[derive(Default)]
pub struct ThoughtChain {
    /// Highest accepted `thought_number` within the current sequence.
    highest_number: u32,
    /// Accepted thoughts in the current sequence.
    sequence_count: u32,
    /// Accepted thoughts across the whole run (all sequences).
    run_count: u32,
    /// 1-based id of the current sequence.
    sequence_id: u32,
    /// Whether the current sequence was closed (`next_thought_needed =
    /// false`), allowing the next `thought_number = 1` to start a new one.
    sequence_closed: bool,
}

impl ThoughtChain {
    /// Creates an empty chain.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            highest_number: 0,
            sequence_count: 0,
            run_count: 0,
            sequence_id: 1,
            sequence_closed: false,
        }
    }

    /// Accepted thoughts across the whole run (§9.48.6 counters).
    #[must_use]
    pub const fn run_count(&self) -> u32 {
        self.run_count
    }

    /// Whether a call with these numbers is admissible under the §9.48.1
    /// ordering and §9.48.3 budget rules. Returns the refusal reason on
    /// rejection. A revision re-opens a closed sequence (the chain is
    /// revisited, not advanced).
    pub fn validate(
        &self,
        thought_number: u32,
        total_thoughts: u32,
        is_revision: bool,
        revises_thought: Option<u32>,
    ) -> Result<(), &'static str> {
        // Run-total budget first: once the run is exhausted, every further
        // thought is refused — non-fatal and receipted (§9.48.3).
        if self.run_count >= MAX_THOUGHTS_PER_RUN {
            return Err(REASON_THOUGHT_BUDGET_EXHAUSTED);
        }
        if is_revision {
            // A revision must name its target, and the target must have
            // been accepted at some point in the run.
            let Some(target) = revises_thought else {
                return Err(REASON_REVISION_TARGET_UNKNOWN);
            };
            if target == 0 || self.highest_number == 0 || target > self.highest_number {
                return Err(REASON_REVISION_TARGET_UNKNOWN);
            }
            return Ok(());
        }
        // Sequence transitions: `thought_number = 1` after a closed sequence
        // starts a fresh sequence (§9.48.3 run-total semantics).
        if thought_number == 1 && self.sequence_closed {
            return Ok(());
        }
        // The sequence ceiling (§9.48.3).
        if thought_number > MAX_THOUGHTS_PER_SEQUENCE {
            return Err(REASON_THOUGHT_BUDGET_EXHAUSTED);
        }
        // Linear-chain ordering: must advance past the previous accepted
        // number unless the call is a revision (§9.48.1).
        if thought_number <= self.highest_number {
            return Err(REASON_THOUGHT_NUMBER_REGRESSED);
        }
        if total_thoughts == 0 {
            return Err(REASON_THOUGHT_NUMBER_REGRESSED);
        }
        Ok(())
    }

    /// Records one accepted thought, advancing the counters. `thought_number
    /// = 1` on a closed sequence starts a fresh sequence (§9.48.3).
    pub fn accept(&mut self, thought_number: u32, next_thought_needed: bool) {
        if thought_number == 1 && self.sequence_closed {
            self.sequence_id = self.sequence_id.saturating_add(1);
            self.highest_number = 0;
            self.sequence_count = 0;
        }
        self.sequence_closed = !next_thought_needed;
        self.highest_number = self.highest_number.max(thought_number);
        self.sequence_count = self.sequence_count.saturating_add(1);
        self.run_count = self.run_count.saturating_add(1);
    }

    /// The current sequence id (1-based).
    #[must_use]
    pub const fn sequence_id(&self) -> u32 {
        self.sequence_id
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn linear_chain_must_advance() {
        let mut chain = ThoughtChain::new();
        assert!(chain.validate(1, 5, false, None).is_ok());
        chain.accept(1, true);
        // Regression without is_revision is refused.
        assert_eq!(
            chain.validate(1, 5, false, None),
            Err(REASON_THOUGHT_NUMBER_REGRESSED)
        );
        // Advancing is accepted.
        assert!(chain.validate(2, 5, false, None).is_ok());
    }

    #[test]
    fn revision_requires_known_target() {
        let mut chain = ThoughtChain::new();
        assert_eq!(
            chain.validate(1, 5, true, Some(1)),
            Err(REASON_REVISION_TARGET_UNKNOWN)
        );
        assert_eq!(
            chain.validate(1, 5, true, None),
            Err(REASON_REVISION_TARGET_UNKNOWN)
        );
        chain.accept(3, true);
        assert!(chain.validate(3, 5, true, Some(2)).is_ok());
        assert_eq!(
            chain.validate(3, 5, true, Some(4)),
            Err(REASON_REVISION_TARGET_UNKNOWN)
        );
    }

    #[test]
    fn sequence_ceiling_refuses_above_256() {
        let mut chain = ThoughtChain::new();
        chain.accept(256, true);
        // Above the ceiling: receipted refusal.
        assert_eq!(
            chain.validate(257, 300, false, None),
            Err(REASON_THOUGHT_BUDGET_EXHAUSTED)
        );
        // The ceiling boundary itself is admissible.
        let mut boundary = ThoughtChain::new();
        boundary.accept(255, true);
        assert!(boundary.validate(256, 300, false, None).is_ok());
    }

    #[test]
    fn closed_sequence_then_one_starts_fresh() {
        let mut chain = ThoughtChain::new();
        chain.accept(4, false);
        assert!(chain.validate(1, 7, false, None).is_ok());
        chain.accept(1, true);
        assert_eq!(chain.validate(2, 7, false, None), Ok::<(), &str>(()));
        assert_eq!(chain.sequence_id(), 2);
    }

    #[test]
    fn run_total_budget_refuses_at_512() {
        let mut chain = ThoughtChain::new();
        chain.run_count = MAX_THOUGHTS_PER_RUN;
        assert_eq!(
            chain.validate(1, 5, false, None),
            Err(REASON_THOUGHT_BUDGET_EXHAUSTED)
        );
    }
}
