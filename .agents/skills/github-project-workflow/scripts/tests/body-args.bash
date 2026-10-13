#!/usr/bin/env bash
# Unit tests for `orc_lib_body_arg` / `orc_lib_require_body_file` in _lib.sh
# (github-pr-lifecycle and github-project-workflow skills; issue #587).
#
# Body arguments flowing into `gh -f body=...` / `--body ...` must never post a
# literal file path as content: `@<path>` expands (documented convention), a
# bare existing path is a typed error, literal text passes through. Stubs `gh`
# nowhere — no network, no gh calls; the lib function is sourced directly.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d)"
HOME_SECRETS_TO_CLEAN=""
HOME_SECRETS_LIST="$WORK/home_secrets_list"
trap 'SECRETS="$(cat "$HOME_SECRETS_LIST" 2>/dev/null)"; rm -rf "$WORK"; for s in $SECRETS; do rm -f "$s"; done' EXIT

fail() {
  echo "FAIL $1" >&2
  exit 1
}

# Both skills' _lib.sh must implement the same contract; test each copy.
for LIB in "$HERE/../_lib.sh" "$HERE/../../../github-pr-lifecycle/scripts/_lib.sh"; do
  [ -f "$LIB" ] || fail "missing lib: $LIB"
  # Source the lib in a subshell per case via a helper that exports the
  # functions without executing script logic: _lib.sh is sourced, not executed,
  # so sourcing it directly is safe (it defines functions and exit codes only).
  (
    # shellcheck source=../_lib.sh
    source "$LIB"

    printf 'line one\nline two\n' > "$WORK/body.md"
    : > "$WORK/empty.md"

    # (a) @existing-file expands to the file's content.
    OUT="$(orc_lib_body_arg "body" "@$WORK/body.md")"
    [ "$OUT" = "$(cat "$WORK/body.md")" ] || fail "@file expansion ($LIB): got: $OUT"

    # (b) an existing file path WITHOUT @ is the misuse case: typed error, exit 2.
    STATUS=0
    OUT="$(orc_lib_body_arg "body" "$WORK/body.md" 2>"$WORK/err")" || STATUS=$?
    [ "$STATUS" -eq 2 ] || fail "path-shaped value must exit 2 ($LIB), got: $STATUS"
    grep -q "$WORK/body.md" "$WORK/err" || fail "path-shaped error must name the file ($LIB): $(cat "$WORK/err")"
    grep -q "@$WORK/body.md" "$WORK/err" || fail "path-shaped error must suggest @path ($LIB): $(cat "$WORK/err")"

    # (c) literal text passes through verbatim.
    OUT="$(orc_lib_body_arg "body" "plain text body with @ sign inside")"
    [ "$OUT" = "plain text body with @ sign inside" ] || fail "literal passthrough ($LIB): got: $OUT"

    # (d) nonexistent @path errors (typed, exit 2).
    STATUS=0
    OUT="$(orc_lib_body_arg "body" "@$WORK/does-not-exist.md" 2>"$WORK/err")" || STATUS=$?
    [ "$STATUS" -eq 2 ] || fail "nonexistent @file must exit 2 ($LIB), got: $STATUS"
    grep -q "does-not-exist.md" "$WORK/err" || fail "nonexistent @file error must name the file ($LIB): $(cat "$WORK/err")"

    # (e) empty @file errors (a silently empty body would be a silent no-op).
    STATUS=0
    OUT="$(orc_lib_body_arg "body" "@$WORK/empty.md" 2>"$WORK/err")" || STATUS=$?
    [ "$STATUS" -eq 2 ] || fail "empty @file must exit 2 ($LIB), got: $STATUS"

    # (f) a literal value that merely CONTAINS @ is untouched.
    OUT="$(orc_lib_body_arg "body" "see @user for details")"
    [ "$OUT" = "see @user for details" ] || fail "embedded @ must stay literal ($LIB): got: $OUT"

    # (f2) a short literal body that collides with an existing file or
    # directory name in the CURRENT DIRECTORY is rejected by the guard: the
    # bare-path check is cwd-relative by design (issue #587). A caller who
    # genuinely wants text that names an existing path passes it via a
    # temp file with @<path>, or as a heredoc body file. The collision
    # fixtures are created in "$WORK" and the test runs the check with
    # "$WORK" as cwd, so the caller's working directory is never polluted
    # (test-hygiene finding, CodeRabbit PR #599).
    collision_dir="$WORK/cwd-collision"
    mkdir -p "$collision_dir"
    : > "$collision_dir/LGTM"
    mkdir -p "$collision_dir/lgtm-dir"
    STATUS=0
    OUT="$(cd "$collision_dir" && orc_lib_body_arg "body" "LGTM" 2>"$WORK/err")" || STATUS=$?
    [ "$STATUS" -eq 2 ] || fail "short literal colliding with cwd file must exit 2 ($LIB), got: $STATUS"
    grep -q "LGTM" "$WORK/err" || fail "cwd-collision error must name the value ($LIB): $(cat "$WORK/err")"
    STATUS=0
    OUT="$(cd "$collision_dir" && orc_lib_body_arg "body" "lgtm-dir" 2>/dev/null)" || STATUS=$?
    [ "$STATUS" -eq 2 ] || fail "directory-name collision must also exit 2 ($LIB), got: $STATUS"

    # (f3) @file trust boundary: paths outside the workspace/temp trusted
    # roots are rejected before the file is read (agent-supplied args must
    # not be able to exfiltrate arbitrary local files as body content).
    STATUS=0
    OUT="$(orc_lib_body_arg "body" "@/etc/passwd" 2>"$WORK/err")" || STATUS=$?
    [ "$STATUS" -eq 2 ] || fail "untrusted @file must exit 2 ($LIB), got: $STATUS"
    grep -q "trusted roots" "$WORK/err" || fail "untrusted @file error must name the boundary ($LIB): $(cat "$WORK/err")"

    STATUS=0
    OUT="$(orc_lib_body_arg "body" "@/etc/hostname" 2>/dev/null)" || STATUS=$?
    [ "$STATUS" -eq 2 ] || fail "second untrusted @file must exit 2 ($LIB), got: $STATUS"

    # (f4) symlink traversal: a symlink INSIDE a trusted directory pointing
    # OUTSIDE all trusted roots must be refused. The old check canonicalized
    # only the path's DIRECTORY, so the link sat inside /tmp and cat followed
    # it, leaking the target's content (CodeRabbit PR #599). Assert the
    # forbidden effect did NOT happen — the secret must not appear in the
    # expansion — not merely that an error occurred (spec §21.4).
    secret_file="$WORK/secret-outside-trusted.txt"
    printf 'TOP_SECRET_BODY_599\n' > "$secret_file"
    # Unique path under $HOME (mktemp-style): never clobbers a pre-existing
    # file, and it is registered for cleanup the moment it is created, so a
    # failing assertion cannot leave it behind (CodeRabbit PR #599 review).
    HOME_SECRET="$HOME/.body-args-599-secret-$(date +%s)-$$"
    mv "$secret_file" "$HOME_SECRET"
    HOME_SECRETS_TO_CLEAN="$HOME_SECRETS_TO_CLEAN $HOME_SECRET"
    ln -sf "$HOME_SECRET" "$WORK/link-to-secret"
    STATUS=0
    OUT="$(orc_lib_body_arg "body" "@$WORK/link-to-secret" 2>"$WORK/err")" || STATUS=$?
    [ "$STATUS" -eq 2 ] || fail "symlink escaping trusted dir must exit 2 ($LIB), got: $STATUS"
    if printf '%s' "$OUT" | grep -q 'TOP_SECRET_BODY_599'; then
      fail "symlink traversal LEAKED the target content ($LIB) — forbidden effect occurred"
    fi
    grep -q "outside the trusted roots" "$WORK/err" || fail "symlink error must name the boundary ($LIB): $(cat "$WORK/err")"

    # (f5) a legit @file inside a trusted dir that happens to BE a symlink
    # pointing at another file INSIDE a trusted dir still expands.
    printf 'inner content\n' > "$WORK/real-inner.md"
    ln -sf "$WORK/real-inner.md" "$WORK/inner-link.md"
    OUT="$(orc_lib_body_arg "body" "@$WORK/inner-link.md")"
    [ "$OUT" = "inner content" ] || fail "in-root symlink must still expand ($LIB): got: $OUT"

    # (g) require_body_file: missing file, empty file -> typed errors.
    # The helper calls `exit` (script semantics), so run each failing call in
    # a subshell to capture its exit code without killing this test shell.
    STATUS=0
    ( orc_lib_require_body_file "--body-file" "$WORK/missing.md" 2>/dev/null ) || STATUS=$?
    [ "$STATUS" -eq 2 ] || fail "require_body_file missing must exit 2 ($LIB), got: $STATUS"
    STATUS=0
    ( orc_lib_require_body_file "--body-file" "$WORK/empty.md" 2>/dev/null ) || STATUS=$?
    [ "$STATUS" -eq 2 ] || fail "require_body_file empty must exit 2 ($LIB), got: $STATUS"
    STATUS=0
    ( orc_lib_require_body_file "--body-file" "$WORK/body.md" 2>/dev/null ) || STATUS=$?
    [ "$STATUS" -eq 0 ] || fail "require_body_file valid file must pass ($LIB), got: $STATUS"

    # Hand the registered $HOME fixtures back to the parent (this is a
    # subshell — plain variable assignment would be lost on exit).
    printf '%s\n' "$HOME_SECRET" >> "$WORK/home_secrets_list"

    echo "PASS body-args ($(basename "$LIB"))"
  )
done

echo "PASS body-args"
