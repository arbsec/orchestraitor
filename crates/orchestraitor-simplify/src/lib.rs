//! Pre-landing code simplification pass (spec §9.5 normalization classes).
//!
//! The simplify pass is a batch, repo-scoped code-quality engine: it runs the
//! rule-driven tools the repository already uses (cargo clippy, cargo fmt,
//! rumdl, cargo-machete) and renders their findings as typed
//! [`Suggestion`]s in the three §9.5 classes:
//!
//! - **Format** — semantics-preserving (rustfmt, rumdl fixes); auto-applied
//!   when the configuration allows it.
//! - **Safe fix** — clippy machine-applicable suggestions; suggest-only by
//!   default, auto-applied only when the configuration explicitly allows it
//!   AND the CLI fix policy is `safe`; each suggestion is marked applied
//!   only when a verify check no longer reports it. (The Arbitraitor
//!   output-classification gate that will additionally scope safe-fix
//!   auto-apply is PR-2 scope.)
//! - **Semantic** — dead-code hints and pedantic findings; suggest-only,
//!   never auto-rewritten.
//!
//! Fail semantics are **fail-open, loudly** (design §1.4): an absent tool or
//! a timeout yields a typed warning and the pass continues; a non-zero tool
//! exit is normal (it is how tools report findings) and still parses. The
//! pass never blocks a commit, a worker, or a push.
//!
//! The crate is synchronous and I/O-injectable like the delivery modules:
//! every process spawn goes through the [`executor::SimplifyExecutor`]
//! trait, so tests script tool output instead of spawning real toolchains.

#![forbid(unsafe_code)]

pub mod clippy;
pub mod deadcode;
pub mod executor;
pub mod format;
pub mod report;

use std::path::Path;
use std::time::Duration;

pub use executor::{
    ExecOutput, ProcessExecutor, SimplifyError, SimplifyExecutor, ToolOutcome, ToolSpec,
};
pub use report::{SimplifyReport, Suggestion, SuggestionClass, ToolStatus};

/// Rule-driven simplify configuration (the `[simplify]` config table).
///
/// Field defaults mirror the built-in config defaults; the CLI layer maps
/// the parsed `orchestraitor_core::config::SimplifyConfig` onto this struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimplifyConfig {
    /// Include `clippy::pedantic` findings.
    pub pedantic: bool,
    /// Maximum normalization passes over the target set (spec §9.5
    /// convergence bound; the rule-driven tier currently converges in one).
    pub max_passes: u32,
    /// Maximum files examined per pass.
    pub max_files: usize,
    /// Whether the model-driven tier is enabled. Parsed but unused in this
    /// slice: the tier is wired when the review-loop PR lands its role
    /// routing (the flag exists so the config surface is final).
    pub model_pass: bool,
}

impl Default for SimplifyConfig {
    fn default() -> Self {
        Self {
            pedantic: false,
            max_passes: 2,
            max_files: 200,
            model_pass: false,
        }
    }
}

impl SimplifyConfig {
    /// Rejects configuration that would make the pass unbounded (fail-closed
    /// construction, matching the runner convention).
    ///
    /// # Errors
    ///
    /// Returns [`SimplifyError::InvalidConfig`] when a bound is zero.
    pub fn validate(&self) -> Result<(), SimplifyError> {
        if self.max_passes == 0 {
            return Err(SimplifyError::InvalidConfig {
                detail: "max_passes must be at least 1".to_string(),
            });
        }
        if self.max_files == 0 {
            return Err(SimplifyError::InvalidConfig {
                detail: "max_files must be at least 1".to_string(),
            });
        }
        Ok(())
    }
}

/// Fix policy for one pass, resolved from the config plus CLI flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FixPolicy {
    /// Run checks only; never modify files.
    #[default]
    None,
    /// Auto-apply Format-class fixes (rustfmt, rumdl) when the config
    /// allows it.
    Format,
    /// Additionally auto-apply clippy machine-applicable suggestions on
    /// `.rs` files. In this slice the safe-fix class is suggest-only even
    /// under `Safe` unless the config's `auto_apply_safe_fixes` is set;
    /// the Arbitraitor classification gate (PR-2) further scopes it.
    Safe,
}

/// Facade over one rule-driven simplify pass.
///
/// The pass never blocks: every tool outcome is captured in the report and
/// the caller decides what (if anything) to do with it.
pub struct SimplifyPass<'a> {
    config: &'a SimplifyConfig,
    executor: &'a dyn SimplifyExecutor,
    fix: FixPolicy,
    auto_apply_format: bool,
    auto_apply_safe_fixes: bool,
}

impl std::fmt::Debug for SimplifyPass<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SimplifyPass")
            .field("config", &self.config)
            .field("fix", &self.fix)
            .field("auto_apply_format", &self.auto_apply_format)
            .field("auto_apply_safe_fixes", &self.auto_apply_safe_fixes)
            .finish_non_exhaustive()
    }
}

impl<'a> SimplifyPass<'a> {
    /// Creates a pass bound to `config` and the injected executor,
    /// validating the configuration up front (fail-closed construction).
    ///
    /// # Errors
    ///
    /// Returns [`SimplifyError::InvalidConfig`] when the configuration fails
    /// [`SimplifyConfig::validate`].
    pub fn new(
        config: &'a SimplifyConfig,
        executor: &'a dyn SimplifyExecutor,
        fix: FixPolicy,
        auto_apply_format: bool,
        auto_apply_safe_fixes: bool,
    ) -> Result<Self, SimplifyError> {
        config.validate()?;
        Ok(Self {
            config,
            executor,
            fix,
            auto_apply_format,
            auto_apply_safe_fixes,
        })
    }

    /// Runs the rule-driven pass over `root` and returns the report.
    ///
    /// The target set is the worktree itself; `--staged`/`--paths` scoping
    /// filters the reported suggestions and applies only to presentation
    /// (the underlying tools are workspace-scoped). Format fixes are
    /// applied only when the fix policy is [`FixPolicy::Format`] or
    /// [`FixPolicy::Safe`] AND the config allows it; safe fixes auto-apply
    /// only under [`FixPolicy::Safe`] AND `auto_apply_safe_fixes` (and, in
    /// this slice, only for clippy machine-applicable suggestions on `.rs`
    /// files — the Arbitraitor classification gate is pending, so every
    /// non-Format auto-apply stays conservatively narrow).
    #[must_use]
    pub fn run(&self, root: &Path) -> SimplifyReport {
        let started = std::time::Instant::now();
        let mut report = SimplifyReport::new(false);

        // Format class: rustfmt + rumdl. Auto-apply only under policy.
        let apply_format =
            matches!(self.fix, FixPolicy::Format | FixPolicy::Safe) && self.auto_apply_format;
        let (format_statuses, format_suggestions) = format::run(self.executor, root, apply_format);
        for status in format_statuses {
            report.record_tool(status);
        }
        for suggestion in format_suggestions {
            report.push(suggestion);
        }

        // Safe-fix + semantic classes: clippy.
        let apply_safe = matches!(self.fix, FixPolicy::Safe) && self.auto_apply_safe_fixes;
        let (clippy_statuses, clippy_suggestions) =
            clippy::run(self.executor, root, self.config, apply_safe);
        for status in clippy_statuses {
            report.record_tool(status);
        }
        for suggestion in clippy_suggestions {
            report.push(suggestion);
        }

        // Semantic class: dead code (suggest-only, never auto-rewritten).
        let (dead_status, dead_suggestions) = deadcode::run(self.executor, root, self.config);
        report.record_tool(dead_status);
        for suggestion in dead_suggestions {
            report.push(suggestion);
        }

        report.dedup();
        report.set_duration(started.elapsed());
        report
    }
}

/// Wall-clock cap for one tool invocation (fail-open: a timeout is a
/// captured status, never a hang).
pub(crate) const TOOL_TIMEOUT: Duration = Duration::from_mins(2);

#[cfg(test)]
mod tests;
