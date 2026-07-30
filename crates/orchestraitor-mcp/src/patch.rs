//! Unified diff application for `fs.apply_patch`.

pub(crate) fn apply_unified_patch(original: &str, patch: &str) -> Option<String> {
    let source = original.lines().map(str::to_string).collect::<Vec<_>>();
    let mut output = Vec::new();
    let mut source_index = 0usize;
    let patch_lines = patch.lines().collect::<Vec<_>>();
    let mut patch_index = 0usize;
    while patch_index < patch_lines.len() {
        let line = patch_lines[patch_index];
        if !line.starts_with("@@") {
            patch_index += 1;
            continue;
        }
        let old_start = parse_hunk_start(line)?;
        while source_index < old_start.saturating_sub(1) {
            output.push(source.get(source_index)?.clone());
            source_index += 1;
        }
        patch_index += 1;
        while patch_index < patch_lines.len() && !patch_lines[patch_index].starts_with("@@") {
            let hunk_line = patch_lines[patch_index];
            if let Some(context) = hunk_line.strip_prefix(' ') {
                if source.get(source_index)? != context {
                    return None;
                }
                output.push(context.to_string());
                source_index += 1;
            } else if let Some(removed) = hunk_line.strip_prefix('-') {
                if source.get(source_index)? != removed {
                    return None;
                }
                source_index += 1;
            } else if let Some(added) = hunk_line.strip_prefix('+') {
                output.push(added.to_string());
            }
            patch_index += 1;
        }
    }
    output.extend(source.into_iter().skip(source_index));
    let mut rendered = output.join("\n");
    if original.ends_with('\n') {
        rendered.push('\n');
    }
    Some(rendered)
}

fn parse_hunk_start(line: &str) -> Option<usize> {
    let marker = line.strip_prefix("@@ -")?;
    let start = marker.split([',', ' ']).next()?;
    start.parse().ok()
}
