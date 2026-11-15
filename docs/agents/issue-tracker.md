# Issue tracker: Linear

Issues for this repo live in Linear, team **GRE**. Use the Linear MCP tools; never `gh issue`.

## Conventions

- **Create**: `save_issue` with `team: "GRE"`. Check `list_issues` for a duplicate first.
- **Read**: `get_issue`, then `list_comments` for the discussion.
- **Labels**: add or remove with `addLabels` / `removeLabels`. Never pass `labels`, which replaces the whole set.
- **States**: Backlog, Todo, In Progress, In Review, Done, Canceled, Duplicate. `linear-pr-sync.yml` moves a ticket when its PR opens or merges; don't set those states by hand.
- **Blocking**: use Linear's native "blocked by" relation on `save_issue`.
- **PR link**: a PR body says `Fixes GRE-n`. Leftover work becomes a child ticket that quotes the leftover verbatim, never `Refs`.
- **Archive**: Linear has no archive action through the MCP. Set state Canceled and add an `archived-YYYY-MM-DD` label.
- Tickets labelled `umbrella` are indexes, not units of work. Don't implement them.
- Other products share the GRE team. Leave tickets that aren't Print Partner alone.

## PRs as a request surface

No.

## Wayfinding operations

Used by `/wayfinder`.

- **Map**: one issue labelled `wayfinder:map`.
- **Child ticket**: a Linear sub-issue of the map (`parentId`), labelled `wayfinder:<type>` (`research`/`prototype`/`grilling`/`task`).
- **Blocking**: native "blocked by" relations.
- **Frontier query**: the map's open children with no open blocker and no assignee; first in map order wins.
- **Claim**: assign to `me`, the session's first write.
- **Resolve**: comment the answer, set Done, then add a gist and link to the map's Decisions-so-far.
