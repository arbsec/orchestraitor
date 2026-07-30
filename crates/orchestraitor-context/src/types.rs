//! Public context index and query data types.

use orchestraitor_model::Digest;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::provenance::ProvenanceEnvelope;

/// Stable source pointer in a repository or session.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceRef(pub String);

/// Stable symbol identifier inside the index.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SymbolId(pub String);

/// Language classification used by the baseline indexer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageKind {
    /// Rust source.
    Rust,
    /// TypeScript source.
    TypeScript,
    /// TSX source.
    Tsx,
    /// Python source.
    Python,
    /// JavaScript source.
    JavaScript,
    /// JSX source.
    Jsx,
    /// Go source.
    Go,
    /// Bash or POSIX shell source.
    Bash,
}

/// Symbol category emitted by tree-sitter queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// Function or free-standing callable.
    Function,
    /// Method owned by a type or class.
    Method,
    /// Type, struct, class, enum, trait, interface, or alias.
    Type,
    /// Module declaration.
    Module,
    /// Constant or variable declaration.
    Variable,
}

/// One-based line and zero-based byte-column range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocationRange {
    /// First one-based line.
    pub start_line: u32,
    /// First zero-based column in bytes.
    pub start_column: u32,
    /// Last one-based line.
    pub end_line: u32,
    /// Last zero-based column in bytes.
    pub end_column: u32,
}

/// Content-addressed source blob record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRecord {
    /// Repository-relative path.
    pub path: PathBuf,
    /// SHA-256 digest of file bytes.
    pub digest: Digest,
    /// Detected language.
    pub language: Option<LanguageKind>,
    /// Number of times this blob was processed by tree-sitter.
    pub process_count: u64,
    /// Provenance envelope for the blob item.
    pub provenance: ProvenanceEnvelope,
}

/// Indexed symbol record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolRecord {
    /// Stable symbol id.
    pub id: SymbolId,
    /// Symbol display name.
    pub name: String,
    /// Symbol kind.
    pub kind: SymbolKind,
    /// Repository-relative source path.
    pub path: PathBuf,
    /// Source location.
    pub range: LocationRange,
    /// Compact signature text.
    pub signature: String,
    /// Blob digest that owns this symbol.
    pub blob_digest: Digest,
    /// Provenance envelope for this context item.
    pub provenance: ProvenanceEnvelope,
}

/// Textual reference to a symbol-like name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceRecord {
    /// Referenced symbol id, when resolved within this index.
    pub target: Option<SymbolId>,
    /// Reference text.
    pub name: String,
    /// Source path containing the reference.
    pub path: PathBuf,
    /// Reference location.
    pub range: LocationRange,
    /// Provenance envelope for this context item.
    pub provenance: ProvenanceEnvelope,
}

/// Directed call edge between indexed symbols.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEdge {
    /// Caller symbol.
    pub caller: SymbolId,
    /// Callee symbol.
    pub callee: SymbolId,
    /// Provenance envelope for this context item.
    pub provenance: ProvenanceEnvelope,
}

/// Source excerpt with provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excerpt {
    /// Repository-relative path.
    pub path: PathBuf,
    /// First one-based line included.
    pub start_line: u32,
    /// Last one-based line included.
    pub end_line: u32,
    /// Excerpt text.
    pub text: String,
    /// Provenance envelope for this context item.
    pub provenance: ProvenanceEnvelope,
}

/// Any bounded context item returned by query APIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextItem {
    /// Blob metadata.
    Blob(BlobRecord),
    /// Symbol metadata and signature.
    Symbol(SymbolRecord),
    /// Reference location.
    Reference(ReferenceRecord),
    /// Call edge.
    CallEdge(CallEdge),
    /// Source excerpt.
    Excerpt(Excerpt),
}
