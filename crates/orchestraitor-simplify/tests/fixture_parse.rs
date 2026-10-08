//! Fixture-driven parse tests: real cargo-clippy JSON streams (committed
//! under `tests/fixtures`) must parse into the expected typed suggestions.

use orchestraitor_simplify::{SuggestionClass, clippy};

/// Full clippy stream: one machine-applicable clippy lint, one
/// machine-applicable rustc lint, plus non-diagnostic cargo lines.
const CLIPPY_STREAM: &str = include_str!("fixtures/clippy_stream.jsonl");

#[test]
fn fixture_stream_parses_into_expected_suggestions() {
    let suggestions = clippy::parse(CLIPPY_STREAM, false);
    assert_eq!(suggestions.len(), 2, "exactly the two warnings parse");

    let clone_on_copy = &suggestions[0];
    assert_eq!(clone_on_copy.rule, "clippy::clone_on_copy");
    assert_eq!(clone_on_copy.class, SuggestionClass::SafeFix);
    assert_eq!(clone_on_copy.path.as_deref(), Some("src/lib.rs"));
    assert_eq!(clone_on_copy.line, Some(2));
    assert!(!clone_on_copy.applied);

    let unused = &suggestions[1];
    assert_eq!(unused.rule, "unused_variables");
    assert_eq!(unused.class, SuggestionClass::SafeFix);
    assert_eq!(unused.path.as_deref(), Some("src/lib.rs"));
    assert_eq!(unused.line, Some(5));
}

#[test]
fn fixture_stream_applies_only_under_flag() {
    let suggestions = clippy::parse(CLIPPY_STREAM, true);
    assert!(suggestions.iter().all(|suggestion| suggestion.applied));
}

#[test]
fn fixture_parse_is_deterministic() {
    let first = clippy::parse(CLIPPY_STREAM, false);
    let second = clippy::parse(CLIPPY_STREAM, false);
    assert_eq!(first, second);
}
