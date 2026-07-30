//! Tests for the promotion pipeline and transaction graph.

#![allow(clippy::unwrap_used)]

use orchestraitor_events::InMemoryAuditStore;
use orchestraitor_model::{Digest, OutputClass, RepositoryId, WorkspaceId};
use std::path::{Path, PathBuf};

use crate::PromotionError;
use crate::classify::{Classification, FileMetadata};
use crate::controller::{FileState, InMemoryController, TrustedController};
use crate::destination::DestinationSensitivity;
use crate::diff::SemanticDiff;
use crate::graph::{TransactionGraph, VerificationEvidence};
use crate::pipeline::{Change, PolicyDecision, PolicyGate, PromotionInput, PromptSink, promote};

// ---------------------------------------------------------------------------
// Promotion receipt (spec §9.14, §18.5)
// ---------------------------------------------------------------------------

struct AllowGate;
impl PolicyGate for AllowGate {
    fn evaluate(
        &self,
        _classification: &Classification,
        _sensitivity: DestinationSensitivity,
        _diff: &SemanticDiff,
    ) -> PolicyDecision {
        PolicyDecision::Allow
    }
}

struct AutoPrompt;
impl PromptSink for AutoPrompt {
    fn prompt(&self, _message: &str) -> bool {
        true
    }
}

#[test]
fn promotion_receipt_contains_correct_fields() {
    let mut controller = InMemoryController::new();
    controller.seed(Path::new("src/main.rs"), b"fn old() {}".to_vec());

    let input = PromotionInput {
        workspace_id: WorkspaceId::new(),
        source_digest: Digest::new("a".repeat(64)),
        target_repository: RepositoryId::new(),
        changes: vec![Change {
            path: PathBuf::from("src/main.rs"),
            content: b"fn new() {}".to_vec(),
            metadata: FileMetadata::default(),
        }],
        old_content: &|_p| Some(b"fn old() {}".to_vec()),
    };

    let receipt = promote(&input, &AllowGate, &AutoPrompt, &mut controller, None).unwrap();

    assert_eq!(receipt.workspace_id, input.workspace_id);
    assert_eq!(receipt.source_digest, input.source_digest);
    assert_eq!(receipt.target_repository, input.target_repository);
    assert_eq!(receipt.paths.len(), 1);
    assert_eq!(receipt.paths[0].path, Path::new("src/main.rs"));
    assert_eq!(receipt.paths[0].output_class, OutputClass::OrdinarySource);
    assert!(receipt.findings.is_empty());
    assert!(receipt.approvals.is_empty());
    assert!(receipt.resulting_commit.is_none());
}

#[test]
fn promotion_records_audit_event_when_store_provided() {
    let mut controller = InMemoryController::new();
    let mut store = InMemoryAuditStore::default();

    let input = PromotionInput {
        workspace_id: WorkspaceId::new(),
        source_digest: Digest::new("b".repeat(64)),
        target_repository: RepositoryId::new(),
        changes: vec![Change {
            path: PathBuf::from("src/lib.rs"),
            content: b"pub fn x() {}".to_vec(),
            metadata: FileMetadata::default(),
        }],
        old_content: &|_| None,
    };

    let receipt = promote(
        &input,
        &AllowGate,
        &AutoPrompt,
        &mut controller,
        Some(&mut store),
    )
    .unwrap();
    assert_eq!(receipt.paths.len(), 1);
    assert_eq!(store.records().len(), 1, "promotion event must be recorded");
}

#[test]
fn rejected_policy_keeps_change_quarantined() {
    struct RejectGate;
    impl PolicyGate for RejectGate {
        fn evaluate(
            &self,
            _: &Classification,
            _: DestinationSensitivity,
            _: &SemanticDiff,
        ) -> PolicyDecision {
            PolicyDecision::Reject
        }
    }
    let mut controller = InMemoryController::new();
    let input = PromotionInput {
        workspace_id: WorkspaceId::new(),
        source_digest: Digest::new("c".repeat(64)),
        target_repository: RepositoryId::new(),
        changes: vec![Change {
            path: PathBuf::from("src/main.rs"),
            content: b"fn main() {}".to_vec(),
            metadata: FileMetadata::default(),
        }],
        old_content: &|_| None,
    };
    let receipt = promote(&input, &RejectGate, &AutoPrompt, &mut controller, None).unwrap();
    assert!(
        receipt.paths.is_empty(),
        "rejected change must not be promoted"
    );
}

// ---------------------------------------------------------------------------
// Transaction graph — rollback restores previous state (spec §9.4.3)
// ---------------------------------------------------------------------------

#[test]
fn rollback_restores_previous_state() {
    let mut graph = TransactionGraph::new();
    let mut controller = InMemoryController::new();

    let file_a = FileState::new(PathBuf::from("src/app.rs"), b"fn a() {}".to_vec());
    let file_b = FileState::new(PathBuf::from("src/app.rs"), b"fn b() {}".to_vec());

    graph
        .checkpoint(vec![file_a.clone()], "user", "orchestraitor")
        .unwrap();
    graph
        .record_mutation(
            vec![file_b.clone()],
            "agent",
            "claude",
            VerificationEvidence::default(),
        )
        .unwrap();

    let head = graph.head().unwrap().id.clone();
    graph.restore(&head, &mut controller).unwrap();
    let after_restore = controller
        .read(Path::new("src/app.rs"))
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        after_restore, b"fn b() {}",
        "restore must apply the HEAD node's files"
    );

    graph.undo(&mut controller).unwrap();
    let after_undo = controller
        .read(Path::new("src/app.rs"))
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        after_undo, b"fn a() {}",
        "undo must restore the previous state"
    );

    graph.redo(&mut controller).unwrap();
    let after_redo = controller
        .read(Path::new("src/app.rs"))
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        after_redo, b"fn b() {}",
        "redo must re-apply the undone state"
    );
}

#[test]
fn transaction_graph_history_and_branch() {
    let mut graph = TransactionGraph::new();

    let f1 = FileState::new(PathBuf::from("a.rs"), b"1".to_vec());
    let f2 = FileState::new(PathBuf::from("a.rs"), b"2".to_vec());
    let f3 = FileState::new(PathBuf::from("a.rs"), b"3".to_vec());

    graph.checkpoint(vec![f1], "u", "o").unwrap();
    graph
        .record_mutation(vec![f2], "a", "c", VerificationEvidence::default())
        .unwrap();
    let node1 = graph.head().unwrap().id.clone();
    graph
        .record_mutation(vec![f3], "a", "c", VerificationEvidence::default())
        .unwrap();

    assert_eq!(graph.history().len(), 3);
    assert_eq!(graph.head().unwrap().generation, 3);

    graph.branch(&node1).unwrap();
    assert_eq!(graph.head().unwrap().id, node1);

    let f4 = FileState::new(PathBuf::from("a.rs"), b"4".to_vec());
    graph
        .record_mutation(vec![f4], "a", "c", VerificationEvidence::default())
        .unwrap();
    assert_eq!(graph.history().len(), 4);
    assert_eq!(graph.head().unwrap().generation, 4);

    let diffs = graph.compare(&node1, &graph.head().unwrap().id).unwrap();
    assert!(
        !diffs.is_empty(),
        "compare must produce diffs between nodes"
    );
}

#[test]
fn undo_without_parent_errors() {
    let mut graph = TransactionGraph::new();
    let f = FileState::new(PathBuf::from("x.rs"), b"x".to_vec());
    graph.checkpoint(vec![f], "u", "o").unwrap();
    let result = graph.undo(&mut InMemoryController::new());
    assert!(matches!(
        result,
        Err(PromotionError::InvalidOperation { .. })
    ));
}
