//! The bounded §9.50 ingress queue (spec §7.3 resource-exhaustion class):
//! sustained flooding drops surplus and records the `degraded` marker —
//! never unbounded queue growth. `degraded` clears only after a
//! flood-free window; it is observable, never a silent drop.
//!
//! The queue is ordering-insensitive input to kick-off evaluation (spec
//! §9.50(b)): the §9.43 reconcile poll stays the completeness authority,
//! so a dropped surplus event never loses work — the board state the
//! reconcile pass reads still describes it.

use std::collections::VecDeque;
use std::time::Duration;

use parking_lot::Mutex;

use crate::ingress::store::IngressEvent;

/// Default bounded-queue capacity (`watch.ingress.queue_capacity`
/// built-in default): deep enough for a burst across many repositories,
/// small enough that a flood hits the bound instead of memory.
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;

/// The flood-free window (seconds) after which the `degraded` marker
/// clears: the overflow case per spec §9.50 failure behavior clears
/// "only after a flood-free window", never while surplus is still
/// arriving.
pub const DEGRADED_QUIET_WINDOW: Duration = Duration::from_mins(5);

/// Push outcome for one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// The event was enqueued.
    Accepted,
    /// The event was a redelivery inside the §9.50 window: deduplicated,
    /// never a second kick-off.
    Duplicate,
    /// The queue was at capacity: the surplus was DROPPED and the
    /// `degraded` marker was set (spec §7.3 — the bound exists so a flood
    /// hits the drop, not unbounded growth).
    Dropped,
}

/// The bounded §9.50 ingress queue. Shared between the poll task
/// (producer) and the kick-off source (consumer) behind a mutex; the
/// clock is injectable for deterministic tests.
/// The bounded queue's state mutex is a `parking_lot::Mutex`: guards
/// return directly (no poisoning), the critical sections are short, and
/// call sites show locking rather than error boilerplate.
#[derive(Debug)]
pub struct IngressQueue {
    inner: Mutex<QueueState>,
    capacity: usize,
}

#[derive(Debug, Default)]
struct QueueState {
    events: VecDeque<IngressEvent>,
    /// Unix seconds the last drop happened (`None` = not degraded).
    degraded_at: Option<u64>,
}

impl IngressQueue {
    /// Builds a bounded queue. Fail-closed: a zero capacity would make
    /// every push a drop (a silently dead ingress) and is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::WatchConfig`] for a zero capacity.
    pub fn new(capacity: usize) -> Result<Self, crate::error::DaemonError> {
        if capacity == 0 {
            return Err(crate::error::DaemonError::WatchConfig(
                "watch.ingress.queue_capacity must be at least 1 (got 0)".to_string(),
            ));
        }
        Ok(Self {
            inner: Mutex::new(QueueState::default()),
            capacity,
        })
    }

    /// The configured capacity (exposed for assertions and diagnostics).
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Pushes one already-deduplicated event. At capacity the surplus is
    /// dropped and the `degraded` marker set with `now_secs` (the
    /// caller's clock; injectable for determinism).
    pub fn push(&self, event: IngressEvent, now_secs: u64) -> PushOutcome {
        let mut state = self.inner.lock();
        if state.events.len() >= self.capacity {
            state.degraded_at = Some(now_secs);
            return PushOutcome::Dropped;
        }
        state.events.push_back(event);
        PushOutcome::Accepted
    }

    /// Peeks every pending event WITHOUT removing them (a gen-6 cloud
    /// review fix: the kick-off gate must evaluate against pending
    /// events before the pass's planner selects; the destructive
    /// [`Self::take`] runs only after selection succeeds). Order is
    /// queue order; consumers must not rely on it.
    pub fn peek(&self) -> Vec<IngressEvent> {
        let state = self.inner.lock();
        state.events.iter().cloned().collect()
    }

    /// Drains every pending event (kick-off consumption). The events are
    /// ordering-insensitive input (spec §9.50): drain order is queue
    /// order, and consumers must not rely on it.
    pub fn take(&self) -> Vec<IngressEvent> {
        let mut state = self.inner.lock();
        state.events.drain(..).collect()
    }

    /// Removes exactly the events whose delivery keys are in `ids`
    /// (the ids the kick-off gate evaluated); every other event —
    /// including ones that arrived after the peek — stays pending.
    pub fn take_by_ids(&self, ids: &[String]) -> Vec<IngressEvent> {
        let mut state = self.inner.lock();
        let mut drained = Vec::new();
        state.events.retain(|event| {
            if ids.contains(&event.delivery_key) {
                drained.push(event.clone());
                false
            } else {
                true
            }
        });
        drained
    }

    /// Whether the queue is currently degraded (an overflow happened and
    /// the flood-free quiet window has not yet elapsed). `now_secs` is
    /// the caller's clock. The clear is LAZY: evaluated on read, so the
    /// marker clears on the first read after the quiet window even with
    /// no further traffic — and a fresh overflow re-arms it immediately.
    #[must_use]
    pub fn degraded(&self, now_secs: u64) -> bool {
        let mut state = self.inner.lock();
        match state.degraded_at {
            Some(at) if now_secs.saturating_sub(at) >= DEGRADED_QUIET_WINDOW.as_secs() => {
                // Flood-free window elapsed: the overflow case clears.
                state.degraded_at = None;
                false
            }
            Some(_) => true,
            None => false,
        }
    }
}
