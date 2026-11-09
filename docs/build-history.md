# Build history

`GET /plans/{id}/history` returns one page of a Build's history, newest first.
It is part of the replacement Rust API, which mounts only with the default-off
`desktop_rebuild` feature compiled and `PP_DESKTOP_REBUILD` set to `1` or
`true`. With the flag off the route answers 404. Node has no such route.

`pp_core::rebuild_api::read_router` mounts it at `/` and `/api/v2`, beside the
Build and Checkoff reads, from the same `WriterOwner`. It shares the Accepted
Plan Checkoff router's admission (`pp_api::accepted::Gate`): loopback Host and
Origin checks, then a session cookie or an API key.

## Entries

Each page reads every source in one SQLite read transaction
(`pp_storage::read_model::history`). The read writes nothing.

| `type` | Source rows | `at` | `actor` | `undo` |
| --- | --- | --- | --- | --- |
| `revision` | `plan_revisions` | `accepted_at` | `user` | none |
| `snapshot` | `plan_snapshots` | `created_at` | `assistant` when `source` is `assistant`, else `user` | none |
| `checkoff_burst` | one `print_jobs` row, with its `print_job_parts` counted; `print_job_parts` with no job (Node's legacy outcome migration or a deleted job) group by `at` and link into one burst whose id is `outcomes-{first outcome id}` | `at` | `user` | `{"undoable": false, "reason": "no_undo_token"}` |
| `send` | the Build's items in the `printer.send_queue` setting, in any state | `created_at` | `user` | `{"undoable": false, "reason": "printer_command"}` |
| `decision` | `plan_decisions`, the rows `GET /plans/:id/decisions` returns | `created_at` | the row's `actor` (`user` or `assistant`) | none |

Every entry has:

- `id`: `{type}:{row id}`, unique within the Build.
- `type`, `at` (the stored timestamp, unchanged), `actor` and `summary`.
- `link`: `{"kind": type, "id": row id as a string}`, the record the entry opens.
- `undo`: on checkoff bursts and sends only.
- `restorable`: on snapshots only. `true` when the snapshot's `payload_json`
  records an `accepted_plan_revision_id` that names a revision of this Build,
  the check [restore as draft](#restore-a-snapshot-as-a-draft) makes before
  it answers `snapshot_not_restorable`. Node's user and assistant snapshots
  record none, so they read `false`. Restore can still refuse a restorable
  snapshot for the Build's current state, such as `inputs_changed`.

No checkoff burst can be undone from History yet. History does not read burst
undo tokens, so every burst reports `no_undo_token`, including a printer burst
the farm log can undo (GRE-1259 adds that). A send is a printer command, so it
is never undoable.

Old rows stay readable. A decision or snapshot with empty text gets a summary
from its label, kind or source. A timestamp that is not RFC 3339 is returned as
stored and sorts as the oldest entry. A missing, blank or malformed send queue
reads as no sends, as Node reads it. Queue items follow Node's `parseItem`:
an item with an unknown state, or a blank id, filename, artifact path or
printer, is skipped, and `profile_id` is read with JavaScript `Number()`, so
`"1"` names Build 1. An item with no `created_at` is skipped too: Node stamps
it with the time of each load, so it has no fixed place in History.

Not listed:

- Checkoff ticks made one unit at a time on the Checkoff tab. `print_progress`
  stores no time for them.
- Sends that Node has trimmed from the queue. It keeps a bounded tail of
  finished items.
- The fleet-wide farm log, which has its own route ([Farm log](farm-log.md)).

## Paging

| Query | Meaning |
| --- | --- |
| `limit` | Entries per page, 1 to 200. Default 50. Keys and values are URL-decoded first. |
| `cursor` | The previous page's `next_cursor`. |

```json
{"plan_id": 1, "entries": [...], "next_cursor": "djF8..."}
```

`next_cursor` is `null` on the last page. Entries sort by time, then type
(`revision`, `snapshot`, `send`, `checkoff_burst`, `decision`), then row id,
all descending. The cursor holds the sort key of the page's last entry, and
the next page starts after it. None of those fields change once a row exists.
Sends enter History when they are queued, so a later dispatch does not insert
one behind the cursor. An entry recorded while a client pages has a newer
time, so it sorts ahead of the cursor: walking the pages shows each existing
entry once and none twice. The client sees new entries when it reloads the
first page.

Each source is capped at 10,000 rows. A Build past the cap answers `500`
`Build history exceeds the read limit` rather than a partial history.

## Client and UI

The generated client names the route `readBuildHistory` (`BuildHistoryPage`).
`undo`, its `reason` and `restorable` stay omittable members, as above, so the
contract test lists them as members a body may omit. The replacement UI's
History tab (`/builds/{id}/history`, GRE-1101) reads the first page, loads
older pages with `cursor` on Show older entries, and shows each `undo` reason
in words. It offers no Undo yet: listing manual and printer bursts with their
undo state is GRE-1259.

On a snapshot whose `restorable` is `true`, the History tab offers Restore as
draft, as a button and as `R` anywhere in the snapshot's row (GRE-1260). Other
snapshots say Not restorable. The tab:

1. Reads the Build header and the open draft, if any, at that moment, and
   sends `restoreSnapshotAsDraft` with `mode: "draft"`, that `expected_base`
   and `expected_draft`, and a new `Idempotency-Key`.
2. Sends one restore at a time. A send whose answer is lost is resent with the
   same key and body; when every send is lost, the key stays while the
   History tab is open, so Restore again replays the first restore instead of
   making a second draft. A refusal drops the key and reloads History. A
   replay whose draft is no longer open says so and opens nothing.
3. On `200`, opens the Plan tab with the Accept gate on the restored draft.
   The gate names the snapshot and says printed units and printer commands
   stay as they are. Nothing is published until Accept.
4. Says each refusal in the table below in words and moves focus to it. A
   `404` for the Build shows Not found.

## Responses

Errors are RFC 9457 problems (`application/problem+json`, see `pp_api::problem`) whose `detail` is the text below.

| Case | Response |
| --- | --- |
| Build found | `200` with the page |
| Missing or cross-tenant Build, or an id that is not a positive decimal integer | `404` `Profile not found` |
| `limit` outside 1 to 200, or repeated | `400` `Invalid history limit` |
| `cursor` not written by this route, or repeated | `400` `Invalid history cursor` |
| Bad or missing credential | `401` `Authentication required` |
| Writer queue full or storage stopped | `503` `Service temporarily unavailable` |
| More than 10,000 rows in one source | `500` `Build history exceeds the read limit` |

## Tests

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
  --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(build_history)'
```

`build_history_reads_write_nothing` compares every table row and every managed
file under the data directory before and after a full paged walk.
`pp-core`'s `rebuild_read_http` test pages the route through the composed
`read_router` and checks each refusal in the table above.

## Restore a snapshot as a draft

`POST /plans/{id}/snapshots/{snapshot_id}/restore` (GRE-1094) turns a Plan
snapshot into a new open draft. Only Accept publishes it, through the same gate
as any draft. It is mounted on the draft router (`pp_api::drafts`, root and
`/api/v2`) under the same `desktop_rebuild` and `PP_DESKTOP_REBUILD` gates,
needs an `Idempotency-Key`, and is `restoreSnapshotAsDraft` in
`@print-partner/rust-client`. Node's route of the same path restores Source
layers and the kit manifest in place; under the Rust runtime that mode does not
exist.

```json
{"mode": "draft", "expected_base": {"revision_id": 2, "plan_version": 2},
 "expected_draft": null}
```

`mode` accepts only `draft`. `expected_draft` is the open draft the caller
saw, or `null` for none. A 200 answers the restored draft's `DraftWorkspace`,
with Required units selected as a recomputed draft's are; the gate previews and
accepts that draft.

One immediate transaction (`pp_storage::working_drafts::restore`):

1. A draft this actor created with the same key answers again when the request
   is the same (Plan, snapshot, expected base and draft), and `409
   idempotency_conflict` otherwise. The draft's `idempotency_key` holds
   `snapshot-restore-v1:<key SHA-256>:<request digest>`; no schema step.
2. The snapshot must belong to this Build and tenant.
3. The Accepted base and the observed draft are checked as for a draft header
   save.
4. The snapshot must record an Accepted revision of this Build
   (`accepted_plan_revision_id`, which every `pre_apply` snapshot from Rust
   Accept has). Node's user and assistant snapshots hold Source names and kit
   choices only, which a draft cannot hold.
5. The revision's inputs must name the Build's current Source layers, as Accept
   requires.
6. The current Accepted revision must have a Required-unit set; without one
   the restore answers the selection's `422 required_unit_set_unavailable`.
7. The draft's inputs and Parts are the revision's. Each Part's predecessor
   (`base_revision_part_id`) is the current Accepted Part that holds its
   Required-unit tokens, so the selection reuses them when the Part is
   otherwise unchanged. A Part the current Plan no longer has is new, and
   Accept gives it new tokens; a retired token is never reused. Each Part
   keeps the revision's `included` flag, role override, filament color,
   custom hex and Spoolman spool. Accept publishes the revision's values, except
   that a Part with a predecessor takes that Part's live filament, as every
   Accept does (GRE-479).
8. A current Part with printed (completed) units must keep at least that many
   units in the restored draft. This is a quantity check only. When a restored
   Part's artifact, Source or role differs from its predecessor's, the
   selection reports `unsafe_predecessor`, and a `replace` decision at that
   gate mints fresh tokens and retires the printed ones. Accept's preview lists
   those units under `printed_affected` before anything is published.
9. The Build header the snapshot saved (`plan_snapshot_headers`), if any, is
   checked as a header save checks it. An empty order number, customer or
   due date reads as none, as Node stores an unset field. Any other failing
   field answers `422 invalid_header` with `field` and `reason`.
10. Every open draft is abandoned (it stays rebasable), and the new draft gets
    the checked header, if any, as header version 1. Accept then needs
    `expected_header_version: 1`.

Every refusal comes before the first write, so a refused restore leaves the
open draft as it was.

Restore writes only draft rows and `plan.draft_changed` events. Accepted
revisions, `parts`, `print_progress`, Required units, Checkoff bursts
(`print_jobs`, `print_job_parts`), the send queue, Checkoff links and Plates
are unchanged until Accept, and Accept leaves Checkoff bursts, sends and Plate
history as they were. Accept rewrites Checkoff coordinates, under
`remap_checkoff_links`, only for links still `watching` or
`awaiting_verify` and sends still `queued`, `sending` or in `error`. A finished send's
`checkoff_units` and a settled link's `units` keep the `{part_id,
unit_index}` they were recorded with. Each Accept projects the revision onto
new `parts` rows, so such a coordinate names the Part row of the revision it
was recorded against, and through that revision it still resolves to the same
Required-unit token.

| Case | Response |
| --- | --- |
| Restored, or a replay | `200` `DraftWorkspace` |
| Missing `Idempotency-Key`, bad ids, body or `mode` | `400` |
| Build missing or another tenant's | `404` `profile_not_found` |
| Snapshot missing, or another Build's or tenant's | `404` `snapshot_not_found` |
| Archived Build | `422` `build_archived` |
| No Accepted baseline, stale base or draft | `409` `accepted_baseline_required`, `base_changed`, `draft_changed` |
| Snapshot records no Accepted revision of this Build | `409` `snapshot_not_restorable` |
| Source layers differ from the snapshot's | `409` `inputs_changed` |
| Current revision has no Required-unit set | `422` `required_unit_set_unavailable` |
| Snapshot header fails a field rule | `422` `invalid_header` with `field`, `reason` |
| Restored quantities cannot hold the printed units | `409` `printed_units_affected` with `tokens` |
| Same key, different request | `409` `idempotency_conflict` |

Whole-installation backup restore is a different operation.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
  --features pp-storage/desktop_rebuild,pp-core/desktop_rebuild -E 'test(restore_as_draft)'
```

`restore_as_draft_creates_a_draft_that_accept_publishes_without_rewinding_evidence`
accepts a change, restores the saved snapshot, restarts, replays the restore
key, and accepts the restored draft. At each step it compares the Accepted
Plan, Checkoff, sends, Plates and every Required unit's token and progress, and
resolves the Checkoff coordinates of a finished send and a verified link to
their Required-unit tokens. Those tokens stay the same, and their units stay
in the Plan, still completed. The other tests cover the refusals, the
snapshot header check, a revision with no Required-unit set and the
printed-unit rule.
