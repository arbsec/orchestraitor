#!/usr/bin/env bash
# perf-daemon-cycle.sh — single start-to-socket-ready cycle for `orcd`.
#
# Used by hyperfine (via scripts/perf-check.sh) to measure daemon startup
# time. Starts `orcd` on a throwaway socket, polls until the socket appears,
# then sends SIGTERM and waits for exit. The wall-clock duration of this
# script approximates daemon startup + graceful shutdown; with hyperfine
# warmup runs the startup portion dominates.
#
# Usage: perf-daemon-cycle.sh <orcd-binary-path>

set -euo pipefail

orcd="${1:?usage: perf-daemon-cycle.sh <orcd-path>}"

# mktemp -u gives a unused path (no file created) — safe for a Unix socket.
socket="$(mktemp -u "/tmp/orcd-perf-XXXXXX.sock")"
trap 'rm -f "$socket"' EXIT

"$orcd" "$socket" &
pid=$!

# Poll for socket readiness (up to ~2 s at 5 ms intervals).
for _ in $(seq 1 400); do
  [ -S "$socket" ] && break
  sleep 0.005
done

kill -TERM "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
