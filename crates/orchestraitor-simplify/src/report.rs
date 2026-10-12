//! Simplify report types: suggestions, tool statuses, and the aggregated
//! report (design §1.2 `report.rs`).

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The §9.5 normalization classes a suggestion can belong to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuggestionClass {
    /// Semantics-preserving formatting (rustfmt, rumdl).
    Format,
    /// Tool-declared safe fix (clippy machine-applicable).
    SafeFix,
    /// Semantic hint; suggest-only, never auto-rewritten.
    Semantic,
}

impl SuggestionClass {
    /// Static class label for reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Format => "format",
            Self::SafeFix => "safe_fix",
            Self::Semantic => "semantic",
        }
    }
}

/// One typed suggestion produced by the pass.
///
/// Carries evidence (the tool message) and a proposed remediation, shaped so
/// it can feed the same remediation input as review findings (design §1.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suggestion {
    /// §9.5 class.
    pub class: SuggestionClass,
    /// Worktree-relative file the suggestion applies to (when scoped).
    pub path: Option<String>,
    /// 1-based line when the tool pinned one.
    pub line: Option<u32>,
    /// Lint or tool rule identifier (e.g. `clippy::clone_on_copy`).
    pub rule: String,
    /// Human-readable evidence from the tool.
    pub message: String,
    /// Whether the pass auto-applied this suggestion.
    pub applied: bool,
}

impl Suggestion {
    /// Dedup key: class + path + line + rule. Two identical tool reports of
    /// the same location collapse to one suggestion.
    #[must_use]
    pub fn key(&self) -> (SuggestionClass, Option<String>, Option<u32>, String) {
        (self.class, self.path.clone(), self.line, self.rule.clone())
    }
}

/// Status of one underlying tool in the pass (fail-open record).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ToolStatus {
    /// The tool ran; `exit_code` is the process status.
    Ran {
        /// Static tool label.
        tool: String,
        /// Process exit code.
        exit_code: Option<i32>,
    },
    /// The tool could not run (absent binary, spawn failure, timeout).
    Unavailable {
        /// Static tool label.
        tool: String,
        /// Static failure classification.
        reason: String,
    },
}

/// Aggregated result of one simplify pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimplifyReport {
    /// Whether ANY rule-driven tool ran. False means the environment lacks
    /// the tools entirely — a typed warning for the caller, never a block.
    pub ran: bool,
    /// Whether only rule-driven tools ran (true in this slice; the
    /// model-driven tier flips it when wired).
    pub ran_rules_only: bool,
    /// Suggestions in report order, deduplicated by class+path+line+rule.
    pub suggestions: Vec<Suggestion>,
    /// Count of suggestions the pass auto-applied (Format class, and safe
    /// fixes only under the explicit config flag).
    pub auto_applied_count: usize,
    /// Per-tool outcomes in execution order.
    pub tools: Vec<ToolStatus>,
    /// Wall-clock duration of the pass.
    #[serde(with = "duration_millis")]
    pub duration: Duration,
}

/// serde helper: durations as whole milliseconds.
mod duration_millis {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn serialize<S: Serializer>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        u64::try_from(duration.as_millis())
            .unwrap_or(u64::MAX)
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        let millis = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(millis))
    }
}

impl SimplifyReport {
    /// Creates an empty report.
    #[must_use]
    pub fn new(ran: bool) -> Self {
        Self {
            ran,
            ran_rules_only: true,
            suggestions: Vec::new(),
            auto_applied_count: 0,
            tools: Vec::new(),
            duration: Duration::from_millis(0),
        }
    }

    /// Records a tool outcome.
    pub fn record_tool(&mut self, status: ToolStatus) {
        if matches!(status, ToolStatus::Ran { .. }) {
            self.ran = true;
        }
        self.tools.push(status);
    }

    /// Adds a suggestion; `applied` suggestions count toward
    /// `auto_applied_count`.
    pub fn push(&mut self, suggestion: Suggestion) {
        if suggestion.applied {
            self.auto_applied_count += 1;
        }
        self.suggestions.push(suggestion);
    }

    /// Deduplicates suggestions by class+path+line+rule, keeping the first
    /// occurrence (an `applied` record wins over a later duplicate).
    pub fn dedup(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        self.suggestions
            .retain(|suggestion| seen.insert(suggestion.key()));
        // Retained duplicates could shift the applied tally: recount after
        // the retain so `auto_applied_count` always matches the reported
        // suggestion list.
        self.auto_applied_count = self.suggestions.iter().filter(|s| s.applied).count();
    }

    /// Sets the pass duration.
    pub fn set_duration(&mut self, duration: Duration) {
        self.duration = duration;
    }

    /// Count of suggestions still unaddressed (not applied).
    #[must_use]
    pub fn unaddressed(&self, class: Option<SuggestionClass>) -> usize {
        self.suggestions
            .iter()
            .filter(|suggestion| !suggestion.applied && class.is_none_or(|c| suggestion.class == c))
            .count()
    }
}
