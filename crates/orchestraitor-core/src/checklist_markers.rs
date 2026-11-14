//! The MANAGED checklist-marker grammar and the one body scan that decides what
//! a PR body's checklist reading is (arbsec/orchestraitor#799).
//!
//! # Why this module exists
//!
//! The refusal rules used to live in THREE copies — the engine
//! (`crates/orchestraitor-mcp/src/pr_status.rs`), the shell composition helper
//! (`orc_lib_ensure_checklist_markers`) and the Rust delivery neutraliser
//! (`crates/orchestraitor-worker/src/delivery.rs`) — and each copy drifted. Two
//! adversarial reviews in one round found the same shape twice: the delivery
//! lane classified a marker indented four spaces as CANONICAL (its local
//! `trim_start` test) and left it in the composed body, where the engine then
//! reported `indented_marker`; the shell helper skipped the STICKY
//! indented-opener refusal on its append path. Patching a copy produces another
//! copy, so the grammar and the scan live HERE, in a crate every caller already
//! depends on, and the engine is a thin mapping of this scan onto its typed
//! verdict — there is nothing left for a caller to reimplement.
//!
//! The shell engine (`orc_lib__fence_scan` in
//! `.agents/skills/github-pr-lifecycle/scripts/_lib.sh`) is the one remaining
//! independent implementation, because it is a bash program and cannot call
//! Rust. The shared corpus (`scripts/tests/checklist-corpus/`, asserted by BOTH
//! suites) is its contract with this module.
//!
//! # The grammar
//!
//! A managed marker has ONE canonical form:
//!
//! ```text
//! [ \t]*- [BOX] <!-- orc:NAME --> <description>
//! ```
//!
//! - the indent is SPACES and TABS and nothing else;
//! - every separator is EXACTLY ONE ASCII space: between `-` and `[`, between
//!   `]` and `<!--`, and inside `<!-- orc:NAME -->`;
//! - `<BOX>` is exactly one of `x`, `X` or a single SPACE;
//! - `<NAME>` is one of [`MANAGED_MARKER_NAMES`].
//!
//! A line that is NOT canonical but carries BOTH a task-item BOX TOKEN (`[` +
//! exactly one character + `]`) AND a reference to one of the nine names
//! (`orc:NAME`) is a NEAR-MARKER: the author presented a managed marker and the
//! gate cannot tell what they asserted, so the reading is REFUSED. A line that
//! only mentions `orc:NAME` (prose) or carries only a box token (a plain task)
//! is neither.
//!
//! # What makes a reading INDETERMINATE
//!
//! Four constructs, reported as [`UnparseableKind`]:
//!
//! 1. [`UnparseableKind::IndentedFenceOpener`] — a fence OPENER indented 1-3
//!    spaces. Its container (top level, list item, block quote) cannot be
//!    determined without a container model, so the region this scan models can
//!    swallow a genuine unchecked item. The condition is STICKY: a closer does
//!    not clear it, because a closer at a different container level ends the
//!    region EARLIER for a renderer than for this scan.
//! 2. [`UnparseableKind::HtmlConstruct`] — a line outside every fenced region
//!    whose FIRST non-whitespace character is `<`, indented 0-3 spaces. HTML is
//!    not modelled, so the scan cannot tell what such a line hides.
//! 3. [`UnparseableKind::IndentedMarker`] — a managed marker or near-marker
//!    indented FOUR OR MORE spaces, or led by a tab. When it is not inside a
//!    list item it renders as an INDENTED CODE BLOCK, where it is not a
//!    checkbox, and nothing here can tell those apart (arbsec/orchestraitor#802).
//! 4. [`UnparseableKind::NearMarker`] — see above.
//!
//! The markers inside a FENCED region are EXAMPLES, not evidence, and every
//! line of such a region (opener, content and closer) is excluded.

/// The nine MANAGED marker names.
///
/// A managed marker has ONE canonical form and its `<NAME>` is one of these. The
/// list is the SINGLE source of truth for every Rust caller; the shell engine's
/// fallback constant is kept equal to `.github/PULL_REQUEST_TEMPLATE.md` by the
/// drift guard in `test-reconcile-checklist`, and the engine's own drift test
/// pins this array to the same template.
pub const MANAGED_MARKER_NAMES: [&str; 9] = [
    "issue",
    "service-identity",
    "tests",
    "security",
    "docs",
    "rollback",
    "checks",
    "review",
    "threads",
];

/// One line parsed as a CANONICAL managed marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalMarker {
    /// The `orc:NAME` name; one of [`MANAGED_MARKER_NAMES`].
    pub name: &'static str,
    /// Whether the box is ticked (`x` or `X`).
    pub checked: bool,
    /// The description: the text after `-->`, trimmed of SPACES and TABS only
    /// (never of a Unicode whitespace class — that class was a cross-engine
    /// divergence).
    pub description: String,
}

/// The construct that made a body's checklist reading INDETERMINATE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnparseableKind {
    /// A fence OPENER indented 1-3 spaces. STICKY: closing the region does not
    /// clear it. Remedy: dedent the opener to column 0.
    IndentedFenceOpener,
    /// A raw HTML construct: a line-start `<` outside every fenced region,
    /// indented 0-3 spaces. Remedy: fence it, indent it by 4 spaces, or remove
    /// it.
    HtmlConstruct,
    /// A managed marker (or near-marker) indented 4+ spaces or led by a tab.
    /// Remedy: dedent the marker line to column 0.
    IndentedMarker,
    /// A near-marker: a box token plus an `orc:NAME` reference that is not in
    /// the canonical form. Remedy: write the marker canonically.
    NearMarker,
}

/// A fence that OPENS on a line: its character, how many times it is repeated,
/// and its indentation in spaces (0-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FenceOpener {
    /// The fence character: a backtick or a tilde.
    pub character: char,
    /// How many times the character is repeated (3 or more).
    pub run: usize,
    /// The line's indentation in spaces (0-3). A NON-ZERO indent makes the
    /// opener's container scope indeterminate (the condition is sticky).
    pub indent: usize,
}

/// One line of the normalised body with every verdict the grammar has for it.
///
/// Every caller that must agree with the engine reads THESE fields and the
/// methods below; none of them re-derives a rule from the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedLine {
    /// 1-based line number in the normalised body.
    pub number: u64,
    /// The line's content, without its terminator.
    pub text: String,
    /// True when the line is INSIDE a fenced code region — content, opener or
    /// closer alike. Such a line is an EXAMPLE, never evidence, and a caller
    /// must leave it exactly as written.
    pub fenced: bool,
    /// The canonical managed marker this line is, when it is one. Reported even
    /// for an indented (refused) marker: the count is a pure function of the
    /// grammar, and the refusal is a separate verdict.
    pub canonical: Option<CanonicalMarker>,
    /// True when the line is a NEAR-MARKER.
    pub near_marker: bool,
    /// The fence this line OPENS, when it opens one.
    pub opens_fence: Option<FenceOpener>,
    /// True when the leading indentation leaves the 0-3 space window: four or
    /// more spaces, or a tab after 0-3 spaces (a tab reaches the 4-column tab
    /// stop).
    pub indentation_too_deep: bool,
}

impl ScannedLine {
    /// True when this line is a managed marker line: canonical or near-marker.
    #[must_use]
    pub fn is_managed_marker(&self) -> bool {
        self.canonical.is_some() || self.near_marker
    }

    /// True when this line is a raw HTML construct: outside every fenced region,
    /// not a fence opener, and its FIRST non-whitespace character is `<` with
    /// something after it, indented 0-3 spaces.
    #[must_use]
    pub fn is_html_construct(&self) -> bool {
        !self.fenced
            && self.opens_fence.is_none()
            && fence_view(&self.text)
                .is_some_and(|(_, trimmed)| trimmed.len() > 1 && trimmed.starts_with('<'))
    }

    /// True when this line is the managed-marker indentation refusal: a managed
    /// marker (canonical or near-marker) whose indentation is too deep.
    #[must_use]
    pub fn is_indented_marker(&self) -> bool {
        self.is_managed_marker() && self.indentation_too_deep
    }

    /// True when this line opens a fence whose container scope is indeterminate
    /// (indented 1-3 spaces). STICKY: a closer never clears it.
    #[must_use]
    pub fn is_indented_fence_opener(&self) -> bool {
        self.opens_fence.is_some_and(|opener| opener.indent > 0)
    }
}

/// The whole-body scan: every line, in order, and the sticky opener flag.
#[derive(Debug, Clone)]
pub struct BodyScan {
    /// Every line of the normalised body, in order.
    pub lines: Vec<ScannedLine>,
}

impl BodyScan {
    /// The VISIBLE lines — everything outside a fenced region.
    pub fn visible(&self) -> impl Iterator<Item = &ScannedLine> {
        self.lines.iter().filter(|line| !line.fenced)
    }

    /// The EARLIEST construct that makes this body's reading indeterminate, or
    /// `None` when the reading is determinate.
    ///
    /// At the same line the order is [`UnparseableKind`]'s own: a fence opener
    /// first, then an HTML construct, then an indented marker, then a
    /// near-marker — a line that is BOTH indented and a near-marker reports the
    /// marker-indentation rule, because the indentation is what makes it render
    /// as code.
    #[must_use]
    pub fn first_unparseable(&self) -> Option<(u64, UnparseableKind)> {
        for line in &self.lines {
            if line.is_indented_fence_opener() {
                return Some((line.number, UnparseableKind::IndentedFenceOpener));
            }
            if line.is_html_construct() {
                return Some((line.number, UnparseableKind::HtmlConstruct));
            }
            if line.is_indented_marker() {
                return Some((line.number, UnparseableKind::IndentedMarker));
            }
            if line.near_marker {
                return Some((line.number, UnparseableKind::NearMarker));
            }
        }
        None
    }
}

/// Normalises line endings ONCE, before any scan (`CommonMark` §2.1: a lone CR
/// is a line ending and so is CRLF; parsers normalise both to LF first). Every
/// `\r\n` becomes `\n`, then every remaining lone `\r` becomes `\n`, so no CR
/// byte reaches the scan and the closing-fence tail rule is exactly "only spaces
/// and tabs".
#[must_use]
pub fn normalize_line_endings(body: &str) -> String {
    body.replace("\r\n", "\n").replace('\r', "\n")
}

/// The part of `line` at which a fence may open or close, or `None` when the
/// line is indented too far to be a fence line.
///
/// `CommonMark`/GFM allow at most THREE spaces of indentation before a top-level
/// fence or HTML block. Four or more spaces — or a tab, which reaches the
/// 4-column tab stop — is an INDENTED code block, where fence characters and
/// HTML tags are literal text (arbsec/orchestraitor#799 F16). A form feed or a
/// vertical tab is likewise not indentation: it is content, so a `\f`- or
/// `\v`-led fence line opens nothing (F18).
#[must_use]
pub fn fence_view(line: &str) -> Option<(usize, &str)> {
    let indent = line.chars().take_while(|c| *c == ' ').count();
    if indent > 3 {
        return None;
    }
    let rest = line.get(indent..)?;
    if rest.starts_with('\t') {
        return None;
    }
    Some((indent, rest))
}

/// True when the line contains a task-item BOX TOKEN: `[` + exactly ONE
/// CHARACTER + `]`.
///
/// The middle is one `char` and ANY character counts, `]` included: `[]]` is a
/// box token (the first `]` is the middle, the second is the closer), while `[]`
/// is not, because nothing follows the pair. The middle used to exclude `]`,
/// which let `- []] <!-- orc:security --> pending` evade the near-marker rule
/// (arbsec/orchestraitor#799 R15). A multi-byte character still counts as ONE
/// character (so a box holding a check mark is a box token); the shell engine
/// decodes one UTF-8 sequence per box character for the same reason (#799 R12),
/// and the shared corpus pins that the two agree.
#[must_use]
pub fn has_box_token(line: &str) -> bool {
    line.char_indices().any(|(index, character)| {
        if character != '[' {
            return false;
        }
        let Some(rest) = line.get(index + 1..) else {
            return false;
        };
        let mut characters = rest.chars();
        // One character is the middle — `]` is a legitimate middle character,
        // and the NEXT character decides whether the token is complete.
        if characters.next().is_none() {
            return false;
        }
        characters.next() == Some(']')
    })
}

/// Parses one line as a CANONICAL managed marker.
///
/// The grammar is exact and contains NO whitespace class: the indent is
/// spaces/tabs, every separator is exactly one ASCII space, the box is `x`,
/// `X` or a single space, and the name is one of [`MANAGED_MARKER_NAMES`].
/// Returns `None` for anything else — including a NEAR-MARKER, which is
/// reported by the scan, not counted as an item.
#[must_use]
pub fn parse_canonical_marker(line: &str) -> Option<CanonicalMarker> {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let rest = trimmed.strip_prefix("- [")?;
    let (checked_char, after_box) = rest.split_once(']')?;
    if !matches!(checked_char, "x" | "X" | " ") {
        return None;
    }
    // Exactly one ASCII space between `]` and `<!--`.
    let after_box = after_box.strip_prefix(' ')?;
    let rest = after_box.strip_prefix("<!--")?;
    // Exactly one ASCII space after `<!--`, then `orc:NAME`.
    let rest = rest.strip_prefix(' ')?;
    let rest = rest.strip_prefix("orc:")?;
    let (name, rest) = split_managed_marker_name(rest)?;
    // Exactly one ASCII space before `-->`.
    let rest = rest.strip_prefix(' ')?;
    let rest = rest.strip_prefix("-->")?;
    Some(CanonicalMarker {
        name,
        checked: checked_char != " ",
        description: rest.trim_matches([' ', '\t']).to_string(),
    })
}

/// Splits a managed marker NAME out of `orc:NAME -->`, requiring the name to be
/// one of [`MANAGED_MARKER_NAMES`] and to be followed by exactly one ASCII
/// space and `-->`.
fn split_managed_marker_name(rest: &str) -> Option<(&'static str, &str)> {
    for name in MANAGED_MARKER_NAMES {
        if let Some(tail) = rest.strip_prefix(name) {
            return Some((name, tail));
        }
    }
    None
}

/// True when the line carries a task-item box token AND a reference to one of
/// the nine managed names (`orc:NAME`), but is NOT in the canonical form.
///
/// The reference test is a plain substring search and the canonical test is an
/// exact ASCII comparison: no whitespace class is involved, so the two engines
/// cannot disagree about it (arbsec/orchestraitor#799 R1).
#[must_use]
pub fn is_near_marker(line: &str) -> bool {
    if parse_canonical_marker(line).is_some() {
        return false;
    }
    if !has_box_token(line) {
        return false;
    }
    MANAGED_MARKER_NAMES
        .iter()
        .any(|name| line.contains(&format!("orc:{name}")))
}

/// True when a fence OPENS on `line`: at most three leading spaces, THREE OR
/// MORE backticks or tildes, and — for backticks only — an info string that
/// does not contain a backtick (so ```example``` is a CLOSED inline code span,
/// not an opener; a tilde fence's info string may contain backticks).
#[must_use]
pub fn fence_opener(line: &str) -> Option<(char, usize)> {
    let (_, trimmed) = fence_view(line)?;
    let first = trimmed.chars().next()?;
    if first != '`' && first != '~' {
        return None;
    }
    let run = trimmed.chars().take_while(|c| *c == first).count();
    let info_has_backtick = first == '`' && trimmed.chars().skip(run).any(|c| c == '`');
    if run >= 3 && !info_has_backtick {
        Some((first, run))
    } else {
        None
    }
}

/// True when `line` CLOSES the region opened by `(fence_char, fence_len)`: the
/// SAME character repeated at least as many times as the opener, followed by
/// nothing but SPACES and TABS (`CommonMark` 0.31: a closing code fence "may be
/// followed only by spaces or tabs, which are ignored"), after at most three
/// leading spaces. A different fence character never closes the region, and a
/// form feed or a vertical tab in the tail leaves it OPEN (F18).
#[must_use]
pub fn fence_closes(line: &str, fence_char: char, fence_len: usize) -> bool {
    let Some((_, trimmed)) = fence_view(line) else {
        return false;
    };
    if !trimmed.starts_with(fence_char) {
        return false;
    }
    let run = trimmed.chars().take_while(|c| *c == fence_char).count();
    let tail = trimmed.get(run..).unwrap_or_default();
    run >= fence_len && tail.chars().all(|c| c == ' ' || c == '\t')
}

/// Scans a body once: every line with its verdicts.
///
/// This is the ONLY implementation of the fence/model rules on the Rust side.
/// The engine (`parse_checklist` in `orchestraitor-mcp`) maps its
/// [`BodyScan::first_unparseable`] onto its typed verdict and counts
/// [`ScannedLine::canonical`] items; the delivery lane
/// (the `neutralise_summary` caller in `orchestraitor-worker`) reads the same flags
/// to decide what to neutralise before composing a body. Neither may re-derive a
/// rule from the text.
#[must_use]
pub fn scan_body(body: &str) -> BodyScan {
    let normalized = normalize_line_endings(body);
    let mut lines = Vec::new();
    // The open fence: its character and how many times it was repeated.
    let mut fence: Option<(char, usize)> = None;
    for (index, text) in normalized.lines().enumerate() {
        let number = u64::try_from(index + 1).unwrap_or(u64::MAX);
        // Inside a region: only the SAME character, repeated at least as many
        // times as the opener, with nothing but a blank tail after it, closes it.
        if let Some((fence_char, fence_len)) = fence {
            if fence_closes(text, fence_char, fence_len) {
                fence = None;
            }
            lines.push(ScannedLine {
                number,
                text: text.to_string(),
                fenced: true,
                canonical: None,
                near_marker: false,
                opens_fence: None,
                indentation_too_deep: false,
            });
            continue;
        }
        let view = fence_view(text);
        let too_deep = view.is_none();
        let indent = view.map_or(0, |(indent, _)| indent);
        let opener = fence_opener(text);
        let canonical = parse_canonical_marker(text);
        let near_marker = is_near_marker(text);
        if let Some((character, run)) = opener {
            fence = Some((character, run));
            lines.push(ScannedLine {
                number,
                text: text.to_string(),
                fenced: true,
                canonical: None,
                near_marker: false,
                opens_fence: Some(FenceOpener {
                    character,
                    run,
                    indent,
                }),
                indentation_too_deep: too_deep,
            });
            continue;
        }
        lines.push(ScannedLine {
            number,
            text: text.to_string(),
            fenced: false,
            canonical,
            near_marker,
            opens_fence: None,
            indentation_too_deep: too_deep,
        });
    }
    BodyScan { lines }
}
