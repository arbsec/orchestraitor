//! Bootstrap GitHub Projects v2 board provider (spec §9.43 subset).
//!
//! Thin slice for the bootstrap loop: ready-queue reads (leaf Task/Bug with
//! the configured target/status values and no unresolved `blockedBy`, spec
//! §9.40) and verified Status writes. Project/field/option node IDs resolve at
//! runtime from the human-readable names in
//! `.agents/project/github-project.local.toml` and are cached under
//! `$XDG_CACHE_HOME/orchestraitor/` — never inside the repository.
//!
//! Board content is untrusted input (spec §6.1): item titles are carried as
//! inert payload data, never executed or interpolated into commands. Auth is
//! injected via [`BoardAuth`]; the bootstrap stub resolves a configured
//! `secret://` URI and never sniffs ambient credentials.

#![forbid(unsafe_code)]

pub mod auth;
pub mod cache;
pub mod client;
pub mod config;
pub mod error;
mod item;
pub mod queue;

pub use auth::{BoardAuth, SecretUriAuth};
pub use cache::{NodeIdCacheFile, ProjectNodeIds, default_cache_path};
pub use client::{BoardClient, MoveOutcome};
pub use config::BoardProjectConfig;
pub use error::BoardError;
pub use item::{ItemFacts, ItemSkip};
pub use queue::{ReadyItem, SkipWarning, ready_queue};
