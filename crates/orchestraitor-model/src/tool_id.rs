//! Declared tool-id rules shared by the config registry and the worker
//! parser (issue #535): one implementation, so the two surfaces can never
//! diverge (a reserved name added on one side only would either let the
//! model request a name the registry rejects, or let config define a tool
//! the worker parser always refuses).

/// Maximum accepted length for a declared tool id (chars). The id becomes
/// part of receipt records and task ids; the same bound as worker task ids.
pub const MAX_TOOL_ID_CHARS: usize = 64;

/// Validates a declared tool id: 1..=64 chars, ASCII lowercase letters,
/// digits, `-` or `_`, starting with a letter or digit — the same shape
/// rules as role ids, so a tool id can never carry a path or key separator.
#[must_use]
pub fn is_valid_tool_id(tool_id: &str) -> bool {
    let len = tool_id.chars().count();
    if len == 0 || len > MAX_TOOL_ID_CHARS {
        return false;
    }
    let mut chars = tool_id.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Reserves the built-in tool names and the §9.39 coordinator tool names: a
/// declared tool can never shadow a protocol name.
#[must_use]
pub fn is_reserved_tool_id(tool_id: &str) -> bool {
    matches!(
        tool_id,
        "read_file" | "write_file" | "search" | "bash" | "finish"
    ) || matches!(
        tool_id,
        "board.query"
            | "board.move"
            | "decision.record"
            | "router.consult"
            | "worker.delegate"
            | "budget.check"
            | "capability.check"
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn tool_id_shape_rules() {
        assert!(is_valid_tool_id("explain-clippy"));
        assert!(is_valid_tool_id("review_diff"));
        assert!(is_valid_tool_id("a1"));
        assert!(!is_valid_tool_id(""));
        assert!(!is_valid_tool_id("-leading"));
        assert!(!is_valid_tool_id("has.dot"));
        assert!(!is_valid_tool_id("has/slash"));
        assert!(!is_valid_tool_id("UPPER"));
        assert!(!is_valid_tool_id(&"x".repeat(MAX_TOOL_ID_CHARS + 1)));
        assert!(is_valid_tool_id(&"x".repeat(MAX_TOOL_ID_CHARS)));
    }

    #[test]
    fn protocol_and_coordinator_names_are_reserved() {
        for reserved in ["bash", "finish", "read_file", "write_file", "search"] {
            assert!(is_reserved_tool_id(reserved), "{reserved} must be reserved");
        }
        for reserved in ["board.query", "worker.delegate", "decision.record"] {
            assert!(is_reserved_tool_id(reserved), "{reserved} must be reserved");
        }
        assert!(!is_reserved_tool_id("explain-clippy"));
    }
}
