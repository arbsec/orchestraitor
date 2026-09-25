# Eligibility filter for ready-queue (single source; regression-tested by
# scripts/tests/ready-queue-filter.bash, regression: arbsec/orchestraitor#251).
# Input: gh issue list JSON array with fields
#   number, title, url, issueType{name}|null, labels[{name}], blockedBy{nodes[{state}],totalCount}
# Output: the eligible subset, sorted by issue number.
map(
  select(
    # Leaf type: the native issue type wins when configured (org types may be
    # restored at any time); labels are the fallback while org-level native
    # types are unset (see [issue_types] in github-project.example.toml).
    # A conflicting label MUST NOT override an authoritative non-leaf native
    # type (review finding on arbsec/orchestraitor#254).
    if .issueType != null
    then ((.issueType.name // "") | test("^(Task|Bug)$"))
    else (any(.labels[]?.name; . == "task" or . == "bug"))
    end
    and (([.blockedBy.nodes[]? | select(.state != "CLOSED")] | length) == 0)
    # Fail closed on truncated blocker pages sticks to the security rules of
    # fail-closed by construction: gh returns a `nodes` window; when
    # `totalCount` does not equal the returned node count, an unseen blocker
    # may still be open, so the issue is NOT eligible.
    and (.blockedBy as $b | ((($b.nodes // []) | length) == ($b.totalCount // 0)))
    and (any(.labels[]?.name; . == $mvp))
  )
)
| sort_by(.number)
