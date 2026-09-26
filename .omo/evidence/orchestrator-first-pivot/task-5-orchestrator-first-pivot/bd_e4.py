#!/usr/bin/env python3
"""E4 leaf tasks: mini-agent harness (incl. the built-in MCP PROXY)."""

from bd_common import BUDGET_BLOCK

TASKS = [
    {
        "id": "E4-T1",
        "epic": "E4",
        "title": "Task: harness-headless — headless one-shot worker mode (full depth)",
        "risk": "Medium",
        "estimate": 8,
        "labels": [],
        "blocked_by": [],
        "body": """## Objective

Deepen the E0-T4 bootstrap worker into the full headless one-shot mode:
task in, structured result + PR out, exit code — with structured result
schema (task id, session id, exit code, PR ref, evidence paths, failure
class), checkpoint/resume on interruption, and the full mediated toolset
contract.

## References

- `docs/spec/20-harness-worker.md` §10.1 (integration modes)
- `docs/spec/10-orchestrator.md` §9.24 (lifecycle — checkpoint resume)
- `docs/spec/10-orchestrator.md` §9.33.3 (fresh-context implementer)

## Acceptance criteria

- [ ] `orc worker run --task <id> --json` emits the full structured result schema; exit codes typed (success, task-failure, exhaustion, mediation-refusal)
- [ ] Interruption mid-task checkpoints; resume continues from the checkpoint (virtual-clock test)
- [ ] `cargo nextest run --workspace` green; simulator-backed integration tests

## QA scenarios

- Happy: fixture task completes; result JSON validates against the schema; evidence `.omo/evidence/<task-slug>/headless-happy.txt`
- Failure: kill -9 mid-run -> restart resumes from checkpoint (assert no duplicated side effects via idempotency receipts); evidence `.omo/evidence/<task-slug>/headless-resume.txt`

## Non-goals

No interactive mode (E4-T2); no sub-agents; no review behavior.

## Security impact

Same mediation boundary as E0-T4 (all I/O mediated).

## Testing requirements

Integration + crash-resume tests; idempotency assertions (§9.26.3).

## Documentation impact

`docs/cli/` worker docs; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream (deepens E0-T4).

## Rollback implications

New mode; existing paths unchanged.
""",
    },
    {
        "id": "E4-T2",
        "epic": "E4",
        "title": "Task: harness-interactive — standalone interactive mode (`orc`)",
        "risk": "Medium",
        "estimate": 8,
        "labels": [],
        "blocked_by": ["E4-T1"],
        "body": """## Objective

Standalone interactive harness mode: `orc` interactive coding/troubleshooting
on the same loop core as the headless worker — one loop, two entry modes.
Interactive sessions run under the same mediation boundary and event
recording as worker sessions.

## References

- `docs/spec/20-harness-worker.md` §10.1 (integration modes)
- `docs/spec/60-milestones.md` M2 (standalone harness golden path)

## Acceptance criteria

- [ ] `orc` (no subcommand) starts an interactive session on the shared loop core; tool surface identical to the worker's mediated set
- [ ] Session events recorded like worker sessions (durable, replayable)
- [ ] `cargo nextest run --workspace` green; interactive smoke via PTY fixture

## QA scenarios

- Happy: PTY-fixture session performs read/search/write via mediated tools; transcript recorded; evidence `.omo/evidence/<task-slug>/interactive-happy.txt`
- Failure: a tool refused by policy in interactive mode surfaces the typed refusal to the user (no silent skip); evidence `.omo/evidence/<task-slug>/interactive-refusal.txt`

## Non-goals

No TUI dashboard (Icebox epic); no wrapped-harness adapters (Mode C is M2+ scope, separate backlog).

## Security impact

Same boundary; interactive approval prompts route to the trusted UI (§6.4).

## Testing requirements

PTY fixture tests; refusal-surfacing negative.

## Documentation impact

`docs/cli/` interactive docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E4-T1 (loop core).

## Rollback implications

New mode; additive.
""",
    },
    {
        "id": "E4-T3",
        "epic": "E4",
        "title": "Task: harness-toolset — minimal mediated toolset hardening + attempt bounds",
        "risk": "Medium",
        "estimate": 5,
        "labels": [],
        "blocked_by": ["E4-T1"],
        "body": """## Objective

Harden the minimal mediated toolset (read files, search, mediated bash,
transactional write) to production quality: per-tool policy presentation,
attempt bounds per task and per tool class, and the transactional write path
fully wired (capture -> normalize -> verify -> compact diff -> promote or
roll back).

## References

- `docs/spec/60-milestones.md` §998 MVP-6 (built-in coding tools)
- `docs/spec/20-harness-worker.md` §9.5 (filesystem transaction and normalization engine)
- `docs/spec/10-orchestrator.md` §9.26 (retry semantics — bounded)

## Acceptance criteria

- [ ] Attempt bounds enforced per task and per tool class: """ + BUDGET_BLOCK + """
- [ ] Transactional writes produce a reviewable compact diff; rollback restores prior state (test)
- [ ] `cargo nextest run --workspace` green including toolset unit + property tests

## QA scenarios

- Happy: write via transaction -> diff -> promote; rollback path restores byte-identical prior state; evidence `.omo/evidence/<task-slug>/transaction-roundtrip.txt`
- Failure: tool exceeding its attempt budget stops with typed exhaustion; a normalization failure rolls back cleanly; evidence `.omo/evidence/<task-slug>/toolset-bounds.txt`

## Non-goals

No new tools beyond the four; no MCP proxy (E4-T4).

## Security impact

Transactional path is the trusted-output boundary (§9.14 promotion).

## Testing requirements

Property tests for rollback; exhaustion negatives.

## Documentation impact

Tool reference docs; CHANGELOG `[Unreleased]`.

## Dependencies

- Blocked by E4-T1 (loop core).

## Rollback implications

Hardening; behavior compatible.
""",
    },
    {
        "id": "E4-T4",
        "epic": "E4",
        "title": "Task: harness-mcp-proxy — built-in MCP PROXY (one endpoint; routing/policy-surface only)",
        "risk": "Critical",
        "estimate": 8,
        "labels": ["needs-human-review"],
        "blocked_by": [],
        "body": """## Objective

The built-in MCP PROXY per D10/§9.38: agents get ONE endpoint that fronts
(1) the MVP-6 built-in tool surface, (2) approved external MCP servers, and
(3) Arbitraitor's own MCP server. The proxy does tool namespacing,
fingerprint pinning, schema-drift detection, and per-tool policy
presentation. It is NEVER a security authority: no allow/deny/verdict
decisions, no capability issuance, no enforcement beyond what Arbitraitor
enforces; proxying MUST NOT widen any grant. Tool results surfaced to agents
pass `sanitize_for_agent` when quoted into agent-facing context.

## References

- `docs/spec/10-orchestrator.md` §9.38 (built-in MCP proxy — full contract)
- `docs/spec/20-harness-worker.md` §9.18.1 (MCP containment; advisory annotations)
- `docs/spec/tech-stack.md` §2.3 (mandatory Arbitraitor MCP wiring)
- Ledger D10 (mcproxy-go as transport-aggregation pattern-reference ONLY)

## Acceptance criteria

- [ ] One proxy endpoint fronts built-ins + approved externals + arbitraitor's MCP server with namespaced tool identities
- [ ] Fingerprint pinning + schema-drift detection quarantine mismatched servers (drift event recorded)
- [ ] Grant-preservation test: a tool call through the proxy crosses the same Arbitraitor boundary as a direct call — a call denied directly is denied through the proxy (assert the forbidden effect did not occur)
- [ ] Results quoted to agents pass `sanitize_for_agent`
- [ ] `cargo nextest run --workspace` green including proxy unit + adversarial tests

## QA scenarios

- Happy: namespaced call routes to the right backend; drift quarantine works; evidence `.omo/evidence/<task-slug>/proxy-routing.txt`
- Failure: grant-widening fixture (a proxied call shape that would bypass a direct-call denial) -> the suite catches it (call still denied); annotation-only `readOnly` claim does not restore a quarantined server; evidence `.omo/evidence/<task-slug>/proxy-no-widening.txt`

## Non-goals

No security decisions in the proxy; no new MCP servers; no transport reimplementation beyond aggregation.

## Security impact

Trust-boundary component: Risk=Critical, needs-human-review — the invariant is "proxying MUST NOT widen any grant".

## Testing requirements

Adversarial grant-preservation suite; drift negatives; sanitize boundary tests.

## Documentation impact

Proxy architecture docs; CHANGELOG `[Unreleased]`.

## Dependencies

None upstream (consumes E0-T10 approved-server fingerprints at runtime).

## Rollback implications

Proxy is optional-fronting (direct calls remain); disable via config.
""",
    },
]
