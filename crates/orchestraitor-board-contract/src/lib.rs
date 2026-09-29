//! `orchestraitor-board-contract`: the `BoardProvider` contract and
//! write-through read-cache semantics (spec `10-orchestrator.md` §9.43,
//! Ledger D5, issue #318).
//!
//! # The contract (spec §9.43)
//!
//! Planning and task tracking run against a pluggable [`BoardProvider`]
//! covering six areas, each with typed errors ([`BoardContractError`]):
//!
//! 1. **items** — tasks, bugs, epics, features: stable identity
//!    ([`BoardItemId`]), type ([`BoardItemType`]), title, body. Bodies are
//!    opaque untrusted data (spec §6.1): the contract never parses them.
//! 2. **statuses** — board columns / status field values ([`BoardStatus`]).
//! 3. **fields** — typed custom fields (priority, target, risk, size, ...)
//!    with typed values ([`BoardFieldValue`]), never stringly.
//! 4. **dependency edges** — native `blockedBy` edges
//!    ([`DependencyEdge`]), cross-board within a workspace (§9.42). The
//!    board's edges ARE the dependency graph (§9.40): the contract exposes
//!    edges read + write, never a mirrored second graph.
//! 5. **cross-references** — links between items, including cross-repository
//!    links ([`CrossReference`]).
//! 6. **search** — typed, conjunctive filters over items and fields
//!    ([`BoardSearch`]); deliberately not a query language.
//!
//! # Cache semantics (Ledger D5)
//!
//! - **Single canonical provider.** A workspace has exactly one canonical
//!   [`BoardProvider`]; work items live ON the provider — never dual-master.
//! - **Write-through read-cache.** [`CachedBoard`] wraps any provider with
//!   the same trait: writes go through the provider first, then the cache
//!   refreshes from provider state. The cache is never authoritative.
//! - **Last-synced stamps.** Cached reads ([`CachedItem`]) carry
//!   `last_synced_ms` so stale reads are visible to consumers.
//! - **Board-wins reconcile.** After a write-through, the refresh step
//!   compares provider state with what the write assumed; any mismatch
//!   emits [`CacheEvent::BoardDiverged`] and the cache refreshes to the
//!   provider's state.
//! - **Runtime state is out of scope.** Decisions, leases, heartbeats,
//!   events, receipts, and budgets stay local-only elsewhere, keyed by
//!   [`BoardItemId`]; the board sees only coarse status transitions.
//!
//! # Conformance
//!
//! [`InMemoryBoardProvider`] is the deterministic, no-I/O reference
//! implementation. The feature-gated `conformance` module (default-on)
//! exposes `conformance::run_conformance`, which runs ANY `BoardProvider`
//! through the full contract; future providers (GitHub, sqlite — follow-ups,
//! not in this crate) must pass the same suite via a dev-dependency on this
//! crate.
//!
//! # Security posture
//!
//! This crate implements no security primitive. Board content is untrusted
//! input (spec §6.1): titles and bodies are carried as inert payload data,
//! never executed or interpolated. Errors never embed board content or
//! credentials (spec §9.23.4).

#![forbid(unsafe_code)]

pub mod cache;
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod error;
pub mod in_memory;
pub mod provider;
pub mod types;

pub use cache::{CacheEvent, CachedBoard, CachedItem, Divergence};
pub use error::BoardContractError;
pub use in_memory::{BoardSetup, InMemoryBoardProvider};
pub use provider::BoardProvider;
pub use types::{
    BoardField, BoardFieldKind, BoardFieldValue, BoardItem, BoardItemId, BoardItemType,
    BoardSearch, BoardStatus, CrossReference, DependencyEdge,
};
