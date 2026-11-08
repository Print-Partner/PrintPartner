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
`undo` and its `reason` stay omittable members, as above, so the contract
test lists them as members a body may omit. The replacement UI's History tab
(`/builds/{id}/history`, GRE-1101) reads the first page, loads older pages
with `cursor` on Show older entries, and shows each `undo` reason in words.
It offers no Undo yet: listing manual and printer bursts with their undo state
is GRE-1259.

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
