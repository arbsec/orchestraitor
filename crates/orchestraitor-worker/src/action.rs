//! The worker's model-action protocol (mini-swe-agent pattern).
//!
//! The loop speaks a text protocol: every model response must contain exactly
//! one fenced ` ```json ` block selecting one of the four tools, or the
//! `finish` terminator. Parsing is total — anything else is an
//! [`ActionRejection`]: malformed responses consume the attempt's format-error
//! budget, and requests for a capability outside the four-tool set are typed
//! refusals that are recorded and fed back to the model.

/// Maximum accepted length for a requested tool name recorded in a refusal.
const MAX_TOOL_NAME_CHARS: usize = 64;
/// Maximum accepted script size for one `bash` action (bytes).
const MAX_SCRIPT_BYTES: usize = 64 * 1024;
/// Maximum accepted content size for one `write_file` action (bytes).
const MAX_WRITE_CONTENT_BYTES: usize = 1024 * 1024;
/// Maximum accepted search pattern length (chars).
const MAX_PATTERN_CHARS: usize = 512;

/// One parsed model action: the four mediated tools plus the terminator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkerAction {
    /// Read a worktree-relative file.
    ReadFile {
        /// Worktree-relative path.
        path: String,
    },
    /// Write a worktree-relative file (full content).
    WriteFile {
        /// Worktree-relative path.
        path: String,
        /// Full replacement content.
        content: String,
    },
    /// Plain-text content search over the worktree (or a subdirectory).
    Search {
        /// Plain substring to find.
        pattern: String,
        /// Optional worktree-relative subdirectory to scope the search.
        path: Option<String>,
    },
    /// Mediated bash execution (crosses the Arbitraitor boundary).
    Bash {
        /// Bash script bytes (streamed to the interpreter over stdin).
        script: String,
    },
    /// Terminator: the model declares the task done (or not completable).
    Finish {
        /// Model-authored summary of the work.
        summary: String,
        /// Whether the task completed.
        success: bool,
    },
}

/// A rejected model response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ActionRejection {
    /// The response did not contain a parseable action block. Extra blocks
    /// beyond the first are ignored (model verbosity), never a rejection.
    Malformed {
        /// Static reason code (`no-action-block`, `invalid-json`,
        /// `missing-tool-field`, `missing-field`, `wrong-field-type`,
        /// `field-too-large`, `empty-field`).
        reason: &'static str,
    },
    /// The model requested a capability outside the four-tool set. The
    /// sanitized, truncated name is carried for the refusal record.
    UnknownTool {
        /// Sanitized, truncated requested tool name.
        name: String,
    },
}

/// Parses the single fenced-json action block out of a model response.
///
/// A response carrying MULTIPLE fenced blocks is a known glm-5.x failure
/// mode (issue: loop autotomy — verbose models emit per-step blocks in one
/// turn). The FIRST block is the action; the extra blocks are rejected with
/// corrective feedback (the model is told the rest of its response was
/// discarded) instead of counting the whole turn as a fatal format error.
/// Only the first block is ever executed, so protocol-wise this stays
/// exactly as strict as before about what runs — it just recovers instead
/// of burning the attempt budget.
pub(crate) fn parse_action(text: &str) -> Result<WorkerAction, ActionRejection> {
    let blocks = fenced_json_blocks(text);
    // First block IS the action; extras are model verbosity, not protocol
    // violations. An empty response is the only hard rejection here.
    let block = match blocks.first() {
        Some(block) => *block,
        None => {
            return Err(ActionRejection::Malformed {
                reason: "no-action-block",
            });
        }
    };
    let value: serde_json::Value =
        serde_json::from_str(block).map_err(|_| ActionRejection::Malformed {
            reason: "invalid-json",
        })?;
    let tool = value
        .get("tool")
        .and_then(serde_json::Value::as_str)
        .ok_or(ActionRejection::Malformed {
            reason: "missing-tool-field",
        })?;
    match tool {
        "read_file" => Ok(WorkerAction::ReadFile {
            path: required_string(&value, "path")?,
        }),
        "write_file" => {
            let content = required_string(&value, "content")?;
            if content.len() > MAX_WRITE_CONTENT_BYTES {
                return Err(ActionRejection::Malformed {
                    reason: "field-too-large",
                });
            }
            Ok(WorkerAction::WriteFile {
                path: required_string(&value, "path")?,
                content,
            })
        }
        "search" => {
            let pattern = required_string(&value, "pattern")?;
            if pattern.is_empty() {
                return Err(ActionRejection::Malformed {
                    reason: "empty-field",
                });
            }
            if pattern.chars().count() > MAX_PATTERN_CHARS {
                return Err(ActionRejection::Malformed {
                    reason: "field-too-large",
                });
            }
            let path = match value.get("path") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(path)) => Some(path.clone()),
                Some(_) => {
                    return Err(ActionRejection::Malformed {
                        reason: "wrong-field-type",
                    });
                }
            };
            Ok(WorkerAction::Search { pattern, path })
        }
        "bash" => {
            let script = required_string(&value, "script")?;
            if script.len() > MAX_SCRIPT_BYTES {
                return Err(ActionRejection::Malformed {
                    reason: "field-too-large",
                });
            }
            Ok(WorkerAction::Bash { script })
        }
        "finish" => {
            let summary = required_string(&value, "summary")?;
            let success = value
                .get("success")
                .and_then(serde_json::Value::as_bool)
                .ok_or(ActionRejection::Malformed {
                    reason: "missing-field",
                })?;
            Ok(WorkerAction::Finish { summary, success })
        }
        other => Err(ActionRejection::UnknownTool {
            name: sanitize_tool_name(other),
        }),
    }
}

/// Extracts the contents of every ` ```json ... ``` ` fenced block.
fn fenced_json_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```json") {
        let after_open = &rest[start + "```json".len()..];
        let Some(end) = after_open.find("```") else {
            break;
        };
        blocks.push(after_open[..end].trim());
        rest = &after_open[end + "```".len()..];
    }
    blocks
}

/// Reads a required string field from the action object.
fn required_string(
    value: &serde_json::Value,
    field: &'static str,
) -> Result<String, ActionRejection> {
    match value.get(field) {
        None => Err(ActionRejection::Malformed {
            reason: "missing-field",
        }),
        Some(serde_json::Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(ActionRejection::Malformed {
            reason: "wrong-field-type",
        }),
    }
}

/// Sanitizes a requested tool name for the refusal record: printable ASCII
/// only, truncated. The name is untrusted model output (spec §6.1); the
/// record keeps a bounded, log-safe echo.
fn sanitize_tool_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(MAX_TOOL_NAME_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn parses_each_known_tool_action() {
        let cases = [
            (
                "```json\n{\"tool\": \"read_file\", \"path\": \"src/lib.rs\"}\n```",
                WorkerAction::ReadFile {
                    path: "src/lib.rs".to_string(),
                },
            ),
            (
                "```json\n{\"tool\": \"write_file\", \"path\": \"a.txt\", \"content\": \"hi\"}\n```",
                WorkerAction::WriteFile {
                    path: "a.txt".to_string(),
                    content: "hi".to_string(),
                },
            ),
            (
                "```json\n{\"tool\": \"search\", \"pattern\": \"needle\"}\n```",
                WorkerAction::Search {
                    pattern: "needle".to_string(),
                    path: None,
                },
            ),
            (
                "```json\n{\"tool\": \"search\", \"pattern\": \"needle\", \"path\": \"src\"}\n```",
                WorkerAction::Search {
                    pattern: "needle".to_string(),
                    path: Some("src".to_string()),
                },
            ),
            (
                "```json\n{\"tool\": \"bash\", \"script\": \"cargo test\"}\n```",
                WorkerAction::Bash {
                    script: "cargo test".to_string(),
                },
            ),
            (
                "```json\n{\"tool\": \"finish\", \"summary\": \"done\", \"success\": true}\n```",
                WorkerAction::Finish {
                    summary: "done".to_string(),
                    success: true,
                },
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_action(text), Ok(expected), "input: {text}");
        }
    }

    #[test]
    fn unknown_tool_is_a_typed_refusal_with_sanitized_name() {
        let text = "```json\n{\"tool\": \"delete_everything\", \"path\": \"/\"}\n```";
        assert_eq!(
            parse_action(text),
            Err(ActionRejection::UnknownTool {
                name: "delete_everything".to_string()
            })
        );
    }

    #[test]
    fn unknown_tool_name_is_truncated_and_stripped() {
        let hostile_name = format!("{}\n\r\u{0007}evil", "x".repeat(500));
        let text = format!(
            "```json\n{}\n```",
            serde_json::json!({"tool": hostile_name})
        );
        let Err(ActionRejection::UnknownTool { name }) = parse_action(&text) else {
            panic!("hostile name must be an unknown-tool refusal");
        };
        assert_eq!(name.chars().count(), MAX_TOOL_NAME_CHARS);
        assert!(name.chars().all(|c| c.is_ascii_graphic() || c == ' '));
    }

    #[test]
    fn multiple_blocks_take_first_and_ignore_extras() {
        // glm-5.x verbosity mode: one response, many fenced blocks. Only
        // the FIRST block is the action; the rest must be ignored, never
        // counted as format errors (issue: loop attempt-budget exhaustion).
        let text = "Working through it step by step.\n```json\n{\"tool\": \"read_file\", \"path\": \"a\"}\n```\nThen:\n```json\n{\"tool\": \"finish\", \"summary\": \"s\", \"success\": true}\n```";
        assert_eq!(
            parse_action(text),
            Ok(WorkerAction::ReadFile {
                path: "a".to_string()
            })
        );
    }

    #[test]
    fn malformed_responses_carry_static_reasons() {
        let cases = [
            ("no block at all", "no-action-block"),
            ("```json\nnot json\n```", "invalid-json"),
            ("```json\n{\"path\": \"a\"}\n```", "missing-tool-field"),
            ("```json\n{\"tool\": \"read_file\"}\n```", "missing-field"),
            (
                "```json\n{\"tool\": \"read_file\", \"path\": 42}\n```",
                "wrong-field-type",
            ),
            (
                "```json\n{\"tool\": \"search\", \"pattern\": \"\"}\n```",
                "empty-field",
            ),
        ];
        for (text, reason) in cases {
            assert_eq!(
                parse_action(text),
                Err(ActionRejection::Malformed { reason }),
                "input: {text}"
            );
        }
    }

    #[test]
    fn oversized_fields_are_rejected_before_dispatch() {
        let big_write = format!(
            "```json\n{{\"tool\": \"write_file\", \"path\": \"a\", \"content\": \"{}\"}}\n```",
            "x".repeat(MAX_WRITE_CONTENT_BYTES + 1)
        );
        assert_eq!(
            parse_action(&big_write),
            Err(ActionRejection::Malformed {
                reason: "field-too-large"
            })
        );
    }
}
