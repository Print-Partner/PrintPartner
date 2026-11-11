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
vendor. Since 2026-11-11 (owner decision) they are `claude-opus-5-5` (Anthropic) and `grok-4.7`
(xAI). A sensitive PR needs two allowed seats from two different vendors, neither of them the
PR author's vendor (OpenAI for Sol lanes), each scoring at least 9 on the exact head. Repeated
seats count once. Other seats do not qualify.

Missing fields, malformed score lines, unknown seats or vendors, mismatched
seat vendors, same-vendor scoring, conflicting author metadata, and scores
below 9 on the current head refuse the merge. A score record is a line that starts with `pp-score `; other lines, including prose that mentions the token, are ignored;
records for older heads do not count and are not checked against the current seat list, but they must still parse. GitHub errors or unreadable data
also refuse the merge. Owner and lane classifications are unchanged.
