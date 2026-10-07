# Review-thread reply contract

Every remediation answer to an **inline review comment** is posted as a REPLY on
that comment's thread — never as a standalone main-thread PR comment.

Owner directive (2026-10-07): "When commenting on fixed conversations, the
agent should comment in the actual conversation and not directly in the main
thread." A fix answered in the main thread strands the context: the reviewer
who opened the thread is never notified, the thread stays unresolved, and the
PR timeline fragments into an untraversable side channel.

## When to post where

| Content | Where | Mechanism |
|---|---|---|
| Answer/fixed-in reply to an inline review comment | That comment's thread | `pr-thread-reply` (below) |
| Generation summaries, formal resolutions of non-thread findings, gate verdicts | Main thread (issue comment) | `pr-comment` |
| Marking a thread resolved | The thread (GraphQL only) | `resolveReviewThread` mutation |

`review-threads` is **fetch-only** (read-only); it never posts or resolves.

## REST: reply on a review-comment thread (preferred)

The reply endpoint on a review comment. `comment-id` MUST be the thread's
TOP-LEVEL comment id (the REST `id` = the GraphQL `comments.nodes`
`databaseId` of the thread root): GitHub rejects this endpoint for a reply
id. Callers holding only a reply id should use `pr-thread-reply`, which
resolves the root via the comment's `in_reply_to_id`:

```sh
orc github gh-env -- gh api \
  "repos/{owner}/{repo}/pulls/{pull_number}/comments/{comment-id}/replies" \
  -f body="Fixed in <sha>: <explanation>"
```

The `orc github gh-env --` wrapper is REQUIRED: a bare `gh api` call
authenticates with ambient personal credentials and attributes the reply to
the personal account (CWE-863 — the App service identity is the only
authorized principal for PR mutations on this repo).

Verified field name: `body` (same as all comment-creation endpoints; the
response carries `html_url` of the new reply). The PREFERRED form is the
wrapper script, which routes through `orc_lib_gh_service` and fails closed
when the service identity is unavailable:

```sh
scripts/pr-thread-reply <pr-number> <comment-id> <body-file | -f body=...> \
  [-R owner/repo] [--resolve <thread-id>]
```

## GraphQL: addPullRequestReviewThreadReply (alternative)

Reply via the thread instead of the comment — same effect, different handle
(`thread-id` is the GraphQL `PRRT_...` id from `review-threads`). Must run
under `orc github gh-env --` for the service identity, same as above:

```sh
orc github gh-env -- gh api graphql -f query='
  mutation($threadId: ID!, $body: String!) {
    addPullRequestReviewThreadReply(input: { threadId: $threadId, body: $body }) {
      comment { id body url }
    }
  }' -F threadId="$THREAD_ID" -f body="$BODY"
```

The mutation returns only `comment` — selecting a `thread { ... }` field on
it fails GraphQL validation; query the thread separately if its state is
needed.

Note: `addPullRequestReviewThreadReply` takes a **thread** id, not a comment
id. `pr-thread-reply` uses the REST endpoint because the remediation loop
already has the comment `databaseId` from `review-threads` output; the REST
reply automatically lands in the same thread.

Known limitation: `pr-thread-reply --resolve` verifies thread membership
against the first 100 comments of the thread (`comments(first:100)`). A
comment nested deeper than 100 replies in its thread fails the identity
check (fail-closed). CodeRabbit threads are never that deep; no pagination
loop is implemented for this case.

## Resolving a thread

After the fix lands and the reply is posted, the thread can be resolved
(reviewers resolve authoritatively; the remediation loop may resolve threads
it has answered when project policy allows):

```sh
orc github gh-env -- gh api graphql -f query='
  mutation resolveReviewThread($threadId: ID!) {
    resolveReviewThread(input: { threadId: $threadId }) {
      thread { id isResolved }
    }
  }' -F threadId="$THREAD_ID"
```

`pr-thread-reply --resolve <thread-id>` posts the reply and then issues this
mutation in one call. See also [`graphql.md`](graphql.md) for the
`resolveReviewThread` shape and the "actionable thread" definition.

## Anti-pattern (the incident class)

```text
# WRONG: posting a NEW main-thread issue comment that quotes the finding
gh pr comment <pr> --body "Fixed the N+1 query flagged in the review"

# RIGHT: reply on the thread the finding lives in
scripts/pr-thread-reply <pr> <comment-id> reply.md
```

A main-thread comment that merely mentions an inline finding does not resolve
anything and is treated by convergence (`pr-convergence.md`) as noise: the
thread remains actionable until it is replied to in place and resolved.
