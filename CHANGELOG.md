# Changelog

All notable changes to Orchestraitor are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html) once it begins tagging releases.

## [Unreleased]

### Added

- `xtask docs-check` subcommand validating the spec-split invariants the disabled docs
  workflow used to guard: compatibility-index integrity (duplicate identifiers, anchor
  resolution via the GitHub slug algorithm), repo-wide legacy `spec §N` reference
  resolution through the index, and intra-spec markdown links. 14 unit tests including
  failure injections; replaces the previous stub.
- `orchestraitor-delivery` crate (spec §9.33) with validated `TaskMetadata` for the
  autonomous-delivery backlog: stable deterministic task ID, spec-requirement refs, acceptance
  criteria, DAG dependency edges, agent-catalog domain + board `RiskClass` + §9.28
  `DataSensitivity`, expected files, required named verification checks, required reviewer
  domains, autonomy level, routing override, named §9.26 retry-policy profile, and
  completion-evidence kinds. Structural validation rejects empty identity/title/objective,
  self- or duplicate dependencies, and missing spec refs, acceptance criteria, verification,
  reviewer domains, retry profile, or completion evidence. Security-relevant tasks (security
  domain, critical risk, or confidential/restricted data) must require the security reviewer
  (§9.33.4). `SCHEMA_VERSION` anchors the versioned persistence envelope that §9.33.6 durable
  storage wraps around these records (#201).
- `orchestraitor-delivery` `TaskDag`: validated backlog DAG with deterministic Kahn
  topological order (ties break by stable task ID), cycle detection listing the involved tasks,
  construction-time rejection of unknown/duplicate edges, and dependency-satisfied eligibility
  per §9.33.3 (#199).
- `orchestraitor-testkit` crate with a deterministic `OpenAI` Chat Completions mock server
  (spec §21.3): scripted non-streaming, SSE-streaming, and structured-output responses with
  deterministic IDs/timestamps, sequence-ordered script replay (last plan repeats), HTTP
  failures (e.g. 429), and a request-capture API for exact client-behavior assertions. CI can
  now test provider integrations without a live provider (#175).
- Configurable review-loop parameters on `orchestraitor-delivery` (spec §9.33.4):
  `ReviewLoopConfig` with spec-mandated defaults (`max_review_loops: 3`, `max_reviewers: 5`,
  `required_reviewer_domains: ["security"]`, `minimum_severity_to_block: "high"`,
  `allow_same_model`/`require_provider_diversity`/`require_human_review`: false,
  `stop_when_no_blocking_findings: true`), an ordered `Severity` enum with a
  `blocks()` threshold helper, a `for_security_sensitive()` constructor that mandates human
  review per §21.1, and structural validation rejecting zero loop/reviewer limits and blank
  reviewer-domain entries. Serde surfaces `deny_unknown_fields` so drift fails visibly.
  Blocking verdicts remain with the runner/policy layer per the §9.33.7 security boundary
  (#198).
- Review-loop convergence check on `orchestraitor-delivery` (spec §9.33.4): `evaluate` renders
  a deterministic `ConvergenceVerdict` over the findings ledger and `ReviewLoopConfig` —
  `Converged` only when a full review generation confirmed at the current head produced no new
  noteworthy findings, no open non-stale finding meets the blocking threshold, no stale
  finding would block if re-confirmed, and `stop_when_no_blocking_findings` is set;
  `Continue { next_loop }` while remediation budget remains; and `Blocked` with
  `MaxReviewLoops` / `ReviewBudgetHardCeiling` reasons when a configured limit or the absolute
  `DEFAULT_HARD_LOOP_CEILING: 5` is reached — the explicit `blocked`/`needs-human` state
  §9.33.4 mandates, never silent approval. There is no approve/pass variant: a converged
  verdict means "stop the loop; the runner/policy decides promotion" (§2.2, §9.33.7) (#197).
- Delivery failure classification on `orchestraitor-delivery` (spec §9.33.5): `FailureClass`
  with exactly the nine mandated classes (transient provider or network, rate limit, tool or
  process, verification, merge conflict, invalid agent output, policy denial, approval
  required, non-retriable configuration or security), the persisted `FailureRecord` covering
  the §9.33.5 field list (task + attempt IDs, decompose/implement/verify/review/remediate
  `DeliveryPhase`, agent/model/provider display strings, normalized class, retriable status,
  workspace generation, evidence, partial-results flag, next retry time, and a §9.24.2-aligned
  `correlation_id`), a pure `classify` mapping class plus attempt context to a `RetryDecision`
  (`Retry`, rate-limit `retry-after` `Hold`, `FixRootCause`, bounded fresh-context `Reprompt`,
  `AwaitUser`, `Escalate`), and an append-only `FailureLedger` with per-class counts and
  retriable-class queries. Policy denials, missing approvals, and non-retriable configuration
  or security failures can never classify to a retry variant; classification proposes and
  Arbitraitor owns the policy verdicts (§2.2, §9.33.7) (#205).
- Escalation chain on `orchestraitor-delivery` (spec §9.33.5): `EscalationStep` with exactly
  the five mandated ladder steps (same agent with fresh context → alternate model → domain
  expert → revised task plan → human escalation), a configurable `EscalationPolicy` whose
  default is the full ladder in spec order and whose validation rejects empty ladders,
  reordered or duplicated steps, and ladders not ending at `human_escalation` (a policy may
  shorten the ordered chain but never reorder it, §9.22.4), and an `EscalationState` tracker
  with a saturating per-step failure counter, explicit `advance()`, and an append-only ordered
  history of left steps. The pure `next_escalation` proposes only `RetryAt { step }` at the
  current step or the terminal `HumanEscalation` — every step spawns a fresh context
  (§9.33.1, §7.3), per-step attempt budgets stay with the runner's retry policy (§9.26,
  §9.22.1), and no outcome carries auto-approve semantics: escalation proposes, the
  runner/policy layer executes, and Arbitraitor approves (§2.2, §9.33.7) (#206).
- Retry gate and bounded backoff schedule on `orchestraitor-delivery` (spec §9.33.5,
  §9.26.3): `IdempotencyProof` carries the typed evidence that a side-effecting operation
  may be retried (checkpoint resume per §9.24.2, proven-idempotency marker, or rolled-back
  effects — `Unproven` for absent evidence), and the pure `RetryGate::evaluate` re-asserts
  the classification invariants before the runner executes any retry: unproven
  tool/process failures override an incoming `Retry` to `FixRootCause` ("NEVER blindly
  retry side-effecting actions"), and policy denials, approval requirements, and
  non-retriable configuration or security failures always resolve to `Escalate`/`AwaitUser`
  regardless of proof — no proof can launder a policy denial. `RetrySchedule` proposes the
  §9.26.2 doubling backoff (base 200 ms, saturating at the `u64` boundary, capped at
  `max_delay_ms`, no jitter — jitter and budgets stay with the runner) with structural
  validation rejecting a zero base or a cap below the base. The gate is bookkeeping only:
  classification proposes, the gate re-asserts, and Arbitraitor owns every security verdict
  and enforcement decision (§2.2, §9.33.7) (#207).
- Backlog runner execution loop on `orchestraitor-delivery` (spec §9.33.3): a
  deterministic, synchronous, I/O-free `BacklogRunner` state machine that drives the
  validated backlog DAG over injectable per-attempt `AttemptOutcome` values, continuing
  until `BacklogEmpty`, `NoEligibleTasks`, `BudgetExhausted` (one budget unit per started
  attempt), `ApprovalRequired`, `FailuresBlocked`, `SecurityBlock`, or `Paused` — with
  `pause`/`resume` controls (§9.33.6) and an append-only `RunnerEvent` journal as the
  durable decision record. Every failed attempt runs `failures::classify` and then
  `retry_rules::RetryGate::evaluate` before any retry executes, so unproven
  side-effecting tool failures are overridden to `FixRootCause` and policy denials,
  approvals, and non-retriable security failures can never be laundered into a retry
  (§9.26.3, §9.33.5). Transient retries honor bounded `RetrySchedule` backoff and
  rate-limit `RetryHeld` retry-after holds; invalid output reprompts with fresh context
  within a bounded budget; and repeated failures walk
  `escalation::EscalationState` one ladder step per exhausted step budget to the terminal
  `HumanEscalation`, which blocks the task and stops the run with `FailuresBlocked` —
  the explicit blocked/needs-human state §9.33.4 mandates, never silent approval. The
  runner proposes, schedules, and stops only; promotion and verdicts stay with
  Arbitraitor and the policy layer (§2.2, §9.33.7) (#202).
- Change-set review pipeline on `orchestraitor-delivery` (spec §9.33.4): a deterministic,
  synchronous, I/O-free `ReviewPipeline` that triggers on change-set completion, selects
  reviewers once per trigger through `select_reviewers` (#195), and runs the generation-
  bounded adversarial review loop over an injectable per-generation `ReviewOutcome`
  producer (fresh context per generation is the invocation site's §9.33.1 duty; one
  `GenerationStarted` journal event accounts for every generation boundary). Each
  generation records reviewer findings into the `FindingLedger` so identical findings
  deduplicate and unresolved-but-unreported findings resolve across loops (#196), then
  `convergence::evaluate` (#197) renders the stop/continue/blocked decision. The
  terminal `ReviewVerdict` is `StopNoBlockingFindings` when a full generation at the
  current head leaves no finding at or above `minimum_severity_to_block`,
  `MaxLoopsBlocked { reason }` when `max_review_loops` or the hard loop ceiling is
  reached — the explicit `blocked`/`needs-human` state §9.33.4 mandates, never a silent
  approval — `Failed` when reviewer output misses the spec-mandated finding payload,
  and `SkippedEmptyChangeSet` when the trigger named no changed files. The append-only
  `ReviewPipelineEvent` journal (`Triggered` / `GenerationStarted` /
  `GenerationRecorded` / `ConvergenceEvaluated` / `Terminated`) is the §9.33.6 durable
  decision record with snake_case serde. The pipeline proposes loop control only —
  promotion and verdicts stay with Arbitraitor and the policy layer per the §9.33.7
  security boundary (#50).
- Review-finding ledger on `orchestraitor-delivery` for spec §9.33.4 finding deduplication
  and cross-loop tracking: `ReviewFinding` carries the spec-mandated payload (severity,
  evidence, affected paths, violated requirement or rule, proposed remediation, optional
  line span), and `FindingId` derives a stable `(path, line, rule)` dedup key with lexical
  path normalization (leading `./` segments, trailing slashes, surrounding whitespace
  stripped). `FindingLedger` dedups re-reports across loops (tracking first/last loop,
  occurrences, and per-generation heads), updates latest severity while keeping the maximum
  severity ever seen for blocking aggregation, reopens findings that resurface after
  resolution, resolves findings absent from a completed generation via
  `resolve_unreported`, and flags open findings stale on `note_head` HEAD movement so a
  later convergence checker (#197) can discount them. `blocking_open_count` aggregates via
  `Severity::blocks` while discounting stale entries; no verdicts — blocking decisions stay
  with the runner/policy layer per §9.33.7 (#196).
- Reviewer selection on `orchestraitor-delivery` (spec §9.33.4): `select_reviewers` turns a
  pre-classified `ChangeSetProfile` (changed files, languages, task domain, risk,
  dependency/config and auth/permissions/scripts/execution flags, coverage, Arbitraitor
  findings) plus the `ReviewLoopConfig` into a deterministic `ReviewerSet` of `(domain, role)`
  slots with a recorded `SelectionReason`. Selection always appends the general baseline
  first, triggers the security reviewer on auth/permissions/dependencies/CI/scripts/execution
  surface, critical risk, a security task domain, or Arbitraitor findings, matches the task
  domain (covering the example's backend/frontend rows without a hardcoded taxonomy), and adds
  the testing reviewer when coverage or verification changed. Domains deduplicate with the
  highest-priority reason winning, `required_reviewer_domains` are always present and survive
  `max_reviewers` truncation while counting toward the cap, and a required list that exceeds
  the cap fails as `ConfigUnsatisfiable`. The `Language` enum degrades unknown tags to
  `other`. Selection only proposes reviewers — gating and verdicts remain with the runner and
  Arbitraitor per the §9.33.7 security boundary (#195).
- `orchestraitor-provider-neuralwatt` crate implementing `ProviderTransport` against
  the Neuralwatt OpenAI Chat Completions-compatible API for GLM-5.2 BYOK (spec §10.3).
  Default base URL `https://api.neuralwatt.com/v1` (overridable via config); API key
  resolved from `secret://keyring/neuralwatt` or `NEURALWATT_API_KEY` env var
  (tech-stack §3.2). Streaming via `reqwest::Response::bytes_stream()` with SSE parsing
  into `ModelEvent` values. Per-call cost entries emitted per spec §9.19.4 through a
  `CostSink` trait. Wire-level cassette tests for `/v1/models` and
  `/v1/chat/completions` (streaming, non-streaming, and tool calls). The legacy Zhipu
  endpoint `open.bigmodel.cn` is rejected at configuration time (spec §10.3).
- `orchestraitor-daemon` crate with a SQLite WAL metadata store, schema migrations,
  hash-chained event persistence, Arbitraitor receipt/backlog/delegation tables, and an
  Arbitraitor-compatible SHA-256 filesystem CAS for spec §9.17 and tech-stack §11.
- `orchestraitor-context` crate with a content-addressed tree-sitter baseline indexer,
  Appendix E context query API, and spec §9.15.1 provenance envelopes on every emitted item.
- Initial repository governance, contribution guidance, security policy, code of conduct, and
  support documents, adapted from the sibling Arbitraitor repository for Orchestraitor's
  spec-driven, security-first workflow.
- Root [`AGENTS.md`](AGENTS.md) with always-active project rules: Arbitraitor as the exclusive
  security authority (spec §2.2, §16), spec as source of truth, engineering priorities, scope
  and GitHub workflow, review/merge invariants, documentation, testing, and Rust conventions.
- Dual `MIT OR Apache-2.0` licensing, matching Arbitraitor.
- `.github/` scaffolding: `CODEOWNERS`, `dependabot.yml`, issue templates
  (`bug.yml`, `feature.yml`, `task.yml`, `spike.yml`, `config.yml`), a pull-request template
  with stable machine-readable checklist markers, and a workflows `README.md` documenting the
  intended CI layout once application code exists.
- `.agents/project/orchestraitor-workflow.md` project workflow policy: MVP-only scheduling,
  security-first review requirements, Arbitraitor ownership boundaries, cross-repository
  blocker handling, required review domains, documentation and testing expectations, and PR
  convergence requirements.
- `.agents/project/github-project.example.toml` example GitHub Project configuration
  (resolves mutable node IDs at runtime; commits only human-readable names).
- Two reusable Agent Skills under `.agents/skills/`:
  `github-project-workflow` (triage, decomposition, ready-queue, claims, reconciliation) and
  `github-pr-lifecycle` (draft PR, CI inspection, adversarial review convergence, merge
  eligibility, post-merge reconciliation), each with `references/` and composable `scripts/`.
- `orchestraitor-provider-proxy` crate with OpenAI Chat Completions, OpenAI Responses,
  Anthropic Messages, `/v1/models`, short-lived local tokens, upstream BYOK credential
  isolation for child processes, per-completion cost attribution, and explicit Mode D
  trust-boundary reporting per spec §10.1.

### Changed

- Crate-path drift fix from the specification split: path-form references to the
  nonexistent `crates/orchestraitor-arbitraitor-client/` directory now name the real
  `crates/orchestraitor-arb-client/` in the project workflow policy, the tech-stack tree
  diagram, and the spec tree diagram. The crate's package name
  `orchestraitor-arbitraitor-client` remains correct in prose; only path-form references
  were wrong.
- Agent operating docs now mandate the GitHub App service identity (`arbsec-agent`) for
  agent-driven GitHub operations (issues, PRs, Projects v2 board writes, reviews, releases):
  `AGENTS.md` gains a critical rule, the project workflow policy and both GitHub skills
  carry the mandate, and personal owner auth is an explicitly labelled fallback only until
  the E0 App-registration backlog task lands.
- Specification reordered orchestrator-first: the split document set under `docs/spec/` now
  leads with the self-improving orchestration loop — backlog → manager selection → worker →
  pull request → adversarial review → human-gated merge, with every state transition tracked
  on the kanban board — and presents the harness golden path as the loop's worker surface and
  a first-class standalone tool (spec `00-overview.md` §1). Goals are reordered: orchestration
  and self-hosting are the primary goals and the harness golden path moves to secondary goals
  (spec `00-overview.md` §3.1, §3.2). The "autonomous agent swarms" MVP non-goal is re-scoped
  from a blanket exclusion to the bounded, budgeted, board-tracked, human-gated loop;
  unbounded or unbudgeted swarms, autonomy that bypasses the Arbitraitor security boundary,
  and self-modification outside the reviewed loop remain out of scope (spec
  `00-overview.md` §3.3). The harness document frames the harness as the orchestrator's
  worker surface (spec `20-harness-worker.md`). Section headings and legacy `§N` anchors are
  unchanged, so existing issue and pull-request references keep resolving through the
  compatibility index; the §2.2 ownership invariant and §6 principles are unchanged.
- Specification extended with the orchestrator-first normative sections: campaign
  session-per-decision orchestration, watch-daemon supervision (poll tick, stall/orphan
  detection, kick-off conditions, spend/run/subscription budget classes), agent issue
  reporting (report ≠ self-fix, with the blocking-defect fix exception), the MCP-early tool
  strategy with the built-in MCP proxy, coordinator decision tools, blocked-dependency
  semantics, epic-focus scheduling with bug preemption, multi-org workspaces with
  cross-project epics, the kanban board abstraction (`BoardProvider`), and the operator
  chat mode (spec `10-orchestrator.md` §9.35-§9.44); plus role-based model routing with the
  pluggable `DecisionProvider` trait and subscription-aware routing (spec
  `30-model-routing.md` §9.45-§9.46). The TypeSafe/jev entry in the technology stack
  records its early-access license status (not yet allowlisted; adapter default-off). The
  §9.33 intro now states autonomous delivery as the primary product axis, matching the
  reordered goals and the M1 milestone; section headings and legacy `§N` anchors are
  unchanged.

### Fixed

- Lockfile refresh for yanked and advisory-flagged crates so `cargo deny check` and
  `cargo audit` pass again: `chacha20 0.10.1 → 0.10.2` (yanked), `h2 0.4.15 → 0.4.19`
  (RUSTSEC-2026-0258), `rustls 0.23.43 → 0.23.45` (RUSTSEC-2026-0285, with
  `rustls-webpki 0.103.15`), `faster-hex 0.10.0 → 0.10.1` (RUSTSEC-2026-0306 warning).
  All bumps stay inside the existing semver ranges; no manifest changes (#255).
- Restored `crates/orchestraitor-workspace/`, which the provider-proxy squash merge (b96657f, PR
  #223) had deleted after PR #219 merged it (spec §9.4, MVP-4). `orchestraitor-tui` and
  `orchestraitor-provider-neuralwatt` were likewise absent from `[workspace].members`. All crates
  are now listed explicitly so `cargo --workspace` parity-gate commands cover every crate
  regardless of the dependency graph. During verification, `orchestraitor-workspace`
  `tests::snapshot_has_no_dot_git` failed once on a cold cache and then passed in one isolated
  and four consecutive full-suite runs; per spec §21.10 this flake must be root-caused if it
  recurs in CI.
- `ready-queue` (github-project-workflow skill) never produced output: the jq filter closed
  `select(` early (stray `)`), leaf detection relied on null `issueType` (org-level native types
  unset), and `blockedBy.totalCount` counted resolved blockers. The filter now lives in
  `ready-queue.jq`, treats `task`/`bug` labels as leaf, filters only unresolved blockedBy edges,
  and is regression-tested by `scripts/tests/ready-queue-filter.bash`, now run in CI (#251).
  The output is a candidate queue: Project v2 `Status`/`Target` cannot be read by
  `gh issue list`, so the project-manager still verifies those on the board before
  claiming (documented in SKILL.md).
- `orchestraitor-context` index is now keyed by blob digest instead of path: a file move to a
  new path with unchanged content is recognised as reuse, not reparse. Paths present in the
  previous index but absent from the new traversal are also evicted on reindex, so deleted
  files no longer remain queryable. Reference records now carry the provenance of the blob
  that owns them (the reference-occurrence blob, not the target symbol's provenance).
  The Appendix E query API exposes `repository_summary`, `symbol_body`, `related_tests`,
  `diagnostics`, and `expand_context` as MVP stubs pending §9.16 LSP-backed wiring.
  Cross-blob reference keying is deferred; a TODO in `index.rs` flags the digest-keyed
  follow-up for a future iteration.
- `orchestraitor-core` now merges dynamic configuration table entries field-by-field, includes
  structured error causes and source chains, and omits sensitive tracing fields entirely.
- `orchestraitor-cost-ledger` no longer exposes `BudgetScope::Organization` or
  `BudgetScope::User`. The `scope_filter` previously mapped both variants to the SQL
  tautology `?1 = ?1`, so a budget configured for one org or user silently counted every
  ledger row, breaking isolation. The variants are deferred until `cost_entries` ships
  real per-org / per-user attribution columns. Project, Session, Domain, and Agent
  scopes continue to filter on their own columns and gain explicit regression tests
  pinning scope isolation.
- `orchestraitor-daemon` `CasDirectory::load_bytes` now recomputes SHA-256 over the bytes it
  reads and refuses to return them if the digest does not match the address; previously a
  corrupted or out-of-band-written blob would be returned as-is, undermining the
  content-addressed guarantee. A new `StoreError::DigestMismatch { expected, actual }`
  variant reports both digests; an adversarial test (`cas_load_bytes_rejects_corrupted_blob`)
  pins the behaviour against on-disk corruption.
- `orchestraitor-daemon` `DaemonStore::load_event_records` is now exercised by adversarial
  tests (`load_event_records_rejects_tampered_record_json`,
  `load_event_records_rejects_record_json_payload_drift`) that mutate `event_records.record_json`
  via raw `SQL` to confirm the hash-chain validator rejects the drift with
  `EventError::RecordHashMismatch`. A `pub(crate)` `execute_raw` test hook is the only path
  that can bypass the typed CRUD helpers; it is `#[cfg(test)]` and documented as such.

[Unreleased]: https://github.com/arbsec/orchestraitor/compare/HEAD
