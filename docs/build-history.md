# Build history

`GET /plans/{id}/history` returns one page of a Build's history, newest first,
and `POST /plans/{id}/history/{entry_id}/undo` undoes a checkoff burst it lists
(GRE-1259). Both are part of the replacement Rust API, which mounts only with the default-off
`desktop_rebuild` feature compiled and `PP_DESKTOP_REBUILD` set to `1` or
`true`. With the flag off the routes answer 404. Node has no such routes.

`pp_core::rebuild_api::read_router` mounts both at `/` and `/api/v2`, beside the
Build and Checkoff reads, from the same `WriterOwner`: the read in
`pp_api::history`, the undo in `pp_api::checkoff` with the other burst undo
routes. Both use the Accepted Plan Checkoff router's admission
(`pp_api::accepted::Gate`): loopback Host and Origin checks, then a session
cookie or an API key.

## Entries

Each page reads every source in one SQLite read transaction
(`pp_storage::read_model::history`). The read writes nothing; it reads the
Accepted Plans under the data directory's `repos` to check burst undo.

| `type` | Source rows | `at` | `actor` | `undo` |
| --- | --- | --- | --- | --- |
| `revision` | `plan_revisions` | `accepted_at` | `user` | none |
| `snapshot` | `plan_snapshots` | `created_at` | `assistant` when `source` is `assistant`, else `user` | none |
| `checkoff_burst` (printer) | one `print_jobs` row, with its `print_job_parts` counted; `print_job_parts` with no job (Node's legacy outcome migration or a deleted job) group by `at` and link into one burst whose id is `outcomes-{first outcome id}` | `at` | `user` | the burst's undo state, below |
| `checkoff_burst` (manual) | one receipt in the `checkoff.manual_bursts` setting whose `profile_id` is the Build, undone or not; its id is `manual-{SHA-256 of its idempotency key}` | the receipt's `at` | `user` | the burst's undo state, below |
| `send` | the Build's items in the `printer.send_queue` setting, in any state | `created_at` | `user` | `printer_command` |
| `decision` | `plan_decisions`, the rows `GET /plans/:id/decisions` returns | `created_at` | the row's `actor` (`user` or `assistant`) | none |

Every entry has:

- `id`: `{type}:{row id}`, unique within the Build.
- `type`, `at` (the stored timestamp, unchanged), `actor` and `summary`.
- `link`: `{"kind": type, "id": row id as a string}`, the record the entry opens.
- `undo`: on checkoff bursts and sends only. `{"undoable": true}`, or
  `{"undoable": false, "reason": ..., "detail": ...}`, where `detail` is the
  text the undo route would refuse with. An undoable printer burst whose
  burst covered more than this one bed of the Build also has `scope`, such as
  `Undo reverses the whole burst: 3 beds on Builds 2 and 5`.

A manual burst's summary counts what its receipt changed, for example
`Checked off 2 units, assembled 1` or `Unchecked 1 unit`. A printer burst's
counts its outcomes: `Checked off 2 units, rejected 1 from plate_1.gcode`.

### Undo state

A burst's `undo` is what its own undo would answer now. History runs the same
checks as the undo routes, writing nothing, for the entries on the page only:
`burst::undo_blocked` for a printer burst, `manual::undo_blocked` for a manual
one. A printer burst finds its receipt in `printer.checkoff_bursts` through
the receipt's outcome ids: the job those `print_job_parts` rows name is the
entry. History never shows an undo token.

| `reason` | When | `detail` |
| --- | --- | --- |
| none, `undoable: true` | Every check passes | none |
| `no_undo_token` | Storage keeps no undo for the burst: a single verify, an `outcomes-` group or row from Node, or a printer burst whose receipt is older than the newest 100 kept | `No undo is recorded for this Checkoff burst` |
| `already_undone` | The receipt is marked undone. Only manual bursts show it: a printer burst's undo deletes the job it is listed from | `This Checkoff burst was already undone` |
| `state_changed` | The Accepted Plan, a link or progress row the burst wrote, or something that depends on its units changed since | the undo's reason, such as `Checkoff changed since this burst: a later change on its Parts depends on it` |
| `archived` | A Build the burst touched is archived | `Archived Plan Progress cannot be changed` |
| `printer_command` | Every send: a printer command cannot be taken back | `A printer command cannot be undone` |

History has no `receipt_expired`: it cannot tell a printer burst whose receipt
was dropped from one that never had a receipt, so both are `no_undo_token`.
A manual burst is listed from its receipt, so it leaves History when its
receipt is dropped (each setting keeps the newest 100).

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
- A printer burst that was undone: its undo deletes its `print_jobs` and
  `print_job_parts` rows. The farm log keeps a record of the undo.

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

The generated client names the routes `readBuildHistory` (`BuildHistoryPage`)
and `undoBuildHistoryEntry` (`BuildHistoryUndo`; `routePath` percent-encodes
the string `entry_id`). `undo`, its `reason`, `detail` and `scope` are omittable
members, as above, so the contract test lists them as members a body may omit.
The replacement UI's History tab (`/builds/{id}/history`, GRE-1101) reads the
first page, loads older pages with `cursor` on Show older entries, and shows
each `undo` reason in words, with the core's detail for `state_changed`. A
missing Build (`404`) shows Not found and is not retried.

An undoable burst gets an Undo button (GRE-1259). It works as the farm log's
does: one bodyless POST at a time (clicks while it is in flight send nothing),
a lost send is resent, and if every send is lost the tab says the undo may not
have reached the core. After a lost send, a resend answered `409` `This
Checkoff burst was already undone` (a manual burst) or `404` `History entry
not found` (a printer burst, whose undo removed its entry) means the first
send landed, so the tab counts the undo as done. An undo's `scope` shows
beside its Undo button. Focus moves to the outcome message. After an undo the
tab reloads every core read: History, the Build headers, Checkoff, Today and
the farm log.

## Undo

`POST /plans/{id}/history/{entry_id}/undo` takes no body. `entry_id` is an
entry's `id`, such as `checkoff_burst:manual-…`. Storage finds the entry in
the Build's History, then runs the burst's own undo
(`burst::undo_batch` or `manual::undo`) in one Checkoff writer transaction,
so the undo token never leaves the core and the same domain events and farm
log entry are written as through `POST /printer-checkoff/verify-batch/undo`
or `POST /plans/{id}/checkoff/bursts/undo`. A refusal writes nothing.

```json
{"undone_at": "2026-11-07T14:31:01.906Z", "entry": {"id": "checkoff_burst:manual-…", "undo": {"undoable": false, "reason": "already_undone", "detail": "This Checkoff burst was already undone"}, ...}}
```

A printer burst's undo covers every bed and Build in that burst, as
`POST /printer-checkoff/verify-batch/undo` does: one burst is one undo, so
undoing one of its History entries removes the entries for its other beds too,
including those on other Builds. `undo.scope` says so before the click.

`entry` is the entry as History lists it after the undo: a manual burst stays,
marked undone. It is `null` for a printer burst, whose undo removes it.

| Case | Response |
| --- | --- |
| Undone | `200` with the body above |
| Missing or cross-tenant Build, or an id that is not a positive decimal integer | `404` `Profile not found` |
| No entry with that id in the Build's History | `404` `History entry not found` |
| A burst with no undo recorded | `409` `No undo is recorded for this Checkoff burst` |
| A send | `409` `A printer command cannot be undone` |
| A revision, snapshot or decision | `409` `This History entry cannot be undone` |
| Already undone, changed since, or archived | `409` with the burst undo's own detail, as the undo state table above |
| Bad or missing credential | `401` `Authentication required` |

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
`pp-storage`'s `build_history_undo` tests list manual and printer bursts with
each undo state and undo them, and `pp-api`'s `history_undo_http` tests the
undo route's answers.
