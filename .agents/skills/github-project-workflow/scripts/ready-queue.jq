# Eligibility filter for ready-queue (single source; regression-tested by
# scripts/tests/ready-queue-filter.bash, regression: arbsec/orchestraitor#251).
# Input: gh issue list JSON array with fields
#   number, title, url, issueType{name}|null, labels[{name}], blockedBy{nodes[{state}]}
# Output: the eligible subset, sorted by issue number.
map(
  select(
    (
      ((.issueType.name // "") | test("^(Task|Bug)$"))
      or (any(.labels[]?.name; . == "task" or . == "bug"))
    )
    and (([.blockedBy.nodes[]? | select(.state != "CLOSED")] | length) == 0)
    and (any(.labels[]?.name; . == $mvp))
  )
)
| sort_by(.number)
