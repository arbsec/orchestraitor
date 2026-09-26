//! Ready-queue predicate (spec §9.40, §9.43).
//!
//! Mirrors the github-project-workflow `ready-queue` script semantics exactly:
//!
//! - leaf iff the native issue type names a leaf type (a label never
//!   overrides an authoritative non-leaf native type — review finding on
//!   issue 254); while org-level native types are unset, the lowercase
//!   `task`/`bug` labels are the fallback;
//! - no `blockedBy` node with state != `CLOSED`, with truncated windows
//!   failing closed upstream in [`crate::item`] conversion;
//! - `Target == MVP` and `Status == Ready` by exact option-name equality,
//!   with the field/option names coming from the board config `[mvp]` block;
//! - only configured repositories are in scope for the shared board.
//!
//! Output is sorted by issue number, matching the script's `sort_by(.number)`.

use serde::Serialize;

use crate::BoardProjectConfig;
use crate::item::{ItemFacts, ItemSkip};

/// Label names accepted as the leaf-type fallback while org-level native
/// issue types are unset (mirrors the ready-queue script).
const LEAF_FALLBACK_LABELS: [&str; 2] = ["task", "bug"];

/// A board item that satisfies the ready-queue predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReadyItem {
    /// Issue number within its repository.
    pub number: u64,
    /// Issue title (untrusted text; never executed).
    pub title: String,
    /// Issue URL.
    pub url: String,
    /// `org/name` of the issue's repository.
    pub repo: String,
    /// `ProjectV2Item` node id for follow-up board operations.
    pub item_id: String,
}

/// An item the predicate could not safely evaluate; reported as a warning,
/// never a crash (issue 308 QA failure scenario).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkipWarning {
    /// Issue number when one could be read.
    pub number: Option<u64>,
    /// Why the item could not be evaluated.
    pub reason: String,
}

/// Evaluates the ready-queue predicate over parsed item facts.
///
/// Returns the eligible items sorted by issue number. This is a pure
/// predicate over already-validated facts: fail-closed decisions for
/// truncation and malformed content happen during
/// [`crate::item::RawItem::facts`] conversion, which yields the warnings
/// callers surface.
#[must_use]
pub fn ready_queue(facts: &[ItemFacts], config: &BoardProjectConfig) -> Vec<ReadyItem> {
    let mut ready: Vec<ReadyItem> = facts
        .iter()
        .filter(|facts| is_eligible(facts, config))
        .map(|facts| ReadyItem {
            number: facts.number,
            title: facts.title.clone(),
            url: facts.url.clone(),
            repo: facts.repo.clone(),
            item_id: facts.item_node_id.clone(),
        })
        .collect();
    ready.sort_by_key(|item| item.number);
    ready
}

fn is_eligible(facts: &ItemFacts, config: &BoardProjectConfig) -> bool {
    let leaf = match &facts.issue_type {
        Some(native) => config.leaf_types.iter().any(|leaf| leaf == native),
        None => facts.labels.iter().any(|label| {
            LEAF_FALLBACK_LABELS
                .iter()
                .any(|fallback| label == fallback)
        }),
    };
    leaf && facts.open_blockers == 0
        && facts.target.as_deref() == Some(config.target_value.as_str())
        && facts.status.as_deref() == Some(config.ready_value.as_str())
}

/// Renders a skip decision into a warning carrying the issue number when the
/// malformed content still exposed one.
pub(crate) fn skip_warning(raw: &crate::item::RawItem, skip: &ItemSkip) -> SkipWarning {
    let number = raw.content.as_ref().and_then(|content| content.number);
    let reason = match skip {
        ItemSkip::Malformed(reason) => format!("malformed item: {reason}"),
        ItemSkip::Truncated(window) => format!("truncated `{window}` window; failing closed"),
    };
    SkipWarning { number, reason }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> BoardProjectConfig {
        BoardProjectConfig {
            organization: "arbsec".to_string(),
            project_number: 1,
            repos: vec!["arbsec/orchestraitor".to_string()],
            leaf_types: vec!["Task".to_string(), "Bug".to_string()],
            target_field: "Target".to_string(),
            target_value: "MVP".to_string(),
            ready_field: "Status".to_string(),
            ready_value: "Ready".to_string(),
            token_uri: None,
        }
    }

    fn facts(number: u64) -> ItemFacts {
        ItemFacts {
            item_node_id: format!("PVTI_{number}"),
            repo: "arbsec/orchestraitor".to_string(),
            number,
            title: format!("item {number}"),
            url: format!("https://github.com/arbsec/orchestraitor/issues/{number}"),
            issue_type: Some("Task".to_string()),
            labels: Vec::new(),
            open_blockers: 0,
            target: Some("MVP".to_string()),
            status: Some("Ready".to_string()),
        }
    }

    #[test]
    fn qualifying_item_is_included() {
        let ready = ready_queue(&[facts(10)], &config());
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].number, 10);
    }

    #[test]
    fn every_exclusion_class_is_enforced() {
        let mut open_blocker = facts(11);
        open_blocker.open_blockers = 1;
        let mut wrong_target = facts(12);
        wrong_target.target = Some("Next".to_string());
        let mut missing_target = facts(13);
        missing_target.target = None;
        let mut wrong_status = facts(14);
        wrong_status.status = Some("In Progress".to_string());
        let mut missing_status = facts(15);
        missing_status.status = None;
        let mut epic_with_task_label = facts(16);
        epic_with_task_label.issue_type = Some("Epic".to_string());
        epic_with_task_label.labels = vec!["task".to_string()];
        let list = vec![
            open_blocker,
            wrong_target,
            missing_target,
            wrong_status,
            missing_status,
            epic_with_task_label,
        ];
        let ready = ready_queue(&list, &config());
        assert!(ready.is_empty());
    }

    #[test]
    fn label_fallback_applies_only_without_native_type() {
        let mut labeled = facts(20);
        labeled.issue_type = None;
        labeled.labels = vec!["bug".to_string()];
        let mut unlabeled = facts(21);
        unlabeled.issue_type = None;
        let ready = ready_queue(&[labeled, unlabeled], &config());
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].number, 20);
    }

    #[test]
    fn output_is_sorted_by_number() {
        let ready = ready_queue(&[facts(30), facts(10), facts(20)], &config());
        let numbers: Vec<u64> = ready.iter().map(|item| item.number).collect();
        assert_eq!(numbers, [10, 20, 30]);
    }
}
