//! The bounded §9.47 ingress queue (spec §7.3 resource-exhaustion class):
//! sustained flooding drops surplus and records the `degraded` marker —
//! never unbounded queue growth. `degraded` clears only after a
//! flood-free window; it is observable, never a silent drop.
//!
//! The queue is ordering-insensitive input to kick-off evaluation (spec
//! §9.47(b)): the §9.43 reconcile poll stays the completeness authority,
//! so a dropped surplus event never loses work — the board state the
//! reconcile pass reads still describes it.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Duration;

use crate::ingress::store::IngressEvent;

/// Default bounded-queue capacity (`watch.ingress.queue_capacity`
/// built-in default): deep enough for a burst across many repositories,
/// small enough that a flood hits the bound instead of memory.
pub const DEFAULT_QUEUE_CAPACITY: usize = 256;

/// The flood-free window (seconds) after which the `degraded` marker
/// clears: the overflow case per spec §9.47 failure behavior clears
/// "only after a flood-free window", never while surplus is still
/// arriving.
pub const DEGRADED_QUIET_WINDOW: Duration = Duration::from_mins(5);

/// Push outcome for one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// The event was enqueued.
    Accepted,
    /// The event was a redelivery inside the §9.47 window: deduplicated,
    /// never a second kick-off.
    Duplicate,
    /// The queue was at capacity: the surplus was DROPPED and the
    /// `degraded` marker was set (spec §7.3 — the bound exists so a flood
    /// hits the drop, not unbounded growth).
    Dropped,
}

/// The bounded §9.47 ingress queue. Shared between the poll task
/// (producer) and the kick-off source (consumer) behind a mutex; the
/// clock is injectable for deterministic tests.
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
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::WatchConfig`] when the state mutex is
    /// poisoned (a panic inside the queue is an internal defect and must
    /// surface, never silently drop on a broken guard).
    pub fn push(
        &self,
        event: IngressEvent,
        now_secs: u64,
    ) -> Result<PushOutcome, crate::error::DaemonError> {
        let mut state = self.lock()?;
        if state.events.len() >= self.capacity {
            state.degraded_at = Some(now_secs);
            return Ok(PushOutcome::Dropped);
        }
        state.events.push_back(event);
        Ok(PushOutcome::Accepted)
    }

    /// Drains every pending event (kick-off consumption). The events are
    /// ordering-insensitive input (spec §9.47): drain order is queue
    /// order, and consumers must not rely on it.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::WatchConfig`] when the state mutex is
    /// poisoned.
    pub fn take(&self) -> Result<Vec<IngressEvent>, crate::error::DaemonError> {
        let mut state = self.lock()?;
        Ok(state.events.drain(..).collect())
    }

    /// Whether the queue is currently degraded (an overflow happened and
    /// the flood-free quiet window has not yet elapsed). `now_secs` is
    /// the caller's clock. The clear is LAZY: evaluated on read, so the
    /// marker clears on the first read after the quiet window even with
    /// no further traffic — and a fresh overflow re-arms it immediately.
    ///
    /// # Errors
    ///
    /// Returns [`DaemonError::WatchConfig`] when the state mutex is
    /// poisoned.
    pub fn degraded(&self, now_secs: u64) -> Result<bool, crate::error::DaemonError> {
        let mut state = self.lock()?;
        match state.degraded_at {
            Some(at) if now_secs.saturating_sub(at) >= DEGRADED_QUIET_WINDOW.as_secs() => {
                // Flood-free window elapsed: the overflow case clears.
                state.degraded_at = None;
                Ok(false)
            }
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }

    /// Locks the state, mapping poison to a typed error (never
    /// `blocking_lock` — the queue is touched from async contexts, and a
    /// poisoned guard is an internal defect the caller must see).
    fn lock(&self) -> Result<MutexGuard<'_, QueueState>, crate::error::DaemonError> {
        self.inner.lock().map_err(|_| {
            crate::error::DaemonError::WatchConfig("ingress queue state poisoned".to_string())
        })
    }
}
