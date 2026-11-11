# Sensitive merge score records

The coordinator guard reads score comments only from `scorer_authors` in
`scripts/merge-classes.json`, currently `poitee`. That account posts every
seat's score, so the login does not identify the model. A dedicated bot
identity is the follow-up.

Each score occupies a line in this format, with a full lowercase head SHA:

```text
pp-score head=<40 hex> seat=<seat-id> vendor=<openai|anthropic> score=<integer> author=<openai|anthropic>
```

The coordinator records the head branch's author vendor in `author=`.
This is trusted coordinator metadata, not a vendor inferred from the GitHub
account. All score records for the current head must agree on it.

Only these seats are allowed: `gpt-6.1-sol` and `gpt-6-astra` from OpenAI,
and `claude-fable-5-2` and `claude-opus-5-5` from Anthropic. A sensitive
PR needs two distinct allowed seats from the vendor opposite its author,
each scoring at least 9 on the exact head. Repeated seats count once.
OpenCode, Cursor, Grok, and Pi seats do not qualify.

Missing fields, malformed score lines, unknown seats or vendors, mismatched
seat vendors, same-vendor scoring, conflicting author metadata, and scores
below 9 on the current head refuse the merge. A score record is a line that starts with `pp-score `; other lines, including prose that mentions the token, are ignored;
valid scores for older heads do not count. GitHub errors or unreadable data
also refuse the merge. Owner and lane classifications are unchanged.
