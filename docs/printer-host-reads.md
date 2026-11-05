# Printer-host reads (GRE-360, GRE-1044)

With the `desktop_rebuild` Cargo feature and `PP_DESKTOP_REBUILD` exactly `1` or `true`, the Rust-only runtime serves the Moonraker and PrusaLink printer-host reads that Node's `routes/printers.ts` and `routes/integrations.ts` serve, at the same paths. The three printer routes answer at the root, where the web client calls them, and under `/api/v1`; the connection test is under `/api/v1` only, as in Node:

| Route | Answer |
| --- | --- |
| `GET /printers/{id}/capabilities` | `{files, cameras, status}`. All three are `true` for a printer linked to an enabled Moonraker integration. An enabled PrusaLink integration has `files` and `cameras` but `status: false`, because only Moonraker has a status watcher here so far. A printer with no link, a missing or disabled integration, or a host type this runtime cannot read answers all `false`. An unknown printer answers 404 `Printer not found`. |
| `GET /printers/{id}/files?path=` | One directory as `{path, entries}`: of the Moonraker `gcodes` root, or of the PrusaLink pinned or first available storage. An empty or missing `path` is the root. PrusaLink listings may be up to 8 MiB, as in Node. |
| `GET /printers/{id}/cameras` | `{cameras}` from Moonraker's `/server/webcams/list` or PrusaLink's `/api/v1/cameras`. A host type without cameras in this runtime answers `{cameras: []}`. |
| `POST /api/v1/integrations/{id}/test` | Node's `{ok, message}`: Moonraker's `/server/info`, for example `Connected (klippy: ready)`, or PrusaLink's `/api/v1/info` and status. An unknown integration answers 404 `Integration not found`. |

The `files` and `cameras` routes return the same errors as Node: 400 for an invalid storage path, 404 for an unknown printer, 409 `Printer is not linked to a host`, 503 `Printer host is unavailable`, and 502 with the driver's message when the printer request fails. `files` answers 501 `Printer host does not support file browsing` for a host type this runtime cannot read. The connection test answers 501 for those types, and `{ok: false, message: "Unknown integration type: <type>"}` for a type Node has no adapter for. Storage that cannot answer within 500 ms gives 503 `Printer settings are unavailable`.

Node's per-route budgets apply, one window per route per minute: capabilities 120, files 60, cameras 60, connection test 5. As in Node, the root and `/api/v1` registrations each keep their own windows. Over budget, a route answers 429 with `Retry-After` and never reaches the printer.

## How it works

- `pp_storage::printer_hosts::printer_link` resolves a fleet printer (`printer.fleet`, tenant `default`) to its linked integration the way Node's `printerHostCapability` does: the printer id compared as a string, a trimmed `integration_id`, and an integration that exists with `config.enabled` not `false`. `printer_hosts::integration` reads one integration for the connection test.
- `pp_gateway::printer_host::HostReads` runs `pp_drivers::moonraker_driver::Driver` or `pp_drivers::prusalink::Driver` through one `HttpGuard` with a 2 MiB reply cap. Moonraker reads use its 8 s deadline. The PrusaLink driver sets a deadline on each request as Node does: 15 s for listings and the `object_list` prefix, 10 s for storage and the camera list, 8 s for status and info. PrusaLink listings use Node's 8 MiB cap. PrusaLink requests answer Digest challenges, and for a streamed read the deadline also covers the challenge probe. HTTPS hosts and redirects fail as the drivers document until GRE-359.
- `pp_api::printer_host` owns the HTTP mapping, path validation and budgets. `pp-core` implements `PrinterHostPort` over the two, so `pp-core` keeps no `pp-drivers` edge.
- Authentication and loopback admission are the admin routes' gate, as for `/api/v1/integrations/{id}/status`.

Every route is read-only on the printer and sends only GETs. It never uploads, starts a print, deletes or changes Checkoff. A Moonraker read sends each GET once. A PrusaLink read with a password sends a bodyless `GET /api/v1/status` before each GET, to get the Digest challenge. Without a password, a files read or connection test fails before sending anything, and the cameras list sends its GET unauthenticated, with no probe. A files read also lists storage when no storage is pinned. A GET sent with a password that gets a 401 with a fresh challenge is sent once more, as Node's adapter does. Nothing else is retried.

## Not served here yet

- PrusaLink status. The status route, farm reads and `/ws/printers` read only the Moonraker watchers, so PrusaLink printers read as Manual there.
- Bambu reads (GRE-362). Bambu reads as having no capabilities.
- `GET /printers/{id}/files/content` and `/cameras/view`. The PrusaLink driver can already stream both (`open_file`, `open_camera`, below); the routes, the Moonraker side and the print-file snapshot that Node writes for a Build are not built.
- Uploads and the send queue (GRE-365). Uploading needs an explicit user send and durable effect records (GRE-354). No Rust route calls `upload_file`.

## Streamed PrusaLink replies

`pp_net::outbound::HttpGuard::stream` sends one checked GET and returns the reply head with an unread body. The head must arrive within a deadline. The body then follows a `BodyDeadline`: `Total` keeps the head deadline over the whole body, as fetch with `AbortSignal.timeout` does, and `Idle(d)` requires each chunk within `d` of the last (`IdleTimeout`). Dropping the body closes the connection. `ReplyBody::prefix` reads at most a given number of bytes. `HttpGuard::with_body_limit` raises the buffered limit up to `MAX_LISTING_BODY` (8 MiB).

The PrusaLink driver uses these as Node's adapter does:

- `open_file(path)` reads the file's metadata, follows its same-origin `refs.download`, and streams the download with a 120 s idle deadline. The metadata read has a 15 s total deadline. A cross-origin or missing `refs.download` fails. A metadata reply that is not 2xx comes back unread for the caller to map.
- `open_camera(id)` streams `/api/v1/cameras/{id}/snap` for a connected camera whose stored `camera_id` equals `id`. The whole snapshot, head and body, must arrive within 30 s, so a trickling camera cannot hold the read open. As with Node's one `AbortSignal`, the deadline starts before the Digest challenge probe and also covers the probe and any 401 retry. The object-list prefix keeps the 15 s total listing deadline.
- `object_list` reads only the first 64 KiB of the active print file, even when the printer ignores `Range`.
- `browse` accepts listings up to 8 MiB.

## Verification

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
  --features pp-core/desktop_rebuild,pp-api/desktop_rebuild \
  -E 'test(ticket_t_21) | test(ticket_t_22)'
```

The runtime test (`pp-core` `printer_host::ticket_t_21_ticket_t_22_*`) starts the Rust-only launcher on a disposable data directory against a loopback Moonraker and PrusaLink that replay `fixtures/drivers/moonraker/driver/routes.json` and `fixtures/drivers/prusalink/driver/routes.json`. It checks that each route answers 401 without a session, that browse and cameras equal the Node adapters' results in `node-oracle.json` and `node-oracle-reads.json`, that the connection tests succeed, that every request the printers saw was a GET with no upload or print path, and that Checkoff, queue, job and settings rows are unchanged.
