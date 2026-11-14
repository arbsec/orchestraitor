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

/// The delivery body carries the managed checklist UNCHECKED and AHEAD of the
/// model-authored summary: a marker-less body is blocked at merge
/// (`checklist_markers_missing`, arbsec/orchestraitor#788), and pre-ticking a
/// box would assert an unsubstantiated fact. Ordering is load-bearing — see
/// `untrusted_summary_cannot_enclose_the_trusted_checklist`.
#[test]
fn delivery_body_puts_the_trusted_checklist_before_the_summary() {
    let body = format!("{DELIVERY_PR_CHECKLIST}{}", request().summary);
    assert!(body.starts_with("## Merge checklist"), "{body}");
    assert!(body.ends_with("did the thing"), "{body}");
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

    // The trusted checklist comes FIRST and the SUMMARY follows it, and all
    // NINE managed markers ride along UNCHECKED.
    assert!(
        body.starts_with("## Merge checklist"),
        "the trusted checklist must lead the body: {body}"
    );
    assert!(
        body.ends_with("did the thing"),
        "the summary must survive, after the checklist: {body}"
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

/// UNTRUSTED model output must never be able to enclose the trusted checklist
/// (arbsec/orchestraitor#788 gen-3 review, `CodeRabbit` major): a summary that
/// carries a checked managed marker AND leaves a code fence open used to make
/// the nine appended markers land INSIDE that fence, where `strip_fenced_code`
/// drops them — the parser then saw the summary's single checked marker and
/// reported `all_checked: true` for a body with no evidence at all.
///
/// The production body is driven through the REAL `open_pr` and parsed the way
/// the gate parses (the shell/Rust engines share one rule set): the trusted
/// block leads, the untrusted summary follows it, so no fence in the summary
/// can reach the nine markers. `parse_checklist`'s own regression for this exact
/// body shape lives next to the parser
/// (`crates/orchestraitor-mcp/src/pr_status.rs`), because the worker crate does
/// not depend on the MCP gateway and a second parser here is the drift this
/// repository has already paid for.
#[cfg(unix)]
#[tokio::test]
async fn untrusted_summary_cannot_enclose_the_trusted_checklist() {
    use std::os::unix::fs::PermissionsExt;

    // The adversarial summary: a checked managed marker followed by a fence that
    // never closes, echoing the shape the reviewer used.
    let summary = "- [x] <!-- orc:issue --> Linked issue and specification requirements are satisfied\n```\nunclosed";

    let temp = tempfile::tempdir().unwrap();
    let capture = temp.path().join("body.txt");
    let orc_stub = temp.path().join("orc");
    std::fs::write(
        &orc_stub,
        format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = \"--body\" ]; then shift; printf '%s' \"$1\" > '{}'; break; fi; shift; done\nprintf 'https://example.invalid/pull/2\\n'\n",
            capture.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&orc_stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let sink = PushBranchDeliverySink::new(orc_stub, "arbsec/orchestraitor".into(), "main".into())
        .unwrap();
    let request = DeliveryRequest {
        summary: summary.to_string(),
        ..request()
    };
    sink.open_pr(temp.path(), "orc-delivery/branch", &request)
        .await
        .expect("the fake orc succeeds");

    let body = std::fs::read_to_string(&capture).unwrap();
    assert!(
        body.ends_with(summary),
        "the untrusted summary must be the tail of the body: {body}"
    );
    // Nothing before the summary can be fenced by it: the trusted block carries
    // no fence opener, so every marker in it is scanned as evidence.
    let trusted = &body[..body.len() - summary.len()];
    assert!(
        !trusted.contains("```") && !trusted.contains("~~~"),
        "the trusted block must carry no fence opener: {trusted}"
    );
    // Nine trusted markers, ALL unchecked, and all of them ahead of the
    // summary's single checked marker.
    let checked_at = body.find("- [x] <!-- orc:issue -->").unwrap();
    let unchecked: Vec<usize> = body
        .match_indices("- [ ] <!-- orc:")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(unchecked.len(), 9, "{body}");
    assert!(
        unchecked.iter().all(|offset| *offset < checked_at),
        "every trusted marker must precede the untrusted one: {body}"
    );
}

/// The summary neutraliser, exactly: an indented fence opener is dedented to
/// column 0, a line-start `<!--` has its `<` escaped, and nothing else moves.
/// Idempotent, so a retried delivery composes the identical body.
#[test]
fn neutralise_summary_dedents_openers_and_escapes_comments() {
    // Four boundaries in one body: a 2-space backtick opener and its closer
    // (dedented), a column-0 `<!--` (escaped), a TAB-led `<!--` (untouched: a
    // tab is content, so it opens no comment), and a 4-space `<!--` / ` ``` `
    // (untouched: indented code, not a comment opener and not a fence).
    let summary = "- Example:\n  ```\n  sample\n  ```\n<!--\n  \t<!--\n    <!--\n    ```\n```\n";
    let expected = "- Example:\n```\n  sample\n```\n&lt;!--\n  \t<!--\n    <!--\n    ```\n```\n";
    assert_eq!(neutralise_summary(summary), expected);
    assert_eq!(
        neutralise_summary(&neutralise_summary(summary)),
        expected,
        "neutralisation must be idempotent"
    );
}

/// A model-authored summary that carries an INDENTED fence opener must never
/// make the delivery PR unmergeable by construction (arbsec/orchestraitor#809:
/// a 1-3-space opener is `checklist_unparseable`, and that condition is sticky).
/// The lane composes the body from untrusted content, so it owns the guarantee:
/// the opener is dedented to column 0 — the very remedy the gate's message
/// names — and the trusted nine markers still lead, unchecked.
#[cfg(unix)]
#[tokio::test]
async fn delivery_neutralises_an_indented_fence_opener_in_the_summary() {
    use std::os::unix::fs::PermissionsExt;

    let summary = "- Example:\n  ```\n  sample\n  ```\n- a real line after the fence";
    let temp = tempfile::tempdir().unwrap();
    let capture = temp.path().join("body.txt");
    let orc_stub = temp.path().join("orc");
    std::fs::write(
        &orc_stub,
        format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = \"--body\" ]; then shift; printf '%s' \"$1\" > '{}'; break; fi; shift; done\nprintf 'https://example.invalid/pull/3\\n'\n",
            capture.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&orc_stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let sink = PushBranchDeliverySink::new(orc_stub, "arbsec/orchestraitor".into(), "main".into())
        .unwrap();
    let request = DeliveryRequest {
        summary: summary.to_string(),
        ..request()
    };
    sink.open_pr(temp.path(), "orc-delivery/branch", &request)
        .await
        .expect("the fake orc succeeds");

    let body = std::fs::read_to_string(&capture).unwrap();
    // The trusted block leads and carries the nine unchecked markers.
    assert!(body.starts_with("## Merge checklist"), "{body}");
    assert_eq!(body.matches("- [ ] <!-- orc:").count(), 9, "{body}");
    // No indented fence opener survives anywhere in the composed body: that is
    // what makes the checklist reading determinate.
    assert!(
        !body.lines().any(|line| {
            let indent = line.chars().take_while(|c| *c == ' ').count();
            let rest = line.get(indent..).unwrap_or_default();
            (1..=3).contains(&indent) && (rest.starts_with("```") || rest.starts_with("~~~"))
        }),
        "an indented fence opener survived neutralisation: {body}"
    );
    // The summary survives, dedented, and its tail is still readable text.
    assert!(body.contains("- Example:\n```\n  sample\n```\n"), "{body}");
    assert!(
        body.ends_with("- a real line after the fence"),
        "the summary must survive, after the checklist: {body}"
    );
}

/// A model-authored summary that opens an HTML block must not hide its own tail
/// from the reviewer. The nine trusted markers precede the summary and a marker
/// line's `<!--` is never line-start, so convergence is unaffected either way —
/// but an UNCLOSED `<!--` would swallow the rest of the summary, which the lane
/// must not let untrusted text suppress. The opener is rendered as literal text.
#[cfg(unix)]
#[tokio::test]
async fn delivery_neutralises_a_line_start_html_comment_in_the_summary() {
    use std::os::unix::fs::PermissionsExt;

    let summary = "<!-- a note the model wrote\nstill the summary";
    let temp = tempfile::tempdir().unwrap();
    let capture = temp.path().join("body.txt");
    let orc_stub = temp.path().join("orc");
    std::fs::write(
        &orc_stub,
        format!(
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = \"--body\" ]; then shift; printf '%s' \"$1\" > '{}'; break; fi; shift; done\nprintf 'https://example.invalid/pull/4\\n'\n",
            capture.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&orc_stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let sink = PushBranchDeliverySink::new(orc_stub, "arbsec/orchestraitor".into(), "main".into())
        .unwrap();
    let request = DeliveryRequest {
        summary: summary.to_string(),
        ..request()
    };
    sink.open_pr(temp.path(), "orc-delivery/branch", &request)
        .await
        .expect("the fake orc succeeds");

    let body = std::fs::read_to_string(&capture).unwrap();
    assert!(body.starts_with("## Merge checklist"), "{body}");
    assert_eq!(body.matches("- [ ] <!-- orc:").count(), 9, "{body}");
    // No line outside the checklist opens an HTML block: nothing in the summary
    // can hide the summary's own tail from the reviewer.
    let summary_tail = &body[body.find("&lt;!--").expect("the escaped opener")..];
    assert!(
        !summary_tail
            .lines()
            .any(|line| line.trim_start_matches(' ').starts_with("<!--")),
        "a line-start comment opener survived neutralisation: {body}"
    );
    assert!(
        body.ends_with("still the summary"),
        "the summary's tail must stay visible: {body}"
    );
}

/// The summary's LINE ENDINGS must be normalised before the neutraliser reads
/// them (`CommonMark` §2.1): a lone CR is a line ending and so is CRLF, and both
/// production scanners normalise the same way before they read a body.
///
/// The neutraliser used to split on `\n` alone, so a lone-CR summary was ONE
/// line: its indented fence opener survived (the composed PR then reported
/// `checklist_unparseable` — permanently unconvergeable) and its line-start
/// `<!--` survived (the summary's tail was hidden from the reviewer). This
/// test drives the REAL `open_pr` over the three line-ending flavours — LF,
/// CRLF and a lone CR — and asserts the SAME composed body each time.
#[cfg(unix)]
#[tokio::test]
async fn delivery_neutralises_the_summary_regardless_of_its_line_endings() {
    use std::os::unix::fs::PermissionsExt;

    // The same summary in three line-ending flavours: an indented fence opener
    // and a line-start HTML comment.
    let lf = String::from(
        "- Example:\n  ```\n  sample\n  ```\n<!-- a note the model wrote\nstill the summary",
    );
    let crlf = lf.replace('\n', "\r\n");
    let lone_cr = lf.replace('\n', "\r");

    for (flavour, summary) in [
        ("lf", lf.clone()),
        ("crlf", crlf.clone()),
        ("lone-cr", lone_cr.clone()),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let capture = temp.path().join("body.txt");
        let orc_stub = temp.path().join("orc");
        std::fs::write(
            &orc_stub,
            format!(
                "#!/bin/sh\nwhile [ $# -gt 0 ]; do if [ \"$1\" = \"--body\" ]; then shift; printf '%s' \"$1\" > '{}'; break; fi; shift; done\nprintf 'https://example.invalid/pull/5\\n'\n",
                capture.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&orc_stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let sink =
            PushBranchDeliverySink::new(orc_stub, "arbsec/orchestraitor".into(), "main".into())
                .unwrap();
        let request = DeliveryRequest {
            summary: summary.clone(),
            ..request()
        };
        sink.open_pr(temp.path(), "orc-delivery/branch", &request)
            .await
            .expect("the fake orc succeeds");

        let body = std::fs::read_to_string(&capture).unwrap();
        // The trusted block leads and carries the nine unchecked markers.
        assert!(body.starts_with("## Merge checklist"), "{flavour}: {body}");
        assert_eq!(
            body.matches("- [ ] <!-- orc:").count(),
            9,
            "{flavour}: {body}"
        );
        // No indented fence opener survives anywhere in the composed body: that
        // is what makes the checklist reading determinate.
        assert!(
            !body.lines().any(|line| {
                let indent = line.chars().take_while(|c| *c == ' ').count();
                let rest = line.get(indent..).unwrap_or_default();
                (1..=3).contains(&indent) && (rest.starts_with("```") || rest.starts_with("~~~"))
            }),
            "{flavour}: an indented fence opener survived neutralisation: {body}"
        );
        // No line-start HTML comment survives either: the summary's tail must
        // stay visible to the reviewer.
        let summary_tail = &body[body.find("- Example:").expect("the summary")..];
        assert!(
            !summary_tail
                .lines()
                .any(|line| line.trim_start_matches(' ').starts_with("<!--")),
            "{flavour}: a line-start comment opener survived neutralisation: {body}"
        );
        // The comment opener is escaped, so it renders as literal text.
        assert!(
            summary_tail.contains("&lt;!--"),
            "{flavour}: the comment opener was not escaped: {body}"
        );
        // The summary's tail survives, and the body carries no CR byte: the
        // scanners normalise line endings themselves, so the composed body is
        // already in the form they read.
        assert!(
            body.ends_with("still the summary"),
            "{flavour}: the summary's tail must stay visible: {body}"
        );
        assert!(
            !body.contains('\r'),
            "{flavour}: a CR byte survived into the composed body: {body:?}"
        );
    }
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
