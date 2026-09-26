# Task 5 evidence — orchestrator-first implementation backlog

Deliverable: board state on org `arbsec` project 1 "Arbsec Development"
(shared with arbitraitor) + two gap issues in `arbsec/arbitraitor`.
No application code; the only Git-visible output is this evidence.

## What was created (read-back verified: 1304/1304 assertions PASS)

- 12 epics (E0-E10 + ICE icebox placeholder) — E0=P0, E9=P2, E1-E8+E10=P1,
  ICE=P3; E0-E10 Target=MVP, ICE Target=Icebox; every epic has >=1 leaf Task.
- 72 leaf Tasks (native sub-issues, native issue type `Task`), each with a
  DoR-complete body: objective, spec anchors (`docs/spec/…` §), acceptance
  criteria (exact commands), QA happy+failure scenarios with evidence paths,
  budgets where applicable (attempts 3, re-plan 2, worker timeout 45m,
  concurrency 2, stall 10m, backoff 10s·2ⁿ cap 5m, $10/day soft cap, 4h run
  budget — owner-adjustable), non-goals, security impact, testing/docs
  impact, dependencies, rollback. E0 tasks carry THIN SLICE markers naming
  the deepening epic.
- Fields: Target=MVP + Status=Ready for non-gap tasks; E5-T7/E5-T8
  (gap-linked) and ICE-T1 at Backlog; Risk per spec (E5 all-Critical,
  E8 guard/budget tasks Critical, gap-linked Critical; all Critical tasks
  carry `needs-human-review`).
- 63 native blockedBy edges: 10 epic-level wave edges (E1/E2→E3,
  E1/E3/E4/E5/E6→E7, E7→E8, E8→E9/E10) + 53 task-level edges (intra-epic
  chains + cross-epic bootstrap edges), of which 2 are cross-repo to
  arbsec/arbitraitor#746 and #747.
- 2 gap issues in arbsec/arbitraitor (feature_proposal.yml conventions,
  ledger F15 context): #746 headless ApprovalPrompt (only StdinApprovalPrompt
  ships today; ADR-0013 invariants), #747 ADR-0038 pipeline-engine extraction
  (status Proposed; 3 divergent compositions).
- All items UNASSIGNED (ownership = Status + system, ledger D11).

## Files

| file | what |
|---|---|
| `manifest.md` / `manifest.json` | DRY-RUN MANIFEST (criterion e) — written BEFORE any mutation; exact operations + expected items/edges/fields/labels + body sha256 digests |
| `bd_*.py`, `backlog_data.py` | content model (single source of truth; validated: 12 epics, 72 tasks, 63 edges, 2 gaps) |
| `write_manifest.py` | generates the manifest from the model |
| `apply.py` | executes the mutations (sequential, 0.25s pauses, resumable via state.json in /tmp) |
| `verify.py` | read-back assertions (a/b/c) + failure-mode proof (d) |
| `mutation-log.jsonl` | every mutation: op, inputs (node IDs redacted), duration, result — 1 failed call total (the recorded createIssueType scope block) |
| `apply-stdout.log` | apply run output |
| `readback-report.txt` | 1304 PASS / 0 FAIL assertions |
| `readback-items.json` | the fetched board state the report was computed from |
| `failure-mode-proof.txt` | wrong-expected-id → MISMATCH (verifier validates, not rubber-stamps) |
| `set-epic-types.sh` | remediation for the blocked native Epic type (see below) |

## Recorded deviation: native Epic issue type BLOCKED

The org has no `Epic` issue type (only Task/Bug/Feature). Creating it
(`createIssueType`) requires the `admin:org` OAuth scope; the owner token
has `gist, project, read:org, repo, workflow`, and scope elevation is an
interactive owner action the executor must not attempt. Fallback: the 12
epics carry the existing `epic` label — the board's documented current type
dimension (issue arbsec/orchestraitor#252: "the Type dimension lives in
labels today"). Leaf Tasks use the native `Task` type (it exists).
Remediation (owner, one-time): `gh auth refresh -s admin:org`, create the
Epic type, then run `set-epic-types.sh` (12x `gh issue edit --add-type Epic`).

Node IDs are never committed: runtime-resolved, cached in
`/tmp/orchestraitor-board-cache/` (raw log + state.json live there); the
committed `mutation-log.jsonl` is redacted (grep-verified).
