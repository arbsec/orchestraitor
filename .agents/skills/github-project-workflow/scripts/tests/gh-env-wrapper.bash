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

# The wrapper scripts land non-executable through GitHub's createCommitOnBranch
# App-commit path (FileAddition carries no mode), while the invocation sites
# below exec them directly. Ensure the exec bit before any case runs
# (best-effort: a read-only checkout cannot grant it).
for _wrap in pr-create pr-comment pr-review-post pr-mutate; do
  _p="$HERE/../../../github-pr-lifecycle/scripts/$_wrap"
  [ -f "$_p" ] && chmod +x "$_p" 2>/dev/null || true
done

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
# The conflict-gate's `pr view --json mergeable` precondition read rides the
# same gh-env route as the mutating call; answer it so the gate passes on the
# service path (a stub without JSON loops to the UNKNOWN refusal, exit 5).
case "$*" in
  "pr view "*" --json mergeable") printf '{"mergeable":"MERGEABLE"}\n'; exit 0 ;;
esac
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
if [ "${1:-}" = github ] && [ "${2:-}" = gh-env ] && [ -n "${SKILL_CASE:-}" ]; then
  # Service route for the skill-script cases (15+): emulate orc github
  # gh-env — drop the literal "--" separator, then exec the gh stub with the
  # child argv so it lands in GH_LOG exactly as the gh-env child records it
  # (the gh binary is the FIRST child argv; the gate's `pr view` read rides
  # the same route).
  if [ "${3:-}" = "--" ]; then shift 3; else shift 2; fi
  exec "$GH_BIN" "$@"
fi
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
#        warning is present (labelled fallback, never silent). The sandbox
#        TOML has NO pin: the checked-in orchestraitor.toml pins `required`,
#        and this case asserts the unpinned default, not that repo state.
: > "$ORC_LOG"; : > "$GH_LOG"
rm -f "$WORK/orc" # no orc on PATH at all
printf '[github_app]\n' > "$WORK/no-pin.toml"
OUT="$(ORC_REPO_TOML="$WORK/no-pin.toml" run_service issue close 9 2>&1)" || true
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
#        the labelled fallback still applies (default = recommended). The
#        sandbox TOML has NO pin (see case 3): the checked-in
#        orchestraitor.toml pins `required`, which this case does not test.
: > "$ORC_LOG"; : > "$GH_LOG"
rm -f "$WORK/orc" # no orc on PATH at all
printf '[github_app]\n' > "$WORK/no-pin-7.toml"
OUT="$(ORC_REPO_TOML="$WORK/no-pin-7.toml" run_service issue close 16 2>&1)" || true
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

# --- 13. INVALID $ORC_GITHUB_APP_ENFORCEMENT value: fail closed — the pin
#         exists to fail closed, so an unrecognized value must never widen
#         into the personal fallback (matches orc's gh-env behavior).
: > "$ORC_LOG"; : > "$GH_LOG"
rm -f "$WORK/orc" # no orc on PATH at all
( set -euo pipefail; export PATH="$WORK:$PATH"; export GH_BIN="$WORK/gh"
  # An inherited ORC_BIN would bypass the "no orc on PATH" premise (the probe
  # runs the binary at ORC_BIN directly), so unset it like case 16 does.
  unset ORC_BIN
  export ORC_GITHUB_APP_ENFORCEMENT="Required" # wrong case: operator error
  # shellcheck source=/dev/null
  . "$LIB"
  set +e
  orc_lib_gh_service issue close 23
  "$WORK/rcnote" "$?"
) 2>&1 | while IFS= read -r line; do printf '%s\n' "$line" >> "$WORK/case13.out"; done
grep -q 'invalid ORC_GITHUB_APP_ENFORCEMENT' "$WORK/case13.out" \
  || fail "invalid pin value must print the typed config error: $(cat "$WORK/case13.out")"
grep -qx 'rc:2' "$GH_LOG" || fail "invalid pin value must exit 2 (config), got: $(cat "$GH_LOG")"
if grep -q '^gh:' "$GH_LOG"; then
  fail "gh ran on the fallback path despite an invalid pin: $(cat "$GH_LOG")"
fi

echo "PASS gh-env service wrapper routing"

# --- orc_lib_resolve_my_login ------------------------------------------------

# Case A: service identity available — the login comes from commit-author's
# `name=` line, and `gh api user` is NEVER called (it would 401 under an
# installation token and must not mask the identity).
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
if [ "${1:-}" = config ] && [ "${2:-}" = get ]; then printf 'stub\n'; exit 0; fi
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

# Case B: config ABSENT (ambient route, the same route orc_lib_gh_service
# takes for absent config) — falls back to `gh api user` (personal path).
# orc is present on PATH here: route selection must ride the config probe,
# not the orc binary's existence.
: > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
# Config-absent stub: layers read fine (validate passes), but no github_app
# key resolves anywhere (every `config get github_app.*` misses, exit 1).
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
if [ "${1:-}" = config ] && [ "${2:-}" = get ]; then exit 1; fi
if [ "${1:-}" = github ] && [ "${2:-}" = commit-author ]; then
  # Never reached: the ambient route must not consult the bot identity.
  printf 'name=WRONGROUTE[bot]\n'
  exit 0
fi
exit 1
EOF
chmod +x "$WORK/orc"
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
LOGIN="$( ( set -euo pipefail; export PATH="$WORK:$PATH"; export ORC_BIN="$WORK/orc" GH_BIN="$WORK/gh" GH_LOG="$GH_LOG"
    # shellcheck source=/dev/null
    . "$LIB"; orc_lib_resolve_my_login ) )"
[ "$LOGIN" = "somehuman" ] || fail "config-absent ambient route must resolve via gh api user, got: $LOGIN"
grep -q 'gh:api user' "$GH_LOG" || fail "ambient route must call gh api user: $(cat "$GH_LOG")"
if grep -q 'WRONGROUTE' <<<"$LOGIN"; then
  fail "ambient route must not consult commit-author: $LOGIN"
fi

# Case C: orc absent (config probe reports absent) AND gh fails — empty
# output (callers must treat as a typed failure). PATH is controlled: only
# the stub dir plus the minimal system dirs — an inherited directory with a
# stray `orc` binary must not leak into this case.
mv "$WORK/orc" "$WORK/orc.hidden"
cat > "$WORK/gh" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
chmod +x "$WORK/gh"
LOGIN="$( ( set -euo pipefail; export PATH="$WORK:/usr/bin:/bin"; unset ORC_BIN; export GH_BIN="$WORK/gh"
    # shellcheck source=/dev/null
    . "$LIB"; orc_lib_resolve_my_login ) )"
mv "$WORK/orc.hidden" "$WORK/orc"
[ -z "$LOGIN" ] || fail "unresolvable identity must be empty, got: $LOGIN"

# Case C2: config probe ERRORS (orc present, `config validate` fails) —
# fail closed: EMPTY output, no ambient `gh api user`, no commit-author call.
: > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then
  echo "layered config unreadable" >&2
  exit 1
fi
if [ "${1:-}" = github ] && [ "${2:-}" = commit-author ]; then
  printf 'name=WRONGROUTE[bot]\n'
  exit 0
fi
exit 1
EOF
chmod +x "$WORK/orc"
LOGIN="$( ( set -euo pipefail; export PATH="$WORK:$PATH"; export ORC_BIN="$WORK/orc" GH_BIN="$WORK/gh" GH_LOG="$GH_LOG"
    # shellcheck source=/dev/null
    . "$LIB"; orc_lib_resolve_my_login ) )"
[ -z "$LOGIN" ] || fail "config probe error must yield empty (fail closed), got: $LOGIN"
if [ -s "$GH_LOG" ]; then
  fail "config probe error must not consult gh: $(cat "$GH_LOG")"
fi

# Case D: config RESOLVES (service route selected) but commit-author FAILS —
# the ambient `gh api user` fallback must NOT fire: mixing the personal login
# into a service-route decision is a principal mismatch. The result must be
# EMPTY (callers fail closed), and gh must never be consulted.
: > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
if [ "${1:-}" = config ] && [ "${2:-}" = get ]; then printf 'stub\n'; exit 0; fi
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

# Restore the LOGGING gh stub: Case C left the `exit 1` stub in $WORK/gh, and
# every later case (pr-create/pr-comment/pr-review-post/pr-mutate) expects the
# stub that records its argv and exits 0. (Case C rewrites $WORK/orc twice but
# never rewrites $WORK/gh — the last writer was Case C's `exit 1` stub.)
# The stub also emulates `gh pr checks --json` with a fully-passing set: the
# pr-mutate reviewer-request gate runs pr-checks, and a stub env has no real
# checks to report — the gate must pass so the service-path assertion below
# is exercised (a pending/failing classification would block the mutation
# before it routes through gh-env).
cat > "$WORK/gh" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "gh:$*" >> "$GH_LOG"
if [ "${1:-}" = pr ] && [ "${2:-}" = checks ]; then
  printf '%s\n' '[{"name":"Stub Check","state":"SUCCESS","bucket":"pass","workflow":"Stub","link":"http://stub/0"}]'
  exit 0
fi
# Emulate `gh pr view --json mergeable`: MERGEABLE so the conflict gate
# (orc_lib_require_mergeable) passes in the routing cases below. The read
# rides the gh-env child route (the stub orc execs this gh for `github
# gh-env`), so this branch also serves the gate's routed read.
# pr-mutate's --add-reviewer gate reads the review state via the ambient
# orc_lib_gh (read-only calls stay ambient) with a --jq filter; answer it
# so the gate passes (empty requests/reviews: nothing pending or approved).
if [ "${1:-}" = pr ] && [ "${2:-}" = view ]; then
  case "$*" in
    *reviewRequests*) echo '{"requests":[],"approved":[]}'; exit 0 ;;
    *) echo '{"mergeable":"MERGEABLE"}'; exit 0 ;;
  esac
fi
exit 0
EOF
chmod +x "$WORK/gh"

echo "PASS gh-env service wrapper routing (login resolution)"

# --- pr-create / pr-comment / pr-review-post routing (service-identity
#     coverage for PR creation, comments, and review posts — the paths that
#     produced the personal-attribution PRs #475/#476).
PRCREATE="$HERE/../../../github-pr-lifecycle/scripts/pr-create"
PRCOMMENT="$HERE/../../../github-pr-lifecycle/scripts/pr-comment"
PRREVIEW="$HERE/../../../github-pr-lifecycle/scripts/pr-review-post"

run_script() {
  # Run a skill script in a subshell with stub orc/gh on PATH and the
  # enforcement mode declared via $ORC_GITHUB_APP_ENFORCEMENT (empty =
  # unset). Args are passed through to the script.
  local script="$1" enforcement="${2:-}"; shift 2
  (
    set -euo pipefail
    export PATH="$WORK:$PATH"
    export ORC_BIN="$WORK/orc" GH_BIN="$WORK/gh"
    export GH_LOG="$GH_LOG" ORC_LOG="$ORC_LOG"
    # TOML-pin sandbox for the orc-less fallback cases: an inherited
    # ORC_REPO_TOML wins over the (pinned) checked-in orchestraitor.toml.
    if [ -n "${ORC_REPO_TOML:-}" ]; then export ORC_REPO_TOML; fi
    if [ -n "$enforcement" ]; then export ORC_GITHUB_APP_ENFORCEMENT="$enforcement"; else unset ORC_GITHUB_APP_ENFORCEMENT; fi
    export SKILL_CASE=1
    set +e
    "$script" "$@"
    "$WORK/rcnote" "$?"
  )
}

# Stub orc: `config validate` passes (the layered config resolves), every
# other invocation is recorded. `config get github_app.enforcement` reports
# the mode the case sets via $STUB_ENFORCEMENT (unset key when empty).
STUB_ENFORCEMENT=""
new_orc_stub() {
  STUB_ENFORCEMENT="${1:-}"
  cat > "$WORK/orc" <<EOF
#!/usr/bin/env bash
if [ "\${1:-}" = config ] && [ "\${2:-}" = validate ]; then exit 0; fi
if [ "\${1:-}" = config ] && [ "\${2:-}" = get ] && [ "\${3:-}" = github_app.enforcement ]; then
  if [ -n "$STUB_ENFORCEMENT" ]; then printf '%s\n' "$STUB_ENFORCEMENT"; fi
  exit 0
fi
if [ "\${1:-}" = github ] && [ "\${2:-}" = gh-env ]; then
  # Service route: record the full gh-env argv (the tests assert the exact
  # child contract from $ORC_LOG) and then run the gh stub so the MUTATING
  # call records into GH_LOG and exits 0 — the gate's pr-view precondition
  # read rides the same route, so it also lands in GH_LOG as a gh child.
  # NOTE: no backticks in this comment — this heredoc is UNQUOTED and
  # backticks would be command-substituted at stub-generation time.
  printf '%s\n' "orc:\$*" >> "\$ORC_LOG"
  if [ "\${3:-}" = "--" ]; then shift 3; else shift 2; fi
  # Real orc github gh-env consumes the child gh binary path itself and
  # execs it with the remaining argv; emulate that (the gh binary is the
  # FIRST child argv after the separator) so the gate's routed pr-view
  # read reaches the gh stub with its gh subcommand.
  shift
  exec "\$GH_BIN" "\$@"
fi
printf '%s\n' "orc:\$*" >> "\$ORC_LOG"
exit 0
EOF
  chmod +x "$WORK/orc"
}

# --- 12. enforcement=required + complete config: pr-create routes through
#         `orc github gh-env --` (service path); the stub gh is never invoked
#         directly, so no personal-auth write can happen.
: > "$ORC_LOG"; : > "$GH_LOG"
new_orc_stub required
OUT="$(run_script "$PRCREATE" required -R arbsec/orchestraitor --title "t" --body "b" --draft 2>&1)" || true
grep -qx 'orc:github gh-env -- /.*/gh pr create --repo arbsec/orchestraitor --title t --body b --draft' "$ORC_LOG" \
  || fail "pr-create must take the service path: orc=[$(cat "$ORC_LOG")]"
if [ "$(grep -c '^gh:' "$GH_LOG")" -gt 1 ]; then
  fail "pr-create reached gh more than the one gh-env child call: $(cat "$GH_LOG")"
fi
# The single gh invocation must be the gh-env child (precondition read +
# mutation), never a direct ambient call — checked by the ORC_LOG contract
# above (the gh-env line precedes any gh line).
grep -qx 'rc:0' "$GH_LOG" || fail "pr-create service path must not fail the call: $(cat "$GH_LOG")"

# --- 13. missing config + enforcement=required: pr-create FAILS CLOSED
#         (typed config error, exit 2) and gh is NEVER invoked — the
#         forbidden effect (a personal-attribution PR) does not occur.
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'EOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
if [ "${2:-}" = get ]; then exit 1; fi   # no github_app.* key resolves
exit 1
EOF
chmod +x "$WORK/orc"
OUT="$(run_script "$PRCREATE" required -R arbsec/orchestraitor --title "t" --body "b" 2>&1)" || true
grep -q 'enforcement is .required.' <<<"$OUT" || fail "pr-create missing-config+required must print the typed refusal: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "pr-create missing-config+required must exit 2: $(cat "$GH_LOG")"
if [ -s "$ORC_LOG" ] || grep -q '^gh:' "$GH_LOG"; then
  fail "pr-create reached gh or orc gh-env despite required + missing config: orc=[$(cat "$ORC_LOG")] gh=[$(cat "$GH_LOG")]"
fi

# --- 14. same fail-closed shape for pr-comment and pr-review-post (required
#         mode, config absent): typed refusal, gh never reached.
: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(run_script "$PRCOMMENT" required 42 -R arbsec/orchestraitor --body "hi" 2>&1)" || true
grep -q 'enforcement is .required.' <<<"$OUT" || fail "pr-comment required+missing-config must print the typed refusal: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "pr-comment required+missing-config must exit 2"
if grep -q '^gh:' "$GH_LOG"; then fail "pr-comment ran gh on the fallback path in required mode"; fi

: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(run_script "$PRREVIEW" required 42 -R arbsec/orchestraitor --approve --body "ok" 2>&1)" || true
# --approve is refused outright (bot self-approval policy, case 14b below);
# the fail-closed shape for this verdict is the typed approval refusal.
grep -q 'approvals require an independent authorized reviewer' <<<"$OUT" || fail "pr-review-post --approve must print the typed approval refusal: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "pr-review-post --approve must exit 2"
if grep -q '^gh:' "$GH_LOG"; then fail "pr-review-post ran gh on the fallback path in required mode"; fi

: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(run_script "$PRREVIEW" required 42 -R arbsec/orchestraitor --request-changes --body "issue" 2>&1)" || true
grep -q 'enforcement is .required.' <<<"$OUT" || fail "pr-review-post required+missing-config must print the typed refusal: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "pr-review-post required+missing-config must exit 2"
if grep -q '^gh:' "$GH_LOG"; then fail "pr-review-post ran gh on the fallback path in required mode"; fi

# --- 15. required + complete config: pr-comment and pr-review-post route
#         through gh-env; the stub gh is never invoked directly, and the
#         gh-env child argv carries the intended gh subcommand.
: > "$ORC_LOG"; : > "$GH_LOG"
new_orc_stub required
OUT="$(run_script "$PRCOMMENT" required 42 -R arbsec/orchestraitor --body "hi" 2>&1)" || true
grep -qx 'orc:github gh-env -- /.*/gh pr comment 42 --repo arbsec/orchestraitor --body hi' "$ORC_LOG" \
  || fail "pr-comment must take the service path: orc=[$(cat "$ORC_LOG")]"
# The gate's `pr view` precondition read and the mutating comment BOTH ride
# the gh-env child route, so GH_LOG holds only gh-env-child invocations; a
# gh: line without a preceding gh-env line would be a direct ambient call.
GHE_COUNT="$(grep -c 'orc:github gh-env' "$ORC_LOG")"
GH_COUNT="$(grep -c '^gh:' "$GH_LOG")"
if [ "$GH_COUNT" -ne "$GHE_COUNT" ]; then
  fail "pr-comment ran gh directly on the service path (gh:$GH_COUNT vs gh-env:$GHE_COUNT): $(cat "$GH_LOG")"
fi

: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(run_script "$PRREVIEW" required 42 -R arbsec/orchestraitor --comment --body "finding" 2>&1)" || true
grep -qx 'orc:github gh-env -- /.*/gh pr review 42 --repo arbsec/orchestraitor --comment --body finding' "$ORC_LOG" \
  || fail "pr-review-post must take the service path: orc=[$(cat "$ORC_LOG")]"
GHE_COUNT="$(grep -c 'orc:github gh-env' "$ORC_LOG")"
GH_COUNT="$(grep -c '^gh:' "$GH_LOG")"
if [ "$GH_COUNT" -ne "$GHE_COUNT" ]; then
  fail "pr-review-post ran gh directly on the service path (gh:$GH_COUNT vs gh-env:$GHE_COUNT): $(cat "$GH_LOG")"
fi

# --- 16. config present but enforcement UNSET (the live deployment state
#         until this change pins it): the scripts still route through the
#         service path (config resolves) — coverage does not depend on the
#         pin; the pin only closes the fallback when config is missing.
: > "$ORC_LOG"; : > "$GH_LOG"
new_orc_stub ""
OUT="$(run_script "$PRCREATE" "" -R arbsec/orchestraitor --title "t" --body "b" 2>&1)" || true
grep -qx 'orc:github gh-env -- /.*/gh pr create --repo arbsec/orchestraitor --title t --body b' "$ORC_LOG" \
  || fail "pr-create with resolved config must take the service path regardless of the pin: orc=[$(cat "$ORC_LOG")]"

# --- 17. dry-run: writes nothing, never reaches orc gh-env or gh.
: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(run_script "$PRCREATE" required --dry-run -R arbsec/orchestraitor --title "t" --body "b" 2>&1)" || true
grep -q '\[dry-run\] gh pr create' <<<"$OUT" || fail "pr-create --dry-run must preview the gh command: $OUT"
if [ -s "$ORC_LOG" ] || grep -q '^gh:' "$GH_LOG"; then
  fail "pr-create --dry-run must not reach orc gh-env or gh: orc=[$(cat "$ORC_LOG")] gh=[$(cat "$GH_LOG")]"
fi

# --- 18. TOML pin survives a missing orc binary: `github_app.enforcement =
#         "required"` in the repo orchestraitor.toml (the checked-in pin this
#         PR adds) must fail the wrapper closed (exit 2, gh never invoked)
#         even when orc is UNAVAILABLE — the enforcement decision must never
#         depend on tool availability. Simulated with ORC_REPO_TOML pointing
#         at a pinned TOML (the same awk fallback _lib.sh resolves to
#         "$(git rev-parse --show-toplevel)/orchestraitor.toml").
: > "$ORC_LOG"; : > "$GH_LOG"
rm -f "$WORK/orc" # no orc on PATH at all (case 16 left a stub behind)
printf '[github_app]\nenforcement = "required"\n' > "$WORK/pinned.toml"
OUT="$(ORC_REPO_TOML="$WORK/pinned.toml" run_script "$PRCREATE" "" -R arbsec/orchestraitor --title "t" --body "b" 2>&1)" || true
grep -q 'enforcement is .required.' <<<"$OUT" || fail "TOML-pinned required + missing orc must print the typed refusal: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "TOML-pinned required + missing orc must exit 2: $(cat "$GH_LOG")"
if [ -s "$ORC_LOG" ] || grep -q '^gh:' "$GH_LOG"; then
  fail "TOML-pinned required + missing orc reached gh or orc gh-env: orc=[$(cat "$ORC_LOG")] gh=[$(cat "$GH_LOG")]"
fi

# --- 19. same fail-closed shape with an INCOMPLETE github_app block in the
#         TOML: the pin decides (fail closed), not the missing keys.
: > "$ORC_LOG"; : > "$GH_LOG"
rm -f "$WORK/orc" # no orc on PATH at all
printf '[github_app]\nclient_id = "Iv23..."\nenforcement = "required"\n' > "$WORK/pinned-partial.toml"
OUT="$(ORC_REPO_TOML="$WORK/pinned-partial.toml" run_script "$PRCOMMENT" "" 42 -R arbsec/orchestraitor --body "hi" 2>&1)" || true
grep -q 'enforcement is .required.' <<<"$OUT" || fail "TOML-pinned required (partial block) + missing orc must print the typed refusal: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "TOML-pinned required (partial block) + missing orc must exit 2"
if [ -s "$ORC_LOG" ] || grep -q '^gh:' "$GH_LOG"; then
  fail "TOML-pinned required (partial block) reached gh or orc gh-env: orc=[$(cat "$ORC_LOG")] gh=[$(cat "$GH_LOG")]"
fi

echo "PASS pr-create / pr-comment / pr-review-post service-identity routing"

# --- Argument validation & flag passthrough (CodeRabbit PR #477 findings) ---
# These cases assert ARG PARSING behavior: a trailing flag with no value must
# produce the typed argument error (exit 2), never a bash nounset crash, and
# --body-file must reach gh as a flag (gh reads the file), never as --body.

# --- 20. trailing flag with no value: typed error, exit 2, no crash.
arg_err_case() {
  # $1 = script, rest = args. Prints the captured output; caller greps it.
  local script="$1"; shift
  (
    set -euo pipefail
    export PATH="$WORK:$PATH"
    export ORC_BIN="$WORK/orc" GH_BIN="$WORK/gh"
    export GH_LOG="$GH_LOG" ORC_LOG="$ORC_LOG"
    unset ORC_GITHUB_APP_ENFORCEMENT
    set +e
    "$script" "$@"
    "$WORK/rcnote" "$?"
  ) 2>&1
}
for bad_args in \
  "--title" \
  "--body"; do
  : > "$ORC_LOG"; : > "$GH_LOG"
  OUT="$(arg_err_case "$PRCREATE" -R arbsec/orchestraitor "$bad_args")" || true
  grep -q "error: $bad_args requires a" <<<"$OUT" || fail "pr-create trailing $bad_args must print the typed argument error: $OUT"
  grep -q 'rc:2' "$GH_LOG" || fail "pr-create trailing $bad_args must exit 2 (config), got: $(cat "$GH_LOG")"
  if grep -qE 'unbound variable|nounset' <<<"$OUT"; then
    fail "pr-create trailing $bad_args crashed with a bash nounset error: $OUT"
  fi
done
for spec in \
  "PRCOMMENT|--body" \
  "PRCOMMENT|--body-file" \
  "PRREVIEW|--body" \
  "PRREVIEW|--body-file"; do
  script_var="${spec%%|*}"; flag="${spec##*|}"
  script="${!script_var}"
  : > "$ORC_LOG"; : > "$GH_LOG"
  OUT="$(arg_err_case "$script" 42 -R arbsec/orchestraitor "$flag")" || true
  grep -q "error: $flag requires a" <<<"$OUT" || fail "${script_var} trailing $flag must print the typed argument error: $OUT"
  grep -q 'rc:2' "$GH_LOG" || fail "${script_var} trailing $flag must exit 2 (config), got: $(cat "$GH_LOG")"
  if grep -qE 'unbound variable|nounset' <<<"$OUT"; then
    fail "${script_var} trailing $flag crashed with a bash nounset error: $OUT"
  fi
done

echo "PASS trailing-flag argument validation (typed exit 2)"

# --- 21. --body-file survives to gh as a file flag (space AND = forms).
#         pr-review-post with --comment: the file path must NEVER appear as
#         the value of --body (that would post the path as the review text).
#         Cases 18-20 removed the orc stub from $WORK: restore the
#         service-path stub (complete config) so these cases exercise the
#         FLAG PARSING, not the enforcement gate.
new_orc_stub required
bodyfile_case() {
  local script="$1"; shift
  (
    set -euo pipefail
    export PATH="$WORK:$PATH"
    export ORC_BIN="$WORK/orc" GH_BIN="$WORK/gh"
    export GH_LOG="$GH_LOG" ORC_LOG="$ORC_LOG"
    unset ORC_GITHUB_APP_ENFORCEMENT
    set +e
    "$script" "$@"
    "$WORK/rcnote" "$?"
  ) 2>&1
}
: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(bodyfile_case "$PRREVIEW" 42 -R arbsec/orchestraitor --comment --body-file "$WORK/review.md")" || true
grep -qx 'orc:github gh-env -- /.*/gh pr review 42 --repo arbsec/orchestraitor --comment --body-file /.*/review.md' "$ORC_LOG" \
  || fail "pr-review-post --body-file <path> must reach gh-env as a flag: orc=[$(cat "$ORC_LOG")]"
if grep -q -- '--body /.*/review.md' "$ORC_LOG"; then
  fail "pr-review-post mangled --body-file into --body (posts the path as text): $(cat "$ORC_LOG")"
fi
grep -qx 'rc:0' "$GH_LOG" || fail "pr-review-post --body-file service path must not fail: $(cat "$GH_LOG")"

: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(bodyfile_case "$PRREVIEW" 42 -R arbsec/orchestraitor --comment --body-file="$WORK/review.md")" || true
grep -qx 'orc:github gh-env -- /.*/gh pr review 42 --repo arbsec/orchestraitor --comment --body-file /.*/review.md' "$ORC_LOG" \
  || fail "pr-review-post --body-file=<path> must reach gh-env as a flag: orc=[$(cat "$ORC_LOG")]"
grep -qx 'rc:0' "$GH_LOG" || fail "pr-review-post --body-file= service path must not fail: $(cat "$GH_LOG")"

: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(bodyfile_case "$PRCOMMENT" 42 -R arbsec/orchestraitor --body-file "$WORK/comment.md")" || true
grep -qx 'orc:github gh-env -- /.*/gh pr comment 42 --repo arbsec/orchestraitor --body-file /.*/comment.md' "$ORC_LOG" \
  || fail "pr-comment --body-file <path> must reach gh-env as a flag: orc=[$(cat "$ORC_LOG")]"
grep -qx 'rc:0' "$GH_LOG" || fail "pr-comment --body-file service path must not fail: $(cat "$GH_LOG")"

echo "PASS --body-file passthrough (space and = forms)"

# --- 22. bot self-approval refused: --approve never reaches gh, in ANY
#         enforcement mode — approvals require an independent reviewer.
: > "$ORC_LOG"; : > "$GH_LOG"
new_orc_stub required
OUT="$(bodyfile_case "$PRREVIEW" 42 -R arbsec/orchestraitor --approve --body "lgtm")" || true
grep -q 'approvals require an independent authorized reviewer' <<<"$OUT" || fail "pr-review-post --approve must print the typed refusal: $OUT"
if [ -s "$ORC_LOG" ] || grep -q '^gh:' "$GH_LOG"; then
  fail "pr-review-post --approve must be refused before any gh invocation: orc=[$(cat "$ORC_LOG")] gh=[$(cat "$GH_LOG")]"
fi
grep -q 'rc:2' "$GH_LOG" || fail "pr-review-post --approve must exit 2 (config): $(cat "$GH_LOG")"
# request-changes and comment remain supported (no regression).
: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(bodyfile_case "$PRREVIEW" 42 -R arbsec/orchestraitor --request-changes --body "issues found")" || true
grep -qx 'orc:github gh-env -- /.*/gh pr review 42 --repo arbsec/orchestraitor --request-changes --body issues found' "$ORC_LOG" \
  || fail "pr-review-post --request-changes must still be supported: orc=[$(cat "$ORC_LOG")]"
grep -qx 'rc:0' "$GH_LOG" || fail "pr-review-post --request-changes must not fail: $(cat "$GH_LOG")"

# --- 23. --request-changes still requires a body (text OR file); --comment
#         may be empty (gh posts a bodyless comment review).
: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(bodyfile_case "$PRREVIEW" 42 -R arbsec/orchestraitor --request-changes)" || true
grep -q 'is required with --request-changes' <<<"$OUT" || fail "pr-review-post --request-changes without a body must print the typed error: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "pr-review-post --request-changes without a body must exit 2"
: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(bodyfile_case "$PRREVIEW" 42 -R arbsec/orchestraitor --comment)" || true
grep -qx 'orc:github gh-env -- /.*/gh pr review 42 --repo arbsec/orchestraitor --comment' "$ORC_LOG" \
  || fail "pr-review-post --comment with no body must still post: orc=[$(cat "$ORC_LOG")]"
grep -qx 'rc:0' "$GH_LOG" || fail "pr-review-post bodyless --comment must not fail: $(cat "$GH_LOG")"

echo "PASS bot self-approval refusal + verdict body requirements"


# --- pr-mutate service-identity routing (gh-capabilities review finding: the
#     documented `gh pr edit|ready|close` writes must go through the wrapper
#     like pr-create/pr-comment, never ambient personal auth) ---
PRMUTATE="$HERE/../../../github-pr-lifecycle/scripts/pr-mutate"

# --- 24. service path: pr-mutate edit routes through gh-env with the
#         intended gh subcommand. The reviewer-request gate also performs an
#         AMBIENT READ (`gh pr view` via orc_lib_gh — read-only calls are
#         allowed on ambient auth; only MUTATIONS must ride the service
#         path), so the stub gh legitimately logs a `gh:pr view` line here:
#         assert no MUTATING gh call reached the stub directly.
: > "$ORC_LOG"; : > "$GH_LOG"
new_orc_stub required
OUT="$(run_script "$PRMUTATE" required edit 42 -R arbsec/orchestraitor --add-reviewer human1 2>&1)" || true
grep -qx 'orc:github gh-env -- /.*/gh pr edit 42 --repo arbsec/orchestraitor --add-reviewer human1' "$ORC_LOG" \
  || fail "pr-mutate edit must take the service path: orc=[$(cat "$ORC_LOG")] out=[$OUT]"
# The gh stub logs BOTH the ambient gate reads (pr view/pr checks) AND the
# gh-env CHILD execution of the mutating call itself — so a bare `gh:pr edit`
# line is expected (it is the routed child). What is FORBIDDEN is a direct
# ambient mutating call: every mutating gh line must have a matching
# `orc:github gh-env` line in ORC_LOG.
CHILD_ARGV='gh:pr edit 42 --repo arbsec/orchestraitor --add-reviewer human1'
BAD="$( { grep -E '^gh:pr (edit|ready|close|merge|comment|review)' "$GH_LOG" || true; } | { grep -vx "$CHILD_ARGV" || true; })"
if [ -n "$BAD" ]; then
  fail "pr-mutate edit ran a MUTATING gh call directly (personal auth): $BAD"
fi
grep -qx 'rc:0' "$GH_LOG" || fail "pr-mutate edit service path must not fail: $(cat "$GH_LOG")"

# --- 25. ready and close route the same way.
: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(run_script "$PRMUTATE" required ready 42 -R arbsec/orchestraitor 2>&1)" || true
grep -qx 'orc:github gh-env -- /.*/gh pr ready 42 --repo arbsec/orchestraitor' "$ORC_LOG" \
  || fail "pr-mutate ready must take the service path: orc=[$(cat "$ORC_LOG")]"
grep -qx 'rc:0' "$GH_LOG" || fail "pr-mutate ready must not fail: $(cat "$GH_LOG")"

: > "$ORC_LOG"; : > "$GH_LOG"
OUT="$(run_script "$PRMUTATE" required close 42 -R arbsec/orchestraitor --comment "closing" 2>&1)" || true
grep -qx 'orc:github gh-env -- /.*/gh pr close 42 --repo arbsec/orchestraitor --comment closing' "$ORC_LOG" \
  || fail "pr-mutate close must pass extra flags through: orc=[$(cat "$ORC_LOG")]"
grep -qx 'rc:0' "$GH_LOG" || fail "pr-mutate close must not fail: $(cat "$GH_LOG")"

# --- 26. missing config + enforcement=required: pr-mutate FAILS CLOSED
#         (typed refusal, exit 2); gh is never reached — the forbidden
#         effect (a personal-attribution PR write) does not occur.
: > "$ORC_LOG"; : > "$GH_LOG"
cat > "$WORK/orc" <<'ORCEOF'
#!/usr/bin/env bash
if [ "${1:-}" = config ] && [ "${2:-}" = validate ]; then exit 0; fi
if [ "${2:-}" = get ]; then exit 1; fi   # no github_app.* key resolves
exit 1
ORCEOF
chmod +x "$WORK/orc"
OUT="$(run_script "$PRMUTATE" required ready 42 -R arbsec/orchestraitor 2>&1)" || true
grep -q 'enforcement is .required.' <<<"$OUT" || fail "pr-mutate missing-config+required must print the typed refusal: $OUT"
grep -q 'rc:2' "$GH_LOG" || fail "pr-mutate missing-config+required must exit 2: $(cat "$GH_LOG")"
if [ -s "$ORC_LOG" ] || grep -q '^gh:' "$GH_LOG"; then
  fail "pr-mutate reached gh or orc gh-env despite required + missing config: orc=[$(cat "$ORC_LOG")] gh=[$(cat "$GH_LOG")]"
fi

# --- 27. dry-run: previews the gh command, writes nothing. The
#         reviewer-request gate performs its ambient READS even in dry-run
#         mode (they only classify; they write nothing), so assert no gh-env
#         route and no MUTATING direct gh call — not zero network activity.
: > "$ORC_LOG"; : > "$GH_LOG"
new_orc_stub required
OUT="$(run_script "$PRMUTATE" required --dry-run edit 42 -R arbsec/orchestraitor --add-reviewer human1 2>&1)" || true
grep -q '\[dry-run\] gh pr edit 42 --repo arbsec/orchestraitor --add-reviewer human1' <<<"$OUT" \
  || fail "pr-mutate --dry-run must preview the gh command: $OUT"
if grep -q 'orc:github gh-env' "$ORC_LOG"; then
  fail "pr-mutate --dry-run must not route through orc gh-env: $(cat "$ORC_LOG")"
fi
if grep -qE '^gh:pr (edit|ready|close|merge|comment|review)' "$GH_LOG"; then
  fail "pr-mutate --dry-run ran a MUTATING gh call: $(cat "$GH_LOG")"
fi

# --- 28. argument validation: unknown subcommand, missing PR number,
#         non-numeric PR, and edit-without-flags are typed errors (exit 2),
#         never a bash nounset crash, and never a gh invocation.
for bad_case in \
  "ready" \
  "ready abc" \
  "edit 42" \
  "mutate 42"; do
  : > "$ORC_LOG"; : > "$GH_LOG"
  # shellcheck disable=SC2086
  OUT="$(arg_err_case "$PRMUTATE" -R arbsec/orchestraitor $bad_case)" || true
  grep -q 'error:' <<<"$OUT" || fail "pr-mutate $bad_case must print a typed error: $OUT"
  grep -q 'rc:2' "$GH_LOG" || fail "pr-mutate $bad_case must exit 2: $(cat "$GH_LOG")"
  if grep -qE 'unbound variable|nounset' <<<"$OUT"; then
    fail "pr-mutate $bad_case crashed with a bash nounset error: $OUT"
  fi
  if grep -q 'orc:github gh-env' "$ORC_LOG"; then
    fail "pr-mutate $bad_case must be refused BEFORE any gh-env invocation: $(cat "$ORC_LOG")"
  fi
done

echo "PASS pr-mutate service-identity routing + argument validation"
