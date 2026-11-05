# Farm log

The farm log is the Farm screen's record of farm actions across the whole
fleet, newest first, with the undo each entry allows (GRE-1126). It is part of
the replacement Rust API, which mounts only with the default-off
`desktop_rebuild` feature compiled and `PP_DESKTOP_REBUILD` set to `1` or
`true`. With the flag off the routes answer 404. Node has no such route.

| Route | Body | Generated client |
| --- | --- | --- |
| `GET /api/v2/farm-log` | `FarmLogPage` | `listFarmLog` |
| `POST /api/v2/farm-log/{id}/undo` | `FarmLogEntry` (the appended undo entry) | `undoFarmLogEntry` (`routePath` percent-encodes the string `id`) |

Both routes are also served without the `/api/v2` prefix, like the burst
routes beside them in `pp_api::checkoff::checkoff_router`. Admission is
`pp_api::accepted::Gate`: loopback Host and Origin checks, then a session
cookie or an API key. Each request runs as one writer transaction in
`pp_storage::checkoff_progress`, so a page and an undo see the same state.

## What is logged

| `kind` | Source | Undo |
| --- | --- | --- |
| `checkoff_burst` | An entry storage appends to the `printer.farm_log` setting in the same transaction as a new `POST /printer-checkoff/verify-batch` burst | Allowed while the burst's undo checks pass |
| `checkoff_burst_undo` | An entry appended in the same transaction as a successful burst undo, through either undo route | Never (no redo) |
| `send` | The existing items of the `printer.send_queue` setting, read as the farm reads parse them; items without `created_at` are skipped | Never: dispatch and cancel are printer commands |

The log copies nothing it can reuse. A burst entry stores its sequence
number, time, undo token, Build and printer ids, unit counts and bed count; the
burst receipt in `printer.checkoff_bursts` stays the only copy of what undo
restores. A send entry is the queue item itself, so a cancelled send shows
`send_state: "cancelled"` on its one entry rather than a second record.

An idempotent replay of a burst returns its stored receipt before any write,
so it appends nothing. A refused burst or refused undo rolls its transaction
back and appends nothing. The log keeps the newest 10,000 entries
(`MAX_ENTRIES`); sequence numbers keep increasing after older entries are
dropped. The farm log adds no queue reflow, bulk move, requeue or redo.

## Entries

Every `FarmLogEntry` has `id` (`checkoff_burst:{seq}`,
`checkoff_burst_undo:{seq}` or `send:{queue item id}`), `kind`, `at`
(RFC 3339), `summary`, `build_ids`, `printer_ids`, `units_confirmed` and
`units_rejected` (bursts only), `undoes` (an undo's burst entry, when still
kept), `send_state` (sends only) and `undo`:

| `undo` | Meaning |
| --- | --- |
| `{"undoable": true, "reason": null, "detail": null}` | `POST /api/v2/farm-log/{id}/undo` would succeed now |
| `already_undone` | The burst was undone |
| `receipt_expired` | The burst's receipt is older than the 100 receipts storage keeps |
| `state_changed` | A link, progress row or outcome the burst wrote, or a later verify or progress change that depends on its units, changed since; `detail` is the undo route's refusal text |
| `archived` | A Build the burst touched is archived |
| `printer_command` | A send |
| `undo_entry` | An undo entry |

The undo state of a burst entry is computed for the page by the same checks
the undo runs (`burst::undo_blocked`), without writing.

## Undo

`POST /api/v2/farm-log/{id}/undo` takes no body. For a burst entry it runs the
compensating burst undo (see [Checkoff bursts](desktop-rebuild.md#checkoff-bursts-gre-1090))
and answers the `checkoff_burst_undo` entry it appended. It answers 409 with
the reason when the burst was already undone or its touched state changed,
409 `A printer command cannot be undone` for a send, 409 `An undo cannot be
undone` for an undo entry, and 404 `Farm log entry not found` (code `farm_log_entry_not_found`)
for an id that is not in the tenant's log or send queue, whatever its prefix. A burst entry whose receipt is gone answers the burst
undo's 404 `Checkoff burst not found`. No printer, webhook or Spoolman call
happens.

## Paging

| Query | Meaning |
| --- | --- |
| `limit` | Entries per page, 1 to 200. Default 50. |
| `cursor` | The previous page's `next_cursor`. |

Entries sort by time, then id, both descending. The cursor holds the sort key
of the page's last entry and the next page starts after it; `next_cursor` is
`null` on the last page. A new entry has a newer time and sorts ahead of any
cursor, so walking the pages shows each existing entry once. Other query
parameters are ignored; a repeated `cursor` or `limit`, a limit out of range
or a malformed cursor is a 400.

## Tests

```sh
cargo test --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test farm_log
cargo test --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib farm_log
cargo test --manifest-path rust/Cargo.toml -p pp-api --features desktop_rebuild --test checkoff_bursts_http
cargo test --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild --lib client_routes_match_registrations
npm --prefix web run check --workspace @print-partner/rust-client
```
