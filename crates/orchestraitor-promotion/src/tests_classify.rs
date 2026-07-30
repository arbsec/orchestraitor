//! Tests for classification, destination detection, and diff generation.

#![allow(clippy::unwrap_used)]

use orchestraitor_model::OutputClass;
use std::path::{Path, PathBuf};

use crate::classify::{ClassificationInput, FileMetadata, classify};
use crate::destination::{DestinationSensitivity, detect_sensitivity};
use crate::diff::{ChangeKind, DiffLine, compute_semantic_diff, compute_textual_diff};

// ---------------------------------------------------------------------------
// Classification — all 17 output classes (spec §9.14)
// ---------------------------------------------------------------------------

struct ClassCase {
    path: &'static str,
    content: Option<&'static [u8]>,
    metadata: Option<FileMetadata>,
    expected: OutputClass,
}

fn assert_classifications(cases: &[ClassCase]) {
    for case in cases {
        let input = ClassificationInput {
            path: Path::new(case.path),
            content: case.content,
            metadata: case.metadata.as_ref(),
        };
        let result = classify(&input).unwrap();
        assert_eq!(
            result.output_class, case.expected,
            "path `{}` classified as {:?}, expected {:?}",
            case.path, result.output_class, case.expected
        );
    }
}

#[test]
fn classifies_path_based_output_classes() {
    assert_classifications(&[
        ClassCase {
            path: "src/main.rs",
            content: Some(b"fn main() {}"),
            metadata: None,
            expected: OutputClass::OrdinarySource,
        },
        ClassCase {
            path: "tests/integration_test.rs",
            content: Some(b"#[test] fn t() {}"),
            metadata: None,
            expected: OutputClass::Tests,
        },
        ClassCase {
            path: "dist/app.tar.gz",
            content: Some(b"binary"),
            metadata: None,
            expected: OutputClass::PackageArchive,
        },
        ClassCase {
            path: "Cargo.lock",
            content: Some(b"# @generated"),
            metadata: None,
            expected: OutputClass::DependencyLockfile,
        },
        ClassCase {
            path: ".vscode/settings.json",
            content: Some(b"{}"),
            metadata: None,
            expected: OutputClass::IdeConfiguration,
        },
        ClassCase {
            path: ".bashrc",
            content: Some(b"export PATH=/usr/bin"),
            metadata: None,
            expected: OutputClass::ShellConfiguration,
        },
        ClassCase {
            path: ".gitignore",
            content: Some(b"/target"),
            metadata: None,
            expected: OutputClass::GitConfiguration,
        },
        ClassCase {
            path: ".git/hooks/pre-commit",
            content: Some(b"#!/bin/sh"),
            metadata: None,
            expected: OutputClass::GitHook,
        },
        ClassCase {
            path: "AGENTS.md",
            content: Some(b"# Rules"),
            metadata: None,
            expected: OutputClass::AgentConfiguration,
        },
        ClassCase {
            path: ".github/workflows/ci.yml",
            content: Some(b"on: [push]"),
            metadata: None,
            expected: OutputClass::CiWorkflow,
        },
        ClassCase {
            path: "Makefile",
            content: Some(b"all:\n\techo hi"),
            metadata: None,
            expected: OutputClass::BuildSystemPlugin,
        },
        ClassCase {
            path: ".env",
            content: Some(b"FOO=bar"),
            metadata: None,
            expected: OutputClass::EnvironmentFile,
        },
        ClassCase {
            path: "src/app.test.ts",
            content: Some(b"test()"),
            metadata: None,
            expected: OutputClass::Tests,
        },
    ]);
}

#[test]
fn classifies_content_and_metadata_based_output_classes() {
    assert_classifications(&[
        ClassCase {
            path: "src/generated.rs",
            content: Some(b"// Code generated. DO NOT EDIT.\nfn x() {}"),
            metadata: None,
            expected: OutputClass::GeneratedSource,
        },
        ClassCase {
            path: "scripts/run.sh",
            content: Some(b"#!/bin/sh\necho hi"),
            metadata: Some(FileMetadata {
                is_executable: true,
                ..Default::default()
            }),
            expected: OutputClass::Executable,
        },
        ClassCase {
            path: "id_rsa",
            content: Some(b"-----BEGIN RSA PRIVATE KEY-----\nkey\n-----END RSA PRIVATE KEY-----"),
            metadata: None,
            expected: OutputClass::CredentialShapedData,
        },
        ClassCase {
            path: "link.txt",
            content: None,
            metadata: Some(FileMetadata {
                is_symlink: true,
                ..Default::default()
            }),
            expected: OutputClass::Symlink,
        },
        ClassCase {
            path: "/dev/null",
            content: None,
            metadata: Some(FileMetadata {
                is_device: true,
                ..Default::default()
            }),
            expected: OutputClass::DeviceOrSpecialFile,
        },
    ]);
}

#[test]
fn credential_shaped_detection_flags_key_assignments() {
    let input = ClassificationInput {
        path: Path::new("config.txt"),
        content: Some(b"API_KEY=sk-1234567890\nOTHER=foo"),
        metadata: None,
    };
    let result = classify(&input).unwrap();
    assert_eq!(result.output_class, OutputClass::CredentialShapedData);
    assert!(result.credential_shaped);
}

// ---------------------------------------------------------------------------
// Destination sensitivity (spec §9.14)
// ---------------------------------------------------------------------------

#[test]
fn detects_trust_sensitive_destinations() {
    assert_eq!(
        detect_sensitivity(Path::new(".github/workflows/ci.yml")),
        DestinationSensitivity::TrustSensitive
    );
    assert_eq!(
        detect_sensitivity(Path::new(".git/hooks/pre-commit")),
        DestinationSensitivity::TrustSensitive
    );
    assert_eq!(
        detect_sensitivity(Path::new(".env")),
        DestinationSensitivity::TrustSensitive
    );
    assert_eq!(
        detect_sensitivity(Path::new("src/main.rs")),
        DestinationSensitivity::Ordinary
    );
}

// ---------------------------------------------------------------------------
// Diff generation (spec §9.14)
// ---------------------------------------------------------------------------

#[test]
fn textual_diff_produces_added_and_removed_lines() {
    let diff = compute_textual_diff(
        PathBuf::from("src/app.rs"),
        b"fn old() {}\n",
        b"fn new() {}\n",
    );
    assert!(!diff.truncated);
    let has_added = diff
        .hunks
        .iter()
        .any(|h| h.lines.iter().any(|l| matches!(l, DiffLine::Added(_))));
    let has_removed = diff
        .hunks
        .iter()
        .any(|h| h.lines.iter().any(|l| matches!(l, DiffLine::Removed(_))));
    assert!(has_added, "diff must contain added lines");
    assert!(has_removed, "diff must contain removed lines");
}

#[test]
fn semantic_diff_classifies_change_kind() {
    let diff = compute_semantic_diff(PathBuf::from("new.rs"), b"", b"fn main() {}");
    assert_eq!(diff.kind, ChangeKind::Added);
    assert!(diff.lines_added > 0);

    let diff = compute_semantic_diff(PathBuf::from("gone.rs"), b"fn main() {}", b"");
    assert_eq!(diff.kind, ChangeKind::Removed);

    let diff = compute_semantic_diff(PathBuf::from("mod.rs"), b"fn old() {}", b"fn new() {}");
    assert_eq!(diff.kind, ChangeKind::Modified);
}
