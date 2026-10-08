//! Headless one-shot bootstrap mini-worker (issue #310; spec
//! `10-orchestrator.md` §9.38, `60-milestones.md` MVP-6).
//!
//! Mini-swe-agent pattern: a task description goes in, a bounded loop of
//! model calls and tool dispatches runs, and a structured [`WorkerRun`] JSON
//! comes out with a process-style exit code. The loop consumes any
//! [`orchestraitor_provider_api::ProviderTransport`] implementation through
//! the trait boundary; tests drive it with the deterministic simulator in
//! `orchestraitor-testkit` (spec `50-contracts-data.md` §21.3 — no live
//! provider in CI).
//!
//! Exactly four tools exist: `read_file`, `search`, `bash`, `write_file`.
//! Every call is receipted; any capability request outside the four is a
//! typed refusal, recorded and fed back to the model. `bash` crosses the
//! Arbitraitor boundary through the #311 mediation module
//! (`MediatedWorker::spawn` preflight + `run_bash`); file tools are confined
//! to the task worktree with symlink-escape-safe path validation; worker
//! writes are recorded as untrusted output for the delivery seam (spec
//! `40-arbitraitor-integration.md` §9.14). This crate implements no security
//! primitive: sandboxing, classification, and promotion authorization belong
//! to Arbitraitor (spec §2.2).

#![forbid(unsafe_code)]
#![allow(
    clippy::module_name_repetitions,
    reason = "public worker types are clearer with the Worker prefix (WorkerBudgets, WorkerRun, WorkerTask)"
)]

mod action;
pub mod bootstrap;
mod budget;
pub mod delivery;
mod error;
mod mediator;
mod model;
mod paths;
mod prompt;
mod result;
mod run;
mod search;
mod subsession;
mod task;
mod tooldef;
mod tools;

pub use budget::{
    BudgetEcho, DEFAULT_DAILY_SPEND_SOFT_CAP_USD, DEFAULT_MAX_ATTEMPTS,
    DEFAULT_MAX_CONCURRENT_WORKERS, DEFAULT_MAX_CONSECUTIVE_FORMAT_ERRORS, DEFAULT_MAX_REPLANS,
    DEFAULT_MAX_TURNS_PER_ATTEMPT, DEFAULT_RETRY_BASE_DELAY, DEFAULT_RETRY_MAX_DELAY,
    DEFAULT_RUN_BUDGET, DEFAULT_STALL_TIMEOUT, DEFAULT_WORKER_TIMEOUT, WorkerBudgets,
    backoff_delay,
};
pub use delivery::{
    DeliveryError, DeliveryOutcome, DeliveryRequest, DeliverySink, PendingDeliverySink,
};
pub use error::WorkerError;
pub use mediator::{BashMediator, MediatedBashMediator};
pub use orchestraitor_arbitraitor_client::mediation::{MediatedRun, MediationError};
pub use orchestraitor_model::{ModelId, ProviderId};
pub use result::{FailureClass, RunStatus, TypedFailure, UsageTotals, WorkerConfig, WorkerRun};
pub use run::run_worker;
pub use subsession::{
    RoleRoutingEvidence, SubsessionError, SubsessionOutcome, SubsessionParent,
    default_internal_tools, parent_failure_class, run_subsession,
};
pub use task::{FixtureTaskSource, TaskLoadError, TaskSource, WorkerTask};
pub use tooldef::{
    InternalTool, MAX_QUESTION_CHARS, MAX_TOOL_ID_CHARS, ToolBudget, ToolDefinition, ToolMechanism,
    is_reserved_tool_id, is_valid_tool_id,
};
pub use tools::ToolReceipt;
