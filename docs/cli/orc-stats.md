# `orc stats` — token-efficiency statistics

Reporting over the cost ledger (`.orchestraitor/cost.db`, spec
[§9.19.4](../spec/30-model-routing.md)) implementing the measurement
methodology of spec [§13.5.1](../spec/50-contracts-data.md).

```sh
orc stats efficiency [--group-by-profile] [--json]
```

The command requires an existing ledger: it fails with a typed error naming
the missing path when `orc loop` has not created `.orchestraitor/cost.db`
yet — it never creates the file or fabricates an empty report.

## `orc stats efficiency`

Prints token-efficiency rollups over recorded runs. Each rollup joins the
per-call cost rows (provider-reported input/output/cache tokens, spec
§9.19.4) with the context-compiler receipt deltas recorded for the same
session (spec §18.4, §13.5.1):

| column | meaning |
| --- | --- |
| group | the session id, or the profile label with `--group-by-profile` |
| input / output | provider-reported tokens summed over the group's cost entries |
| cached (read) | provider-reported cache-read tokens |
| candidate | summed candidate context tokens from receipts (the compiler's candidate set before selection) |
| selected | summed selected context tokens from receipts |
| savings | `1 − (selected + compacted tool output) / (candidate + raw tool output)` when a receipt exists |
| median savings | the median of the group's per-session savings ratios (`--group-by-profile` only). The A/B comparison stat: a ratio of summed counters would weight sessions by their baseline. |

A `—` in the savings column means no context receipt was recorded for the
group: savings are not measurable there, and `orc stats` never substitutes a
fabricated number (spec §13.5.1). The ratio is a **lower bound** on true
savings — `candidate_tokens` is the compiler's own candidate set; a true
no-compiler baseline would include items the compiler never considered.

Markdown is the default output; `--json` emits the rollups as a stable JSON
array (`savings_ratio` is `null` when unmeasured).

## A/B comparison (--group-by-profile)

`orc stats efficiency --group-by-profile` groups rollups by the profile label
recorded on each cost entry (`roles.<role>.routing.profile`, spec §9.22.5;
entries without a label group together as `(unprofiled)`). To measure what
the token-saving features are worth:

1. Run the same task suite with the features enabled under one profile
   (e.g. `profiles.fast` with `context_profile = "aggressive"`).
2. Run it with them disabled under another profile label.
3. Compare the two groups' savings medians. MVP-10's 30% median
   context-token-reduction gate is evaluated from these rollups.
