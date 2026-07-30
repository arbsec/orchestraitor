#!/usr/bin/env bash
# perf-check.sh — Appendix F performance CI gates for Orchestraitor.
#
# Measures and asserts the performance budgets from spec Appendix F
# (docs/spec/spec.md) on release builds. See §13.1 for baseline budgets.
#
# Per spec §21.9: strict wall-clock regression gates belong on pinned or
# dedicated hardware. This script defaults to generous smoke thresholds
# suitable for noisy shared CI runners; set PERF_MODE=strict for spec budgets.
#
# Gates (spec Appendix F):
#   daemon_idle_rss         <= 60 MiB RSS after 60 s idle
#   tui_idle_rss            <= 35 MiB RSS after 60 s idle
#   daemon_startup_warm_p95 <= 100 ms (strict) / 500 ms (smoke)
#   tui_startup_warm_p95    <= 150 ms (strict) / 750 ms (smoke)
#   no_unbounded_channels   no unbounded_channel / unbounded() in source
#   no_unbounded_log_files  no unbounded file appenders; log size < 10 MiB
#
# Usage: perf-check.sh [--json <path>] [--markdown <path>]
# Exit: 0 if all gates PASS or SKIP; non-zero if any FAIL.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="$REPO_ROOT/target/release"
IDLE_SECS="${PERF_IDLE_WAIT_SECONDS:-60}"
PERF_MODE="${PERF_MODE:-smoke}"
HYPERFINE_RUNS="${PERF_HYPERFINE_RUNS:-10}"
HYPERFINE_WARMUP="${PERF_HYPERFINE_WARMUP:-3}"
LOG_CAP_KIB=$((10 * 1024)) # 10 MiB

json_out=""
md_out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --json) json_out="$2"; shift 2 ;;
    --markdown) md_out="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

# Deterministic gates use spec budgets in both modes.
# Wall-clock gates get 5x headroom in smoke mode (§21.9).
if [[ "$PERF_MODE" == "strict" ]]; then
  DAEMON_RSS_KIB=$((60 * 1024)); TUI_RSS_KIB=$((35 * 1024))
  DAEMON_STARTUP_MS=100; TUI_STARTUP_MS=150
else
  DAEMON_RSS_KIB=$((60 * 1024)); TUI_RSS_KIB=$((35 * 1024))
  DAEMON_STARTUP_MS=500; TUI_STARTUP_MS=750
fi

# Results accumulator: "name|status|measured|budget|unit|detail"
results=()
any_fail=0

record() { # name status measured budget unit detail
  results+=("$1|$2|$3|$4|$5|$6")
  [[ "$2" == "FAIL" ]] && any_fail=1
}

# --- RSS measurement ---------------------------------------------------------

read_rss_kib() { # pid -> kib
  local pid="$1"
  if [[ -r "/proc/$pid/status" ]]; then
    awk '/^VmRSS:/ {print int($2)}' "/proc/$pid/status"
  else
    # macOS: ps returns RSS in KiB with -o rss=
    ps -o rss= -p "$pid" | tr -d ' '
  fi
}

measure_idle_rss() { # binary label budget_kib
  local bin="$1" label="$2" budget="$3"
  local socket tmpdir logfile
  tmpdir="$(mktemp -d)"
  socket="$tmpdir/orcd.sock"
  logfile="$tmpdir/stderr.log"

  # Start binary; redirect stderr to check for unbounded log growth.
  if [[ "$label" == "daemon" ]]; then
    "$bin" "$socket" 2>"$logfile" &
  else
    "$bin" 2>"$logfile" &
  fi
  local pid=$!

  sleep "$IDLE_SECS"

  if ! kill -0 "$pid" 2>/dev/null; then
    record "$label idle RSS" "FAIL" "N/A" "$budget" "KiB" "process exited before idle wait"
    rm -rf "$tmpdir"
    return
  fi

  local rss logfile_kib
  rss="$(read_rss_kib "$pid")"
  logfile_kib="$(wc -c < "$logfile" 2>/dev/null || echo 0)"
  logfile_kib=$((logfile_kib / 1024))

  kill -TERM "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  rm -rf "$tmpdir"

  if [[ "$rss" -le "$budget" ]]; then
    record "$label idle RSS" "PASS" "$rss" "$budget" "KiB" ""
  else
    record "$label idle RSS" "FAIL" "$rss" "$budget" "KiB" "exceeds budget by $((rss - budget)) KiB"
  fi

  # no_unbounded_log_files: check stderr log size after idle period.
  if [[ "$logfile_kib" -le "$LOG_CAP_KIB" ]]; then
    record "$label log file size" "PASS" "$logfile_kib" "$LOG_CAP_KIB" "KiB" ""
  else
    record "$label log file size" "FAIL" "$logfile_kib" "$LOG_CAP_KIB" "KiB" "unbounded log growth"
  fi
}

# --- Startup measurement (hyperfine) -----------------------------------------

measure_startup() { # binary label budget_ms
  local bin="$1" label="$2" budget_ms="$3"
  local tmpjson
  tmpjson="$(mktemp)"

  local cmd
  if [[ "$label" == "daemon" ]]; then
    cmd="bash \"$REPO_ROOT/scripts/perf-daemon-cycle.sh\" \"$bin\""
  else
    # TUI: measure time to first frame via --help as startup proxy.
    cmd="\"$bin\" --help"
  fi

  if ! hyperfine --warmup "$HYPERFINE_WARMUP" --runs "$HYPERFINE_RUNS" \
      --export-json "$tmpjson" "$cmd" >&2; then
    record "$label startup warm p95" "FAIL" "N/A" "$budget_ms" "ms" "hyperfine measurement failed"
    rm -f "$tmpjson"
    return
  fi

  # Compute p95 from per-run times (seconds -> milliseconds).
  local p95_ms
  p95_ms="$(jq -r '
    .results[0].times
    | sort
    | if length == 0 then 0
      else .[((length - 1) * 0.95 | floor)] * 1000 | floor end
  ' "$tmpjson")"
  rm -f "$tmpjson"

  if [[ "$p95_ms" -le "$budget_ms" ]]; then
    record "$label startup warm p95" "PASS" "$p95_ms" "$budget_ms" "ms" ""
  else
    record "$label startup warm p95" "FAIL" "$p95_ms" "$budget_ms" "ms" "exceeds budget by $((p95_ms - budget_ms)) ms"
  fi
}

# --- Static checks -----------------------------------------------------------

check_unbounded_channels() {
  local matches
  matches="$(grep -rnE 'unbounded_channel|unbounded\(\)' \
    --include='*.rs' "$REPO_ROOT/crates" 2>/dev/null || true)"
  local count
  count="$(echo "$matches" | grep -c . || echo 0)"

  if [[ "$count" -eq 0 ]]; then
    record "no unbounded channels" "PASS" "0" "0" "matches" ""
  else
    record "no unbounded channels" "FAIL" "$count" "0" "matches" "see source grep"
    echo "$matches" >&2
  fi
}

check_unbounded_log_files() {
  # Static: grep for unbounded file-append patterns (no rotation).
  local patterns='\.append\(true\)|File::options\(\)\.append|fs::OpenOptions.*append'
  local matches
  matches="$(grep -rnE "$patterns" --include='*.rs' "$REPO_ROOT/crates" 2>/dev/null || true)"
  local count
  count="$(echo "$matches" | grep -c . || echo 0)"

  if [[ "$count" -eq 0 ]]; then
    record "no unbounded log files (static)" "PASS" "0" "0" "matches" ""
  else
    record "no unbounded log files (static)" "FAIL" "$count" "0" "matches" "unbounded file append found"
    echo "$matches" >&2
  fi
}

# --- Main --------------------------------------------------------------------

ORCD="$TARGET/orcd"
TUI_BIN="$TARGET/orchestraitor-tui"

echo "## Perf gates ($PERF_MODE mode, $(uname -s) $(uname -m))" >&2
echo "" >&2

# Build release binaries.
echo "Building release binaries..." >&2
cargo build --release --locked -p orchestraitor-daemon -p orchestraitor-cli >&2

# Static checks (deterministic, no runner noise).
check_unbounded_channels
check_unbounded_log_files

# Daemon gates.
if [[ -x "$ORCD" ]]; then
  echo "Measuring daemon idle RSS (${IDLE_SECS}s)..." >&2
  measure_idle_rss "$ORCD" "daemon" "$DAEMON_RSS_KIB"

  echo "Measuring daemon startup (hyperfine)..." >&2
  measure_startup "$ORCD" "daemon" "$DAEMON_STARTUP_MS"
else
  record "daemon idle RSS" "SKIP" "N/A" "$DAEMON_RSS_KIB" "KiB" "orcd binary not found"
  record "daemon startup warm p95" "SKIP" "N/A" "$DAEMON_STARTUP_MS" "ms" "orcd binary not found"
fi

# TUI gates — SKIP until a TUI binary exists (spec §9.2, tracked separately).
if [[ -x "$TUI_BIN" ]]; then
  echo "Measuring TUI idle RSS (${IDLE_SECS}s)..." >&2
  measure_idle_rss "$TUI_BIN" "tui" "$TUI_RSS_KIB"

  echo "Measuring TUI startup (hyperfine)..." >&2
  measure_startup "$TUI_BIN" "tui" "$TUI_STARTUP_MS"
else
  record "tui idle RSS" "SKIP" "N/A" "$TUI_RSS_KIB" "KiB" "TUI binary not yet built (library-only crate)"
  record "tui startup warm p95" "SKIP" "N/A" "$TUI_STARTUP_MS" "ms" "TUI binary not yet built (library-only crate)"
fi

# --- Output ------------------------------------------------------------------

if [[ -n "$json_out" ]]; then
  echo "{" > "$json_out"
  echo "  \"mode\": \"$PERF_MODE\"," >> "$json_out"
  echo "  \"os\": \"$(uname -s)\"," >> "$json_out"
  echo "  \"arch\": \"$(uname -m)\"," >> "$json_out"
  echo "  \"gates\": [" >> "$json_out"
  for i in "${!results[@]}"; do
    IFS='|' read -r name status measured budget unit detail <<< "${results[$i]}"
    [[ $i -gt 0 ]] && echo "," >> "$json_out"
    printf '    {"name":"%s","status":"%s","measured":"%s","budget":"%s","unit":"%s","detail":"%s"}' \
      "$name" "$status" "$measured" "$budget" "$unit" "$detail" >> "$json_out"
  done
  echo "" >> "$json_out"
  echo "  ]" >> "$json_out"
  echo "}" >> "$json_out"
fi

if [[ -n "$md_out" ]]; then
  echo "## Performance Gates — $(uname -s) $(uname -m) ($PERF_MODE mode)" > "$md_out"
  echo "" >> "$md_out"
  echo "| Gate | Status | Measured | Budget | Detail |" >> "$md_out"
  echo "|---|---|---:|---:|---|" >> "$md_out"
  for r in "${results[@]}"; do
    IFS='|' read -r name status measured budget unit detail <<< "$r"
    echo "| $name | $status | $measured $unit | $budget $unit | $detail |" >> "$md_out"
  done
  echo "" >> "$md_out"
  if [[ "$any_fail" -eq 0 ]]; then
    echo "All gates PASS or SKIP." >> "$md_out"
  else
    echo "**One or more gates FAILED.**" >> "$md_out"
  fi
fi

# Console summary.
echo "" >&2
echo "| Gate | Status | Measured | Budget |" >&2
echo "|---|---|---:|---:|" >&2
for r in "${results[@]}"; do
  IFS='|' read -r name status measured budget unit detail <<< "$r"
  echo "| $name | $status | $measured $unit | $budget $unit |" >&2
done

exit "$any_fail"
