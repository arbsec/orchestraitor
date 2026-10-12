//! Error types for context indexing and query operations.

use std::path::PathBuf;

/// Errors returned by the context indexer and query API.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ContextError {
    /// Repository path could not be opened or traversed.
    #[error("repository path is not readable: {path}")]
    RepositoryPath {
        /// Path supplied by the caller.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// A repository entry path could not be represented relative to the root.
    #[error("path is outside repository root: {path}")]
    PathOutsideRepository {
        /// Path that failed root-relative conversion.
        path: PathBuf,
    },
    /// A source file could not be read.
    #[error("source file is not readable: {path}")]
    ReadSource {
        /// File path being read.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// A requested context item was not found in the index.
    #[error("context item was not found: {id}")]
    NotFound {
        /// Missing symbol, blob, or context identifier.
        id: String,
    },
    /// A caller supplied an invalid excerpt range.
    #[error("invalid excerpt range: start_line={start_line}, end_line={end_line}")]
    InvalidRange {
        /// First requested one-based line number.
        start_line: u32,
        /// Last requested one-based line number.
        end_line: u32,
    },
    /// Tree-sitter rejected a language grammar.
    #[error("tree-sitter language setup failed for {language}")]
    LanguageSetup {
        /// Language name being configured.
        language: &'static str,
    },
}
