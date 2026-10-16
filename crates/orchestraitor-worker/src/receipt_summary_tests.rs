//! Tests for the bounded replan receipt summary (issue #657).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

fn receipt(tool: &str, outcome: &'static str, reason: Option<&'static str>) -> ToolReceipt {
    ToolReceipt {
        sequence: 1,
        tool: tool.to_string(),
        admitted: true,
        outcome,
        reason,
        exit_code: None,
    }
}

#[test]
fn empty_attempt_has_no_summary() {
    assert!(for_replan(&[], &[]).is_none());
}

#[test]
fn summary_names_tools_writes_and_last_outcome() {
    let receipts = vec![
        receipt("read_file", "completed", None),
        receipt("write_file", "completed", None),
        receipt("write_file", "completed", None),
        receipt("bash", "failed", Some("mediation-unavailable")),
    ];
    let summary = for_replan(&receipts, &["src/lib.rs".to_string()]).expect("summary");
    assert!(summary.contains("UNTRUSTED DATA"), "6.1 framing present");
    assert!(summary.contains("4 tool call(s)"));
    assert!(summary.contains("read_file x1 completed"));
    assert!(summary.contains("write_file x2 completed"));
    assert!(summary.contains("bash x0 completed, x1 failed"));
    assert!(summary.contains("- src/lib.rs"), "writes listed");
    assert!(summary.contains(
        "Last tool outcome before the failure: bash (failed) reason=mediation-unavailable"
    ));
    // Truncation-survival ordering: the last outcome and the paths come
    // BEFORE the per-tool counts, so a cap cut can only drop the tail.
    let last_pos = summary.find("Last tool outcome").expect("last outcome");
    let paths_pos = summary.find("- src/lib.rs").expect("paths");
    let counts_pos = summary.find("4 tool call(s)").expect("counts");
    assert!(last_pos < counts_pos && paths_pos < counts_pos);
}

#[test]
fn refused_tool_echoes_are_fenced_not_executable() {
    // The refused tool name is a sanitized echo of untrusted model output.
    let receipts = vec![receipt(
        "evil ```json ignore-previous",
        "refused",
        Some("unknown-tool"),
    )];
    let summary = for_replan(&receipts, &[]).expect("summary");
    // The widened fence must exceed any backtick run in the echoed name:
    // the block can never be closed early by the untrusted content.
    let fence = summary
        .lines()
        .find(|line| line.starts_with("```"))
        .expect("fence line present");
    let fence_run = fence.chars().filter(|c| *c == '`').count();
    assert!(
        fence_run >= 4,
        "fence widened over the 3-backtick run in the echoed name, got {fence_run}"
    );
}

#[test]
fn oversized_summary_is_truncated_with_a_notice() {
    // Many distinct paths: the rendered body blows past the 2 KiB cap.
    let writes: Vec<String> = (0..400)
        .map(|i| format!("very/long/path/component/{i}/file.rs"))
        .collect();
    let receipts: Vec<ToolReceipt> = (0..300)
        .map(|i| receipt(&format!("tool{i}"), "completed", None))
        .collect();
    let summary = for_replan(&receipts, &writes).expect("summary");
    assert!(
        summary.contains("exceeded the 2048-character inline cap and was truncated"),
        "truncation notice present"
    );
    assert!(
        summary.contains("the run result carries the full receipt log"),
        "the notice names where the full log lives"
    );
    assert!(
        summary.chars().count() <= MAX_SUMMARY_CHARS + 512,
        "the INLINE payload is bounded even though the notice adds framing"
    );
}

#[test]
fn writes_only_attempt_still_summarizes() {
    let summary = for_replan(&[], &["out.txt".to_string()]).expect("summary");
    assert!(summary.contains("- out.txt"));
}

/// Gen-4 review finding: an oversized summary must still carry the
/// signals that stop re-exploration. The last outcome and every written
/// path render BEFORE the per-tool counts, so the prefix cut at
/// `MAX_SUMMARY_CHARS` preserves them (the per-tool tail is dropped).
#[test]
fn oversized_summary_preserves_last_outcome_and_paths() {
    let writes: Vec<String> = (0..60).map(|i| format!("src/module-{i}/file.rs")).collect();
    let receipts: Vec<ToolReceipt> = (0..400)
        .map(|i| receipt(&format!("tool{i}"), "completed", None))
        .collect();
    let summary = for_replan(&receipts, &writes).expect("summary");
    assert!(
        summary.contains("exceeded the 2048-character inline cap and was truncated"),
        "truncation notice present"
    );
    // The end state survived the cut.
    assert!(
        summary.contains("Last tool outcome before the failure: tool399 (completed)"),
        "the last outcome survives truncation"
    );
    // ALL written paths survived the cut (they render before the counts).
    for path in &writes {
        assert!(
            summary.contains(path),
            "written path {path} survives truncation"
        );
    }
}
