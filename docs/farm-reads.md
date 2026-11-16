# Farm reads

The farm reads give Today, Farm and Production the configured printers, the
printer Checkoff results, the unmatched prints and the send queue (GRE-1125).
They are part of the replacement Rust API, which mounts only with the
default-off `desktop_rebuild` feature compiled and `PP_DESKTOP_REBUILD` set to
`1` or `true`. With the flag off the routes answer 404. The live printer
stream, `GET /ws/printers`, is GRE-1014 ([Printer stream](printer-stream.md))
and sends the same projection.

| Route | Body | Generated client |
| --- | --- | --- |
| `GET /api/v2/printers` | `{printers: FarmPrinter[]}` | `listPrinters`, `FarmPrinterList` |
| `GET /api/v2/printer-checkoff` | `{results: CheckoffResult[]}` | `listPrinterCheckoff`, `CheckoffResultList` |
| `GET /api/v2/printer-checkoff/unattributed` | `{prints: UnmatchedPrint[]}` | `listUnmatchedPrints`, `UnmatchedPrintList` |
| `GET /api/v2/printer-checkoff/unattributed/suggestions` | `{suggestions: MatchSuggestion[]}` | `listMatchSuggestions`, `MatchSuggestionList` |
| `GET /api/v2/printer-send-queue` | `{items: QueuedSend[]}` | `readSendQueue`, `SendQueue` |

## Composition and admission

`pp_core::printer_status::StatusRoutes` mounts `pp_api::farm::farm_router` in
the Rust-only launcher, next to the Moonraker status route, once the gateway
hands over its `MoonrakerStatus`. Admission is `pp_api::accepted::Gate`:
loopback Host and Origin checks, then a session cookie or an API key. A desktop
session alone is refused with 401.

Each request reads the tenant's `printer.fleet`, `integrations`,
`printer.checkoff_links` and `printer.unattributed_prints` settings and the
send queue in one deferred SQLite read (`pp_storage::read_model::farm`). The
send queue is the `printer_send_queue` table, or the `printer.send_queue`
setting while that setting exists ([Send queue storage](send-queue.md)). A
setting over 4 MiB answers 500 `Farm settings exceed the read limit`; a
missing or malformed setting reads as an empty list, as Node reads it.

## No writes, no polling

The routes write nothing. Node's `GET /printer-checkoff` repairs empty
awaiting links and its `GET /printer-send-queue` reconciles live sends first;
the Rust reads do neither. A row Node would repair or drop is skipped, and a
`sending` item stays `sending` until the send's owner reconciles it.

Printer state comes from the Moonraker and PrusaLink status watchers' read-only port
(`PrinterObservations`, GRE-449), never from a request to a printer. Monitoring
never sends, checks off or dispatches. The launcher test checks that the only
printer requests are status GETs and that every production table and setting
is value-identical after the reads.

## Printers

Each `FarmPrinter` has `id`, `name`, `model`, `integration` (`{id, kind}` with
kind `moonraker`, `prusalink` or `bambu`, or `null`), `state`, `reason`,
`last_heard_at`, `progress_percent` and `loaded_filaments` (one row per slot,
1 to 4, as Node's `ensureSlots` fills them). Bodies carry no host address, API
key, file path or printer error text; only a send queue item carries the
error of its own last send.

`state` is the generated `PrinterStateView`: `execution` and `bed_clear` are
separate facts and `display` is `pp_domain::printer_display_state`'s summary of
the two.

| Printer | `execution` | `reason` |
| --- | --- | --- |
| No `integration_id` | `not_set_up` | `not_set_up` |
| `integration_id` names no integration, or a non-printer one such as Spoolman | `not_set_up` | `integration_missing` |
| Integration disabled | `manual` | `monitoring_disabled` |
| An HTTPS Moonraker or PrusaLink host, a redirecting Moonraker host, or a Bambu host while the tenant's `bambu` feature is off or its toggles are invalid, or a Moonraker, PrusaLink or Bambu host the watchers never take (no address, or past the 128-host cap; PrusaLink and Bambu rows come after every Moonraker row) | `manual` | `monitoring_not_supported` |
| Moonraker, PrusaLink or Bambu with no status yet, or monitoring stopped | `unknown` | `awaiting_status` |
| Moonraker or PrusaLink that did not answer its last poll; Bambu whose session dropped, or that has no usable pin and so is never connected | `unreachable` | `unreachable` |
| Moonraker, PrusaLink or Bambu `printing`, `paused` | `printing`, `paused` | `null` |
| Moonraker, PrusaLink or Bambu `idle` or `complete` | `idle` | `null` |
| Moonraker, PrusaLink or Bambu `error` | `failed` | `null` |
| Moonraker or PrusaLink `unknown`, or a Bambu `gcode_state` outside the mapped set | `unknown` | `null` |

`last_heard_at` is the time of the last successful status poll or Bambu report, kept while the
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
  `host_outcome`, `unit_count`, `resolved_unit_count`, `units`,
  `resolved_units`, `created_at`, `completed_at`, `applied_at`. Every stored result, in stored order. As in
  Node, a missing `host_outcome` reads `success` for verified and awaiting
  results and `failed` for `host_failed`, and a missing `completed_at` reads
  `applied_at`.
  `units` lists each stored unit with a whole positive `part_id` and a
  non-negative `unit_index`, in stored order, with `part_filename` from the
  tenant's `parts` row read in the same transaction (null when the Part is
  gone or its file name is empty). `resolved_units` lists each stored decision
  with a valid unit and a `result` of `confirmed` or `rejected`. Entries that
  fail those checks are left out, never guessed; `unit_count` and
  `resolved_unit_count` still count the stored lists. These are what
  `POST /printer-checkoff/verify-batch` decides, so a client can build a burst
  and show what it will mark ([Checkoff bursts](records/d19-publication-client.md#checkoff-bursts-gre-1090)).
- `UnmatchedPrint`: open prints only (not claimed, not dismissed) that no
  watching, awaiting or verified result matches by integration and
  case-insensitive basename, as Node's `filterLinkedUnattributedPrints`
  does. `object_count` and `candidate_count` count the stored lists. The
  newest 100, in stored order.
- `QueuedSend`: `id`, `filename`, `printer_id`, `profile_id`, `state`
  (`queued`, `sending`, `done`, `error`, `cancelled`), `start`,
  `wait_for_idle`, `unit_count`, `created_at`, `updated_at`, `error`,
  `upload_job_id` and `outcome_unknown`. `error` is the stored reason of the
  last failed send, or null: the printer's refusal (`Upload failed (HTTP
  500): ...`) or a fixed text, never an API key or password; the one user owns
  the printer and needs the reason. `upload_job_id` is the last send's durable
  job, which Reconcile names. `outcome_unknown` is computed on each read, not
  stored: true for an `error` item whose upload job awaits reconciliation,
  or, with no Rust job, whose error is Node's restart text. Send now refuses
  exactly those items with 409 `send_outcome_unknown`. A quarantined upload
  job reads as `false`: Send now and Reconcile cannot resolve it, and no route
  does yet. The artifact path and
  host name stay server-side. Every stored item, in stored order. The send
  routes answer the same type ([Send queue storage](send-queue.md#routes-gre-365)).

## Match suggestions

`GET /api/v2/printer-checkoff/unattributed/suggestions` (GRE-1091) returns one
`MatchSuggestion` per print `/unattributed` lists, in the same order. It
reads the farm settings plus the open (not archived) Builds' included part
files, the Required units the prints name, each Build's current accepted
Plates and the file names of checked-off print jobs, in the same deferred
transaction. It writes nothing, and no job, timer or other route confirms a
suggestion. The operator claims a print through Checkoff (GRE-1169) or the
Match prints review (`/farm/match`, GRE-1181), which claims a checked print for
its suggested Build or dismisses it only when its group's Apply is pressed, through
`POST /api/v2/printer-checkoff/unattributed/{id}/claim` and `.../dismiss`. The
launcher test checks that production rows and settings are value-identical
after the read.

`POST .../unattributed/{id}/dismiss/undo` (GRE-1549) opens a dismissed print
again. It removes `dismissed` and `dismissed_at` and keeps every other stored
field, so the print reads and is suggested as before the dismiss. It answers
`{"ok": true, "restored": true}`, or `"restored": false` with nothing written
when the print is not dismissed, so a repeat is safe. A print no longer stored
(pruned 7 days after its dismissal, dropped by the 100-entry cap, or never
stored) is a 404 problem with code `print_not_found`. A print whose file a
watching, awaiting-verify or verified Checkoff link on the same integration
matched since stays dismissed: a 409 problem with code
`print_already_matched`, because the reads would hide it while that link
lives. The Match prints screen has no Undo button yet.

Each row has `print_id`, `printer_id`, `filename`, `completed_at`,
`confidence`, `starts_checked`, `build` (`{id, name}` or `null`), `plate`
(`{plate_id, ordinal}` or `null`) and `evidence`:

- `file_name.build_ids`: open Builds whose stored Checkoff results, send queue
  items or checked-off print jobs used the same file name (trimmed basename,
  case-insensitive).
- `object_list`: `object_count`; `unit_token_count`, the objects whose name
  carries a Required-unit token (`ppu_` and 32 lowercase hex digits, which
  Print Partner writes into every exported object name);
  `unit_build_ids`, the open Builds owning those tokens; and
  `part_build_ids`, the open Builds whose part files match every object as
  `pp_domain::gcode_objects` matches them, computed only when no object
  carries a token.
- `time_window`: `start` (72 hours before completion), `end` (the completion)
  and the open Builds with a result or send to the same printer created in
  that window; `null` when `completed_at` does not parse.
- `basis`: which of `unit_tokens`, `object_list`, `file_name` and
  `time_window` support the suggested Build; empty for `none`.

| Confidence | Rule |
| --- | --- |
| `exact` | Every object carries a token, every token belongs to the same open Build, and the tokens are exactly the units of one Plate in that Build's current accepted layout |
| `likely` | Tokens name one open Build but do not make a whole Plate; or, with no tokens, the object list matches exactly one Build's part files; or, with no objects, the file name names exactly one Build. When the object list matches several Builds, the file name and then the time window may leave one; when the file name of a print with no objects names several Builds, the time window may leave one |
| `none` | Tokens name more than one Build or none, objects match no Build, the evidence ties, or only the time window points anywhere |

`plate` is set for `exact`, for `likely` when exactly one current Plate holds
every token, and for a tokenless `likely` when exactly one current Plate's
units come from the same part files as the printed objects, compared without
slicer suffix or extension and counted with copies. A unit's part file comes
from the accepted Plan revision, not from its exported object name, which
replaces non-ASCII characters.

`starts_checked` is `true` only for `exact`. A Match prints review starts
`likely` and `none` rows unchecked. Accuracy on the real 18-print backlog has
not been hand-counted yet (GRE-1091 MCH-02), so no confidence is treated as
safe to confirm without the operator.

## Tests

```sh
cargo test --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --lib read_model::farm
cargo test --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild --test match_suggestions
cargo test --manifest-path rust/Cargo.toml -p pp-drivers --lib farm_reads
cargo test --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild --test runtime_m0 farm_reads
cargo test --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild --lib client_routes_match_registrations
npm --prefix web run check --workspace @print-partner/rust-client
```
