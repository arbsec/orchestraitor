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

**Pre-PR generations count (no doubling).** The pre-PR local review gate
in `pr-create` requires a CLEAN local review generation at the exact
(head, base) pair being proposed — that same generation IS the PR's
"first required generation": the pre-create run satisfies rule item 1's
clean-at-HEAD requirement and no additional generation is needed before
the first review loop, as long as neither anchor has moved. The usual
invalidation rule applies unchanged: any post-PR commit (or base
advance) creates a new (head, base) pair and the pre-create evidence is
stale from that point — the next generation must run against the new
HEAD/base like any other.

**Every base advance invalidates earlier convergence too.** The reviewed diff is head-vs-base: a review generation run against (base A, head H) says nothing about the diff after base moves to A+1 — new content enters the comparison even with an unchanged HEAD. Convergence evidence therefore records BOTH anchors: the command, the HEAD SHA, and the **base OID** the diff was reviewed against. `convergence-status` and `merge-gate` validate both: a clean record whose base OID differs from the PR's current base OID does not converge, and a base advance between the convergence check and the merge is rejected the same way a head movement is. Whenever the base advances, a new local review generation runs against the new (base, head) pair before the merge gate.

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
    AND a `CLEAN` agent-review record for the current HEAD exists (see
    "How `convergence-status` computes the verdict" below — a missing,
    stale, or non-clean record blocks convergence)
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

## Structural skips (not-applicable)

A **structural skip** is a `SKIPPED` check that can never run for this PR shape, not one that was expected to run and did not. `pr-checks` classifies two kinds as `not-applicable`, excludes them from the `all_passed` gate, and records why in each check's `skip_reason`:

- `known-disabled-job` — a workflow job deliberately disabled or event-gated repo-side (`Documentation`; see arbsec/orchestraitor#404). The set holds only jobs that are still disabled on pull requests: `Docs Consistency` left it when `markdown.yml` re-enabled it (arbsec/orchestraitor#741), so its skip is an unexplained skip and blocks the gate.
- `bot-author` — `CodeRabbit` on a PR authored by a GitHub App bot user. CodeRabbit reports `SKIPPED` with the check-run reason "Review skipped: bot user not eligible for review", and every PR this repo's agents open has that author. The GitHub-side check is a no-op regardless: the review mechanism is the LOCAL `pr-review-local` CLI (owner directive 2026-10-09, above), never the GitHub-side bot.

The exemption never overrides state. Only the `skipping` bucket can become `not-applicable`; a known-disabled job or `CodeRabbit` that actually ran and reported `pass`, `fail`, or `pending` is classified from that state, so a failed job is never laundered into a pass.

Every other unexplained skip still blocks. `skip_reason` is `null` when a check is merely `skip`, and `all_passed` requires every entry to be `pass` or `not-applicable`. The `CodeRabbit` exemption requires all three of: the `CodeRabbit` name, the skipped bucket, and a bot/App PR author (`app/...` or `<slug>[bot]`); an unknown author fails closed. `gh pr checks --json description` is empty for these runs today, so the author check carries the discrimination — the description match is only a narrowing that will apply if gh ever populates the field (a rate-limit, config-error, or out-of-credits reason would then not be exempted).

## How `convergence-status` computes the verdict

The script combines:

- `pr-checks` — all required + non-optional checks pass at current HEAD;
- `review-threads` — all actionable threads resolved (`isResolved = true`);
- `reconcile-checklist` — the body must CARRY the managed `<!-- orc:* -->` markers **and** every one must be checked based on evidence. A body with **zero** managed markers is *missing* evidence, never a satisfied checklist (arbsec/orchestraitor#788: an empty parse reported `all_checked: true`, which reached `merge-gate` and merged a PR with no checklist evidence at all). It blocks with the distinct reason `checklist_markers_missing`; a marker-bearing body with unticked boxes keeps the separate `unchecked_items(N)` reason.
  - **Satisfiability at creation.** `pr-create` appends the nine `.github/PULL_REQUEST_TEMPLATE.md` markers UNCHECKED to any supplied body (`--body` or `--body-file`) that carries none — and to a bodyless PR, whose body then carries only the checklist (with a stderr note that it still needs a summary) — so the rule is always satisfiable; a body that already carries a marker is never rewritten. The delivery lane (`orc worker run --deliver` / `orc loop --deliver`) **leads** its PR body with the same nine markers, ahead of the model-authored summary: the summary is untrusted, and a summary that left a code fence open would otherwise enclose the checklist and hide it from the scan. A supplied body that ends inside an unclosed fence is refused with exit 2 by both routes (see below).
  - **Every `pr-mutate` body route carries the checklist.** `pr-mutate edit --body` and `pr-mutate edit --body-file` append the same nine markers (through the same `_lib.sh` helper `pr-create` uses, so the two cannot drift) when the supplied body carries none; a supplied body that already carries a marker is set with its CONTENT unchanged — the routes resolve the body through a command substitution, so trailing newlines are normalised to nothing, and nothing else is altered. Replacing a PR body therefore cannot drop the checklist and return the PR to `checklist_markers_missing` — which is exactly what the bodyless-PR note's advice depends on. `--body-file` is READ by the script (not forwarded to `gh`), so the path goes through the same trusted-root guard as `--body @file` and `-` reads the body from STDIN; a body that ENDS inside an unclosed code fence is refused with exit 2 (appending there would render the markers as code and hide them — close the fence and rerun).
  - **Remediation for a PR whose body is already set.** A PR opened before the rule (or opened by a bot) carries no markers and would block forever. One command clears it: `pr-mutate edit <pr> --inject-checklist` reads the PR's current body, appends the managed checklist when it carries none, and sets the body. It is idempotent — a body that already carries the checklist is left verbatim, and in that case only the BODY update is skipped: any other edit flag in the same invocation (`--add-label`, `--add-assignee`, …) still runs. It refuses to be combined with `--body`/`--body-file`, which now carry the checklist themselves.
  - **No author-granted opt-out.** There is no body sentinel, flag, or comment that lets a PR author exempt their own PR from the marker requirement: the obligation in `AGENTS.md` ("the PR checklist items are checked based on evidence") and `.agents/project/orchestraitor-workflow.md` ("the managed PR-checklist items are checked based on evidence") is absolute, and an author cannot grant themselves an exemption from it. Any exemption must come from an authority the author does not hold (arbsec/orchestraitor#801).
  - **Fenced code is not checklist state.** A marker inside a fenced code region (3+ backticks or 3+ tildes) is an EXAMPLE, not a checklist item: the whole region — including its opening and closing fence lines — is excluded from the marker scan. A fence line carries at most THREE leading spaces; four or more spaces (or a leading tab) is an indented code block, where fence characters are literal text and neither open nor close a region. A backtick fence's info string may not contain a backtick — ```` ```example``` ```` is a closed inline code span, not an opener — while a tilde fence's info string may. A marker AFTER a closed fence is a real marker. A CLOSING fence may be followed ONLY by spaces or tabs (CommonMark 0.31); a single trailing carriage return that terminates a CRLF line is the line ending, not content. A form feed or a vertical tab after the closing run is CONTENT — it leaves the region OPEN in both engines, so a marker behind such a closer is still inside the fence. Only a space or a tab indents a checklist item (arbsec/orchestraitor#799 F18). Both engines agree on this (the shell `orc_lib_strip_fenced_code` and the Rust helper next to `parse_checklist`), and the shared corpus in `.agents/skills/github-pr-lifecycle/scripts/tests/checklist-corpus/` is asserted by BOTH suites so the two cannot drift. Because an appended checklist inside an unclosed fence would be invisible to the scan, `pr-create`/`pr-mutate` REFUSE a supplied body that ends inside one (exit 2, error naming the fence) instead of appending into it or silently repairing the author's body.
- the review-generation state — a `CLEAN` agent-review record in `.orchestraitor/reviews/` whose full head SHA **and** full base OID exactly match the current PR head and base, plus the GitHub review decision and draft state. A missing, stale, or non-clean record blocks convergence (a base advance with an unchanged head is a stale record, not convergence). The CodeRabbit review evidence is the LOCAL CLI run at the final HEAD — `pr-review-local` (`coderabbit review --agent --base <base>`) with zero actionable findings, recorded in the PR description (command, HEAD SHA, base OID, findings=0) — not a GitHub-side bot review. Never post `@coderabbitai review` comments (owner directive 2026-10-09; bot triggers are no-ops during plan pauses and burn quota).

The script exits `0` only when all four are true. Exit `5` (`ORC_ERR_BLOCKED`) means convergence has not been reached — `blocked`/`needs-human`, not mergeable.

## The policy that runs must be the trusted base's

`pr-checks`, `convergence-status` and `merge-gate` resolve `$SCRIPT_DIR` from their own location, so an invoking checkout supplies the *policy*, not just the data. A checkout that predates a policy change therefore reports a confident wrong verdict — the #740/#748 defect: a pre-#707 `pr-checks` had no bot-author exemption and reported `checks_not_passing` for a PR GitHub reported `MERGEABLE`/`CLEAN`, while a fresh tree reported no such reason for the same PR. The observed direction was fail-closed, but a stale copy can equally carry an older, more permissive rule and produce a false PASS, which is why this is a bug and not a papercut.

Before emitting anything (and, for `merge-gate`, before any merge command is constructed), each script compares its own git blob against the blob at the PR's `baseRefOid` — the exact base commit GitHub reports, i.e. the trusted base branch. Any difference (older, branch-modified, or uncommitted) is refused with exit `6` and a typed `stale-review-policy` object that carries no verdict keys; the message names the file, the running commit, the base OID, and — whenever the base OID is known — the exact base-worktree command that yields a trustworthy verdict. When the base commit is absent locally the remediation also carries the fetch that makes it present: a plain `git fetch origin`, or `git fetch --unshallow origin` when `git rev-parse --is-shallow-repository` reports `true`, because a `--depth 1` checkout stays shallow across a plain fetch and the same refusal would otherwise repeat. `base-unreadable` (no `baseRefOid` from `gh`) has no base commit to name, so it prints no remediation. A version constant cannot detect this class of staleness: a whole stale directory carries a stale constant too. The comparison is local git only, so it costs no network call per run. Consequence: a PR that changes the gate is gated by the BASE policy, never by its own copy.
