# Printer-host reads (GRE-360)

With the `desktop_rebuild` Cargo feature and `PP_DESKTOP_REBUILD` exactly `1` or `true`, the Rust-only runtime serves the Moonraker printer-host reads that Node's `routes/printers.ts` and `routes/integrations.ts` serve, at the same `/api/v1` paths:

| Route | Answer |
| --- | --- |
| `GET /api/v1/printers/{id}/capabilities` | `{files, cameras, status}`. All three are `true` for a printer linked to an enabled Moonraker integration. A printer with no link, a missing or disabled integration, or a host type this runtime cannot read answers all `false`. An unknown printer answers 404 `Printer not found`. |
| `GET /api/v1/printers/{id}/files?path=` | One directory of the Moonraker `gcodes` root as `{path, entries}`. An empty or missing `path` is the root. |
| `GET /api/v1/printers/{id}/cameras` | `{cameras}` from `/server/webcams/list`. A host type without cameras in this runtime answers `{cameras: []}`. |
| `POST /api/v1/integrations/{id}/test` | Node's `{ok, message}` from `/server/info`, for example `Connected (klippy: ready)`. An unknown integration answers 404 `Integration not found`. |

The `files` and `cameras` routes return the same errors as Node: 400 for an invalid storage path, 404 for an unknown printer, 409 `Printer is not linked to a host`, 503 `Printer host is unavailable`, and 502 with the driver's message when the printer request fails. `files` answers 501 `Printer host does not support file browsing` for a host type this runtime cannot read. The connection test answers 501 for those types, and `{ok: false, message: "Unknown integration type: <type>"}` for a type Node has no adapter for. Storage that cannot answer within 500 ms gives 503 `Printer settings are unavailable`.

Node's per-route budgets apply, one window per route per minute: capabilities 120, files 60, cameras 60, connection test 5. Over budget, a route answers 429 with `Retry-After` and never reaches the printer.

## How it works

- `pp_storage::printer_hosts::printer_link` resolves a fleet printer (`printer.fleet`, tenant `default`) to its linked integration the way Node's `printerHostCapability` does: the printer id compared as a string, a trimmed `integration_id`, and an integration that exists with `config.enabled` not `false`. `printer_hosts::integration` reads one integration for the connection test.
- `pp_gateway::printer_host::MoonrakerReads` runs the `pp_drivers::moonraker_driver::Driver` through one `HttpGuard` with an 8 s deadline and a 2 MiB reply cap. HTTPS hosts and redirects fail as the driver documents until GRE-359.
- `pp_api::printer_host` owns the HTTP mapping, path validation and budgets. `pp-core` implements `PrinterHostPort` over the two, so `pp-core` keeps no `pp-drivers` edge.
- Authentication and loopback admission are the admin routes' gate, as for `/api/v1/integrations/{id}/status`.

Every route is read-only on the printer. It sends only the GETs it needs, once per request, and never uploads, starts a print, deletes, retries or changes Checkoff.

## Not served here yet

- PrusaLink and Bambu reads (GRE-1044, GRE-362). Until then they read as having no capabilities.
- `GET /printers/{id}/files/content` and `/cameras/view`, which stream past the 2 MiB guard limit and, for print files, write a snapshot.
- Uploads and the send queue (GRE-365). Uploading needs an explicit user send and durable effect records (GRE-354). No Rust route calls `upload_file`.

## Verification

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail \
  --features pp-core/desktop_rebuild,pp-api/desktop_rebuild \
  -E 'test(ticket_t_21)'
```

The runtime test (`pp-core` `printer_host::ticket_t_21_*`) starts the Rust-only launcher on a disposable data directory against a loopback Moonraker that replays `fixtures/drivers/moonraker/driver/routes.json`. It checks that each route answers 401 without a session, that browse, cameras and the connection test equal the Node adapter's results in `node-oracle.json`, that every request the printer saw was a GET with no upload or print path, and that Checkoff, queue, job and settings rows are unchanged.
