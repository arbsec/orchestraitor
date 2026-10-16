//! Tests for the push-branch delivery sink (issue #656): typed failures,
//! no silent drops, and the no-worktree contract.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn request() -> DeliveryRequest {
    DeliveryRequest {
        task_id: "board-arbsec_orchestraitor-42".to_string(),
        summary: "did the thing".to_string(),
        untrusted_writes: vec!["src/lib.rs".to_string()],
        worktree: None,
        worktree_branch: None,
    }
}

/// A run without a filesystem worktree/branch context is a typed
/// `delivery-no-worktree` failure — never a silent drop, never a panic.
#[tokio::test]
async fn missing_worktree_context_is_a_typed_failure() {
    // A real (non-executed) file: construction validates existence only.
    let orc_dir = tempfile::tempdir().unwrap();
    let orc_stub = orc_dir.path().join("orc");
    std::fs::write(&orc_stub, b"").unwrap();
    let sink = PushBranchDeliverySink::new(orc_stub, "arbsec/orchestraitor".into(), "main".into())
        .unwrap();
    let error = sink
        .deliver(&request())
        .await
        .expect_err("no worktree context must fail typed");
    assert_eq!(error.reason, DELIVERY_NO_WORKTREE);
}

/// A non-git worktree fails at the commit step with a typed reason —
/// never a panic, never a silent drop.
#[tokio::test]
async fn non_git_worktree_is_a_typed_branch_state_failure() {
    let temp = tempfile::tempdir().unwrap();
    let worktree = temp.path().to_path_buf();
    let request = DeliveryRequest {
        worktree: Some(worktree),
        worktree_branch: Some("orc-loop/t-1".to_string()),
        ..request()
    };
    // A real file: construction passes; the COMMIT step fails first (the
    // orc binary is never reached).
    let orc_dir = tempfile::tempdir().unwrap();
    let orc_stub = orc_dir.path().join("orc");
    std::fs::write(&orc_stub, b"").unwrap();
    let sink = PushBranchDeliverySink::new(orc_stub, "arbsec/orchestraitor".into(), "main".into())
        .unwrap();
    let error = sink
        .deliver(&request)
        .await
        .expect_err("a non-git worktree must fail typed");
    assert_eq!(error.reason, DELIVERY_BRANCH_STATE_FAILED);
}

/// The reason codes are static strings (spec §9.23.4): no paths, task ids,
/// or command lines leak into the codes themselves.
#[test]
fn reason_codes_are_static_and_log_safe() {
    assert_eq!(DELIVERY_NO_WORKTREE, "delivery-no-worktree");
    assert_eq!(DELIVERY_BRANCH_STATE_FAILED, "delivery-branch-state-failed");
    assert_eq!(DELIVERY_PUSH_FAILED, "delivery-push-failed");
    assert_eq!(DELIVERY_PR_OPEN_FAILED, "delivery-pr-open-failed");
    assert_eq!(DELIVERY_EMPTY_DIFF, "delivery-empty-diff");
    for reason in [
        DELIVERY_NO_WORKTREE,
        DELIVERY_BRANCH_STATE_FAILED,
        DELIVERY_PUSH_FAILED,
        DELIVERY_PR_OPEN_FAILED,
        DELIVERY_EMPTY_DIFF,
    ] {
        assert!(
            reason.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "reason code '{reason}' is kebab-case only (log-safe)"
        );
    }
}
