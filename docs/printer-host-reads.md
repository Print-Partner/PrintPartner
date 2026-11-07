# Printer-host reads (GRE-360, GRE-1044, GRE-1174)

With the `desktop_rebuild` Cargo feature and `PP_DESKTOP_REBUILD` exactly `1` or `true`, the Rust-only runtime serves the Moonraker and PrusaLink printer-host reads that Node's `routes/printers.ts` and `routes/integrations.ts` serve, at the same paths. The printer routes answer at the root, where the web client calls them, and under `/api/v1`; the connection test is under `/api/v1` only, as in Node:

| Route | Answer |
| --- | --- |
| `GET /printers/{id}/capabilities` | `{files, cameras, status}`. All three are `true` for a printer linked to an enabled Moonraker or PrusaLink integration; both have status watchers (PrusaLink since GRE-1175). A PrusaLink row the watchers do not poll (past the 128-host cap, on an id a Moonraker row already has, or with no address) answers `status: false`. A printer with no link, a missing or disabled integration, or a host type this runtime cannot read answers all `false`. An unknown printer answers 404 `Printer not found`. |
| `GET /printers/{id}/files?path=` | One directory as `{path, entries}`: of the Moonraker `gcodes` root, or of the PrusaLink pinned or first available storage. An empty or missing `path` is the root. PrusaLink listings may be up to 8 MiB, as in Node. |
| `GET /printers/{id}/cameras` | `{cameras}` from Moonraker's `/server/webcams/list` or PrusaLink's `/api/v1/cameras`. A host type without cameras in this runtime answers `{cameras: []}`. |
| `GET /printers/{id}/files/content?path=` | The print file's bytes, streamed from the printer with its `Content-Type` and `Content-Length`. The path must be listed as a file when the route browses its directory first, as Node does, so a crafted path cannot reach a file outside the browsable tree; an unlisted path answers 404 `Printer file not found`. A missing or blank `path` answers 400 `path is required`. A request with `profile_id` answers 501 (see below). |
| `GET /printers/{id}/cameras/view?id=` | One listed camera's view, streamed: a Moonraker camera's same-origin MJPEG stream, else its snapshot, or a PrusaLink camera's `/snap` image. A camera id the camera list does not return, or a host type without cameras here, answers 404 `Printer camera not found`. A missing or blank `id` answers 400 `id is required`. |
| `POST /api/v1/integrations/{id}/test` | Node's `{ok, message}`: Moonraker's `/server/info`, for example `Connected (klippy: ready)`, or PrusaLink's `/api/v1/info` and status. An unknown integration answers 404 `Integration not found`. |

The `files` and `cameras` routes return the same errors as Node: 400 for an invalid storage path, 404 for an unknown printer, 409 `Printer is not linked to a host`, 503 `Printer host is unavailable`, and 502 with the driver's message when the printer request fails. `files` answers 501 `Printer host does not support file browsing` for a host type this runtime cannot read. The connection test answers 501 for those types, and `{ok: false, message: "Unknown integration type: <type>"}` for a type Node has no adapter for. Storage that cannot answer within 500 ms gives 503 `Printer settings are unavailable`.

Node's per-route budgets apply, one window per route per minute: capabilities 120, files 60, cameras 60, file content 20, camera view 20, connection test 5. As in Node, the root and `/api/v1` registrations each keep their own windows. Over budget, a route answers 429 with `Retry-After` and never reaches the printer.

## How it works

- `pp_storage::printer_hosts::printer_link` resolves a fleet printer (`printer.fleet`, tenant `default`) to its linked integration the way Node's `printerHostCapability` does: the printer id compared as a string, a trimmed `integration_id`, and an integration that exists with `config.enabled` not `false`. `printer_hosts::integration` reads one integration for the connection test.
- `pp_gateway::printer_host::HostReads` runs `pp_drivers::moonraker_driver::Driver` or `pp_drivers::prusalink::Driver` through one `HttpGuard` with a 2 MiB reply cap. Moonraker reads use its 8 s deadline. The PrusaLink driver sets a deadline on each request as Node does: 15 s for listings and the `object_list` prefix, 10 s for storage and the camera list, 8 s for status and info. PrusaLink listings use Node's 8 MiB cap. PrusaLink requests answer Digest challenges, and for a streamed read the deadline also covers the challenge probe. HTTPS hosts and redirects fail as the drivers document until GRE-359.
- `pp_api::printer_host` owns the HTTP mapping, path validation and budgets. `pp-core` implements `PrinterHostPort` over the two, so `pp-core` keeps no `pp-drivers` edge.
- Authentication and loopback admission are the admin routes' gate, as for `/api/v1/integrations/{id}/status`.

Every route is read-only on the printer and sends only GETs. It never uploads, starts a print, deletes or changes Checkoff. A Moonraker read sends each GET once. A PrusaLink read with a password sends a bodyless `GET /api/v1/status` before each GET, to get the Digest challenge. Without a password, a files read or connection test fails before sending anything, and the cameras list sends its GET unauthenticated, with no probe. A files read also lists storage when no storage is pinned. A GET sent with a password that gets a 401 with a fresh challenge is sent once more, as Node's adapter does. Nothing else is retried.

## Not served here yet

- Bambu reads (GRE-362). Bambu reads as having no capabilities.
- The print-file snapshot for a Build. Node's `files/content?profile_id=` saves the bytes it streams and returns an `X-PrintPartner-Snapshot` token that only the print-file assignment route redeems. That route is not served here yet (GRE-1210 builds the snapshot store; GRE-1214 wires it in), so a request with `profile_id` answers 501 `Print-file inspection for a Build is not available in this runtime yet` and never reaches the printer.
- Uploads and the send queue (GRE-365). Uploading needs an explicit user send and durable effect records (GRE-354). No Rust route calls `upload_file`.

## Streamed downloads and camera views

`files/content` and `cameras/view` stream the printer's reply as Node's `sendUpstreamResponse` does. A 2xx reply streams through with its `Content-Type`, `Content-Length` and `Cache-Control: private, no-store` (the gateway then sets `no-store` on every admitted reply). Unlike Node, both add `X-Content-Type-Options: nosniff` and `Content-Security-Policy: default-src 'none'; sandbox`, `files/content` adds `Content-Disposition: attachment`, and `cameras/view` passes only `image/jpeg`, `image/png`, `image/webp`, `image/gif` and `multipart/x-mixed-replace`, sending any other type as `application/octet-stream`. A printer can serve a listed file or a same-origin camera URL as HTML or SVG, and these replies come from Print Partner's own origin, so this keeps such a reply from running as a page of the app. The web client reads `files/content` with `fetch` and shows `cameras/view` in an `<img>`, which these headers do not change. A 404 from the printer answers 404 `Printer resource not found`; any other status answers 502 `Printer host returned HTTP <status>`. Neither reads the printer's body. A failed driver request answers 502 with its message, as the other routes do.

`pp_gateway::printer_host::HostReads` reads the body on a task through a four-chunk channel, so the printer is read only as fast as the client takes the bytes. A read error ends the client's body with an error instead of a clean end. Time spent waiting for a slow client counts against the printer body's deadline (the same absolute deadline, or the same idle allowance), so a client that stops reading cannot hold the printer connection past it; the client's body then ends with an error. A client that goes away stops a pending printer read at once and drops the printer connection. A non-2xx reply is never pumped: its body is dropped unread, which closes the connection.

The Moonraker driver opens these with its API key:

- `open_file(path)` streams `/server/files/gcodes/<path>`, each segment percent-encoded. The head and body share Node's 120 s deadline.
- `open_camera(id)` finds the camera the list reports under `id` (the trimmed `uid`, else `camera-<index>`). A disabled camera, or one whose stream or snapshot URL is cross-origin, is refused. It streams the MJPEG stream URL, else the snapshot URL. An MJPEG stream never ends on its own, so one view holds the connection for at most Node's 60 s.

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

The runtime test (`pp-core` `printer_host::ticket_t_21_ticket_t_22_*`) starts the Rust-only launcher on a disposable data directory against a loopback Moonraker and PrusaLink that replay `fixtures/drivers/moonraker/driver/routes.json` and `fixtures/drivers/prusalink/driver/routes.json`. It checks that each route answers 401 without a session, that browse and cameras equal the Node adapters' results in `node-oracle.json` and `node-oracle-reads.json`, that the connection tests succeed, that a listed print file and camera view from each printer stream through byte for byte with their length, that unlisted, cross-origin and disabled cameras, unlisted files and `profile_id` requests are refused, that every request the printers saw was a GET with no upload or print path, and that Checkoff, queue, job and settings rows are unchanged.
