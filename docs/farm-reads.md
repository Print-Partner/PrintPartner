# Farm reads

The farm reads give Today, Farm and Production the configured printers, the
printer Checkoff results, the unmatched prints and the send queue (GRE-1125).
They are part of the replacement Rust API, which mounts only with the
default-off `desktop_rebuild` feature compiled and `PP_DESKTOP_REBUILD` set to
`1` or `true`. With the flag off the routes answer 404. The live printer
stream, `GET /ws/printers`, is GRE-1014 and is not part of these routes.

| Route | Body | Generated client |
| --- | --- | --- |
| `GET /api/v2/printers` | `{printers: FarmPrinter[]}` | `listPrinters`, `FarmPrinterList` |
| `GET /api/v2/printer-checkoff` | `{results: CheckoffResult[]}` | `listPrinterCheckoff`, `CheckoffResultList` |
| `GET /api/v2/printer-checkoff/unattributed` | `{prints: UnmatchedPrint[]}` | `listUnmatchedPrints`, `UnmatchedPrintList` |
| `GET /api/v2/printer-send-queue` | `{items: QueuedSend[]}` | `readSendQueue`, `SendQueue` |

## Composition and admission

`pp_core::printer_status::StatusRoutes` mounts `pp_api::farm::farm_router` in
the Rust-only launcher, next to the Moonraker status route, once the gateway
hands over its `MoonrakerStatus`. Admission is `pp_api::accepted::Gate`:
loopback Host and Origin checks, then a session cookie or an API key. A desktop
session alone is refused with 401.

Each request reads the tenant's `printer.fleet`, `integrations`,
`printer.checkoff_links`, `printer.unattributed_prints` and
`printer.send_queue` settings in one deferred SQLite read
(`pp_storage::read_model::farm`). A setting over 4 MiB answers 500 `Farm
settings exceed the read limit`; a missing or malformed setting reads as an
empty list, as Node reads it.

## No writes, no polling

The routes write nothing. Node's `GET /printer-checkoff` repairs empty
awaiting links and its `GET /printer-send-queue` reconciles live sends first;
the Rust reads do neither. A row Node would repair or drop is skipped, and a
`sending` item stays `sending` until the send's owner reconciles it.

Printer state comes from the Moonraker watchers' read-only port
(`PrinterObservations`, GRE-449), never from a request to a printer. Monitoring
never sends, checks off or dispatches. The launcher test checks that the only
printer requests are status GETs and that every production table and setting
is value-identical after the reads.

## Printers

Each `FarmPrinter` has `id`, `name`, `model`, `integration` (`{id, kind}` with
kind `moonraker`, `prusalink` or `bambu`, or `null`), `state`, `reason`,
`last_heard_at`, `progress_percent` and `loaded_filaments` (one row per slot,
1 to 4, as Node's `ensureSlots` fills them). Bodies carry no host address, API
key, file path or printer error text.

`state` is the generated `PrinterStateView`: `execution` and `bed_clear` are
separate facts and `display` is `pp_domain::printer_display_state`'s summary of
the two.

| Printer | `execution` | `reason` |
| --- | --- | --- |
| No `integration_id` | `not_set_up` | `not_set_up` |
| `integration_id` names no integration, or a non-printer one such as Spoolman | `not_set_up` | `integration_missing` |
| Integration disabled | `manual` | `monitoring_disabled` |
| PrusaLink or Bambu (no Rust watcher yet), or an HTTPS or redirecting Moonraker host | `manual` | `monitoring_not_supported` |
| Moonraker with no status yet, or monitoring stopped | `unknown` | `awaiting_status` |
| Moonraker that did not answer its last poll | `unreachable` | `unreachable` |
| Moonraker `printing`, `paused` | `printing`, `paused` | `null` |
| Moonraker `idle` or `complete` | `idle` | `null` |
| Moonraker `error` | `failed` | `null` |
| Moonraker `unknown` | `unknown` | `null` |

`last_heard_at` is the time of the last successful status poll, kept while the
printer is unreachable and `null` when no watcher has heard from it.
`progress_percent` is a whole percent while printing or paused, else `null`.

`bed_clear` is `clear` only after an operator confirmation: a verified result
on that printer whose `applied_at` is not older than any later finished print.
It is `needs_clearing` while printing or paused, when Moonraker reports
`error`, when an awaiting or failed result or an open unmatched print on that
printer is newer than the last verification, and when Moonraker reports
`complete` for a file other than the latest verified result's (including a job
run outside Print Partner, which has no result) or for that file but entered
`complete` after the verification was applied (a direct reprint of the same
file). The watcher records when each host entered its last answered state. An
`offline` or `unknown` spell under five minutes that ends in the same state
does not move it, so a missed poll keeps a confirmed bed clear; a longer
outage could hide a reprint, so the host's `complete` counts as new. The record lives in this process
only, so after a restart a printer still reporting `complete` reads
`needs_clearing` until the operator confirms again: Moonraker's status carries
no job identity, and a part printed while Print Partner was stopped must not
read as cleared. Otherwise it is
`unknown`, which displays as Bed to clear.

## Checkoff results, unmatched prints and the send queue

- `CheckoffResult`: `id`, `profile_id`, `printer_id`, `integration_id`,
  `filename`, `state` (`watching`, `awaiting_verify`, `host_failed`,
  `dismissed`, `verified`; Node's legacy `applied` reads as `verified`),
  `host_outcome`, `unit_count`, `resolved_unit_count`, `created_at`,
  `completed_at`, `applied_at`. Every stored result, in stored order.
- `UnmatchedPrint`: open prints only (not claimed, not dismissed) that no
  watching, awaiting or verified result matches by integration and
  case-insensitive basename, as Node's `filterLinkedUnattributedPrints`
  does. `object_count` and `candidate_count` count the stored lists. The
  newest 100, in stored order.
- `QueuedSend`: `id`, `filename`, `printer_id`, `profile_id`, `state`
  (`queued`, `sending`, `done`, `error`, `cancelled`), `start`,
  `wait_for_idle`, `unit_count`, `created_at`, `updated_at`. The artifact path
  and error text stay server-side. Every stored item, in stored order.

## Tests

```sh
cargo test --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib read_model::farm
cargo test --manifest-path rust/Cargo.toml -p pp-drivers --lib farm_reads
cargo test --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild --test runtime_m0 farm_reads
cargo test --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild --lib client_routes_match_registrations
npm --prefix web run check --workspace @print-partner/rust-client
```
