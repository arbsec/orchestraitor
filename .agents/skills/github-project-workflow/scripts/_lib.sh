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
EOF
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
# assignee-ownership checks. On the service path the caller's identity is the
# App bot user, so it is resolved from `orc github commit-author` (the
# canonical `name=<slug>[bot]` line). The `gh api user` probe stays ONLY on
# the labelled personal fallback path, where it is the correct identity
# source.
#
# Usage: orc_lib_resolve_my_login -> prints the caller's login (bot or human).
#   - gh-env available (service path) -> `<slug>[bot]` from commit-author.
#   - personal fallback path          -> login from `gh api user`.
#   - both fail                       -> empty output; callers must treat an
#     empty login as a typed failure, never a wildcard match.
orc_lib_resolve_my_login() {
  local login=""
  if command -v "${ORC_BIN:-orc}" >/dev/null 2>&1; then
    login="$("${ORC_BIN:-orc}" github commit-author 2>/dev/null | sed -n 's/^name=//p' || true)"
  fi
  if [ -n "$login" ]; then
    printf '%s' "$login"
    return 0
  fi
  # Personal fallback path only: `gh api user` is user-context and works with
  # the ambient personal auth, never with an installation token.
  login="$(orc_lib_gh api user --jq '.login' 2>/dev/null || true)"
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
# github_app.enforcement`. A missing orc binary is NOT "recommended": when
# the declaration says `required`, failing closed is the only safe reading —
# a labelled personal fallback must never depend on tool availability.
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
    ORC_LIB_ENFORCEMENT_PROBE_STATUS=0
    [ "$ORC_GITHUB_APP_ENFORCEMENT" = "required" ]
    return
  fi
  ORC_LIB_ENFORCEMENT_PROBE_STATUS=0
  command -v "${ORC_BIN:-orc}" >/dev/null 2>&1 || return 1
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

# --- Pre-flight check ----------------------------------------------------------
# Verifies gh is installed + authenticated for the required scope.
orc_lib_require_gh_scope() {
  local required_scope="${1:-}"
  if ! command -v gh >/dev/null 2>&1; then
    echo "error: gh CLI not found. Install from https://cli.github.com/" >&2
    exit "$ORC_ERR_CONFIG"
  fi
  if ! gh auth status >/dev/null 2>&1; then
    echo "error: not authenticated to gh. Run 'gh auth login'." >&2
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
