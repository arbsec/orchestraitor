//! Output quarantine, promotion pipeline, and versioned transaction graph.
//!
//! This crate implements the Orchestraitor-owned developer workflow for spec §9.14
//! (output quarantine and promotion) and §9.4.3 (versioned transaction graph).
//!
//! # Security boundary
//!
//! Per spec §9.14, "Security-sensitive classification, policy, and promotion
//! authorization are owned by Arbitraitor; Orchestraitor owns the developer
//! workflow and presentation." This crate therefore provides:
//!
//! - **Descriptive classification** of changed paths and content into the 17
//!   output classes defined in [`orchestraitor_model::OutputClass`]. This
//!   classification is *input* to Arbitraitor's policy engine — it is never an
//!   allow/deny decision.
//! - **Trust-sensitive destination detection** — a descriptive flag indicating
//!   whether a target path lives in a location that affects host or tool
//!   execution. The actual enforcement belongs to Arbitraitor.
//! - **Semantic and textual diff generation** for the review surface.
//! - **Promotion pipeline orchestration** — the workflow that classifies,
//!   diffs, delegates to a [`PolicyGate`] (implemented by Arbitraitor), prompts
//!   through a [`PromptSink`] (implemented by the trusted UI), and applies
//!   through a [`TrustedController`] (implemented by the workspace controller).
//!   This crate never makes a security decision.
//! - **Versioned transaction graph** — the history DAG from spec §9.4.3 with
//!   `history`, `checkpoint`, `restore`, `branch`, `compare`, `undo`, and
//!   `redo` operations.
//!
//! This crate contains **no security enforcement logic**. It never decides
//! whether a promotion is allowed — that is exclusively Arbitraitor's role
//! (spec §2.2, §16).

#![forbid(unsafe_code)]

pub mod classify;
pub mod controller;
pub mod destination;
pub mod diff;
pub mod error;
pub mod graph;
pub mod pipeline;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_classify;

pub use classify::{Classification, ClassificationInput, FileMetadata, classify};
pub use controller::{AppliedChange, FileState, InMemoryController, TrustedController};
pub use destination::{DestinationSensitivity, detect_sensitivity};
pub use diff::{
    ChangeKind, DiffHunk, DiffLine, SemanticDiff, TextualDiff, compute_semantic_diff,
    compute_textual_diff,
};
pub use error::PromotionError;
pub use graph::{NodeId, TransactionGraph, TransactionNode, VerificationEvidence};
pub use pipeline::{Change, PolicyDecision, PolicyGate, PromotionPipeline, PromptSink, promote};
