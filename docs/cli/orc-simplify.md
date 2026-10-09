# `orc simplify` — the pre-landing code simplification pass

The rule-driven quality pass over a worktree (spec §9.5 normalization
classes). `orc simplify` runs the tools the repository already uses —
cargo clippy, cargo fmt, rumdl, cargo-machete — and renders their findings
as typed suggestions in three classes:

| Class | Examples | Behavior |
| --- | --- | --- |
| **Format** | rustfmt, rumdl fixes | Auto-applies when the config allows it (`simplify.auto_apply_format`) |
| **Safe fix** | clippy machine-applicable suggestions on `.rs` files | Suggest-only by default; auto-applies only under `--fix safe` AND `simplify.auto_apply_safe_fixes = true` |
| **Semantic** | dead-code hints, unused dependencies, pedantic findings | Suggest-only, never auto-rewritten |

```sh
orc simplify run [--staged] [--path <PATH> ...] [--fix none|format|safe] [--pedantic-check] [--json]
```

## Fail semantics: fail-open, loudly

Simplify is a quality tool, never a blocker. An absent tool, a spawn
failure, a timeout, or an unresolvable configuration (parse or validation
failure in any layer) is recorded as a typed warning carrying error code
`ORC-SIMPLIFY-001` and the pass continues — it never fails a commit, a
worker, or a push. A configuration failure skips the pass entirely
(exit 0, warning on stderr); it is never propagated as a non-zero exit.
The ONLY non-zero exit is `--pedantic-check` (below).

Hooks are fast-feedback affordances, not enforcement points: they are
bypassable and untrusted-adjacent. The enforcement point for pre-landing
quality is the push path (the review-loop PR's push-branch gate).

## Options

- `--staged` — scope the report to files staged for commit (what the
  pre-commit hook passes). Workspace-level findings without a file path are
  always reported. Scope resolution distinguishes two cases: git itself
  failing (or git absent) drops the staged filter entirely — the report is
  unscoped rather than silently narrowed to nothing; a readable index with
  NOTHING staged is a real empty scope — only findings without a file path
  are reported. `--staged` also implies check-only: format fixes never
  auto-apply over a staged scope, because rustfmt/rumdl rewrite whole
  files and would touch unstaged (or partially staged) hunks the user
  never asked to modify.
- `--path <PATH>` — restrict the report to specific paths (repeatable).
- `--fix none|format|safe` — fix policy (default `none`):
  - `format` auto-applies Format-class fixes when
    `simplify.auto_apply_format` is true (the default): the check runs
    first, then a second fixing invocation (`cargo fmt` without `--check`,
    `rumdl check --fix`) performs the rewrite; suggestions are marked
    applied only when that fixing command succeeded;
  - `safe` additionally runs `cargo clippy --fix --allow-dirty` after the
    check pass, then re-runs the check as a verify pass: a clippy
    machine-applicable suggestion on `.rs` files is marked applied only
    when the verify check no longer reports it (a zero fix exit alone
    proves nothing — rustfix can fail to apply and roll back individual
    suggestions, and fixes that shift lines must not misattribute
    surviving findings). Pending the Arbitraitor output-classification
    gate (review-loop PR), safe-fix auto-apply is limited to exactly this
    surface — everything above Format is suggest-only otherwise.
- `--pedantic-check` — exit 1 when unaddressed suggestions above Format
  exist (safe-fix + semantic), reported under error code
  `ORC-SIMPLIFY-002`. This is the pre-push hook's fast-feedback signal,
  not a gate.
- `--json` — emit the typed report: `ran`, `ran_rules_only`,
  `unaddressed`, `auto_applied_count`, `suggestions[]`, `tools[]`.

## Configuration

Layered config keys (built-in defaults shown):

```toml
[simplify]
enabled = true               # master switch; hooks no-op (exit 0) when false
auto_apply_format = true     # Format class (§9.5 semantics)
auto_apply_safe_fixes = false # Safe-fix class; see the classification note above
max_passes = 2               # §9.5 convergence bound
max_files = 200              # per-pass file bound
pedantic = false             # include clippy::pedantic findings
model_pass = false           # model-driven tier: parsed but NOT wired yet —
                             # it activates with the review-loop PR's shared
                             # sub-session runtime
```

## Hook wiring (lefthook)

```yaml
pre-commit:
  commands:
    simplify:
      run: orc simplify run --staged
      glob: "*.rs"
      stage_fixed: false     # simplify proposes; it does not rewrite the index
pre-push:
  commands:
    simplify-push:
      run: orc simplify run --pedantic-check
      glob: ["*.rs", "**/*.rs"]
```

`stage_fixed: false` matches every existing hook: the pass proposes and
reports; the developer reviews and stages. The pre-commit run is
deliberately check-only: `--staged` never auto-applies format fixes
(whole-file rewrites would clobber unstaged hunks), so the hook passes no
`--fix` flag. The pre-push check is deliberately unscoped: at pre-push time
the branch is fully committed and a `--staged` scope would be empty. Both
hooks are fast-feedback only — the real pre-landing gate is the push path.

## Error codes

- `ORC-SIMPLIFY-001` — a simplify tool could not run (absent binary, spawn
  failure, timeout) or the pass configuration could not be resolved (parse
  or validation failure in any layer). Fail-open: the warning names the
  tool or the config cause; execution continues (a config failure skips
  the pass entirely, still exiting 0).
- `ORC-SIMPLIFY-002` — `--pedantic-check` found unaddressed suggestions
  above Format class. The only non-zero exit path in the command.
