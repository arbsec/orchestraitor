#!/usr/bin/env bash
# Regression test for scripts/ready-queue.jq (arbsec/orchestraitor#251):
# the filter must parse, treat task/bug labels as leaf when issueType is null,
# and ignore resolved (CLOSED) blockedBy edges.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
JQ_PROGRAM="$HERE/../ready-queue.jq"

run_case() {
  local name="$1" fixture="$2" expected="$3"
  local got
  got="$(printf '%s' "$fixture" | jq --arg mvp "MVP" -f "$JQ_PROGRAM" | jq -c 'map(.number)')"
  if [ "$got" != "$expected" ]; then
    echo "FAIL $name: expected $expected, got $got" >&2
    exit 1
  fi
  echo "PASS $name"
}

run_case "empty input parses" '[]' '[]'

run_case "leaf/label/blocker semantics" '[
  {"number":1,"title":"leaf task","issueType":null,"labels":[{"name":"task"},{"name":"MVP"}],"blockedBy":{"nodes":[],"totalCount":0}},
  {"number":2,"title":"open blocker","issueType":null,"labels":[{"name":"bug"},{"name":"MVP"}],"blockedBy":{"nodes":[{"state":"OPEN"}],"totalCount":1}},
  {"number":3,"title":"closed blocker only","issueType":null,"labels":[{"name":"task"},{"name":"MVP"}],"blockedBy":{"nodes":[{"state":"CLOSED"}],"totalCount":1}},
  {"number":4,"title":"epic not leaf","issueType":null,"labels":[{"name":"epic"},{"name":"MVP"}],"blockedBy":{"nodes":[],"totalCount":0}},
  {"number":5,"title":"not MVP","issueType":null,"labels":[{"name":"task"}],"blockedBy":{"nodes":[],"totalCount":0}},
  {"number":6,"title":"native type Task without labels","issueType":{"name":"Task"},"labels":[{"name":"MVP"}],"blockedBy":{"nodes":[],"totalCount":0}}
]' '[1,3,6]'

echo "ready-queue-filter: all cases passed"
