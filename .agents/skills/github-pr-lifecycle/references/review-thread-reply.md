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

The reply endpoint on a review comment. `comment-id` is the REST `id` (=
`databaseId` from the GraphQL `comments.nodes`):

```sh
gh api "repos/{owner}/{repo}/pulls/{pull_number}/comments/{comment-id}/replies" \
  -f body="Fixed in <sha>: <explanation>"
```

Verified field name: `body` (same as all comment-creation endpoints; the
response carries `html_url` of the new reply). This is exactly what
`pr-thread-reply` posts via `orc_lib_gh_service`:

```sh
scripts/pr-thread-reply <pr-number> <comment-id> <body-file | -f body=...> \
  [-R owner/repo] [--resolve <thread-id>]
```

## GraphQL: addPullRequestReviewThreadReply (alternative)

Reply via the thread instead of the comment — same effect, different handle
(`thread-id` is the GraphQL `PRRT_...` id from `review-threads`):

```sh
gh api graphql -f query='
  mutation($threadId: ID!, $body: String!) {
    addPullRequestReviewThreadReply(input: { threadId: $threadId, body: $body }) {
      comment { id body url }
      thread { id isResolved }
    }
  }' -F threadId="$THREAD_ID" -f body="$BODY"
```

Note: `addPullRequestReviewThreadReply` takes a **thread** id, not a comment
id. `pr-thread-reply` uses the REST endpoint because the remediation loop
already has the comment `databaseId` from `review-threads` output; the REST
reply automatically lands in the same thread.

## Resolving a thread

After the fix lands and the reply is posted, the thread can be resolved
(reviewers resolve authoritatively; the remediation loop may resolve threads
it has answered when project policy allows):

```sh
gh api graphql -f query='
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
