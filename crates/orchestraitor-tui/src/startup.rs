//! Startup progress feedback (spec §13.3.1).
//!
//! Any startup or shutdown operation expected to take ≥200 ms MUST emit
//! user-visible progress feedback. The TUI shows a single-line status banner
//! naming the operation, an indeterminate progress indicator, elapsed time,
//! and an item count. Operations exceeding 1 s offer an explanation and allow
//! the user to skip non-critical ones.

use std::time::{Duration, Instant};

/// Threshold above which a progress banner is shown (spec §13.3.1).
pub const BANNER_THRESHOLD: Duration = Duration::from_millis(200);

/// Threshold above which a skip affordance and explanation are shown.
pub const SKIP_THRESHOLD: Duration = Duration::from_secs(1);

/// A named startup or shutdown operation tracked by the progress banner.
#[derive(Debug, Clone)]
pub struct StartupOperation {
    /// Human-readable operation name shown in the banner.
    pub name: String,
    /// Optional explanation shown when the operation exceeds the skip threshold.
    pub explanation: Option<String>,
    /// Whether the operation is skippable (non-critical).
    pub skippable: bool,
    /// Current item index (1-based) when loading multiple items.
    pub current: Option<u32>,
    /// Total item count when loading multiple items.
    pub total: Option<u32>,
}

impl StartupOperation {
    /// Creates a new startup operation with the given name.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            explanation: None,
            skippable: false,
            current: None,
            total: None,
        }
    }

    /// Sets the explanation text shown after the skip threshold.
    #[must_use]
    pub fn with_explanation(mut self, explanation: impl Into<String>) -> Self {
        self.explanation = Some(explanation.into());
        self
    }

    /// Marks the operation as skippable.
    #[must_use]
    pub fn skippable(mut self) -> Self {
        self.skippable = true;
        self
    }

    /// Sets the current and total item counts.
    #[must_use]
    pub fn with_count(mut self, current: u32, total: u32) -> Self {
        self.current = Some(current);
        self.total = Some(total);
        self
    }

    /// Returns the count text, e.g. "loading 4 of 12 MCP servers".
    #[must_use]
    pub fn count_text(&self) -> Option<String> {
        match (self.current, self.total) {
            (Some(current), Some(total)) => Some(format!("{current} of {total}")),
            _ => None,
        }
    }
}

/// Live progress state for a single in-flight operation.
#[derive(Debug, Clone)]
pub struct StartupProgress {
    /// The operation being tracked.
    pub operation: StartupOperation,
    /// When the operation started.
    pub started_at: Instant,
}

impl StartupProgress {
    /// Creates a new progress tracker for the given operation.
    #[must_use]
    pub fn new(operation: StartupOperation) -> Self {
        Self {
            operation,
            started_at: Instant::now(),
        }
    }

    /// Returns elapsed time since the operation started.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Returns whether the banner should be visible (elapsed ≥ threshold).
    #[must_use]
    pub fn should_show(&self) -> bool {
        self.elapsed() >= BANNER_THRESHOLD
    }

    /// Returns whether the skip affordance should be shown.
    #[must_use]
    pub fn should_offer_skip(&self) -> bool {
        self.operation.skippable && self.elapsed() >= SKIP_THRESHOLD
    }
}

/// Overall startup state machine.
#[derive(Debug, Clone, Default)]
pub enum StartupState {
    /// No startup operation is in flight; the main layout is active.
    #[default]
    Idle,
    /// A startup operation is in progress.
    InProgress(StartupProgress),
}

impl StartupState {
    /// Returns whether a startup banner should be rendered.
    #[must_use]
    pub fn is_active(&self) -> bool {
        match self {
            Self::Idle => false,
            Self::InProgress(progress) => progress.should_show(),
        }
    }

    /// Returns the progress data if a banner is active.
    #[must_use]
    pub fn progress(&self) -> Option<&StartupProgress> {
        match self {
            Self::Idle => None,
            Self::InProgress(progress) => Some(progress),
        }
    }

    /// Transitions to an in-progress state with the given operation.
    pub fn start(&mut self, operation: StartupOperation) {
        *self = Self::InProgress(StartupProgress::new(operation));
    }

    /// Transitions to idle, clearing any in-progress operation.
    pub fn finish(&mut self) {
        *self = Self::Idle;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn banner_hidden_under_threshold() {
        let progress = StartupProgress::new(StartupOperation::new("test"));
        assert!(!progress.should_show());
    }

    #[test]
    fn count_text_formats_correctly() {
        let op = StartupOperation::new("loading MCP servers").with_count(4, 12);
        assert_eq!(op.count_text().as_deref(), Some("4 of 12"));
    }

    #[test]
    fn skip_offered_after_threshold() {
        let mut progress = StartupProgress::new(
            StartupOperation::new("models.dev refresh")
                .skippable()
                .with_explanation("using bundled snapshot, prices may be stale"),
        );
        // Simulate elapsed time by creating a progress that started in the past.
        progress.started_at = Instant::now()
            .checked_sub(SKIP_THRESHOLD + Duration::from_millis(10))
            .unwrap();
        assert!(progress.should_offer_skip());
    }
}
