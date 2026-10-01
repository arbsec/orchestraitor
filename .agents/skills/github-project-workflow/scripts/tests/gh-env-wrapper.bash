#!/usr/bin/env bash
# Integration test for `orc_lib_gh_service` in _lib.sh (github-project-workflow
# and github-pr-lifecycle skills; arbsec/orchestraitor#460 review findings).
#
# Asserts the FORBIDDEN EFFECT did not happen (spec 50-contracts-data.md
# §21.4): the child `gh` reached by the service wrapper must be the intended
# argv — the literal token `command` must never leak into the gh-env child
# argv, and a resolution-error config must never reach gh at all. Stubs
# `orc` and `gh` echo their argv; no network, no credentials.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB="$HERE/../_lib.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fail() {
  echo "FAIL $1" >&2
  exit 1
}

# Stub binaries record their argv (shell-quoted) into files.
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "orc:$*" >> "$ORC_LOG"
exit 0
EOF
cat > "$WORK/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "gh:$*" >> "$GH_LOG"
exit 0
EOF
cat > "$WORK/rcnote" <<'EOF'
#!/usr/bin/env bash
echo "rc:$1" >> "$GH_LOG"
EOF
chmod +x "$WORK/orc" "$WORK/gh" "$WORK/rcnote"
export ORC_LOG="$WORK/orc.log" GH_LOG="$WORK/gh.log"
: > "$ORC_LOG"; : > "$GH_LOG"

# `orc config validate` must succeed so the probe takes the service path.
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
printf '%s\n' "orc:$*" >> "$ORC_LOG"
exit 0
EOF
chmod +x "$WORK/orc"

run_service() {
  # Run in a fresh shell with errexit (as the skill scripts set it); stderr
  # passes through. Args: all gh arguments.
  ( set -euo pipefail; export PATH="$WORK:$PATH"; export ORC_BIN="$WORK/orc" GH_BIN="$WORK/gh"
    # shellcheck source=/dev/null
    . "$LIB"
    set +e
    orc_lib_gh_service "$@"
    "$WORK/rcnote" "$?"
  )
}

# --- 1. complete github_app config: routes through `orc github gh-env` with
#        the gh binary as the FIRST child argv (never a stray `command` token).
OUT="$(run_service issue edit 42 --repo org/repo --add-assignee @me 2>&1)" || true
grep -qx 'orc:github gh-env -- /.*/gh issue edit 42 --repo org/repo --add-assignee @me' "$ORC_LOG" \
  || fail "service path argv wrong: $(cat "$ORC_LOG")"
if grep -q 'orc:github gh-env -- command' "$ORC_LOG"; then
  fail "literal 'command' token leaked into gh-env child argv (HIGH-1 regression)"
fi
# The stub gh is NOT executed on the service path (orc resolves the child); it
# only runs on the labelled fallback path.
if grep -q '^gh:' "$GH_LOG"; then
  fail "gh ran directly on the service path: $(cat "$GH_LOG")"
fi

# --- 2. resolution error (`config validate` fails): typed failure, gh never
#        reached — no silent personal-auth fallback on a broken config.
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then
  echo "layered configuration provider failed" >&2
  exit 1
fi
printf '%s\n' "orc:$*" >> "$ORC_LOG"
exit 0
EOF
chmod +x "$WORK/orc"
OUT="$(run_service pr merge 7 --squash 2>&1)" || true
if [ "$(grep -c 'rc:2' "$GH_LOG" || true)" -ne 1 ]; then
  fail "resolution error must exit 2 (config), got: $(cat "$GH_LOG")"
fi
if [ -s "$ORC_LOG" ] || grep -q '^gh:' "$GH_LOG"; then
  fail "gh or orc was reached despite config resolution error: orc=[$(cat "$ORC_LOG")] gh=[$(cat "$GH_LOG")]"
fi

# --- 3. config absent: labelled personal fallback still reaches gh, and the
#        warning is present (labelled fallback, never silent).
: > "$ORC_LOG"; : > "$GH_LOG"
rm -f "$WORK/orc" # no orc on PATH at all
OUT="$(run_service issue close 9 2>&1)" || true
grep -q 'WARNING: service-identity fallback' <<<"$OUT" || fail "missing fallback warning"
grep -qx 'gh:issue close 9' "$GH_LOG" || fail "fallback argv wrong: $(cat "$GH_LOG")"
grep -qx 'rc:0' "$GH_LOG" || fail "fallback must not fail the call"

# --- 4. partial config (client_id only): labelled fallback, not a hard gate
#        failure, not the service path.
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
if [ "${3:-}" = github_app.client_id ]; then echo "Iv23..."; exit 0; fi
exit 1
EOF
chmod +x "$WORK/orc"
OUT="$(run_service issue edit 11 --add-blocked-by 12 2>&1)" || true
grep -q 'github_app config is not set' <<<"$OUT" || fail "partial config must take the labelled fallback"
grep -qx 'gh:issue edit 11 --add-blocked-by 12' "$GH_LOG" || fail "partial-config fallback argv wrong: $(cat "$GH_LOG")"

echo "PASS gh-env service wrapper routing"
