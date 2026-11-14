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

/// The delivery body carries the managed checklist UNCHECKED: a marker-less
/// body is blocked at merge (`checklist_markers_missing`,
/// arbsec/orchestraitor#788), and pre-ticking a box would assert an
/// unsubstantiated fact.
#[test]
fn delivery_body_appends_the_managed_checklist_unchecked() {
    let body = format!("{}{DELIVERY_PR_CHECKLIST}", request().summary);
    assert!(body.starts_with("did the thing"));
    assert_eq!(body.matches("<!-- orc:").count(), 9, "{body}");
    assert!(!body.contains("- [x]"), "delivery never pre-ticks a box");
}

/// The PRODUCTION body carries the managed checklist: a marker-less body is
/// blocked at merge (`checklist_markers_missing`, arbsec/orchestraitor#788),
/// so the delivery lane's own PRs must not be unmergeable by construction.
///
/// This test drives the REAL `PushBranchDeliverySink::open_pr` against a fake
/// `orc` executable and asserts the `--body` argument the production code
/// actually passes — a test that rebuilds the body itself cannot detect the
/// delivery lane dropping the checklist.
#[cfg(unix)]
#[tokio::test]
async fn delivery_pr_body_carries_the_managed_checklist() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let capture = temp.path().join("body.txt");
    // The fake `orc` records the `--body` value it was handed and prints a PR
    // URL on stdout (the shape `open_pr` expects on success). The body is one
    // argv entry that spans many lines, so it is captured WHOLE — a
    // line-oriented capture would only ever see its first line.
    let orc_stub = temp.path().join("orc");
    std::fs::write(
        &orc_stub,
        format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = \"--body\" ]; then shift; printf '%s' \"$1\" > '{}'; break; fi; shift; done\nprintf 'https://example.invalid/pull/1\\n'\n",
            capture.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&orc_stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let sink = PushBranchDeliverySink::new(orc_stub, "arbsec/orchestraitor".into(), "main".into())
        .unwrap();
    let url = sink
        .open_pr(temp.path(), "orc-delivery/branch", &request())
        .await
        .expect("the fake orc succeeds");
    assert_eq!(url, "https://example.invalid/pull/1");

    let body = std::fs::read_to_string(&capture).unwrap();

    // The SUMMARY survives, and all NINE managed markers ride along UNCHECKED.
    assert!(
        body.starts_with("did the thing"),
        "the summary must survive: {body}"
    );
    assert_eq!(body.matches("<!-- orc:").count(), 9, "{body}");
    for marker in [
        "orc:issue",
        "orc:service-identity",
        "orc:tests",
        "orc:security",
        "orc:docs",
        "orc:rollback",
        "orc:checks",
        "orc:review",
        "orc:threads",
    ] {
        assert!(
            body.contains(&format!("- [ ] <!-- {marker} -->")),
            "the production body is missing the unchecked marker {marker}: {body}"
        );
    }
    assert!(!body.contains("- [x]"), "delivery never pre-ticks a box");
}

/// Drift guard: the checklist appended to delivery PRs is the repository's
/// managed checklist, verbatim (the marker lines in
/// `.github/PULL_REQUEST_TEMPLATE.md`).
#[test]
fn delivery_checklist_matches_the_pr_template() {
    let template = include_str!("../../../.github/PULL_REQUEST_TEMPLATE.md");
    let template_markers: Vec<&str> = template
        .lines()
        .map(str::trim_end)
        .filter(|line| line.starts_with("- [ ] <!-- orc:"))
        .collect();
    assert_eq!(template_markers.len(), 9, "template marker count moved");
    for line in template_markers {
        assert!(
            DELIVERY_PR_CHECKLIST.contains(line),
            "delivery checklist is missing the template line: {line}"
        );
    }
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
