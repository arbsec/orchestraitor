#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use orchestraitor_model::{ContextOrigin, Digest, TrustClass};
use tempfile::TempDir;

use crate::{BlobRecord, ContextAge, ContextItem, ContextQuery, Indexer, LanguageKind, SymbolKind};

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
    let digest = indexer
        .index()
        .paths()
        .get(Path::new("src/lib.rs"))
        .unwrap()
        .clone();
    let blob = indexer.index().blob_for_digest(&digest).unwrap();
    assert_eq!(blob.process_count, 0);
}

#[test]
fn moved_blob_is_not_reprocessed() {
    let repo = fixture_repo();
    let mut indexer = Indexer::default();

    // First pass: index at the original location.
    let first = indexer.index_repository(repo.path()).unwrap();
    let original_digest = indexer
        .index()
        .paths()
        .get(Path::new("src/lib.rs"))
        .cloned()
        .unwrap();
    assert_eq!(first.processed_blobs, 1);

    // Move the file within the repository and reindex. The content is identical,
    // so the second pass should not reparse the blob.
    let original = repo.path().join("src/lib.rs");
    let moved = repo.path().join("src/math.rs");
    fs::rename(&original, &moved).unwrap();
    let second = indexer.index_repository(repo.path()).unwrap();

    assert_eq!(second.processed_blobs, 0, "moved blob should be reused");
    assert_eq!(second.skipped_unchanged_blobs, 1);
    assert!(
        indexer
            .index()
            .paths()
            .get(Path::new("src/math.rs"))
            .is_some(),
        "new path must be visible in the index"
    );
    assert_eq!(
        indexer.index().paths().get(Path::new("src/lib.rs")),
        None,
        "old path must be evicted once the file moves"
    );
    assert_eq!(
        indexer.index().blobs().get(&original_digest).map(blob_path),
        Some(Path::new("src/math.rs").to_path_buf()),
    );
}

#[test]
fn deleted_files_are_evicted_on_reindex() {
    let repo = fixture_repo();
    let mut indexer = Indexer::default();

    let first = indexer.index_repository(repo.path()).unwrap();
    assert_eq!(first.observed_blobs, 1);

    let deleted = repo.path().join("src/lib.rs");
    fs::remove_file(&deleted).unwrap();

    let second = indexer.index_repository(repo.path()).unwrap();
    assert_eq!(second.observed_blobs, 0);
    assert_eq!(second.skipped_unchanged_blobs, 0);
    assert_eq!(second.processed_blobs, 0);
    assert!(indexer.index().paths().is_empty());
    assert!(indexer.index().symbols().is_empty());
    assert!(indexer.index().blobs().is_empty());
    assert!(indexer.index().references().is_empty());
    assert!(indexer.index().calls().is_empty());
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
fn reference_provenance_matches_owning_blob() {
    let repo = fixture_repo();
    let mut indexer = Indexer::default();

    indexer.index_repository(repo.path()).unwrap();
    let query = ContextQuery::new(indexer.index());
    let add = query
        .find_symbol("add", Some(SymbolKind::Function), None)
        .pop()
        .unwrap();

    let refs = query.find_references(&add.id, None);
    // `twice` calls `add` and is the only consumer in the fixture.
    let call_site = refs
        .iter()
        .find(|reference| reference.path == Path::new("src/lib.rs"))
        .unwrap_or_else(|| panic!("call site reference must be present"));
    assert_eq!(call_site.blob_digest, add.blob_digest);
    assert_eq!(call_site.provenance.digest, add.blob_digest);
    assert!(call_site.provenance.source_ref.0.contains("src/lib.rs"));
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

    let summary = query.repository_summary();
    assert_eq!(summary.blob_count, 1);
    assert_eq!(summary.path_count, 1);
    assert_eq!(summary.symbol_count, 2);
    assert_eq!(summary.call_edge_count, 1);

    let signature = query.symbol_signature(&symbol.id).unwrap();
    let body = query.symbol_body(&symbol.id, Some(1)).unwrap();
    let refs = query.find_references(&symbol.id, Some(1));
    let hits = query.search_text("add", Some(".rs"), Some(1));
    let excerpt = query.read_excerpt(Path::new("src/lib.rs"), 1, 3).unwrap();
    let related = query.related_tests(&symbol.id);
    let diagnostics = query.diagnostics(Some("add"));
    let expanded = query.expand_context(&symbol.id.0);

    assert!(matches!(signature, ContextItem::Symbol(_)));
    assert_eq!(refs.len(), 1);
    assert_eq!(hits.len(), 1);
    assert!(excerpt.text.contains("pub fn add"));
    assert_eq!(excerpt.provenance.trust_class, TrustClass::Untrusted);
    assert_eq!(body.lines_used, 1);
    assert!(body.truncated);
    assert!(related.is_empty());
    assert_eq!(diagnostics.len(), 1);
    assert!(
        expanded
            .items
            .iter()
            .any(|item| matches!(item, ContextItem::Symbol(_)))
    );
}

#[test]
fn expand_context_resolves_by_path_and_digest() {
    let repo = fixture_repo();
    let mut indexer = Indexer::default();

    indexer.index_repository(repo.path()).unwrap();
    let query = ContextQuery::new(indexer.index());
    let path = Path::new("src/lib.rs");
    let digest: Digest = indexer.index().paths().get(path).cloned().unwrap();

    let by_path = query.expand_context(&path.display().to_string());
    assert!(
        by_path
            .items
            .iter()
            .any(|item| matches!(item, ContextItem::Blob(_)))
    );

    let by_digest = query.expand_context(&digest.to_string());
    assert!(
        by_digest
            .items
            .iter()
            .any(|item| matches!(item, ContextItem::Blob(_)))
    );

    let blob = indexer.index().blob_for_digest(&digest).unwrap();
    let _: &BlobRecord = blob;
    let _: LanguageKind = LanguageKind::Rust;
}

fn blob_path(blob: &BlobRecord) -> PathBuf {
    blob.path.clone()
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
