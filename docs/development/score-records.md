# Sensitive merge score records

The coordinator guard reads score comments only from `scorer_authors` in
`scripts/merge-classes.json`, currently `poitee`. That account posts every
seat's score, so the login does not identify the model. A dedicated bot
identity is the follow-up.

Each score occupies a line in this format, with a full lowercase head SHA. `vendor=`
takes a name from `vendors` (the `vendors` list in `scripts/merge-classes.json`). `author=`
takes one of those names, or `cursor-auto` or `unknown`. `obstacles=` comes last:

```text
pp-score head=<40 hex> seat=<seat-id> vendor=<anthropic|deepseek|openai|xai|zhipu> score=<integer> author=<anthropic|deepseek|openai|xai|zhipu|cursor-auto|unknown> obstacles=<none|refusal,guard,approval-stall>
```

`obstacles=` is the scorer's disclosure of what it hit while scoring (Chad, 2026-11-17): `refusal`
(the model refused part of the task), `guard` (`scripts/agent-guard.py` refused a command), and
`approval-stall` (a tool approval stalled). Write `none`, or the ones hit, comma-separated, each
once, in any order. A score that leaves out an obstacle the scorer hit is invalid. The parsers
cannot check that `none` is true, so the scorer brief carries that rule; they make the field
required. Listing an obstacle does not fail the score by itself. A line without the field, or with
any other token, does not count for `pp/score`, and on the current head it refuses a sensitive
merge. Records for older heads written before the field existed still parse.

`score=` is a whole number; `8.5` does not parse. A `pp-score-trial` line is not a record.

A scorer thread posts its own record from its scorer worktree (branch
`t3/score-<pr>-<seat>-<yyyymmdd>`); see [Safety guard](agent-workflow.md#safety-guard).

The coordinator records the head branch's author family in `author=`.
This is trusted coordinator metadata, not a vendor inferred from the GitHub
account. All score records for the current head must agree on it.
`cursor-auto` is for Cursor Auto-authored PRs. `unknown` is for an author family
that cannot be named. Those two are not seat vendors.

The allowed seats are data: `scorer_seats` in `scripts/merge-classes.json` maps each seat to its
vendor. Since 2026-11-12 (owner decision) they are `claude-opus-5-5` (Anthropic), `grok-4.7`
(xAI), `glm-5.3-flash-free` (Zhipu) and `deepseek-v4.1-flash-free` (DeepSeek). OpenCode runs the
last two as `opencode/glm-5.3-flash-free` and `opencode/deepseek-v4.1-flash-free`, variant max.
Since 2026-11-18 (owner decision) `gpt-6.2-sol` (OpenAI) and `composer-2.5` (Cursor) are also
sensitive-merge seats, and on 2026-11-18 `gpt-6.2-astra` (OpenAI) joined them. `gpt-6.1-sol` is not.
A sensitive PR needs two allowed seats from two different vendors, neither of them the PR author's
vendor (Anthropic for Claude lanes, OpenAI for Sol lanes), each scoring at least 9 on the exact
head. Repeated seats count once. Other seats do not qualify.

Missing fields, malformed score lines, unknown seats or vendors, mismatched
seat vendors, same-vendor scoring, conflicting author metadata, and scores
below 9 on the current head refuse the merge. A score record is a line that starts with `pp-score ` at column 0; other lines, including prose that mentions the token, are ignored. Prose that happens to start a line with `pp-score ` is read as a record, fails to parse and refuses the merge, the safe side;
records for older heads do not count and are not checked against the current seat list, but they must still parse. GitHub errors or unreadable data
also refuse the merge. Owner and lane classifications are unchanged.

Never edit a score record; post a new one. The guard refuses a sensitive merge when a trusted comment holding a record for the current head has `updated_at` different from `created_at`; edits to records for older heads do not matter, because those never count. Through the `gh` routes (`gh pr comment`, `gh issue comment`, `gh api` and its GraphQL endpoint), the guard refuses edits and deletes of PR comments outside the coordinator worktree: any method but GET or HEAD on `issues/comments/<id>` or `pulls/comments/<id>` (including POST, fields without `-X`, and an `X-HTTP-Method-Override` header), `--edit-last`, `--delete-last`, and the GraphQL update and delete comment mutations. A program that calls the GitHub API with the account's token directly is not covered, and a deleted record cannot be detected, until score records move to an identity lanes cannot use (GRE-1408).

The allowed seats (`scorer_seats`, seat to vendor) and the vendors a record may name (`vendors`) are data in `scripts/merge-classes.json`; the record pattern is built from them. Removing a vendor makes old records that name it unparseable, so a PR that carries one refuses the merge.

When a trusted `scorer_authors` account posts a well-formed record, the `post-pp-score-status` workflow also posts a `pp/score` commit status on that exact head SHA (`N/10 from <seat>`, success at 8 or more). The status is copied from the comment; it is never invented or re-derived. Records for other SHAs are left untouched. For this status only:

- A record may be wrapped in inline-code backticks (`` `pp-score ...` ``) or sit inside a code fence; surrounding whitespace and backticks are dropped before the line is matched. A record embedded in prose is still ignored.
- A record whose `vendor=` equals its `author=` is a same-family score and fails the head, as does a head whose records disagree on `author=`. The old `Score: N/10 from <seat> on head <sha>` form names no author family and no longer counts.
- If `author=` is `cursor-auto` or `unknown`, the head also needs a trusted Anthropic record and a trusted OpenAI record (`gpt-6.1-sol` or another OpenAI seat) on that exact SHA. The status uses the lower of those scores and succeeds only at 8 or more. A missing family, a score below 8, or a complete pair on a stale SHA fails closed, including on `merge_group`. Known families are unchanged: one trusted cross-family score of 8 or more is enough, and same-family scores still fail.
- `pp_score_seats` in `scripts/merge-classes.json` adds seats the status accepts on top of `scorer_seats`. It holds the Codex seats `gpt-6.1-sol`, `gpt-6.2-sol` and `gpt-6.2-astra` (all OpenAI; 6.2-sol added 2026-11-17; 6.2-sol and 6.2-astra are also in `scorer_seats` since 2026-11-18). The sensitive merge guard still reads `scorer_seats` only.
- The `post-pp-score-status` workflow runs the parser from the base branch, never from the PR head. A change to the record format takes effect for every PR once it merges, including the PR that makes it.
- A head with no records of its own can carry the records of an earlier head H0 across clean merges of main (Chad, 2026-11-18 11:02 PM ET). Each hop to a new head H1 must hold all of these: H1's parents are exactly `[H0, M]` with M on `main` (GitHub's update-branch or `git merge origin/main`; a rebase or force-push does not qualify); `git merge-tree --write-tree` of H0 and M is clean and its tree equals H1's tree, so the merge has no hand edits; and every required check of `main`'s rulesets other than `pp/score` (read from GitHub, today `actionlint`, `zizmor` and `rust (linux)`) passed on H1, where `skipped` counts as passing as it does for GitHub. A chain H0 to H1 to H2 carries only if every hop holds, and the walk stops at the first head that has records, so fresh records always win. The carried status reads `carried from <sha7>: N/10 from <seat>`. Grok records never carry, a same-family record still fails, and when the PR touches a sensitive or owner path every carried seat must score above 8. Anything else needs fresh scores. The merge base comes from GitHub's compare API, and the script fetches only the commits it needs, so the shallow workflow checkout works.
- The workflow runs on a new comment, on `workflow_dispatch` and on `merge_group`; a push alone does not re-run it. After an update-branch, once CI is green on the new head, dispatch the workflow with the PR number (or post any comment) to post the carried status. `--carry-only` previews the carry for the current head as if it had no records.
- On `merge_group`, the workflow checks out the queue entry's base SHA, reads the PRs in the group from its squash commits (`... (#N)`), and posts `pp/score` on the merge group SHA. It posts success only when every PR has a trusted score of 8 or more on its current head; otherwise, or when anything cannot be read, it posts failure naming the PR or the reason, so the entry leaves the queue instead of waiting for the timeout.

`pp/score` is a plain commit status. While scores post as `poitee`, any account or workflow with write access to statuses can also post `pp/score=success`; a dedicated scorer identity was deferred by the owner on 2026-11-16.
