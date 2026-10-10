# `decision.record` — the append-only decision tool

`decision.record` persists ONE decision record with rationale per the §9.35
shape (spec [10-orchestrator.md](../spec/10-orchestrator.md) §9.35, §9.39,
issue #334): append-only and replayable. It is the coordinator decision tool
the campaign loop's records flow through — the record shape and store are the
SAME ones `orc campaign run --once` writes into `<config-dir>/campaign.db`
(see [orc-campaign](orc-campaign.md)); no second record format exists.

## Surface

The tool is exposed on the MCP gateway (always registered — unlike
`board.query` it has no external provider to configure):

```json
{
  "kind": "selected",                       // or "no-op"
  "no_op_reason": null,                     // required on no-op: empty-queue | all-blocked | epic-exhausted
  "selected": {                             // required on selected, refused on no-op
    "repo": "arbsec/orchestraitor",
    "number": 334,
    "title": "decision.record tool",
    "url": "https://github.com/arbsec/orchestraitor/issues/334",
    "item_node_id": "PVTI_1",
    "task_id": "board-arbsec_orchestraitor-334"
  },
  "role": "implement",
  "provider": "neuralwatt",
  "model": "glm-5.2",
  "precedence_path": "bootstrap-default",
  "fallback_reason": null,
  "worker_args": ["worker", "run", "--task", "board-arbsec_orchestraitor-334"],
  "rationale": "first eligible P0 item in ready order (spec §9.35)",
  "alternatives": [ /* each with repo, number, title, open_blockers, target, status */ ],
  "blocked_graph": [ /* attached on all-blocked no-ops */ ],
  "skipped": [ /* fail-closed data-quality channel, §9.43 */ ],
  "delegation_chain": ["user:alice", "session:sess_7e3f"]
}
```

The tool returns the STORED record: the payload plus the store-assigned row
id and creation timestamp. Replay is identity: `by_id`/`list` on the store
read back the record byte-for-byte as written (asserted by tests).

## Append-only

The tool surface has exactly one effect — INSERT. There is no update, delete,
or overwrite path: the operation is not exposed on the tool, and the
underlying store exposes only append (`record`) and reads (`by_id`, `list`).
Re-recording an identical decision appends a NEW row (ids strictly increase);
the original row is never touched (negative-tested). A bad decision is
corrected by a NEW pass and a NEW record — never a rewrite — which is what
makes the store a safe rollback surface (append-only local state, per the
issue's rollback note).

## Validation on write (§9.35)

Malformed records are REFUSED with typed reasons (thiserror at the tool
boundary; the error text is a static label — record content never enters an
error string). Refusals leave the store AND the audit log untouched:

- missing required fields (`role`, `provider`, `model`, `precedence_path`,
  `rationale`) — a required field absent from the call entirely fails
  deserialization BEFORE the tool runs and surfaces as an MCP
  `invalid_params` protocol error (`failed to deserialize parameters:
  missing field …`); present-but-invalid values are refused inside the
  tool with typed reasons and a structured `decision_record_failed`
  payload;
- a `selected` decision without the selected task, or with a no-op reason;
- a `no-op` decision without one of the three typed reasons, or with a
  selected task;
- field bounds (identifier fields 256 chars, text fields 4096, worker argv
  64 × 1024 chars, carried lists 256 entries) — a record must replay EXACTLY
  as written, so overflow is refused, never silently truncated.

## Secrets: fail-closed refusal (recorded gap)

Records must not contain secrets. The spec does not yet define
decision-record redaction rules — payload classification is Arbitraitor's job
(spec §9.39, §9.28.4) — so this slice implements a CONSERVATIVE local check
mirroring the §9.23.4 trace-redaction heuristics (spec
[40-arbitraitor-integration.md](../spec/40-arbitraitor-integration.md)) and
the §20.4.2 payload redaction classes (spec
[50-contracts-data.md](../spec/50-contracts-data.md)): `secret://` URIs,
`sk-`-prefixed keys, Bearer tokens, GitHub token shapes, long hex runs, and
long base64 runs are REFUSED, not redacted — a decision record is an audit
artifact and must never be silently rewritten with redactions. The check is
deliberately over-eager (a false refusal is recoverable; a false pass leaks
permanently). **Recorded gap:** full redaction-rule ownership belongs to
Arbitraitor; when Arbitraitor exposes decision-payload classification, this
local heuristic should be replaced by that answer (a follow-up issue, not a
local workaround — spec §16.2).

## Injection boundary (§6.1 / §9.39)

Tool arguments are untrusted input. Instruction-shaped content in the record
(rationale, titles, skip reasons) is inert DATA: it is stored and replayed
verbatim, never executed or interpreted (negative-tested: a hostile rationale
replays byte-identically and produces the same typed result as a benign one).
Only secret-shaped material triggers refusal.

## Event recording (§9.25.1)

Every successful append records one `ToolRequest` event in the audit event
store — the existing §9.17 mechanism — carrying the tool name, a summary of
the outcome (decision kind, typed no-op reason, stored row id — NEVER the
rationale or other record content), and the §9.25.1 delegation chain. The
chain is CLIENT-ASSERTED: labels are recorded under `chain_source:
client-asserted` with a `claimed:` prefix per label, truncated like any
untrusted value, and bounded — provenance is never fabricated (same posture
as `board.query`). The record content itself stays in the decision store; the
audit event carries only the summary.

**Audit persistence:** identical posture to `board.query` in this slice. When
the connection carries a shared audit store, the invocation event CONTINUES
that store's hash chain (seed events and other invocations' events survive;
the merged chain validates end to end). When none is configured, the event is
written and hash-chain-validated but the store is per-invocation-volatile —
dropped with the invocation. Durable persistence lands with the daemon
event-store wiring (§9.17).

## Persistence note

In this slice the MCP tool runs against the campaign §9.35 store IN MEMORY:
a session MAY carry a session-scoped store (every `decision.record` call on
the connection appends into that one store, so append-only row ids are
observable across the session's invocations); without one, each invocation
opens its own store. Either way the record shape and code path are identical
to `orc campaign run --once` (which persists durably at
`<config-dir>/campaign.db`). Wiring the MCP tool to that same on-disk store
lands with the daemon decision-tool wiring (§9.17); only the store handle
differs, not the record format.
