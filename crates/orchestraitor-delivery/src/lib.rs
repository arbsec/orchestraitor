//! Spec-driven autonomous delivery (spec §9.33).
//!
//! This crate owns the orchestration machinery that turns the project backlog
//! into merge-ready change sets: task metadata (§9.33.2), the backlog DAG,
//! the runner loop, failure classification, and the adversarial review loop.
//! Security decisions are never made here — policy, approvals, sandboxing, and
//! receipts belong to Arbitraitor (spec §2.2, §9.33.7).

pub mod dag;
pub mod metadata;
pub mod schedule;

pub use dag::{DagError, TaskDag};
pub use metadata::{
    Autonomy, BacklogTaskId, CompletionEvidence, DomainId, MetadataError, RiskClass,
    SCHEMA_VERSION, SpecRef, TaskMetadata, VerificationRef,
};
pub use schedule::{ParallelScheduler, SchedulerConfig, SchedulerConfigError};
