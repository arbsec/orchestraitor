//! Tree-sitter baseline context indexer and bounded context query API.
//!
//! This crate implements the MVP slice of spec §9.15, §9.15.1, §9.16, and
//! Appendix E. It performs local, content-addressed repository indexing only;
//! language servers are deliberately not used by the indexer. Security and
//! data-release enforcement remain Arbitraitor-owned.

#![forbid(unsafe_code)]

mod error;
mod index;
mod language;
mod parser;
mod provenance;
mod query;
mod types;
mod walker;

pub use error::ContextError;
pub use index::{ContextIndex, IndexReport, Indexer};
pub use provenance::{ContextAge, ProvenanceEnvelope};
pub use query::{
    ContextDiagnostic, ContextQuery, ExpandedContext, RelatedTest, RepositorySummary, SearchHit,
    SymbolBody,
};
pub use types::{
    BlobRecord, CallEdge, ContextItem, Excerpt, LanguageKind, LocationRange, ReferenceRecord,
    SourceRef, SymbolId, SymbolKind, SymbolRecord,
};

#[cfg(test)]
mod tests;
