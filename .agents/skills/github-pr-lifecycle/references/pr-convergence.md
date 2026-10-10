# PR convergence

Convergence is the gate between "review is happening" and "review is done". Getting it wrong ships bugs or security holes. Getting it honestly right is the difference between "an agent that ships when green" and "an agent that ships when it gives up".

## The rule

A PR has converged ONLY when ALL hold, checked against the **current HEAD** (not a prior commit):

1. The review mechanism is the LOCAL CodeRabbit CLI: `bash
   .agents/skills/github-pr-lifecycle/scripts/pr-review-local` (wraps
   `coderabbit review --agent --base <base>`; read-only, never posts to
   GitHub). A local CLI review generation against the current HEAD reports
   **zero actionable findings** — recorded by the session in the PR
   description (command, HEAD SHA, findings=0).
   NEVER post `@coderabbitai review` comments (owner directive 2026-10-09;
   bot comment triggers are no-ops during plan pauses and burn quota — the
   local CLI is the review mechanism).
2. All earlier **blocking** findings (CRITICAL/HIGH/MEDIUM) are fixed or formally resolved with recorded reasoning.
3. LOW findings may be deferred only with an explicit justification comment on the finding.

**Every new commit invalidates earlier convergence.** A review generation run against commit A says nothing about commit A+1. The next generation re-examines the full diff at the new HEAD.

**Exception: verified-commit ref swing.** A force-swing of the branch ref (replaying the same commits as GitHub-signed commits — see [verified-commit-path.md](verified-commit-path.md)) does **not** invalidate convergence by itself, **if** BOTH hold: (a) the resulting tree is byte-identical to the reviewed head — `git rev-parse <old-oid>^{tree}` and `git rev-parse <new-oid>^{tree}` must return the same tree object ID (tree identity, not `git diff` emptiness: `git diff` can run configured `textconv` filters, which may be one-way and lossy, so different trees can display no diff); AND (b) the comparison base and base-to-head diff are unchanged — the replay base is the PR's recorded base (see [verified-commit-path.md](verified-commit-path.md)): the reviewed-base OID and the current-base OID MUST be recorded explicitly (never inferred from a branch name), and the base-to-head diff is evidenced by a content hash recorded verbatim alongside the old/new head tree IDs — `git diff <reviewed-base> <old-oid> | sha256sum` and `git diff <current-base> <new-oid> | sha256sum` — with both hashes equal (diff content, not `git diff` output emptiness: the same textconv caveats as (a) apply, and a bare `git diff` yields no tree or content ID to compare). The reviewer/orchestrator MUST verify all recorded OIDs, tree IDs, and hashes and record them on the PR; an unverified swing, a changed comparison base, a hash mismatch, or any tree difference invalidates convergence as with any push.

## What "noteworthy" means

CRITICAL, HIGH, and MEDIUM findings are noteworthy. The CodeRabbit review mechanism is the LOCAL CLI (`pr-review-local`): its findings are remediated and re-checked locally until a run at the final HEAD reports zero actionable findings. Agent review generations report findings in the fixed shape of [review-message-template.md](review-message-template.md), whose `VERDICT` footer reflects this convergence rule. They MUST be resolved before merge. A finding is "resolved" when:
- the code is fixed AND the reviewer confirms the fix, OR
- the finding is formally accepted with recorded reasoning in the review thread (e.g. "This is a known limitation; tracking in #N; accepted because X").

LOW findings are not blocking but MUST be tracked. A PR with 50 unacknowledged LOW findings has NOT converged — the reviewer must explicitly defer each one.

## Fresh contexts (spec `10-orchestrator.md` §9.33.3)

Each review generation runs in a **fresh agent context** — never the implementer's session. The implementer may not approve their own security-sensitive changes (spec `50-contracts-data.md` §21.1). Fresh context prevents:

- accumulated authority leakage (the reviewer inherits the implementer's tool grants);
- context poisoning from prior review loops;
- confirmation bias from reviewing one's own reasoning.

## The loop

```text
review generation N (fresh context, current HEAD)
  → collect findings (deduplicated — see review-findings.md)
  → fix all CRITICAL/HIGH/MEDIUM in a fresh implementer context
  → push (new commit → HEAD moved → prior convergence invalidated)
  → review generation N+1 (fresh context, new HEAD)
  → ...
  → per commit, between remediation and the next generation:
    local coderabbit review --agent (pr-review-local) feeds findings into
    remediation
  → STOP when a local CLI review generation at the final HEAD reports
    zero actionable findings
    AND all earlier blocking findings are resolved
  → record the evidence in the PR description: the pr-review-local command,
    the final HEAD SHA, findings=0
```

## Limits are safety valves, not convergence

Reaching `max_review_loops` (default 3), a cost budget, or an elapsed-time limit produces a **`blocked`** or **`needs-human`** state (spec `10-orchestrator.md` §9.24, §9.33.4). It NEVER counts as successful convergence. The implementer:

1. Adds a human reviewer (`pr-mutate edit <num> --add-reviewer <human>`)
2. Posts a comment summarizing remaining findings and what was tried
3. Moves to the next task in the queue

The PR stays open, unmerged, in `blocked` state until a human resolves the remaining findings.

## What does NOT count as convergence

- "No findings reported" when no review actually ran (missing generation = not converged). A local `pr-review-local` run that was never recorded (no command + SHA + findings count in the PR description) is unevidenced and does not count.
- "All findings resolved" against a prior HEAD after new commits pushed (stale = not converged).
- Reviewer and implementer agree the PR is "basically fine" without a full generation (opinion ≠ evidence).
- Hitting the loop limit (giving up ≠ converging).
- The implementer approving their own PR (spec `50-contracts-data.md` §21.1 — not valid for security-sensitive changes).
- An admin using `--admin` to bypass a red check (spec `50-contracts-data.md` §21.10 — forbidden).
- A local `pr-review-local` run clean at a PRIOR HEAD, or not recorded in the PR description — evidence must be a local CLI generation with zero actionable findings at the FINAL HEAD (command + SHA + findings=0).

## How `convergence-status` computes the verdict

The script combines:

- `pr-checks` — all required + non-optional checks pass at current HEAD;
- `review-threads` — all actionable threads resolved (`isResolved = true`);
- `reconcile-checklist` — all `<!-- orc:* -->` markers checked based on evidence;
- the review-generation state — a `CLEAN` agent-review record in `.orchestraitor/reviews/` whose full head SHA exactly matches the current PR head, plus the GitHub review decision and draft state. A missing, stale, or non-clean record blocks convergence. The CodeRabbit review evidence is the LOCAL CLI run at the final HEAD — `pr-review-local` (`coderabbit review --agent --base <base>`) with zero actionable findings, recorded in the PR description (command, HEAD SHA, findings=0) — not a GitHub-side bot review. Never post `@coderabbitai review` comments (owner directive 2026-10-09; bot triggers are no-ops during plan pauses and burn quota).

The script exits `0` only when all four are true. Exit `5` (`ORC_ERR_BLOCKED`) means convergence has not been reached — `blocked`/`needs-human`, not mergeable.
