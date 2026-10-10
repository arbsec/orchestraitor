//! Minimal local content search for the worker's `search` tool.
//!
//! Plain substring matching over UTF-8 text files in sorted relative-path
//! order; MCP-server-based search is a later lane (issue #310 thin-slice
//! scope).

use std::fs;
use std::path::{Path, PathBuf};

/// Per-file size cap for `search` (bytes).
const MAX_SEARCH_FILE_BYTES: u64 = 1024 * 1024;
/// File count cap for one `search` walk.
const MAX_SEARCH_FILES: usize = 500;
/// Match count cap for one `search`.
const MAX_SEARCH_MATCHES: usize = 50;
/// Displayed line length cap inside search matches (chars).
const MAX_MATCH_LINE_CHARS: usize = 200;

/// Truncates text to at most `max` BYTES with an explicit marker, never
/// splitting a UTF-8 character (byte budgets stay byte budgets: multibyte
/// text cannot exceed the cap the way a char cut would).
pub(crate) fn truncate_bytes(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n[truncated]", &text[..cut])
}

/// Truncates text to `max` chars with an explicit marker (model-facing
/// observations stay bounded regardless of tool output size).
pub(crate) fn truncate_chars(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let taken: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{taken}\n[truncated]")
    } else {
        taken
    }
}

/// Minimal local content search: plain substring match over UTF-8 text files
/// under `base`, in sorted relative-path order, skipping symlinks and `.git`.
/// MCP-server-based search is a later lane (issue #310 thin-slice scope).
pub(crate) fn search_files(base: &Path, root: &Path, pattern: &str) -> Vec<String> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            if file_type.is_dir() {
                if entry.file_name() != ".git" {
                    stack.push(path);
                }
            } else if file_type.is_file() {
                files.push(path);
            }
        }
        if files.len() >= MAX_SEARCH_FILES {
            break;
        }
    }
    files.sort();

    let mut matches = Vec::new();
    for file in files {
        if matches.len() >= MAX_SEARCH_MATCHES {
            break;
        }
        let Ok(metadata) = fs::metadata(&file) else {
            continue;
        };
        if metadata.len() > MAX_SEARCH_FILE_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(&file) else {
            continue;
        };
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        let relative = file.strip_prefix(root).map_or(file.as_path(), |rel| rel);
        for (index, line) in content.lines().enumerate() {
            if matches.len() >= MAX_SEARCH_MATCHES {
                break;
            }
            if line.contains(pattern) {
                matches.push(format!(
                    "{}:{}: {}",
                    relative.display(),
                    index + 1,
                    truncate_chars(line, MAX_MATCH_LINE_CHARS)
                ));
            }
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::truncate_bytes;

    #[test]
    fn truncate_bytes_caps_multibyte_text_at_the_byte_bound() {
        // Regression (CR finding #5): a char-based cut lets multibyte text
        // exceed a byte budget (3-byte chars → up to ~3× the bound). The
        // byte cut must keep the total under the cap including the marker.
        let text = "é".repeat(10_000); // 20_000 bytes, 10_000 chars
        let capped = truncate_bytes(&text, 1_024);
        assert!(capped.len() <= 1_024 + "\n[truncated]".len());
        assert!(capped.ends_with("\n[truncated]"));
        // The kept body is byte-bound-faithful: every retained char is whole
        // (the string is valid UTF-8 by construction of the boundary walk).
        let body = capped.strip_suffix("\n[truncated]").unwrap();
        assert_eq!(body.chars().count(), 512);
        // Exact boundary: ASCII content is cut at exactly the cap.
        let ascii = "a".repeat(2_000);
        assert_eq!(
            truncate_bytes(&ascii, 1_024).len(),
            1_024 + "\n[truncated]".len()
        );
        // Under the cap: untouched.
        assert_eq!(truncate_bytes("short", 1_024), "short");
    }
}
