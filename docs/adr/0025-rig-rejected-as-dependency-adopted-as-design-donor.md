# ADR 0025: Re-evaluate and maintain the rejection of `rig` as a provider/LLM dependency; adopt it as a design donor only

**Status:** Accepted
**Date:** 2026-10-13
**Issue:** #605

## Context

Spec `20-harness-worker.md` §9.49 (typed provider options, refusal, and retryable error
classification) borrows its design shape from rig 0.44.0 (0xPlaygrounds). Before §9.49 was
specified, rig was re-evaluated as a candidate dependency — it had previously been rejected
at 0.40.0 (tech-stack §4.4, "Here be dragons... breaking changes").

Evaluation inputs (2026-10-13, preserved verbatim below with rig's MIT license permitting
quotation):

1. The dependency evaluation memo ([evidence 0025-a](0025-a-rig-0.44.0-dependency-evaluation.md))
   — what rig 0.44.0 is, its maintenance signals (MIT, MSRV 1.95.0, ~17 breaking changes per
   release, ~150-crate minimal tree), the pros/cons against this repo's constraints, the
   comparison against the pinned genai baseline, and the verdict.
2. The design-borrow memo ([evidence 0025-b](0025-b-rig-0.44.0-design-borrow-memo.md)) — the
   ranked list of design ideas worth reimplementing natively (typed generation options with
   per-provider refusal, retryable error classification, content-block messages, streaming
   tool-arg deltas, conformance harness, structured-output ladder) and the layers explicitly
   cut (catalog, cost auto-fill, agent loop, RAG).

## Decision

- **Maintain the existing rejection of `rig-core` / `rig` as a dependency.** The compliance
  gates pass (license, MSRV, cargo-deny, unsafe); the disqualifiers are architectural and
  economic: rig's differentiating layers (agent/tool loop, model catalog, USD cost auto-fill,
  RAG) are precisely the layers spec `30-model-routing.md` §9.19.4 and §9.29 assign to
  project-owned, Arbitraitor-mediated code, and behind `ProviderTransport` only the HTTP wire
  layer remains — a slice the pinned genai baseline already occupies at lower dependency
  weight.
- **Adopt rig as a design donor only.** The typed-options/refusal design of §9.49 is
  reimplemented natively behind the project-owned `ProviderTransport` seam; no rig code ships.

## Consequences

- tech-stack.md §4.4 and §16 record the re-evaluation with this ADR as the durable rationale.
- The reconsideration trigger from evidence 0025-a stands: only a semver-stable 1.x with a
  measurably lower breaking-change rate AND a demonstrated provider feature gap would reopen
  the question — and even then wires-only, feature-gated, never the catalog/cost/agent layers.
- The remaining borrowed designs are named as future sections in §9.49.5 and are NOT
  authorized by §9.49.
