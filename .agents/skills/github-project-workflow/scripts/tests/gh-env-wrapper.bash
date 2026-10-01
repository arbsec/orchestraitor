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

run_service_required_env() {
  # Same as run_service but with the wrapper-level enforcement declaration
  # pinned to `required` and NO orc binary on PATH (case 8) — then with orc
  # present (case 9): exercises $ORC_GITHUB_APP_ENFORCEMENT precedence.
  local orc_state="${1:-absent}"; shift
  # The exports are deliberately subshell-local (each case isolates its env);
  # shellcheck's SC2030/SC2031 notes about that are expected here.
  # shellcheck disable=SC2030,SC2031
  ( set -euo pipefail; export PATH="$WORK:$PATH"; export GH_BIN="$WORK/gh"
    export ORC_GITHUB_APP_ENFORCEMENT=required
    if [ "$orc_state" = present ]; then export ORC_BIN="$WORK/orc"; else unset ORC_BIN; fi
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

# --- 4. partial config (client_id only), recommended mode: labelled
#        fallback, not a hard gate failure, not the service path.
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

# --- 5. partial config, REQUIRED mode: typed config failure (exit 2), no
#        fallback, gh never reached — fail closed, no personal auth.
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
if [ "${3:-}" = github_app.client_id ]; then echo "Iv23..."; exit 0; fi
if [ "${3:-}" = github_app.enforcement ]; then echo "required"; exit 0; fi
exit 1
EOF
chmod +x "$WORK/orc"
OUT="$(run_service issue edit 13 --add-blocked-by 14 2>&1)" || true
grep -q 'enforcement is .required.' <<<"$OUT" || fail "required mode must print the typed refusal: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "required mode must exit 2 (config)"
if [ -s "$ORC_LOG" ] || grep -q '^gh:' "$GH_LOG"; then
  fail "gh or orc gh-env was reached in required mode: orc=[$(cat "$ORC_LOG")] gh=[$(cat "$GH_LOG")]"
fi

# --- 6. config absent, REQUIRED mode: same typed refusal, no fallback.
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
if [ "${3:-}" = github_app.enforcement ]; then echo "required"; exit 0; fi
exit 1
EOF
chmod +x "$WORK/orc"
OUT="$(run_service issue close 15 2>&1)" || true
grep -q 'enforcement is .required.' <<<"$OUT" || fail "required mode (absent config) must print the typed refusal: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "required mode (absent config) must exit 2"
if grep -q '^gh:' "$GH_LOG"; then
  fail "gh ran on the fallback path in required mode: $(cat "$GH_LOG")"
fi

# --- 7. config absent, enforcement unset: the get-probe is inconclusive and
#        the labelled fallback still applies (default = recommended).
: > "$ORC_LOG"; : > "$GH_LOG"
rm -f "$WORK/orc" # no orc on PATH at all
OUT="$(run_service issue close 16 2>&1)" || true
grep -q 'WARNING: service-identity fallback' <<<"$OUT" || fail "unset enforcement must keep the labelled fallback"
grep -qx 'rc:0' "$GH_LOG" || fail "unset-enforcement fallback must not fail the call"

# --- 8. required declared via $ORC_GITHUB_APP_ENFORCEMENT but orc missing:
#        fail closed (typed config failure) — a labelled personal fallback
#        must never depend on tool availability.
: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(run_service_required_env absent issue close 17 2>&1)" || true
grep -q 'enforcement is .required.' <<<"$OUT" || fail "required-declared + missing orc must fail closed: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "required-declared + missing orc must exit 2"
if grep -q '^gh:' "$GH_LOG"; then
  fail "gh ran on the fallback path despite required declaration: $(cat "$GH_LOG")"
fi

# --- 9. required declared via env, orc present, complete config: service
#        path wins over the declaration check (no behavior change).
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
printf '%s\n' "orc:$*" >> "$ORC_LOG"
exit 0
EOF
chmod +x "$WORK/orc"
OUT="$(run_service_required_env present issue edit 18 --add-blocked-by 19 2>&1)" || true
grep -qx 'orc:github gh-env -- /.*/gh issue edit 18 --add-blocked-by 19' "$ORC_LOG" \
  || fail "required-declared + complete config must take the service path: $(cat "$ORC_LOG")"
grep -qx 'rc:0' "$GH_LOG" || fail "service path must not fail the call"

echo "PASS gh-env service wrapper routing"
