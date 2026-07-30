//! Checkpoint and replay planning.

use std::time::Duration;

use orchestraitor_model::{OperationId, SessionState};
use serde::{Deserialize, Serialize};

/// Policy for periodic checkpoint emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointPolicy {
    /// Emit after this many completed tool calls.
    pub after_tool_calls: u32,
    /// Emit after this many seconds since the last checkpoint.
    pub after_seconds: u64,
}

impl CheckpointPolicy {
    /// Creates a policy from typed durations.
    #[must_use]
    pub const fn new(after_tool_calls: u32, after: Duration) -> Self {
        Self {
            after_tool_calls,
            after_seconds: after.as_secs(),
        }
    }
}

/// Reason a checkpoint should be emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckpointTrigger {
    /// Tool-call threshold was reached.
    ToolCalls,
    /// Time budget was reached.
    TimeBudget,
}

/// Mutable checkpoint cursor for one running task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointCursor {
    /// Completed tool calls since the previous checkpoint.
    pub tool_calls_since_checkpoint: u32,
    /// Unix timestamp seconds when the previous checkpoint was emitted.
    pub last_checkpoint_unix_secs: u64,
}

impl CheckpointCursor {
    /// Creates a cursor at the supplied checkpoint timestamp.
    #[must_use]
    pub const fn new(last_checkpoint_unix_secs: u64) -> Self {
        Self {
            tool_calls_since_checkpoint: 0,
            last_checkpoint_unix_secs,
        }
    }

    /// Records one completed tool call and returns a checkpoint trigger when due.
    #[must_use]
    pub fn record_tool_call(
        &mut self,
        now_unix_secs: u64,
        policy: CheckpointPolicy,
    ) -> Option<CheckpointTrigger> {
        self.tool_calls_since_checkpoint = self.tool_calls_since_checkpoint.saturating_add(1);
        if policy.after_tool_calls != 0
            && self.tool_calls_since_checkpoint >= policy.after_tool_calls
        {
            self.reset(now_unix_secs);
            return Some(CheckpointTrigger::ToolCalls);
        }
        self.check_time_budget(now_unix_secs, policy)
    }

    /// Returns a time-budget checkpoint trigger when due.
    #[must_use]
    pub fn check_time_budget(
        &mut self,
        now_unix_secs: u64,
        policy: CheckpointPolicy,
    ) -> Option<CheckpointTrigger> {
        let elapsed = now_unix_secs.saturating_sub(self.last_checkpoint_unix_secs);
        if policy.after_seconds != 0 && elapsed >= policy.after_seconds {
            self.reset(now_unix_secs);
            return Some(CheckpointTrigger::TimeBudget);
        }
        None
    }

    fn reset(&mut self, now_unix_secs: u64) {
        self.tool_calls_since_checkpoint = 0;
        self.last_checkpoint_unix_secs = now_unix_secs;
    }
}

/// Durable checkpoint payload for replay-from-checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Stable checkpoint operation id.
    pub checkpoint_id: OperationId,
    /// State at checkpoint time.
    pub state: SessionState,
    /// Event ids for completed tool calls already preserved.
    pub completed_tool_calls: Vec<OperationId>,
    /// Event ids for model responses already preserved.
    pub model_responses: Vec<OperationId>,
    /// Event ids for partial patches already preserved.
    pub partial_patches: Vec<OperationId>,
}

/// Replay plan derived from the latest checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayPlan {
    /// Checkpoint to resume from.
    pub checkpoint_id: OperationId,
    /// State to restore before resuming work.
    pub resume_state: SessionState,
    /// Tool-call event ids that must not be re-run.
    pub completed_tool_calls: Vec<OperationId>,
    /// Model response event ids to replay into the context window.
    pub model_responses: Vec<OperationId>,
    /// Partial patch event ids available for promotion/review.
    pub partial_patches: Vec<OperationId>,
}

impl ReplayPlan {
    /// Creates a replay plan from the newest checkpoint.
    #[must_use]
    pub fn from_checkpoint(checkpoint: &Checkpoint) -> Self {
        Self {
            checkpoint_id: checkpoint.checkpoint_id.clone(),
            resume_state: checkpoint.state,
            completed_tool_calls: checkpoint.completed_tool_calls.clone(),
            model_responses: checkpoint.model_responses.clone(),
            partial_patches: checkpoint.partial_patches.clone(),
        }
    }
}
