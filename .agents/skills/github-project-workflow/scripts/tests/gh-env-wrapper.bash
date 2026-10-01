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
  # SC2030/SC2031: the subshell-local exports are intentional; capture
  # relies on `|| probe_status=$?` below, so the notes are expected here.
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
# The enforcement key is UNSET here: `config get` succeeds with empty output
# (exit 0 = key unset, never a read failure); every other key is an error.
if [ "${3:-}" = github_app.enforcement ]; then exit 0; fi
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

# --- 10. UNSET enforcement key (config get exit 0, empty output): the
#         labelled fallback still applies — unset is not a read failure.
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
# Mirrors real `orc` exit codes: an UNSET key exits 1 with empty output
# ("is not set"); a FAILED read (unreadable layers, provider crash) also
# exits non-zero. The two are distinguished below by the recorded output,
# so this stub records whether it emitted a diagnostic.
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
if [ "${2:-}" = get ]; then
  # github_app.* keys ABSENT: exit 1, no diagnostic (unset, not a failure).
  exit 1
fi
printf '%s\n' "orc:$*" >> "$ORC_LOG"
exit 0
EOF
chmod +x "$WORK/orc"
OUT="$(run_service issue close 20 2>&1)" || true
grep -q 'WARNING: service-identity fallback' <<<"$OUT" || fail "unset enforcement key must keep the labelled fallback: $OUT"
grep -qx 'gh:issue close 20' "$GH_LOG" || fail "unset-enforcement fallback must reach gh: $(cat "$GH_LOG")"
grep -qx 'rc:0' "$GH_LOG" || fail "unset-enforcement fallback must not fail the call"

# --- 11. FAILED enforcement read (config get exits non-zero, config absent):
#         fail closed — a broken read must never permit the personal fallback.
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then
  # The layered config resolves (an absent github_app is a legal state), so
  # the absence probe returns 1 and the decision falls to the enforcement read.
  exit 0
fi
if [ "${2:-}" = get ]; then
  echo "layered configuration provider failed" >&2
  exit 1
fi
printf '%s\n' "orc:$*" >> "$ORC_LOG"
exit 0
EOF
chmod +x "$WORK/orc"
OUT="$(run_service issue close 21 2>&1)" || true
grep -q 'failed to read github_app.enforcement' <<<"$OUT" || fail "failed enforcement read must print the typed config error: $OUT"
grep -qx 'rc:2' "$GH_LOG" || fail "failed enforcement read must exit 2 (config), got: $(cat "$GH_LOG")"
if grep -q '^gh:' "$GH_LOG"; then
  fail "gh ran on the fallback path despite a failed enforcement read: $(cat "$GH_LOG")"
fi
if [ -s "$ORC_LOG" ]; then
  fail "orc gh-env was reached despite a failed enforcement read: $(cat "$ORC_LOG")"
fi

echo "PASS gh-env service wrapper routing"

# --- orc_lib_resolve_my_login ------------------------------------------------

# Case A: service identity available — the login comes from commit-author's
# `name=` line, and `gh api user` is NEVER called (it would 401 under an
# installation token and must not mask the identity).
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = github ] && [ "${2:-}" = commit-author ]; then
  printf 'name=arbsec-agent[bot]\nemail=334074867+arbsec-agent[bot]@users.noreply.github.com\n'
  exit 0
fi
exit 1
EOF
cat > "$WORK/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "gh:$*" >> "$GH_LOG"
exit 0
EOF
chmod +x "$WORK/orc" "$WORK/gh"
LOGIN="$( ( set -euo pipefail; export PATH="$WORK:$PATH"; export ORC_BIN="$WORK/orc" GH_BIN="$WORK/gh" GH_LOG="$GH_LOG"
    # shellcheck source=/dev/null
    . "$LIB"; orc_lib_resolve_my_login ) )"
[ "$LOGIN" = "arbsec-agent[bot]" ] || fail "service identity must resolve to the bot login, got: $LOGIN"
if [ -s "$GH_LOG" ]; then
  fail "gh api user must not be called on the service path: $(cat "$GH_LOG")"
fi

# Case B: orc absent — falls back to `gh api user` (personal path).
: > "$GH_LOG"
mv "$WORK/orc" "$WORK/orc.hidden"
cat > "$WORK/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "gh:$*" >> "$GH_LOG"
# Emulate `gh api user --jq '.login'`: apply the filter with real jq.
if [ "${1:-}" = api ] && [ "${3:-}" = --jq ]; then
  echo '{"login":"somehuman"}' | jq -r "$4"
  exit 0
fi
echo '{"login":"somehuman"}'
exit 0
EOF
chmod +x "$WORK/gh"
LOGIN="$( ( set -euo pipefail; export PATH="$WORK:$PATH"; unset ORC_BIN; export GH_BIN="$WORK/gh" GH_LOG="$GH_LOG"
    # shellcheck source=/dev/null
    . "$LIB"; orc_lib_resolve_my_login ) )"
mv "$WORK/orc.hidden" "$WORK/orc"
[ "$LOGIN" = "somehuman" ] || fail "personal fallback must resolve via gh api user, got: $LOGIN"
grep -q 'gh:api user' "$GH_LOG" || fail "personal fallback must call gh api user: $(cat "$GH_LOG")"

# Case C: orc absent AND gh fails — empty output (callers must treat as a
# typed failure).
mv "$WORK/orc" "$WORK/orc.hidden"
cat > "$WORK/gh" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$WORK/gh"
LOGIN="$( ( set -euo pipefail; export PATH="$WORK:$PATH"; unset ORC_BIN; export GH_BIN="$WORK/gh"
    # shellcheck source=/dev/null
    . "$LIB"; orc_lib_resolve_my_login ) )"
mv "$WORK/orc.hidden" "$WORK/orc"
[ -z "$LOGIN" ] || fail "unresolvable identity must be empty, got: $LOGIN"

# Case D: orc PRESENT (service route selected) but commit-author FAILS — the
# ambient `gh api user` fallback must NOT fire: mixing the personal login
# into a service-route decision is a principal mismatch. The result must be
# EMPTY (callers fail closed), and gh must never be consulted.
: > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = github ] && [ "${2:-}" = commit-author ]; then
  echo "jwt signature rejected" >&2
  exit 1
fi
exit 1
EOF
chmod +x "$WORK/orc"
LOGIN="$( ( set -euo pipefail; export PATH="$WORK:$PATH"; export ORC_BIN="$WORK/orc" GH_BIN="$WORK/gh" GH_LOG="$GH_LOG"
    # shellcheck source=/dev/null
    . "$LIB"; orc_lib_resolve_my_login ) )"
[ -z "$LOGIN" ] || fail "service-route commit-author failure must yield empty (fail closed), got: $LOGIN"
if [ -s "$GH_LOG" ]; then
  fail "ambient gh api user must not fire on a failing service-route resolution: $(cat "$GH_LOG")"
fi

echo "PASS gh-env service wrapper routing (login resolution)"
