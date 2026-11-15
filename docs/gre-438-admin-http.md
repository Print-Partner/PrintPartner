# GRE-438: admin routes over HTTP

With the default-off `desktop_rebuild` feature compiled, `pp_api::admin`
mounts these `GET` and `HEAD` routes at `/` and `/api/v1`, where Node
registers its core routes, only when stored `PP_DESKTOP_REBUILD` is exactly `1`
or `true`. `pp_core::rebuild_api::read_router` mounts them beside the
Accepted Plan reads, from the same `WriterOwner`.

| Route | Body |
| --- | --- |
| `/legal/summary` | `LICENSE-SUMMARY.md` |
| `/legal/license` | `LICENSE` |
| `/legal/attribution` | `ATTRIBUTION.md` |
| `/legal/third-party` | `THIRD_PARTY_NOTICES.md` |
| `/filaments/custom` | `{filaments}` read from `<data>/custom_filaments.json` |

Legal documents are compiled in from `web/apps/server/src/data/legal`. The
Node build copies that directory to `dist/data/legal` and requires every file,
and `legal.ts` reads that copy first, so Node never reaches its other
candidates or its 404.

Each request first proves its credential with an empty `ReadClient` batch, so
a missing or wrong credential answers `401` `Authentication required` and a
full queue answers `503`, as on the Accepted Plan routes.

`/filaments/custom` reads the store as `services/custom-filaments.ts::loadStore`
does and serializes it as `JSON.stringify` would:

| Store file | Response |
| --- | --- |
| Missing, a directory, unreadable, or text `JSON.parse` rejects (including a byte order mark) | `200` `{"filaments":[]}` |
| A JSON object with `filaments` | `200` `{"filaments":<value>}`, re-serialized with JavaScript number, string and key-order rules |
| Any other JSON value, or an object without `filaments` | `200` `{}` |
| The literal `null` | `500` with Fastify's `TypeError` body |

The read behavior remains as above; the Rust-only mutation and catalog routes
are described below.

One known difference: Rust's JSON parser stops at 128 levels of nesting and
then treats the store as unparsable (`{"filaments":[]}`), while Node serves
the nested value. Node writes flat entries, so only a hand-edited store nests
that deep. `admin_http` pins this case.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-api -p pp-storage -p pp-core \
  --features pp-api/desktop_rebuild,pp-storage/desktop_rebuild,pp-core/desktop_rebuild \
  --no-tests=fail -E 'test(ticket_t_62_admin) | binary(rebuild_read_http)'
```

## Date format, build tracking and default Spoolman settings (GRE-1357)

`pp_api::settings` mounts `GET` (with `HEAD`) and `PUT` for these routes at
`/` and `/api/v1` under the same flag and credential check, and
`read_router` mounts them beside the admin reads. Each request reads or writes
one `app_settings` row on the writer thread, in the tenant its credential
resolves to in the same transaction. The `pp_storage::app_settings` client names fixed supported keys; this group
uses the three rows below.

| Route | Row | `GET` body | `PUT` |
| --- | --- | --- | --- |
| `/settings/date-format` | `date_format` | `{format}`, `mdy_12h` when empty | `400` unless `format` is a `DATE_FORMAT_PRESETS` id |
| `/settings/build-tracking` | `build_tracking_assembly` | `{assembly_tracking}`, true unless empty or `"0"` | stores `"1"` for any truthy value; answers `true` only for boolean `true` |
| `/settings/spoolman-default` | `default_spoolman_integration_id` | `{integration_id}`, `null` when empty | trims the id; a non-string or empty id stores `""`; `400` unless the first stored integration with that id has `type` `spoolman` |

A cookie-authenticated `PUT` needs the listener's own `Origin`, as on the
other rebuild routes; without it the request answers `403` before any write.
A full writer queue, a stopped writer, or a write lock past its admission
deadline answers `503`.

The Spoolman check reads only the matched integration's `type`, never its
`config`. Rust writes these rows only in the Rust-only runtime; Node keeps its
own routes and there is no dual write or fallback.

`fixtures/admin/settings-cases.json` lists 52 requests;
`fixtures/admin/settings-oracle.json` records Node's reply and the three stored
rows after each one. `settings_routes_http` replays them and matches status on
48 requests, and cache headers and stored rows on all 52. It matches the body
on every `200` and each Node `400` with a `detail`. These differ by design:

- Errors are `application/problem+json`; the `detail` matches Node's.
- An empty or unparsable JSON body answers `400` `Request is invalid` instead
  of Fastify's `FST_ERR_CTP_*` body; an unsupported content type answers `415`.
- Rust also answers `400` for JSON that Fastify accepts but `serde_json`
  refuses: a leading byte order mark, invalid UTF-8, a number out of `f64`
  range such as `1e400`, or nesting past 128 levels.
- A chunked empty body with no content type answers `400`; Fastify answers
  `415`.
- A `null` JSON body, or a `PUT` with no body and no content type, answers
  `400` with the malformed-request problem type and detail `Request is invalid`,
  without writing. Node answers `500` with its `TypeError`. The four recorded
  cases assert this intended difference; separate HTTP coverage checks both
  bodies on all three routes at `/` and `/api/v1` (GRE-1368).
- A stored `integrations` value with an unpaired UTF-16 surrogate escape holds
  no integrations for Rust, so the Spoolman `PUT` answers `400` where Node
  would accept it.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-api -p pp-storage -p pp-core \
  --features pp-api/desktop_rebuild,pp-storage/desktop_rebuild,pp-core/desktop_rebuild \
  --no-tests=fail -E 'binary(settings_routes_http) | binary(rebuild_read_http) | test(spoolman_lookup) | test(/^settings::/)'
(cd web && UPDATE_ADMIN_ORACLE=1 npm run test -w @print-partner/server -- src/routes/admin-routes.oracle.test.ts)
```

## Core diagnostics

The feature and runtime switches also mount authenticated logging config
(`GET`, `POST`), log tail (`GET`, `DELETE`), stats (`GET`) and `/metrics`
(`GET`) at `/` and `/api/v1`. Both aliases share one view. Cookie writes
require the listener Origin. Admission and credential checks use the admin gate.

These routes describe Rust's `core.log`, including its six rotated archives,
not Node's in-memory workflow logger. Reads use the desktop reader's limits:
64 KiB per file and complete rows of at most 8 KiB. The view shows the
newest 1,000 records. Clear observes up to 1,000 rows per file before that
view cap, so every retained file contributes its end offset.
Invalid rows are omitted. Every returned record passes the unchanged Scrubber
again. Rows use the existing UI WorkflowLog shape. `id` is the stable file identity
and end offset; `timestamp` converts epoch-second `time` to RFC 3339.
Non-HTTP records use empty method/URL and zero duration/status. Original
scrubbed core fields remain in `context`. Severity maps trace/debug to
`debug`, info to `info`, warn to `warn`, and error/fatal to `error`.

Config starts with `minSeverity: "info"`, `maxLogs: 1000` and
`enableWorkflowTracking: true`. POST accepts only `minSeverity` and
`enableWorkflowTracking`, with the same enum and boolean validation as Node.
These settings filter the diagnostics view; the crash writer continues to
record independently. Config is process-local and resets on restart.
`severity` and `method` tail filters match exactly; `limit` defaults to 100,
is capped at 1,000, and accepts nonnegative integers (zero returns no rows).
Invalid query values return 400.

DELETE records the current per-file end offsets and clears the view,
not the retained crash files. Clear pins up to seven read handles until the
next clear or view shutdown; deleted files can retain up to about 70 MiB.
Renamed archives keep their identities; later appends have larger offsets.
Restart creates a fresh view over retained files,
as opposed to Node's empty in-memory buffer. It never unlinks an active writer
or removes compat/supervisor records. Stats summarize the same bounded view:
`totalLogs`, `bySeverity`, `byMethod`, `avgDuration` and `errorCount`.
Rows without method do not contribute to `byMethod`; absent durations count
as zero. Timestamps, methods and HTTP durations are not invented.

`/metrics` returns Prometheus text version 0.0.4 with `core_log_records`
gauges by severity and `app_info` with version and Rust runtime. Counts can
fall after clear or rotation. Node HTTP counters, print-job totals, Plan
progress and live printer gauges are not inferred from log messages or
reported as fabricated zeroes; this endpoint makes no printer network calls.
Read failures return 500 without exposing paths or raw records. All responses
use `private, no-store`.

| Node metric names | Rust status |
| --- | --- |
| `app_info` | Served with version and `runtime="rust"`; no Node-version label |
| `http_requests_total`, `http_request_duration_ms` (quantiles), `http_request_duration_ms_sum`, `http_request_duration_ms_count`, `http_errors_total`, `http_success_total`, `http_error_rate` | Missing; HTTP instrumentation in GRE-1395 |
| `plates_sent_total`, `plates_completed_total`, `plates_failed_total`, `filament_consumed_g` | Missing; domain metrics in GRE-1395 |
| `plan_progress_collection_available`, `plan_progress_state`, `parts_remaining`, `printers_idle`, `printers_printing` | Missing; domain metrics in GRE-1395 |

[GRE-1395](https://linear.app/greetz/issue/GRE-1395/port-remaining-node-http-request-and-domain-prometheus-metrics-to-rust)
tracks the coordinator-approved split. `core_log_records` is a Rust-only gauge.
The core writer currently captures info and above; selecting debug in the view
cannot recover events the writer did not capture. External in-place truncation
of a retained file is not supported; rotation creates a new file identity.

## Module write route (GRE-1341)

`PUT /api/v2/modules/{module}` turns one Module on or off for the replacement
Settings screen. `{module}` is one of the five `GET /health` lists (`farm`,
`spoolman`, `orders`, `bambu`, `assembly_tracking`) and the body is exactly
`{"on": true}` or `{"on": false}`. It is `setModule` in the generated
`@print-partner/rust-client`.

- It mounts only under `/api/v2` and only when `PP_DESKTOP_REBUILD` is `1` or
  `true`, so only the Rust-only runtime serves it. Node keeps
  `PATCH /settings/features` at `/` and `/api/v1` in mixed mode, as
  `operations.json` records, so the two never write the same setting.
- The accepted gate admits loopback requests with an exact Host and Origin and
  a credential; storage proves the credential inside the write's transaction
  (401, or 403 when the account needs an owner mapping). 30 requests a minute
  that reach the write, whatever storage answers, then 429.
- `FeatureClient::set_module` writes and reads back in one transaction.
  `farm`, `spoolman`, `orders` and `bambu` write the user's `feature_toggles`
  layer. Plan overrides (`feature_toggles:plan:<id>`) and every other stored key
  stay as they were. `assembly_tracking` writes `build_tracking_assembly` as
  `"1"` or `"0"`, as Node's `PUT /settings/build-tracking` does.
- `200` answers `{"modules": [...]}`: every Module's state read from the same
  rows `GET /health` and send admission read.
- `404` `Module not found` for any other name. `400` for any other body.
- When the stored `feature_toggles` doesn't parse (as `/health` reads it), a
  feature Module answers
  `500` with code `FEATURE_TOGGLES_INVALID` (Node's text) and nothing is
  written. `/health` keeps reporting those Modules `setting_unreadable`.
  `assembly_tracking` is its own row and still writes.
- A writer that stays busy past its admission deadline answers `503`
  `transaction_unavailable`; a full or stopping writer answers `503` as the
  other routes do. Nothing was written in either case.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core -p pp-storage -p pp-api \
  --features pp-core/desktop_rebuild,pp-storage/desktop_rebuild,pp-api/desktop_rebuild \
  --no-tests=fail -E 'test(module_route) | test(client_routes_match_registrations) | test(health_modules) | test(unreadable_stored_layer)'
```

## Integration create, edit and delete routes (GRE-1439, GRE-1481)

`POST /api/v2/integrations` adds a Moonraker, PrusaLink or Spoolman integration
and `DELETE /api/v2/integrations/{id}` removes one of any kind, for the
replacement Settings screen. They are `createIntegration` and
`deleteIntegration` in the generated `@print-partner/rust-client`. Both answer
`200` with an `IntegrationView`: `id`, `kind`, `name`, `enabled`, `base_url`,
`username`, `has_api_key` and `has_password`. Delete answers the view it
removed. No response or problem carries an API key or a password.

- They mount only under `/api/v2` and only when `PP_DESKTOP_REBUILD` is `1` or
  `true`, so only the Rust-only runtime serves them. The accepted gate admits
  loopback requests with an exact Host and Origin and a credential; storage
  proves the credential inside the write's transaction (`401`, or `403` when the
  account needs an owner mapping).
- The create body is `kind`, `name` and `base_url`, plus optional `enabled`
  (default `true`), `api_key` (Moonraker and Spoolman) and `username` and
  `password` (PrusaLink). Unknown keys are refused.
- `PATCH /api/v2/integrations/{id}` (`updateIntegration`) edits one and
  answers the same view. Its body takes `name`, `base_url`, `enabled`,
  `username`, `api_key` and `password`, all optional, and applies what it is
  sent in one write, so an address and its credentials change together. A field that is absent keeps
  its stored value. For `username`, `api_key` and `password`, null clears the
  stored value; for the other three, null keeps it. `kind` is not a field: an
  integration's kind never changes. Only the fields a request sends are
  checked, so a row that predates a check can still be edited.
- An edit that changes `base_url` must also send each stored `api_key` and
  `password`, as the value to use at the new address or as null. Without that
  the stored secret would be sent to whatever host the caller named, so the
  edit is refused and names the field. The view's `has_api_key` and
  `has_password` say which are stored.
- Bambu, Home Assistant, AI assistant and slicer integrations cannot be edited
  here. They are kept as stored and can only be deleted.
- A new or edited host needs no restart: the status watchers re-read the
  stored hosts about once a second, and `POST /api/v1/integrations/{id}/test`
  reads the stored row.
- Delete also removes the integration's TLS pin and clears a Spoolman default
  that names it, in the same transaction. A printer that linked it keeps the id
  and shows `integration: null` in `GET /api/v2/printers`. The status route
  answers `404` once the watchers have dropped the host.
- `pp-storage::printer_setup` owns the rules and answers a typed
  `PrinterSetupError`; `pp-api/src/integrations.rs` is the one place that maps
  it. Failures every write shares go through `pp_api::problem::writer_failure`.

| Case | Status | `code` |
| --- | --- | --- |
| A value is invalid, or the body is not the request shape. `errors` names each field by JSON Pointer, never its value. Nothing is stored. | `400` | `invalid_integration` |
| No stored integration has that id | `404` | `integration_not_found` |
| The installation already has 64 integrations | `409` | `integration_limit` |
| An edit names an integration of a kind that cannot be edited here | `409` | `integration_not_editable` |
| The stored `integrations` document is not the typed shape. Every write is refused and the stored text is left as it is. | `500` | `integrations_unreadable` |
| The write could not start before its deadline | `503` | `transaction_unavailable` |

Invalid values:

- `name`: empty after trimming, or longer than 200 bytes.
- `base_url`: not an `http` or `https` URL, holds a username or password, is
  longer than 2,048 bytes, or has whitespace or a control character in it.
- `api_key`, `username`, `password`: an empty string (omit the field or send
  `null` for none), longer than 4,096 bytes, or a field the kind does not use.
  On an edit, also a stored `api_key` or `password` left out of a body that
  changes `base_url`.
- `kind`, on create: anything but `moonraker`, `prusalink` or `spoolman`.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core -p pp-storage -p pp-api \
  --features pp-core/desktop_rebuild,pp-storage/desktop_rebuild,pp-api/desktop_rebuild \
  --no-tests=fail -E 'test(integration_setup) | test(integrations::tests) | test(printer_setup::) | test(client_routes_match_registrations)'
```

## Integration list route (GRE-1342)

`GET /api/v2/integrations` lists every stored integration for the replacement
Settings screen, in stored order. It is `listIntegrations` in the generated
`@print-partner/rust-client` and answers `200` with
`{"integrations": [IntegrationView, ...]}`, the same view the create and
delete routes answer. Kinds those routes cannot create (Bambu, Home Assistant,
AI assistant, slicer sidecar) are listed too, with `base_url: null`, so they
can be deleted. No secret is read into the answer: a view says only whether an
API key or password is set.

- Same mount, gate and credential proof as create and delete; storage reads
  through the read seam (`SettingsClient::read`), so the credential check and
  the list are one snapshot. No per-minute window: the gate's admission cap
  bounds it, as for the other `/api/v2` reads.
- An unknown credential answers `401`. A stored `integrations` document that is
  not the typed shape answers `500` `integrations_unreadable`.

The two TLS pin routes in [Printer host reads](printer-host-reads.md) are
`readTlsPin` and `resetTlsPin` in the same client, with the bodies `TlsPin` and
`TlsPinReset`.

## Printer routes (GRE-1452, GRE-1448)

`POST /api/v2/printers` adds a printer, and `GET`, `PATCH` and `DELETE
/api/v2/printers/{id}` read, edit and remove one, for the replacement Settings
screen. They are `createPrinter`, `readPrinter`, `updatePrinter` and
`deletePrinter` in the generated `@print-partner/rust-client`. Each answers
`200` with a `PrinterConfig`: `id`, `name`, `model`, `bed_width_mm`,
`bed_depth_mm`, `bed_height_mm`, `margin_mm`, `max_filament_slots` and
`integration_id`. Delete answers the printer it removed. `GET /api/v2/printers`
is still the Farm list, which adds the live state.

- They mount like the integration routes above: `/api/v2` only, Rust-only
  runtime only, behind the accepted gate, with the credential proved in the
  operation's own transaction.
- Lengths are whole millimetres, as integers. A fractional value in a request
  is refused with `400` naming the field. `bed_height_mm` is null only for a
  printer that came from an installation without one.
- Create takes `name`, `model`, `bed_width_mm`, `bed_depth_mm` and
  `bed_height_mm`, plus optional `margin_mm` (default 4), `max_filament_slots`
  (default 1) and `integration_id`.
- `PATCH` changes only the fields it sends; a field that is absent or null
  keeps its stored value. The exception is `integration_id`: null unlinks the
  printer and only leaving the field out keeps the link.
- An edit checks only the values it sets, so a printer with an older value
  these rules would refuse can still be edited. Two things follow from the
  stored row: a new bed size is compared with the stored margin, and a link is
  checked only when it changes.
- Every invalid field is named in one answer. A slot that holds filament
  answers `409` only when the values themselves are valid.
- A new or relinked printer needs no restart: the Farm list reads the stored
  printers on each request.
- Delete removes the printer only. Its integration stays.
- A stored printer or integration row, and an integration's `config`, can
  hold keys the typed shape does not name. Only the [Node import](node-import.md)
  is to write them (GRE-1523). No route, MCP tool or log line reads or returns
  such a key, an edit stores it back unchanged, and a request cannot add one.
  A row without such keys reads and writes as before.
- The three writes run through the write seam. The read runs through the read
  seam (`pp-storage/src/read.rs`) on the read-only pool, so it does not wait in
  the writer queue.

| Case | Status | `code` |
| --- | --- | --- |
| A value is invalid, or the body is not the request shape. `errors` names each field. Nothing is stored. | `400` | `invalid_printer` |
| No stored printer has that id | `404` | `printer_not_found` |
| The installation already has 64 printers | `409` | `printer_limit` |
| The edit lowers `max_filament_slots` below a slot that holds filament | `409` | `filament_slot_in_use` |
| The stored `printer.fleet` document is not the typed shape, for example one printer has a fractional length, which Node accepted. Every operation is refused and the stored text is left as it is. The import is to round such lengths (GRE-412); that conversion is not built yet. | `500` | `printers_unreadable` |
| The write could not start before its deadline, or no reader came free in time | `503` | `transaction_unavailable` |

Invalid values:

- `name`, `model`: empty after trimming, or longer than 200 bytes.
- `bed_width_mm`, `bed_depth_mm`, `bed_height_mm`: not a whole number from 1 to
  10,000.
- `margin_mm`: not a whole number, or half the bed width or depth, or more.
  Accept applies the same test and cannot place on a printer that fails it.
- `max_filament_slots`: not 1 to 4.
- `integration_id`: names no stored Moonraker, PrusaLink or Bambu integration.

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-core -p pp-storage -p pp-api \
  --features pp-core/desktop_rebuild,pp-storage/desktop_rebuild,pp-api/desktop_rebuild \
  --no-tests=fail -E 'test(integration_setup) | test(printers::tests) | test(printer_setup::) | test(read::tests) | test(client_routes_match_registrations)'
```

## GitHub PAT and Discord notification settings (GRE-1360)

The settings router mounts `/settings/github-pat`, `/settings/discord-notify`
(`GET`, `HEAD`, `PUT`) at `/` and
`/api/v1`. The default-off rebuild gates and the existing credential and Origin
checks apply. Rust owns these operations only in the Rust-only runtime.

Following the coordinator's 2026-11-11 decision, storage uses Node's existing
`app_settings` keys and text formats. `github_pat` holds the trimmed token;
`discord_notify_webhook_url` holds the trimmed URL. Empty strings clear them.
Discord flags use `"1"` and `"0"` in `discord_notify_on_update`,
`discord_notify_on_sync`, and `discord_auto_sync_updates`; missing defaults are
true, false, true. A Discord partial update and its readback share one writer
transaction. PAT replies expose only `configured` and `masked`, matching Node.
Native secret storage and migration remain later work under GRE-1330.

Differences by design:

- Discord GET and PUT return `https://<host>/[REDACTED]` for a configured webhook,
  preserving scheme and host but masking the entire path, query and fragment.
  Invalid legacy text returns `[REDACTED]`. Node returns the complete URL.
  A masked response is not a valid replacement URL; omit `webhook_url` to keep
  the stored value. These settings routes never return the stored secret.
- Errors use the Rust problem envelope, with Node validation detail where applicable.
- Null Discord JSON bodies and non-string webhook values answer 400 rather than
  Node's TypeError. PAT null or absent bodies use the Rust malformed-request
  detail instead of Node's `Token must be a string`.
  Backslashes and control characters in webhook tokens are rejected.

Explicit test delivery moves to GRE-1403; external-access settings and their
admission policy move to GRE-1404. Each child quotes the unmet parent items.
They start from main after this settings change merges. Neither operation is
mounted by this change. Sharing, listener binding and MCP authentication stay
unchanged.

`fixtures/admin/secret-settings-cases.json` contains 32 requests using obviously
fake values. The Node oracle records exact stored bytes; Rust checks them,
asserts the stricter response redaction, refuses unauthorized/cross-site writes,
and reopens the database to check restart persistence.

## Custom filament mutations and catalog (GRE-1358)

In Rust-only mode, authenticated `POST /filaments/custom` adds a trimmed name,
RGB hex and optional product line; `DELETE /filaments/custom/:id` removes the
matching entry. Both root and `/api/v1` aliases use one credentialed writer
command. The family facade dispatches to an item-gated implementation; without
`desktop_rebuild` its request type is uninhabited, so no filament request can be
constructed. Cookie writes require the listener's Origin. There is no Node fallback.
Rust keeps Node's entry fields and the `custom-` plus eight hex digit ID shape.
The writer writes a new temporary file in the data directory, syncs it, renames
it onto `custom_filaments.json`, then syncs the directory. An interrupted write
leaves the previous published file readable. At writer startup, Rust removes
regular `.custom-filaments-*` temporary files older than one hour and keeps
recent temporary files. A directory-sync error after a successful live-store rename is
logged; the write still succeeds because the entry is already published, so
the response must not invite a duplicate retry. The live-format version is 1;
regenerate its declaration with
`python3 scripts/generate-filament-format.py`.

`GET /filaments/catalog` combines the bundled colors and custom colors. Spoolman
access is checked before connector lookup and again after the network read.
Disabled access withholds Spoolman colors and the stored default connector.
Rust refuses updates to a Node-valid store it cannot decode losslessly (for
example, an isolated UTF-16 surrogate or nesting past 128 levels). POST and
DELETE return a 400 problem without writing, preserving the original file bytes.
Only a real JSON syntax failure uses Node's empty-store fallback. As a deliberate
difference from Node, before POST replaces malformed bytes, Rust preserves an
exact copy at the sibling `custom_filaments.json.corrupt-<UTC timestamp>`.
The copy uses a temporary file, file fsync, rename and directory fsync before
the fresh store is published. UTC timestamps include nanoseconds and use filesystem-safe digits
and `T`, `Z`, `.` separators. A copy failure refuses the update and keeps the
original store. Reads still return the empty store and do not create copies;
corrupt copies are retained and are not part of temporary-file cleanup.
Catalog reads still preserve escaped UTF-16 values using the same Node-compatible parser as
`GET /filaments/custom`. For a store nested past 128 levels, both GET aliases
keep today's 200 response: custom GET returns `{"filaments":[]}`; catalog GET
returns the bundled colors and `custom_colors: []`. Neither GET changes the file.
External requests use the existing bounded, checked Rust connector. Connector
failures return a generic message so configuration and credentials stay private.

Path-hints remains read-only here. Missing custom hints use the shipped rules;
partial or corrupt documents refuse preparation instead of publishing changed
Plan state. A valid replacement can be read again after recovery. GRE-1394 tracks
the Rust publisher and its separate live-format ownership decision.

## Update checks, printer bindings, help and OpenAPI

`pp_api::settings_support` mounts under the same compile-time and runtime
rebuild gates. Settings and help use the existing credential and Origin gate
at `/` and `/api/v1`.

- Desktop and launcher startup fix update checks off and ignore the repository,
  version override and cache environment variables, matching Node desktop.
  Only `pp-server`/Docker opts into Node server environment settings. Disabled
  desktop checks have no HTTP client and make no outbound request.
- `GET /settings/update-check` on the server follows Node's enabled flag, version override,
  release response cap (1 MiB), version comparison, cache interval and
  `refresh=1`. Upstream failures return no latest release and expose no
  upstream error text. The client uses no proxy, matching Node undici defaults.
  Redirects are deliberately disabled by coordinator decision, unlike Node's
  default fetch behavior. A renamed or transferred `GITHUB_REPO` can therefore
  yield no latest release; configure its current canonical name. Fetches run
  outside the cache lock and auth admission permit. Tests use a loopback fake,
  never GitHub.
- `GET`/`PUT /settings/printer-plan-bindings` and
  `DELETE /settings/printer-plan-bindings/{integration_id}` read or update
  `printer.plan_bindings`. Credential proof and each read-modify-write run
  in one writer transaction. Replacement preserves position; deletion removes
  all matching entries. Timestamps use UTC milliseconds, as Node does.
- `GET /help/workflow` returns Node's text. The API tip follows the stored
  `external_access_mode`, including Node's default for an unknown value.
- `/openapi.json` redirects with `302` to `/api/v1/openapi.json`.
  That document and `/api/v2/openapi.json` are public, as in Node: Node exempts
  `.json` paths from the session hook and explicitly exempts OpenAPI from
  API-key authentication. Help and settings remain authenticated. Sharing
  defaults and MCP authentication stay unchanged.

The OpenAPI document contains route and schema shapes only: no credentials,
user data, data-directory paths or hostnames. A test checks string values for
`token`, `secret`, `password`, absolute filesystem paths and URLs. The sole
structural exception is a path parameter whose `name` is `token`; it contains
no token value. The document is generated from Node, with repeated route
shapes grouped in `fixtures/admin/settings-support.json`. The Node oracle
checks its freshness; Rust expands the groups without consulting stored data.

The binding fixtures compare replies and persisted values after each request
and after reopening storage. As on the other settings routes, malformed JSON
and null request bodies use Rust's redacted problem responses. Numeric ids and
numeric strings written by the UI preserve Node's coercion. Exotic JavaScript
coercions and unpaired UTF-16 surrogates remain outside the supported parity
scope. Source update-check is deferred to GRE-1400 after GRE-1360 because it
shares Discord auto-sync state.

```sh
npm --prefix web run test -w @print-partner/server -- src/routes/settings-support.oracle.test.ts src/routes/admin-routes.oracle.test.ts
# Regenerate the Node snapshot only when its contract changes:
UPDATE_ADMIN_ORACLE=1 npm --prefix web run test -w @print-partner/server -- src/routes/settings-support.oracle.test.ts
```
