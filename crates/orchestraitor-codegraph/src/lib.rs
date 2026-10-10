//! Read-only codegraph MCP server (spec `10-orchestrator.md` §9.38):
//! symbol/call-graph queries over the [`orchestraitor_context`] index.
//!
//! The server is a thin, read-only projection over one content-addressed
//! index file. It executes nothing, reads no network, and never touches the
//! working tree — containment (§9.18.1) is fingerprinting plus the
//! Arbitraitor sandbox around the process, not policy inside this crate.
//!
//! Tool surface (all read-only):
//!
//! - `symbols` — name search over indexed symbols
//! - `symbol` — signature + bounded body for one symbol id
//! - `references` — textual references to a symbol
//! - `calls` — call-graph edges (callers or callees)
//!
//! Persistence: the index is serialized to `.orchestraitor/codegraph.json`
//! (serde over the index maps — all record types are `Serialize`).

pub mod error;
pub mod persist;
pub mod server;

pub use error::CodegraphError;
pub use persist::{load_index, store_index};
pub use server::{CodegraphServer, serve_stdio};
