//! Lifecycle cancellation integration tests.

#![allow(
    clippy::unwrap_used,
    reason = "tests unwrap join handles to keep failure location direct"
)]

use std::time::Duration;

use orchestraitor_lifecycle::{
    CancellationController, CancellationOutcome, CleanupReport, ResourceId,
};

#[tokio::test(flavor = "current_thread")]
async fn cancellation_releases_resources_within_bounded_grace() {
    // Given: a cancellation controller and worker token.
    let controller = CancellationController::new(Duration::from_millis(50));
    let mut token = controller.token();
    let worker = tokio::spawn(async move {
        token.cancelled().await;
        CleanupReport {
            released: vec![ResourceId::new("process:1")],
            unreleased: Vec::new(),
        }
    });

    // When: cancellation is requested and cleanup completes.
    let (outcome, report) = controller
        .cancel_with_cleanup(vec![ResourceId::new("process:1")], async move {
            worker.await.unwrap()
        })
        .await;

    // Then: resources are released inside grace with no unreleased set.
    assert_eq!(outcome, CancellationOutcome::Released);
    assert!(report.completed_within_grace);
    assert!(report.cleanup.unreleased.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn cancellation_records_unreleased_set_after_grace_expiry() {
    // Given: cleanup that cannot finish within grace.
    let controller = CancellationController::new(Duration::from_millis(10));
    let resource = ResourceId::new("socket:blocked");

    // When: cancellation times out.
    let (outcome, report) = controller
        .cancel_with_cleanup(vec![resource.clone()], async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            CleanupReport::default()
        })
        .await;

    // Then: unreleased resources are preserved for audit visibility.
    assert_eq!(outcome, CancellationOutcome::GraceExpired);
    assert!(!report.completed_within_grace);
    assert_eq!(report.cleanup.unreleased, vec![resource]);
}
