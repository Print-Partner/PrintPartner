# Sensitive merge score records

The coordinator guard reads score comments only from `scorer_authors` in
`scripts/merge-classes.json`, currently `poitee`. That account posts every
seat's score, so the login does not identify the model. A dedicated bot
identity is the follow-up.

Each score occupies a line in this format, with a full lowercase head SHA:

```text
pp-score head=<40 hex> seat=<seat-id> vendor=<openai|anthropic|xai> score=<integer> author=<openai|anthropic|xai>
```

The coordinator records the head branch's author vendor in `author=`.
This is trusted coordinator metadata, not a vendor inferred from the GitHub
account. All score records for the current head must agree on it.

The allowed seats are data: `scorer_seats` in `scripts/merge-classes.json` maps each seat to its
vendor. Since 2026-11-12 (owner decision) they are `claude-opus-5-5` (Anthropic), `grok-4.7`
(xAI), `glm-5.3-flash-free` (Zhipu) and `deepseek-v4.1-flash-free` (DeepSeek). OpenCode runs the
last two as `opencode/glm-5.3-flash-free` and `opencode/deepseek-v4.1-flash-free`, variant max.
`gpt-6.1-sol` is not a seat; `openai` stays a vendor because Sol-authored PRs name it in `author=`.
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
