#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use orchestraitor_model::{ContextOrigin, TrustClass};
use tempfile::TempDir;

use crate::{ContextAge, ContextItem, ContextQuery, Indexer, SymbolKind};

#[test]
fn index_fixture_repo_and_find_symbol_tuple() {
    let repo = fixture_repo();
    let mut indexer = Indexer::default();

    let report = indexer.index_repository(repo.path()).unwrap();
    let query = ContextQuery::new(indexer.index());

    let symbols = query.find_symbol("add", Some(SymbolKind::Function), Some(Path::new("src")));
    assert_eq!(report.observed_blobs, 1);
    assert_eq!(symbols.len(), 1);
    assert_eq!(symbols[0].path, Path::new("src/lib.rs"));
    assert_eq!(symbols[0].range.start_line, 1);
    assert_eq!(
        symbols[0].signature,
        "pub fn add(left: i32, right: i32) -> i32 {"
    );
}

#[test]
fn unchanged_blob_is_not_reprocessed() {
    let repo = fixture_repo();
    let mut indexer = Indexer::default();

    let first = indexer.index_repository(repo.path()).unwrap();
    let second = indexer.index_repository(repo.path()).unwrap();

    assert_eq!(first.processed_blobs, 1);
    assert_eq!(second.processed_blobs, 0);
    assert_eq!(second.skipped_unchanged_blobs, 1);
    assert_eq!(
        indexer.index().blobs()[Path::new("src/lib.rs")].process_count,
        0
    );
}

#[test]
fn provenance_envelope_is_present_and_untrusted() {
    let repo = fixture_repo();
    let mut indexer = Indexer::default();

    indexer.index_repository(repo.path()).unwrap();
    let query = ContextQuery::new(indexer.index());
    let symbol = query
        .find_symbol("add", Some(SymbolKind::Function), None)
        .pop()
        .unwrap();

    assert_eq!(symbol.provenance.origin, ContextOrigin::RepositoryContent);
    assert_eq!(symbol.provenance.trust_class, TrustClass::Untrusted);
    assert_eq!(symbol.provenance.age, ContextAge::Current);
    assert!(symbol.provenance.source_ref.0.contains("src/lib.rs"));
}

#[test]
fn appendix_e_queries_return_bounded_provenance_items() {
    let repo = fixture_repo();
    let mut indexer = Indexer::default();

    indexer.index_repository(repo.path()).unwrap();
    let query = ContextQuery::new(indexer.index());
    let symbol = query
        .find_symbol("add", Some(SymbolKind::Function), None)
        .pop()
        .unwrap();

    let signature = query.symbol_signature(&symbol.id).unwrap();
    let refs = query.find_references(&symbol.id, Some(1));
    let hits = query.search_text("add", Some(".rs"), Some(1));
    let excerpt = query.read_excerpt(Path::new("src/lib.rs"), 1, 3).unwrap();

    assert!(matches!(signature, ContextItem::Symbol(_)));
    assert_eq!(refs.len(), 1);
    assert_eq!(hits.len(), 1);
    assert!(excerpt.text.contains("pub fn add"));
    assert_eq!(excerpt.provenance.trust_class, TrustClass::Untrusted);
}

fn fixture_repo() -> TempDir {
    let repo = TempDir::new().unwrap();
    let src = repo.path().join("src");
    fs::create_dir(&src).unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub fn add(left: i32, right: i32) -> i32 {\n    left + right\n}\n\npub fn twice(value: i32) -> i32 {\n    add(value, value)\n}\n",
    )
    .unwrap();
    repo
}
