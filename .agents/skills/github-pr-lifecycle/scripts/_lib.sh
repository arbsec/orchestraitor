#!/usr/bin/env bash
# Shared helpers for the github-project-workflow and github-pr-lifecycle skill scripts.
# Sourced, not executed. Provides: arg parsing for --help/--json/--dry-run/--repo,
# config loading, gh invocation, jq parsing, and stable exit codes.
#
# Conventions enforced (see SKILL.md "Safety conditions"):
#   - set -euo pipefail; no eval; no embedding credentials.
#   - never guess identity: repo/org/project come from config or --repo flag, never defaults.
#   - validate-before-mutate; --dry-run for remote-changing ops; idempotent where practical.
#   - stable exit codes: 0 ok | 1 unrecoverable | 2 config/state | 3 policy | 4 concurrent | 5 blocked.

set -euo pipefail

# --- Exit codes (mirrored from both SKILL.md files) ----------------------------
ORC_OK=0
ORC_ERR_UNRECOVERABLE=1
ORC_ERR_CONFIG=2
ORC_ERR_POLICY=3
ORC_ERR_CONCURRENT=4
ORC_ERR_BLOCKED=5
# 6: the running review-policy script is not the trusted base's copy (see
# `orc_lib_require_policy_current`), or a base ref could not be resolved
# (`pr-create` predates the shared constant and declares its own 6 inline).
ORC_ERR_STALE_POLICY=6

# --- Common flag parsing -------------------------------------------------------
# Usage: orc_lib_parse_common "$@" then read OPT_HELP OPT_JSON OPT_DRY_RUN OPT_REPO
orc_lib_parse_common() {
  OPT_HELP=false; OPT_JSON=false; OPT_DRY_RUN=false; OPT_REPO=""
  ORC_LIB_EXTRA_ARGS=()
  while [ $# -gt 0 ]; do
    case "$1" in
      -h|--help)       OPT_HELP=true; shift ;;
      --json)          OPT_JSON=true; shift ;;
      --dry-run)       OPT_DRY_RUN=true; shift ;;
      -R|--repo)       OPT_REPO="${2:-}"; shift 2 ;;
      -R=*|--repo=*)   OPT_REPO="${1#*=}"; shift ;;
      --)              shift; ORC_LIB_EXTRA_ARGS+=("$@"); break ;;
      *)               ORC_LIB_EXTRA_ARGS+=("$1"); shift ;;
    esac
  done
}

# --- Help renderer -------------------------------------------------------------
# Usage: orc_lib_print_help <script-name> <synopsis>
orc_lib_print_help() {
  local name="$1" synopsis="$2"
  cat <<EOF
Usage: $name [options] [args]

$synopsis

Options:
  -h, --help        Show this help and exit 0
  --json            Emit machine-readable JSON on stdout (human form on stderr)
  --dry-run         Print the exact gh/GraphQL that would run; write nothing; exit 0
  -R, --repo OWNER/REPO   Target repository (required; never inferred from cwd)

Exit codes:
  0  success (or dry-run preview)
  1  unrecoverable error (network, auth, unexpected gh output)
  2  config/state error (missing config, unknown field, not found)
  3  policy violation (would violate MVP-only scheduling, would merge on red, etc.)
  4  concurrent edit detected (resource updatedAt changed since read — retry)
  5  blocked (review loop limit hit — produces blocked/needs-human, never silent approval)
  6  untrusted review policy — a gate script (pr-checks, convergence-status,
     merge-gate) is not the trusted base's copy (rerun the printed base-worktree
     command), or (pr-create) the PR's base ref cannot be resolved (fetch or
     repair the base ref, then rerun)
EOF
}

# --- Body-argument resolution (@file expansion + path-shaped-arg guard) --------
# Body values flowing into `gh -f body=...` / `--body ...` are CONTENT, but
# operators (and agents) familiar with other tools' `@file` convention pass a
# path — which used to be posted as the literal string (issue #587: PR #585
# replies, issue #586's body, PR #596's double-encoded body). This helper makes
# the convention explicit and fail-closed:
#
#   - `@<path>` with an existing readable file -> prints the file's content
#     (documented expansion; the file must exist and be non-empty).
#   - a value that is an EXISTING file path WITHOUT `@` -> typed error naming
#     the argument and the file: this is the classic misuse; never silently
#     post a path string. Use `@<path>` explicitly.
#   - anything else -> printed verbatim (normal literal text).
#
# Usage: orc_lib_body_arg <arg-name> <value>   (prints the resolved body)
# Exit 2 (ORC_ERR_CONFIG) on a misuse-shaped value.
#
# @file trust boundary (CodeRabbit PR #599): these scripts run with agent- and
# tool-supplied arguments, so an @<path> value could exfiltrate an arbitrary
# local file (e.g. @/etc/passwd) as a posted body. @file paths are therefore
# resolved to a canonical absolute path and must fall inside one of the
# trusted roots: the current workspace (git toplevel of the cwd) and the
# system/user temp directories (TMPDIR, /tmp, /var/tmp). Everything else is
# rejected with exit 2 BEFORE any file is read.
orc_lib_body_arg() {
  local arg_name="$1" value="$2"
  if [ -z "$value" ]; then
    printf '%s' "$value"
    return 0
  fi
  if [[ "$value" == @* ]]; then
    local file="${value#@}"
    if [ -z "$file" ]; then
      echo "error: $arg_name: \`@\` with no path (expected \`@<path>\` to read the body from a file)" >&2
      exit "$ORC_ERR_CONFIG"
    fi
    orc_lib_body_arg__check_trusted "$arg_name" "$file" || exit "$ORC_ERR_CONFIG"
    if [ ! -f "$file" ]; then
      echo "error: $arg_name: @file not found: $file" >&2
      exit "$ORC_ERR_CONFIG"
    fi
    if [ ! -r "$file" ]; then
      echo "error: $arg_name: @file is not readable: $file" >&2
      exit "$ORC_ERR_CONFIG"
    fi
    if [ ! -s "$file" ]; then
      echo "error: $arg_name: @file is empty: $file" >&2
      exit "$ORC_ERR_CONFIG"
    fi
    cat "$file"
    return 0
  fi
  if [ -e "$value" ]; then
    echo "error: $arg_name: value \`$value\` is an existing file path, not body text — this would post the path string as content. Use \`@$value\` to load the file's content, or quote/inline the text." >&2
    exit "$ORC_ERR_CONFIG"
  fi
  printf '%s' "$value"
}

# The git toplevel of the CURRENT WORKING DIRECTORY, with the ambient git
# LOCATION scrubbed.
#
# `git -C <dir>` does NOT isolate the git location: a hook-exported
# GIT_DIR/GIT_WORK_TREE wins over the cwd, so an inherited location resolves
# the "workspace" root to the CALLER's repository — and, in a fixture, makes
# the fixture's own `git add -A` / `git commit` stage and commit the caller's
# working tree. GIT_INDEX_FILE has the same effect on the index.
#
# Scope: every `@file` resolution (`orc_lib_body_arg`) and every `--body-file`
# route (`orc_lib_read_body_file`, used by BOTH `pr-create` and `pr-mutate`)
# takes its "workspace" root from here. Production gate scripts that still call
# `git rev-parse --show-toplevel` directly are NOT covered — tracked in
# arbsec/orchestraitor#806.
orc_lib_git_toplevel() {
  env -u GIT_DIR -u GIT_WORK_TREE -u GIT_INDEX_FILE \
    git rev-parse --show-toplevel 2>/dev/null || true
}

# Canonicalize an @file path and verify it falls inside a trusted root
# (workspace git toplevel, or a system/user temp directory). Prints a typed
# error and returns 1 when the path is outside every trusted root; returns 0
# with no output otherwise. Symlink-safe: resolves the FULL TARGET with
# `realpath -e` (every symlink in the path, including the final component, is
# followed before the trusted-root check), so a symlink inside a trusted
# directory pointing outside all trusted roots is refused — a directory-only
# canonicalization once let such a link through and `cat` leaked the target's
# content (CodeRabbit PR #599).
orc_lib_body_arg__check_trusted() {
  local arg_name="$1" file="$2" resolved root
  if ! resolved="$(realpath -e -- "$file" 2>/dev/null)" || [ -z "$resolved" ]; then
    # Unresolvable path (missing component or dangling symlink): let the
    # caller's own not-found check produce the typed error.
    return 0
  fi
  local roots=()
  if root="$(orc_lib_git_toplevel)" && [ -n "$root" ]; then
    roots+=("$root")
  fi
  local t
  for t in "${TMPDIR:-/tmp}" /tmp /var/tmp; do
    if root="$(cd -- "$t" 2>/dev/null && pwd -P)" && [ -n "$root" ]; then
      roots+=("$root")
    fi
  done
  for root in "${roots[@]}"; do
    case "$resolved" in
      "$root"|"$root"/*) return 0 ;;
    esac
  done
  echo "error: $arg_name: @file path is outside the trusted roots (workspace or temp directory): $file" >&2
  return 1
}

# Guard for flows that pass a --body-file path straight to gh: the file must
# exist, be readable, and be non-empty (gh would otherwise post an empty or
# error out late with an ambiguous message).
orc_lib_require_body_file() {
  local arg_name="$1" file="$2"
  if [ ! -f "$file" ]; then
    echo "error: $arg_name: body file not found: $file" >&2
    exit "$ORC_ERR_CONFIG"
  fi
  if [ ! -r "$file" ]; then
    echo "error: $arg_name: body file is not readable: $file" >&2
    exit "$ORC_ERR_CONFIG"
  fi
  if [ ! -s "$file" ]; then
    echo "error: $arg_name: body file is empty: $file" >&2
    exit "$ORC_ERR_CONFIG"
  fi
}

# Reads a `--body-file` value: a PATH, or `-` for STDIN. Prints the content.
#
# `--body-file -` is supported EXPLICITLY: `gh pr create --body-file -` reads
# the body from stdin, and the base `pr-create` forwarded `--body-file` to gh,
# so a caller that pipes a body must keep working. A path is resolved through
# the SAME trusted-root guard as `--body @file` — an out-of-root path is
# refused with exit 2 (the tightened rule, recorded in CHANGELOG.md).
#
# Usage: orc_lib_read_body_file <arg-name> <path-or-dash>
orc_lib_read_body_file() {
  local arg_name="$1" file="$2" content=""
  if [ "$file" = "-" ]; then
    if [ -t 0 ]; then
      echo "error: $arg_name: '-' reads the body from STDIN, but STDIN is a terminal — pipe the body in, or pass a file path" >&2
      exit "$ORC_ERR_CONFIG"
    fi
    content="$(cat)"
    if [ -z "$content" ]; then
      echo "error: $arg_name: no body on STDIN" >&2
      exit "$ORC_ERR_CONFIG"
    fi
    printf '%s' "$content"
    return 0
  fi
  orc_lib_body_arg "$arg_name" "@$file"
}

# --- Managed PR checklist (arbsec/orchestraitor#788) ---------------------------
# `reconcile-checklist` reports a body with ZERO `<!-- orc:* -->` markers as
# MISSING evidence, never as "all checked", and `convergence-status` blocks that
# body with the distinct reason `checklist_markers_missing`. The gate is only
# satisfiable if PR CREATION gives a wholesale body the managed checklist, so
# `pr-create` appends these markers to a body that carries none (#683 class:
# a fail-closed rule no caller can satisfy is worse than the bug).
#
# `.github/PULL_REQUEST_TEMPLATE.md` is the single source of truth; this
# constant is the fallback for a checkout that does not carry it. The drift
# guard in `test-reconcile-checklist` asserts the two agree verbatim.
ORC_LIB_CHECKLIST_FALLBACK='- [ ] <!-- orc:issue --> Linked issue and specification requirements are satisfied
- [ ] <!-- orc:service-identity --> All GitHub writes (PR creation, comments, review posts, merges, commits) attributed to arbsec-agent — no personal-account (@mekwall) writes (owner mandate; .agents/project/orchestraitor-workflow.md "GitHub service identity")
- [ ] <!-- orc:tests --> Tests are added or updated
- [ ] <!-- orc:security --> Security impact is reviewed
- [ ] <!-- orc:docs --> Human-facing documentation is updated, or not required with justification
- [ ] <!-- orc:rollback --> Rollback or recovery behavior is verified where applicable
- [ ] <!-- orc:checks --> All required and non-optional checks pass
- [ ] <!-- orc:review --> All noteworthy findings are resolved
- [ ] <!-- orc:threads --> All actionable review threads are resolved'

# --- Fenced code regions (arbsec/orchestraitor#788) ---------------------------
# A managed checklist marker inside a FENCED code region is an EXAMPLE, not
# evidence: the marker scan must ignore it, or a body that merely SHOWS the
# checklist shape satisfies the gate. The shell engine (this helper) and the
# Rust engine (the helper next to `parse_checklist` in
# `crates/orchestraitor-mcp/src/pr_status.rs`) agree on these semantics
# exactly:
#
#   - LINE ENDINGS are normalised ONCE, before anything is scanned (CommonMark
#     §2.1: a lone CR is a line ending and so is CRLF; parsers normalise both to
#     LF before parsing). Every `\r\n` becomes `\n`, then every remaining lone
#     `\r` becomes `\n`, so no CR byte reaches the scanners and no engine strips
#     a trailing CR ad hoc;
#   - a fence OPENS on a line with at most THREE leading spaces whose next
#     characters are 3+ backticks OR 3+ tildes (four or more spaces — or a
#     tab, which reaches the 4-column tab stop — is an INDENTED code block,
#     where fence characters are literal text; arbsec/orchestraitor#799 F16);
#   - a backtick fence's info string MUST NOT contain a backtick: a line like
#     ```example``` is a CLOSED inline code span, not an opener (GFM); a tilde
#     fence MAY carry backticks in its info string (arbsec/orchestraitor#799
#     F17);
#   - the region CLOSES at the first later line whose first 0-3 characters are
#     spaces and whose next characters are the SAME fence character repeated
#     at least as many times as the opener, followed by nothing but SPACES and
#     TABS — CommonMark 0.31: the closing fence "may be followed only by
#     spaces or tabs, which are ignored." A form feed, a vertical tab, an info
#     string or any other character leaves the region OPEN; a DIFFERENT fence
#     character never closes it (arbsec/orchestraitor#799 F18);
#   - an unclosed fence runs to the end of the body;
#   - every line inside the region — INCLUDING the opening and the closing
#     fence lines — is excluded from marker scanning;
#   - an HTML COMMENT (`<!--` … `-->`) is not code and not evidence: its
#     delimiter lines and everything between them are invisible to every scan,
#     and a fence inside it neither opens nor closes a region. A checklist item
#     line is NOT a comment opener: its `<!-- orc:NAME -->` is the marker
#     delimiter, so `- [ ] <!-- orc:x -->` stays scannable. An unclosed
#     comment runs to the end of the body (fail closed: everything after it is
#     invisible evidence);
#   - a fence OPENER indented 1-3 spaces is INDETERMINATE SCOPE: it may equally
#     be inside a list item or a block quote, and this scanner has no container
#     model, so `orc_lib_fence_unparseable_line` reports it and every checklist
#     verdict BLOCKS with `checklist_unparseable` rather than guessing
#     (arbsec/orchestraitor#809 tracks real container tracking).
#
# Usage: orc_lib_strip_fenced_code   (reads the body on STDIN, prints it with
# every fenced region and every HTML comment removed).
#
# ONE state machine, three modes: `orc_lib__fence_scan` owns the rules above,
# and every caller drives it. A second, separately written scanner is exactly
# how the shell and Rust engines drifted before (arbsec/orchestraitor#788).
orc_lib_strip_fenced_code() { orc_lib__fence_scan strip; }

# The scan state at the END of the body: `closed`, or `open <char> <len>` for a
# fence that never closed. Reads the body on STDIN.
orc_lib_fence_state() { orc_lib__fence_scan state; }

# The 1-based line number of the FIRST fence opener indented 1-3 spaces, when
# the body carries one; nothing at all otherwise. Such an opener's container
# scope is indeterminate without a container model, so a checklist reading from
# this body must BLOCK, never guess (arbsec/orchestraitor#809). Reads the body
# on STDIN.
orc_lib_fence_unparseable_line() { orc_lib__fence_scan indeterminate; }

# CommonMark §2.1 line endings, normalised ONCE before any scan: replace every
# `\r\n` with `\n`, then every remaining lone `\r` with `\n`. Reading the body
# on STDIN, this prints it with no CR byte left, so every later rule is written
# against LF lines only. `str::lines()` in the Rust engine makes exactly one
# strip; the scanners used to strip a second CR by hand and disagreed on
# `` ```\r\r\n `` (arbsec/orchestraitor#799 gen-7).
orc_lib_normalize_line_endings() {
  awk '{ line = $0; gsub(/\r$/, "", line); gsub(/\r/, "\n", line); print line }'
}

# Usage: orc_lib__fence_scan <strip|state|indeterminate>   (stdin = body)
orc_lib__fence_scan() {
  orc_lib_normalize_line_endings |
  awk -v mode="${1:?fence scan mode required}" '
    # The part of the line a fence may open or close at. CommonMark/GFM allow
    # at most THREE leading spaces before a top-level fence; four or more — or
    # a tab, which reaches the 4-column tab stop — is an INDENTED code block,
    # where fence characters are literal text (#799 F16).
    #
    # A form feed and a vertical tab are NOT indentation: they are content, so
    # a `\f`- or `\v`-led fence line opens nothing (#799 F18, corpus case 38).
    function fence_view(line,   n, rest) {
      n = 0
      while (substr(line, n + 1, 1) == " ") n++
      if (n > 3) return ""
      rest = substr(line, n + 1)
      if (substr(rest, 1, 1) == "\t") return ""
      return rest
    }
    function run_length(s, c,   n) {
      n = 0
      while (substr(s, n + 1, 1) == c) n++
      return n
    }
    # The CLOSING tail test. CommonMark 0.31: a closing code fence "may be
    # followed only by spaces or tabs, which are ignored."
    #
    # Everything else keeps the region OPEN: a form feed (Rust
    # `is_ascii_whitespace` accepts it, the spec does not — this used to be a
    # divergence), a vertical tab, an info string. POSIX `[[:space:]]` is
    # wrong too: it carries the vertical tab, so gawk closed on "```\v" while
    # GitHub renders the fence as still open (#799 F18). There is no carriage
    # return to special-case: the body is normalised to LF before this scan.
    function closes_here(s, n) {
      return substr(s, n + 1) ~ /^[ \t]*$/
    }
    # A checklist marker line is NOT an HTML comment opener: its `<!-- orc:NAME
    # -->` pair is the MARKER delimiter on a visible task-list line, so it must
    # stay in the scan output. Exactly the shape the marker scan accepts below.
    function is_marker_line(line,   t) {
      t = line
      sub(/^[ \t]+/, "", t)
      return t ~ /^- *\[[xX ]\] *<!-- *orc:[A-Za-z0-9_-]+ *-->/
    }
    {
      s = fence_view($0)
      if (fence_char != "") {
        # Inside a region: only the SAME character, repeated at least as many
        # times as the opener, with nothing but a blank tail after it, closes
        # it (see `closes_here`).
        if (s != "" && substr(s, 1, 1) == fence_char) {
          n = run_length(s, fence_char)
          if (n >= fence_len && closes_here(s, n)) {
            fence_char = ""
            fence_len = 0
          }
        }
        next
      }
      if (in_comment) {
        # Inside an HTML comment: the delimiter line that carries "-->" ends it
        # (inclusive — CommonMark ends a type-2 HTML block at the END of that
        # line, so text after the delimiter is still invisible), and nothing on
        # the line can open or close a fence.
        if (index($0, "-->") > 0) in_comment = 0
        next
      }
      if (s != "") {
        c = substr(s, 1, 1)
        if (c == "`" || c == "~") {
          n = run_length(s, c)
          # A backtick fence info string must NOT contain a backtick (GFM):
          # ```example``` is a CLOSED inline span, not an opener. A tilde
          # fence MAY carry backticks (#799 F17).
          if (n >= 3 && !(c == "`" && index(substr(s, n + 1), "`") > 0)) {
            # A 1-3-space indent means the opener may sit inside a list item or
            # a block quote: its container scope is indeterminate here, so the
            # line number is recorded and the verdict BLOCKS (#809).
            ind = 0
            while (substr($0, ind + 1, 1) == " ") ind++
            if (ind > 0 && unparseable_line == 0) unparseable_line = NR
            fence_char = c
            fence_len = n
            next
          }
        }
      }
      # `<!--` opens an HTML comment. The delimiter line itself is invisible, and
      # a fence inside the comment opens nothing; an unclosed comment runs to
      # the end of the body (fail closed).
      if (index($0, "<!--") > 0 && !is_marker_line($0)) {
        if (index(substr($0, index($0, "<!--") + 4), "-->") == 0) in_comment = 1
        next
      }
      if (mode == "strip") print $0
    }
    END {
      if (mode == "state") {
        if (fence_char != "") printf "open %s %d\n", fence_char, fence_len
        else print "closed"
      } else if (mode == "indeterminate" && unparseable_line > 0) {
        print unparseable_line
      }
    }
  '
}

# True (exit 0) when the text is already CHECKLIST-BEARING: at least one
# `- [ ] <!-- orc:NAME -->` item line OUTSIDE any fenced code region. Such a
# body is never rewritten: author content is evidence, not something a helper
# edits.
#
# The marker line must carry a REAL box character — a space (unchecked), `x`
# or `X` (checked) — and nothing else: any other character is not a checklist
# line at all (the Rust engine accepts exactly the same three).
#
# Only SPACE and TAB indent a checklist item — Rust trims
# `trim_start_matches([' ', '\t'])` and nothing else. The leading class is
# deliberately not PCRE `\s`, which also matches a vertical tab: that would
# count a `\v`-led marker line the Rust engine skips (#799 F18). The `\s*`
# between the dash and `[` is a SEPARATE, still-open divergence
# (arbsec/orchestraitor#807) and is not narrowed here.
#
# There is NO author-granted opt-out: a body that carries no marker is
# missing evidence and fails closed with `checklist_markers_missing`
# (arbsec/orchestraitor#788). An exemption may only come from an authority
# the author does not hold — tracked in arbsec/orchestraitor#801.
orc_lib_has_checklist() {
  local text="$1" stripped
  stripped="$(printf '%s\n' "$text" | orc_lib_strip_fenced_code)"
  printf '%s\n' "$stripped" \
    | grep -cP '^[ \t]*-\s*\[[xX ]\]\s*<!--\s*orc:[A-Za-z0-9_-]+\s*-->' >/dev/null
}

# Prints the managed checklist (nine UNCHECKED markers). Reads
# `<root>/.github/PULL_REQUEST_TEMPLATE.md` when it exists, normalizing any
# pre-ticked box back to unchecked — a template is a baseline, never evidence —
# and falls back to ORC_LIB_CHECKLIST_FALLBACK.
orc_lib_checklist_markers() {
  local root="${1:-}" markers=""
  if [ -n "$root" ] && [ -f "$root/.github/PULL_REQUEST_TEMPLATE.md" ]; then
    markers="$(grep -oP '^\s*-\s*\[[ xX]\]\s*<!--\s*orc:[A-Za-z0-9_-]+\s*-->.*$' \
      "$root/.github/PULL_REQUEST_TEMPLATE.md" || true)"
  fi
  if [ -z "$markers" ]; then
    markers="$ORC_LIB_CHECKLIST_FALLBACK"
  fi
  printf '%s\n' "$markers" | sed -E 's/^([[:space:]]*)-[[:space:]]*\[[xX]\]/\1- [ ]/'
}

# Prints the body, appending the managed checklist when it carries none (#788).
# Usage: orc_lib_ensure_checklist_markers <body> [repo-root]
#
# An EMPTY body is a legitimate input: `pr-create` with no `--body`/`--body-file`
# still owes the PR a compliant body, and a body that carries only the
# checklist is better than a body that can never converge (#683 class: a
# fail-closed rule no caller can satisfy is worse than the bug).
#
# A body that ends INSIDE an unclosed fenced code region is REFUSED (exit 2):
# anything appended there renders as code and is dropped by every marker scan,
# so the append would look like a remediation while the body still reported
# `checklist_markers_missing` — and a second `--inject-checklist` run would
# append yet another invisible copy. The author's body is neither reordered nor
# repaired: the error names the fence to close (#788 gen-3 review, CodeRabbit
# major).
orc_lib_ensure_checklist_markers() {
  local body="$1" root="${2:-}" fence_state="" fence_char="" fence_len="" fence_name=""
  if orc_lib_has_checklist "$body"; then
    printf '%s' "$body"
    return 0
  fi
  if [ -n "$body" ]; then
    fence_state="$(printf '%s\n' "$body" | orc_lib_fence_state)"
    if [ "$fence_state" != "closed" ]; then
      fence_char="${fence_state#open }"
      fence_len="${fence_char#* }"
      fence_char="${fence_char%% *}"
      case "$fence_char" in
        '`') fence_name="backticks" ;;
        '~') fence_name="tildes" ;;
        *) fence_name="fence characters" ;;
      esac
      echo "error: refusing to append the managed merge checklist: the PR body ends inside an unclosed code fence (opened with $fence_len $fence_name)" >&2
      echo "  a checklist appended there would render as code and be ignored by the merge gate (arbsec/orchestraitor#788): nothing is appended and your body is not modified" >&2
      echo "  fix: close the fence before adding the checklist — add a line of at least $fence_len $fence_name — then rerun this command" >&2
      exit "$ORC_ERR_CONFIG"
    fi
    printf '%s\n\n' "$body"
  fi
  printf '## Merge checklist\n\n'
  printf '%s\n\n' '<!-- Added by pr-create/pr-mutate: this body carried no managed checklist marker. Checkboxes are verified facts — tick each with evidence (arbsec/orchestraitor#788). -->'
  orc_lib_checklist_markers "$root"
}

# --- Config loader -------------------------------------------------------------
# Loads .agents/project/github-project.local.toml (preferred) or falls back to the
# committed example ONLY for field/option NAME discovery (node IDs are never taken
# from config — they are resolved at runtime via GraphQL).
# Exits 2 with an actionable message if neither exists.
orc_lib_load_project_config() {
  local repo_root
  repo_root="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "error: not inside a git repository; cannot locate .agents/project/" >&2
    exit "$ORC_ERR_CONFIG"
  }
  local cfg_local="$repo_root/.agents/project/github-project.local.toml"
  local cfg_example="$repo_root/.agents/project/github-project.example.toml"
  # Mutating scripts require the local config (never fall back to the example).
  if [ "$OPT_DRY_RUN" = false ]; then
    if [ ! -f "$cfg_local" ]; then
      echo "error: local project config not found at .agents/project/github-project.local.toml" >&2
      echo "       copy github-project.example.toml -> github-project.local.toml and fill in real values." >&2
      echo "       (the example is for documentation only; mutating scripts require the local copy)" >&2
      exit "$ORC_ERR_CONFIG"
    fi
    ORC_PROJECT_CONFIG="$cfg_local"
  elif [ -f "$cfg_local" ]; then
    ORC_PROJECT_CONFIG="$cfg_local"
  elif [ -f "$cfg_example" ]; then
    ORC_PROJECT_CONFIG="$cfg_example"
  else
    echo "error: no project config found at .agents/project/github-project.local.toml" >&2
    exit "$ORC_ERR_CONFIG"
  fi
  export ORC_PROJECT_CONFIG
}

# --- gh wrapper: fail fast with context ----------------------------------------
# Usage: orc_lib_gh <args...>
orc_lib_gh() {
  command "${GH_BIN:-gh}" "$@"
}

# --- Service-identity gh routing (AGENTS.md; .agents/project/orchestraitor-workflow.md) ---
# Agent-driven GitHub operations MUST authenticate as the arbsec-agent GitHub
# App service identity, never a personal account. Mutating call sites (the
# `orc_lib_run_or_dry_run` execution path, plus per-script precondition reads
# and the create-blocker cross-repo edge write) route through
# `orc_lib_gh_service`, which runs `orc github gh-env --` so the child's
# GH_TOKEN is a freshly minted installation token (the token is never printed
# by orc; it exists only in the child's environment). Read-only helpers
# (`orc_lib_gh`) stay on the ambient `gh` auth.
#
# Identity resolution: `gh api user` is a USER-context endpoint and fails with
# an installation token (401), silently yielding an empty login that breaks
# assignee-ownership checks. The route is chosen with the SAME probe
# `orc_lib_gh_service` uses (`orc_lib_has_github_app_config`): when the
# github_app config resolves (service route), the caller's identity is the App
# bot user, resolved from `orc github commit-author` (the canonical
# `name=<slug>[bot]` line); the `gh api user` probe is used ONLY when the
# AMBIENT route is selected (config absent — the same state that sends the
# mutations to the labelled personal fallback) — never as a fallback for a
# failing service-route resolution: with complete App config the operations run
# as the App installation, so mixing in the personal login would create a
# principal mismatch (claim/release ownership checks evaluated against the
# wrong identity). A probe resolution error or a failing service-route
# resolution yields an EMPTY result (fail closed), which the calling scripts
# treat as a typed config failure.
#
# Usage: orc_lib_resolve_my_login -> prints the caller's login (bot or human).
#   - config absent (ambient route)   -> login from `gh api user`.
#   - config resolves (service route) -> `<slug>[bot]` from commit-author;
#     commit-author failure           -> EMPTY output (callers fail closed).
#   - config probe resolution error   -> EMPTY output (callers fail closed).
#   - callers must treat an empty login as a typed failure, never a wildcard
#     match.
orc_lib_resolve_my_login() {
  local login="" probe_status=0
  orc_lib_has_github_app_config || probe_status=$?
  if [ "$probe_status" -eq 1 ]; then
    # Ambient route — the same route orc_lib_gh_service takes for absent
    # config: `gh api user` is user-context and works with the ambient
    # personal auth; the service identity plays no role here.
    login="$(orc_lib_gh api user --jq '.login' 2>/dev/null || true)"
    printf '%s' "$login"
    return 0
  fi
  [ "$probe_status" -eq 0 ] || return 0 # probe error -> EMPTY (fail closed)
  # Service route: the principal is the App bot user, full stop.
  login="$("${ORC_BIN:-orc}" github commit-author 2>/dev/null | sed -n 's/^name=//p' || true)"
  printf '%s' "$login"
}
#
# Enforcement mode comes from the layered config key
# `github_app.enforcement`:
#   - `recommended` (default): config absent/partial -> labelled
#     personal-auth fallback, loud WARNING.
#   - `required`: config absent/partial -> typed failure, no fallback, no
#     personal auth. In this mode the wrapper also verifies the repo git
#     identity matches the service-identity bot pattern before delegating
#     (agent `git commit` paths must not produce personal-identity commits).
# Wrapper-only deployments may pin the mode via $ORC_GITHUB_APP_ENFORCEMENT
# (takes precedence over the layered config; when set to `required`, even a
# missing orc binary fails closed instead of falling back).
#
# Usage: orc_lib_gh_service <args...>
#   - complete github_app config -> gh runs as the App installation.
#   - recommended + absent/partial -> labelled personal-auth fallback, loud warning.
#   - required + absent/partial    -> typed config failure (exit 2).
#   - config resolution error      -> typed failure; never silently personal.
# The labeling-chained pr-create route does NOT go through here: in required
# enforcement pr-create calls `orc github gh-env-pr-create-chain` directly
# (orc executes the labeling obligation itself); every OTHER mutating call
# rides this wrapper.
orc_lib_gh_service() {
  # `|| probe_status=$?` keeps the capture safe under `set -e` regardless of
  # the caller's context, and is always the status of THIS probe call.
  local probe_status=0
  orc_lib_has_github_app_config || probe_status=$?
  if [ "$probe_status" -eq 0 ]; then
    command "${ORC_BIN:-orc}" github gh-env -- "${GH_BIN:-gh}" "$@"
  elif [ "$probe_status" -eq 2 ]; then
    echo "error: github_app configuration is present but could not be resolved;" >&2
    echo "       refusing to fall back to personal auth for a mutating GitHub call." >&2
    return "$ORC_ERR_CONFIG"
  elif orc_lib_enforcement_required; then
    echo "error: service-identity enforcement is \`required\` (github_app.enforcement);" >&2
    echo "       refusing to fall back to personal auth for a mutating GitHub call." >&2
    echo "       resolve the github_app config (client_id, installation_id, private_key_uri)" >&2
    echo "       or set github_app.enforcement = \"recommended\"; see docs/cli/orc-github.md" >&2
    return "$ORC_ERR_CONFIG"
  elif [ "$(orc_lib_enforcement_probe_status)" -eq 2 ]; then
    # The enforcement read itself failed (orc available, config get errored):
    # fail closed — a broken read must never widen into the personal fallback.
    return "$ORC_ERR_CONFIG"
  else
    echo "WARNING: service-identity fallback — github_app config is not set;" >&2
    echo "         running gh as the PERSONAL account (policy: labelled fallback only)." >&2
    command "${GH_BIN:-gh}" "$@"
  fi
}

# Reads the effective `github_app.enforcement` for this deployment. Exit
# codes: 0 = `required` (fail closed), 1 = `recommended`/unset/unknown,
# 2 = the `orc config get` probe itself failed (orc crash, unreadable
# layered config) — a state the caller must fail closed on, never read as
# "recommended". The status of the most recent probe is also available via
# `orc_lib_enforcement_probe_status` so callers can distinguish the three.
# Precedence: $ORC_GITHUB_APP_ENFORCEMENT (wrapper-only deployments where the
# declaration must survive a missing orc binary) > `orc config get
# github_app.enforcement` > the pinned TOML. When orc is UNAVAILABLE, the
# probe falls back to grepping the repo's orchestraitor.toml directly for the
# `github_app.enforcement` declaration (resolved from $ORC_REPO_TOML or the
# working tree root `$(git rev-parse --show-toplevel)/orchestraitor.toml`): a repo that has pinned `required` must stay
# fail closed even without the tooling — a labelled personal fallback must
# never depend on tool availability. A missing pin line is "recommended"
# (exit 1); the file is readable but declares an unrecognized value, or the
# file exists yet cannot be read, is ambiguous (exit 2, fail closed).
# An absent/no-repo context is "recommended" (exit 1): the wrapper-wide
# default still applies and $ORC_GITHUB_APP_ENFORCEMENT remains the
# deployment-independent escape hatch.
#
# A FAILED `orc config get` (non-zero exit) is NOT the same as an UNSET key
# (exit 0, empty output): unset keeps the recommended fallback, a read
# failure fails closed — in required mode a broken read must never widen
# into the personal fallback.
ORC_LIB_ENFORCEMENT_PROBE_STATUS=0
orc_lib_enforcement_probe_status() {
  printf '%s' "$ORC_LIB_ENFORCEMENT_PROBE_STATUS"
}

orc_lib_enforcement_required() {
  if [ -n "${ORC_GITHUB_APP_ENFORCEMENT:-}" ]; then
    # The pin is authoritative: `required` fails closed, `recommended` keeps
    # the labelled fallback, and ANY other value is an operator error — the
    # pin exists to fail closed, so an unrecognized value must never widen
    # into the personal fallback (that would be the fail-open outcome the
    # pin prevents).
    case "$ORC_GITHUB_APP_ENFORCEMENT" in
      required)
        ORC_LIB_ENFORCEMENT_PROBE_STATUS=0
        return 0
        ;;
      recommended)
        ORC_LIB_ENFORCEMENT_PROBE_STATUS=0
        return 1
        ;;
      *)
        ORC_LIB_ENFORCEMENT_PROBE_STATUS=2
        echo "error: invalid ORC_GITHUB_APP_ENFORCEMENT value \`$ORC_GITHUB_APP_ENFORCEMENT\`" >&2
        echo "       (expected \`recommended\` or \`required\`); refusing to fall back to" >&2
        echo "       personal auth under an ambiguous enforcement declaration." >&2
        return 2
        ;;
    esac
  fi
  ORC_LIB_ENFORCEMENT_PROBE_STATUS=0
  if ! command -v "${ORC_BIN:-orc}" >/dev/null 2>&1; then
    # orc is unavailable: read the pinned declaration straight from the repo
    # TOML (path documented in the block comment above). Fail closed when the
    # pin says `required`; the un-pinned/absent default stays "recommended".
    local toml pin
    if [ -n "${ORC_REPO_TOML:-}" ]; then
      toml="$ORC_REPO_TOML"
    else
      # The pin is a checked-in repo file: resolve it against the WORKING TREE
      # (not the git dir — a linked worktree's git dir is under the main
      # repo's .git/worktrees/ and does not contain the TOML). Outside a git
      # repository the pin is UNREADABLE, not absent — fail closed (a
      # treeless invocation must not widen into the personal fallback; an
      # operator can always point ORC_REPO_TOML at the declaration).
      if ! toml="$(git rev-parse --path-format=absolute --show-toplevel 2>/dev/null)"; then
        ORC_LIB_ENFORCEMENT_PROBE_STATUS=2
        echo "error: orc is unavailable and the repo working tree (the pinned" >&2
        echo "       github_app.enforcement in orchestraitor.toml) cannot be resolved;" >&2
        echo "       refusing to default to the personal-auth fallback — run from a" >&2
        echo "       checkout or set ORC_GITHUB_APP_ENFORCEMENT / ORC_REPO_TOML." >&2
        return 2
      fi
      toml="$toml/orchestraitor.toml"
    fi
    if [ ! -f "$toml" ]; then
      return 1 # no pinned declaration: wrapper-wide default applies
    fi
    pin="$(awk '
      /^[[:space:]]*\[github_app\][[:space:]]*$/ { inblock = 1; next }
      inblock && /^[[:space:]]*\[/ { inblock = 0 }
      inblock && match($0, /^[[:space:]]*enforcement[[:space:]]*=/) {
        sub(/^[^=]*=[[:space:]]*/, ""); sub(/[[:space:]]+$/, "")
        gsub(/^"|"$/, ""); gsub(/^'"'"'|'"'"'$/, ""); print; exit
      }
    ' "$toml" 2>/dev/null)" || {
      ORC_LIB_ENFORCEMENT_PROBE_STATUS=2
      echo "error: failed to read github_app.enforcement from $toml;" >&2
      echo "       refusing to default to the personal-auth fallback — fix the" >&2
      echo "       file or set ORC_GITHUB_APP_ENFORCEMENT explicitly." >&2
      return 2
    }
    if [ -z "$pin" ]; then
      return 1 # TOML exists but the pin line is absent: recommended default
    fi
    if [ "$pin" = "required" ]; then
      return 0
    fi
    if [ "$pin" != "recommended" ]; then
      ORC_LIB_ENFORCEMENT_PROBE_STATUS=2
      echo "error: invalid github_app.enforcement value \`$pin\` in $toml" >&2
      echo "       (expected \`recommended\` or \`required\`); refusing to fall back to" >&2
      echo "       personal auth under an ambiguous enforcement declaration." >&2
      return 2
    fi
    return 1
  fi
  local mode probe_status=0
  mode="$("${ORC_BIN:-orc}" config get github_app.enforcement 2>/dev/null)" || probe_status=$?
  if [ "$probe_status" -ne 0 ]; then
    # A failed `orc config get` is ambiguous between "key unset" (a legal
    # deployment state: real `orc` exits non-zero for an unset key with no
    # diagnostic) and "read failure" (unreadable layers, provider crash: a
    # diagnostic is printed to stderr). Distinguish by the diagnostic: a
    # silent non-zero exit is the documented unset shape; anything that
    # printed an error is a read failure -> fail closed.
    if [ -n "$("${ORC_BIN:-orc}" config get github_app.enforcement 2>&1 >/dev/null)" ]; then
      ORC_LIB_ENFORCEMENT_PROBE_STATUS=2
      echo "error: failed to read github_app.enforcement from the layered config;" >&2
      echo "       refusing to default to the personal-auth fallback — fix the" >&2
      echo "       configuration or set ORC_GITHUB_APP_ENFORCEMENT explicitly." >&2
      return 2
    fi
  fi
  [ "$mode" = "required" ]
}

# Detects whether the layered orc config resolves a COMPLETE github_app block
# (all of client_id, installation_id, private_key_uri). Exit codes:
#   0 (yes)    -> the caller may take the service-identity path.
#   1 (absent) -> no github_app key resolves anywhere: the labelled personal
#                 fallback is allowed (config-absent, not an orc failure).
#   2 (error)  -> orc is available but the layered config could not be
#                 resolved: fail closed (never silently fall back to personal
#                 auth on a broken configuration).
orc_lib_has_github_app_config() {
  if ! command -v "${ORC_BIN:-orc}" >/dev/null 2>&1; then
    return 1
  fi
  if ! "${ORC_BIN:-orc}" config validate >/dev/null 2>&1; then
    return 2
  fi
  local key
  for key in client_id installation_id private_key_uri; do
    if ! "${ORC_BIN:-orc}" config get "github_app.${key}" >/dev/null 2>&1; then
      # A partial config is incomplete for minting, but it is a deployment
      # state, not a resolution error: the caller takes the labelled fallback.
      return 1
    fi
  done
  return 0
}

# --- jq wrapper: parse gh --json safely ----------------------------------------
# Usage: orc_lib_jq_filter <gh-json-stdin> <jq-filter>  -> prints filtered result
orc_lib_jq_filter() {
  jq -r "$2" < "$1" 2>/dev/null || {
    echo "error: failed to parse gh JSON output with filter: $2" >&2
    exit "$ORC_ERR_UNRECOVERABLE"
  }
}

# --- Repo resolution (never guess) ---------------------------------------------
# Exits 2 if --repo was not provided AND no unambiguous default exists.
orc_lib_resolve_repo() {
  if [ -n "$OPT_REPO" ]; then echo "$OPT_REPO"; return; fi
  # Try the gh default host/repo detection (requires being inside a repo w/ gh origin).
  local detected
  detected="$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null)" || true
  if [ -n "$detected" ]; then echo "$detected"; return; fi
  echo "error: could not resolve repository. Pass --repo OWNER/REPO explicitly." >&2
  exit "$ORC_ERR_CONFIG"
}

# --- Review-policy provenance (arbsec/orchestraitor#748) -----------------------
# The gate scripts resolve $SCRIPT_DIR from THEIR OWN location, so an agent that
# runs them from a stale checkout silently applies that checkout's review policy
# instead of the trusted base branch's (AGENTS.md: "Review policy comes from the
# trusted base branch, not the PR being reviewed"). The observed damage is a
# confident wrong verdict — e.g. a pre-#707 `pr-checks` classifies a bot-authored
# CodeRabbit SKIPPED as a failing skip and convergence-status reports
# `checks_not_passing` for a MERGEABLE/CLEAN PR. The failure direction seen so far
# is false-blocking, but a stale copy can equally hold an older, more permissive
# rule and produce a false PASS.
#
# Why not a policy VERSION constant: a constant travels WITH the file, so a whole
# stale directory carries a stale constant too; any "minimum" declared next to the
# consumer agrees with itself and proves nothing. The anchor must live outside the
# running tree. The PR's `baseRefOid` is that anchor: GitHub reports the exact
# commit the PR merges into, and the blob at `<baseRefOid>:<path>` is the trusted
# policy. The check is local (`git cat-file`/`rev-parse`/`hash-object`), so it
# costs no network call and is safe to run in tight loops.
#
# Policy changes are deliberately gated by the BASE policy: a branch copy that
# differs from base (older OR newer) is refused, because a branch that edits the
# gate never certifies itself. The refusal names the base-checkout command that
# yields a trustworthy verdict.
#
# Usage (call sites read the PR's baseRefOid first, then guard BEFORE any verdict
# is computed — for `merge-gate`, before any merge command):
#   orc_lib_require_policy_current <json|text> <base-sha> <entry-name> \
#     <entry-args> <file>...
#     <base-sha>    the PR's baseRefOid (40-hex); empty/unknown refuses.
#     <entry-name>  gate script to re-run from the base (e.g. convergence-status).
#     <entry-args>  its arguments, for the printed remediation.
#     <file>...     running policy files to verify (absolute paths, same dir).
orc_lib_require_policy_current() {
  local mode="$1" base_sha="$2" entry="$3" entry_args="$4"
  shift 4
  local file dir root prefix rel running_blob base_blob origin head_sha head_blob
  local entry_rel=""
  for file in "$@"; do
    dir="$(dirname -- "$file")"
    root="$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null)" || root=""
    if [ -z "$root" ]; then
      orc_lib_policy_refuse "$mode" "not-in-checkout" "$dir" "$file" "" "" \
        "" "$base_sha" "" "$entry" "$entry_args"
      return "$ORC_ERR_STALE_POLICY"
    fi
    prefix="$(git -C "$dir" rev-parse --show-prefix 2>/dev/null)" || prefix=""
    rel="${prefix}$(basename -- "$file")"
    [ -n "$entry_rel" ] || entry_rel="${prefix}${entry}"

    if ! [[ "$base_sha" =~ ^[0-9a-f]{40}$ ]]; then
      orc_lib_policy_refuse "$mode" "base-unreadable" "$root" "$file" "$rel" \
        "" "" "$base_sha" "" "$entry_rel" "$entry_args"
      return "$ORC_ERR_STALE_POLICY"
    fi

    running_blob="$(git -C "$root" hash-object -- "$file" 2>/dev/null)" || running_blob=""
    head_sha="$(git -C "$root" rev-parse --verify --quiet HEAD 2>/dev/null)" || head_sha=""
    head_blob="$(git -C "$root" rev-parse --verify --quiet "HEAD:$rel" 2>/dev/null)" || head_blob=""
    if [ -n "$head_sha" ] && [ "$running_blob" = "$head_blob" ]; then
      origin="this checkout's HEAD $head_sha"
    else
      origin="this checkout's working tree (uncommitted content)"
    fi

    if ! git -C "$root" cat-file -e "${base_sha}^{commit}" 2>/dev/null; then
      orc_lib_policy_refuse "$mode" "base-not-present" "$root" "$file" "$rel" \
        "$running_blob" "$origin" "$base_sha" "" "$entry_rel" "$entry_args"
      return "$ORC_ERR_STALE_POLICY"
    fi
    base_blob="$(git -C "$root" rev-parse --verify --quiet "${base_sha}:${rel}" 2>/dev/null)" || base_blob=""
    if [ -z "$base_blob" ]; then
      orc_lib_policy_refuse "$mode" "path-not-in-base" "$root" "$file" "$rel" \
        "$running_blob" "$origin" "$base_sha" "" "$entry_rel" "$entry_args"
      return "$ORC_ERR_STALE_POLICY"
    fi
    if [ "$running_blob" != "$base_blob" ]; then
      orc_lib_policy_refuse "$mode" "blob-differs" "$root" "$file" "$rel" \
        "$running_blob" "$origin" "$base_sha" "$base_blob" "$entry_rel" "$entry_args"
      return "$ORC_ERR_STALE_POLICY"
    fi
  done
}

# Emits the typed stale-review-policy refusal and exits ORC_ERR_STALE_POLICY.
# The human message always goes to stderr; in `json` mode the same facts are
# also written to stdout as an object whose top-level `error` is
# "stale-review-policy" and which carries NO verdict keys (`mergeable`,
# `reasons`, `all_passed`, ...), so no caller can mistake it for a gate result.
#
# Usage: orc_lib_policy_refuse <json|text> <reason> <root> <file> <rel> \
#          <running-blob> <running-origin> <base-sha> <base-blob> \
#          <entry-rel> <entry-args>
orc_lib_policy_refuse() {
  local mode="$1" reason="$2" root="$3" file="$4" rel="$5" \
    running_blob="$6" origin="$7" base_sha="$8" base_blob="$9"
  local entry_rel="${10}" entry_args="${11}"
  local detail remediation="" fetch_cmd="fetch origin"
  case "$reason" in
    blob-differs)
      detail="the running file differs from the trusted base's policy (a stale or branch-modified copy)" ;;
    base-not-present)
      detail="the trusted base commit is not present in this repository (fetch it, then rerun)" ;;
    path-not-in-base)
      detail="the trusted base commit does not track '$rel' (wrong repository or unfetched base)" ;;
    not-in-checkout)
      detail="the running file is not inside a git checkout, so its provenance cannot be established" ;;
    base-unreadable)
      detail="the PR's trusted base commit could not be read from GitHub (gh pr view --json baseRefOid returned no OID)" ;;
    *)
      detail="the running file's provenance could not be verified" ;;
  esac
  if [ -n "$root" ] && [ -n "$entry_rel" ] && [[ "$base_sha" =~ ^[0-9a-f]{40}$ ]]; then
    remediation="tmp=\"\$(mktemp -d)\" && git -C $root worktree add --detach \"\$tmp\" $base_sha && bash \"\$tmp/$entry_rel\" $entry_args"
    if [ "$reason" = "base-not-present" ]; then
      # A `--depth 1` checkout stays shallow across a plain `git fetch`, so the
      # missing base commit is still absent afterwards and the same refusal is
      # printed again — a remediation that does not remediate. `--unshallow` is
      # the fetch that actually brings the base's history down.
      if [ "$(git -C "$root" rev-parse --is-shallow-repository 2>/dev/null)" = "true" ]; then
        fetch_cmd="fetch --unshallow origin"
      fi
      remediation="git -C $root $fetch_cmd && $remediation"
    fi
  fi

  {
    echo "error: stale review policy: $file is not the copy at the PR's trusted base"
    [ -n "$running_blob" ] &&
      echo "  running:      blob $running_blob (from ${origin:-an unknown tree})"
    echo "  trusted base: ${base_blob:+blob $base_blob at }${base_sha:-<unknown>} (the PR's baseRefOid)"
    echo "  reason:       $detail"
    echo "  review policy comes from the trusted base branch, never from the checkout under review (AGENTS.md)"
    echo "  refusing instead of reporting a confident verdict from a stale policy"
    if [ -n "$remediation" ]; then
      echo "  trusted verdict: run this gate from a checkout of that base commit:"
      echo "    $remediation"
    fi
  } >&2

  if [ "$mode" = "json" ]; then
    jq -n \
      --arg reason "$reason" \
      --arg file "$file" \
      --arg running_blob "$running_blob" \
      --arg running_origin "$origin" \
      --arg base_ref_oid "$base_sha" \
      --arg base_blob "$base_blob" \
      --arg repository "$root" \
      --arg remediation "$remediation" \
      '{
        error: "stale-review-policy",
        reason: $reason,
        file: $file,
        running_blob: $running_blob,
        running_origin: $running_origin,
        base_ref_oid: $base_ref_oid,
        base_blob: $base_blob,
        repository: $repository,
        remediation: $remediation
      }'
  fi
  exit "$ORC_ERR_STALE_POLICY"
}

# --- Conflict gate (owner directive 2026-10-07) --------------------------------
# NO review of any kind may be requested or triggered while a PR conflicts
# with its base: a human reviewer request on a DIRTY/CONFLICTING PR consumes
# human attention on an unmergeable diff, and an automated reviewer
# (@coderabbitai review) would review a diff that cannot land. Callers
# (pr-request-review, pr-review-post, pr-comment) invoke this before their
# mutating call and exit ORC_ERR_BLOCKED (typed refusal) when the PR is not
# mergeable.
#
# Usage: orc_lib_require_mergeable <pr-number> <owner/repo>
#   - mergeable=MERGEABLE                -> return 0 (may proceed).
#   - mergeable=CONFLICTING or DIRTY     -> typed refusal to stderr, exit 5.
#   - mergeable=UNKNOWN (recomputing)    -> one bounded retry, then typed
#                                           refusal (fail closed: an UNKNOWN
#                                           state must never pass the gate).
#   - mergeable=UNKNOWN (recomputing)    -> one bounded retry, then typed
#                                           refusal (fail closed: an UNKNOWN
#                                           state must never pass the gate).
#   - PR read failure (gh command fails) -> typed error, exit 1
#     (ORC_ERR_UNRECOVERABLE, fail closed — distinct from a readable PR
#     whose state is unparseable, which takes the blocked path, exit 5).
orc_lib_require_mergeable() {
  local pr="$1" repo="$2" state attempts=0
  while :; do
    # Read the raw JSON then apply the filter locally: a gh that ignores
    # --jq (or a test stub that does not implement it) still yields parseable
    # state instead of a misread. The read is a PRECONDITION to the mutating
    # call, and the enforcement decision for that call happens later inside
    # orc_lib_gh_service — so this read must use the ROUTED gh (service route
    # when the App config resolves), never bypass it: in required mode the
    # ambient gh is exactly the path the gate exists to keep unused, and the
    # read must never be the ambient call that precedes a refused write.
    local raw
    # Pure jq read over gh — NO side effects beyond the read itself, and no
    # routing decision of its own: the enforcement decision for the MUTATING
    # call happens later inside orc_lib_gh_service, which still fails closed
    # (typed config error, gh never invoked) in required mode with missing
    # config. This gate only inspects state; it never authenticates a write.
    # IMPORTANT ordering contract: callers invoke this gate only AFTER their
    # enforcement refusal point would already have fired (the gate sits
    # immediately before the mutating call), so in required+missing-config
    # the typed config refusal wins and this read never runs.
    local probe_status=0
    orc_lib_has_github_app_config || probe_status=$?
    local read_failed=0
    if [ "$probe_status" -eq 2 ]; then
      # Config PRESENT but unresolved (orc available, layered config broken):
      # fail closed here — the ambient route below would authenticate the
      # precondition read with personal credentials the enforcement gate
      # refuses for the mutation.
      echo "error: github_app configuration is present but could not be resolved;" >&2
      echo "       refusing to fall back to personal auth for a mutating GitHub call." >&2
      exit "$ORC_ERR_CONFIG"
    elif [ "$probe_status" -eq 1 ] && [ "$(orc_lib_enforcement_probe_status)" -eq 2 ]; then
      # Invalid enforcement pin (the probe recorded status 2): fail closed
      # BEFORE any read — an unrecognized pin must never widen into the
      # ambient fallback (the fail-open outcome the pin prevents).
      echo "error: invalid github_app.enforcement pin;" >&2
      echo "       refusing the ambient read — set the pin to required or recommended." >&2
      exit "$ORC_ERR_CONFIG"
    elif [ "$probe_status" -eq 1 ] && orc_lib_enforcement_required; then
      # Ambient route is forbidden in required mode: the read must not become
      # the personal-auth call the enforcement gate exists to refuse. Same
      # typed error, same exit class (2) as the mutating-call refusal. (An
      # invalid enforcement pin already failed closed inside the probe with
      # its own typed error and exit 2.)
      echo "error: service-identity enforcement is \`required\` (github_app.enforcement);" >&2
      echo "       refusing to fall back to personal auth for a mutating GitHub call." >&2
      echo "       resolve the github_app config (client_id, installation_id, private_key_uri)" >&2
      echo "       or set github_app.enforcement = \"recommended\"; see docs/cli/orc-github.md" >&2
      exit "$ORC_ERR_CONFIG"
    elif [ "$probe_status" -eq 1 ]; then
      # Config absent, recommended/unset enforcement: labelled ambient read.
      raw="$(orc_lib_gh pr view "$pr" --repo "$repo" --json mergeable,mergeStateStatus 2>/dev/null)" || { raw=""; read_failed=1; }
    else
      # probe_status 0: service route — the read rides the App installation
      # token via gh-env, the same route the subsequent mutating call takes.
      raw="$(command "${ORC_BIN:-orc}" github gh-env -- "${GH_BIN:-gh}" pr view "$pr" --repo "$repo" --json mergeable,mergeStateStatus 2>/dev/null)" || { raw=""; read_failed=1; }
    fi
    if [ "$read_failed" -eq 1 ]; then
      # A FAILED read is not the same as an UNKNOWN mergeable state: GitHub
      # reporting "still computing" is retryable, a read failure is not —
      # retrying cannot make a broken route answer. Fail closed immediately
      # with a distinct typed message.
      echo "error: failed to read PR #$pr mergeable state (the pr view read itself failed): refusing to proceed fail-closed — check auth/route and retry once the read succeeds" >&2
      # main's gh-failure refusal folded in: a gh COMMAND failure is a typed
      # unrecoverable error, distinct from a readable PR whose state is
      # unparseable (which takes the blocked path above).
      exit "$ORC_ERR_BLOCKED"
    fi
    state="$(printf '%s' "$raw" | jq -r '"\(.mergeable // "UNKNOWN")|\(.mergeStateStatus // "UNKNOWN")"' 2>/dev/null)" || state=""
    local mergeable="${state%%|*}" merge_state="${state#*|}"
    [ "$mergeable" = "$state" ] && mergeable="$state" merge_state=""

    case "$mergeable" in
      MERGEABLE)
        if [ "$merge_state" = "DIRTY" ]; then
          echo "error: PR #$pr is unmergeable (mergeStateStatus=DIRTY): resolve conflicts with base first — no review request or review trigger while the PR is unmergeable (owner directive 2026-10-07)" >&2
          exit "$ORC_ERR_BLOCKED"
        fi
        return 0 ;;
      CONFLICTING)
        echo "error: PR #$pr conflicts with its base branch (mergeable=CONFLICTING): resolve conflicts with base first — no review request or review trigger while the PR is unmergeable (owner directive 2026-10-07)" >&2
        exit "$ORC_ERR_BLOCKED" ;;
      DIRTY)
        echo "error: PR #$pr is unmergeable (mergeStateStatus=DIRTY, mergeable=CONFLICTING): resolve conflicts with base first — no review request or review trigger while the PR is unmergeable (owner directive 2026-10-07)" >&2
        exit "$ORC_ERR_BLOCKED" ;;
      UNKNOWN)
        attempts=$((attempts + 1))
        if [ "$attempts" -ge 3 ]; then
          echo "error: PR #$pr mergeable state is UNKNOWN (GitHub still computing): refusing to proceed on an unproven state; retry once GitHub reports MERGEABLE" >&2
          exit "$ORC_ERR_BLOCKED"
        fi
        sleep 5 ;;
      *)
        echo "error: cannot read PR #$pr mergeable state${state:+ (got: $state)}: refusing to proceed fail-closed" >&2
        exit "$ORC_ERR_BLOCKED" ;;
    esac
  done
}

# --- Pre-flight check ----------------------------------------------------------
# Verifies gh is installed and that the gh route that will actually run the
# calls is authenticated. Under service-identity enforcement (github_app
# config resolves, or enforcement is pinned `required`) mutations run with an
# App installation token inside `orc github gh-env --` — the AMBIENT
# `gh auth status` says nothing about that route (an installation token never
# appears there), so the ambient check is skipped rather than failing a valid
# service-only deployment. On the ambient route (no github_app config,
# `recommended` enforcement) the ambient `gh auth` state is the real auth.
orc_lib_require_gh_scope() {
  local required_scope="${1:-}"
  if ! command -v gh >/dev/null 2>&1; then
    echo "error: gh CLI not found. Install from https://cli.github.com/" >&2
    exit "$ORC_ERR_CONFIG"
  fi
  local probe_status=0
  orc_lib_has_github_app_config || probe_status=$?
  case "$probe_status" in
    0) return 0 ;; # service route: the installation token minted by orc gh-env carries the App permissions
    2)
      echo "error: the layered github_app config could not be resolved;" >&2
      echo "       refusing the ambient personal-auth check on a broken configuration." >&2
      exit "$ORC_ERR_CONFIG"
      ;;
    # 1 = config absent: the ambient gh auth state is the real auth.
  esac
  if ! gh auth status >/dev/null 2>&1; then
    echo "error: not authenticated to gh and no github_app service config is set." >&2
    echo "       Either resolve the github_app config (service route) or run 'gh auth login'" >&2
    echo "       for the labelled fallback (only where enforcement is 'recommended')." >&2
    exit "$ORC_ERR_CONFIG"
  fi
  if [ -n "$required_scope" ]; then
    # Check scopes from the OAuth token header (gh sets X-Oauth-Scopes).
    local scopes
    scopes="$(gh auth status 2>&1 | grep -i 'Token scopes' || true)"
    if ! echo "$scopes" | grep -q "$required_scope"; then
      echo "error: missing gh OAuth scope '$required_scope'. Run: gh auth refresh -s $required_scope" >&2
      exit "$ORC_ERR_CONFIG"
    fi
  fi
}

# --- Dry-run-aware execution (no eval) -----------------------------------------
# Scripts build their gh/GraphQL command as a bash ARRAY (named array), then call:
#   orc_lib_run_or_dry_run <array-name> [gh|graphql]
# The array is expanded with `"${array[@]}"` — never eval. In dry-run mode the
# array is printed (one shell-quoted word per element) and written nothing otherwise.
orc_lib_run_or_dry_run() {
  local -n _arr="$1"
  local kind="${2:-gh}"
  if [ "$OPT_DRY_RUN" = true ]; then
    printf '[dry-run] %s ' "$kind" >&2
    printf '%q ' "${_arr[@]}" >&2
    printf '\n' >&2
    return 0
  fi
  case "$kind" in
    gh)        orc_lib_gh_service "${_arr[@]}" ;;
    graphql)   orc_lib_gh_service api graphql "${_arr[@]}" ;;
    *)         printf 'error: unknown run kind %q\n' "$kind" >&2; return "$ORC_ERR_CONFIG" ;;
  esac
}
