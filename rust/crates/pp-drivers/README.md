# pp-drivers

Printer protocol adapters for Moonraker and PrusaLink, plus HTTP Digest auth and farm filament verdicts. Every printer request goes through `pp-net`'s `HttpGuard`. It is not send authority: printer sends need an explicit user action, decided outside this crate.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Speaks the Moonraker and PrusaLink HTTP protocols: connection test, status, filament used, `object_list`, cameras, storage browse, and upload with an optional start flag. Ported from the Node adapters.
- Runs bounded, cancellable, status-only Moonraker pollers for the gateway (`moonraker::Watchers`).
- Derives filament slot states and swap verdicts from slot inventory and host status (`farm_filament`, `desktop_rebuild` only).

## Not responsible for

- **Send authority.** This crate runs a request when called; it never decides that a print should be sent. A printer send needs an explicit user action, admitted outside this crate: `pp-storage` owns admitted queue and effect records and `pp-core` runs the workers. No Rust production code calls `upload_file` today (the printer-host routes use only the read methods); only this crate's tests do, against local simulators. Never call it against a real printer to test something. Each upload request is sent once; when `UploadResult::outcome_unknown` is set, reconcile the printer state instead of resending.
- Queue, effect, deduction and delivery records, and any SQL: `pp-storage`.
- Watcher lifetime and snapshot intake: `pp-gateway` holds `Watchers` and answers `/api/v1/integrations/:id/status` today; the target owner is `pp-core`, which also runs workers.
- DNS, address policy, deadlines and body limits: `pp-net`.
- Status DTOs: `pp-api-types::printer_status`.
- Bambu, Spoolman, Home Assistant, webhooks and Discord: no adapter here yet.

## Interface

| Module | Main items |
| --- | --- |
| `moonraker_driver` | `Driver::new(guard, base_url, api_key)`; `test_connection`, `status`, `filament_used`, `object_list`, `cameras`, `browse`, `upload_file(source, filename, start)`. `UploadSource`, `UploadResult`, `TestResult`, `StorageListing`, `Camera`. |
| `prusalink` | `Driver::new(guard, base_url, username, password, storage)` with the same methods, plus `open_file(path)` (streams the same-origin `refs.download`, 120 s idle deadline) and `open_camera(id)` (streams `/snap` within a 30 s total deadline), both returning `pp_net::outbound::StreamReply`. Listings may be 8 MiB. Uploads go to the pinned storage, default `usb`; `start` sets the `print-after-upload` header. |
| `moonraker` | `Watchers` (`new`, `with_guard`, `without_node_fallback`, `observations`, `replace`, `status`, `route`, `stop`), `Host`, `Route`, `recorded_status`, limits `MAX_HOSTS` and `MAX_SNAPSHOT_BYTES`. `PrinterObservations` is the read-only port over the latest status per host (`latest`, `all`, `changed`, and `last_heard`: the last successful poll, kept while offline), backed by a `watch` channel so a slow reader never blocks a poller; a stopped host leaves it. `without_node_fallback` (Rust-only runtime) reports HTTPS and redirecting hosts offline (`HTTPS_UNAVAILABLE`, `REDIRECT_UNAVAILABLE`) instead of routing them to Node. No storage or send APIs. |
| `digest` | `parse_www_authenticate`, `authorization`, `cnonce`: RFC 7616 MD5 and MD5-sess, `qop=auth`. |
| `farm_filament` (`desktop_rebuild`) | `printer_filament_status`, `slot_filament_status`, `host_reports_filament_runout`, `SlotState`, `LOW_FILAMENT_THRESHOLD_G`. Pure; inventory comes through a `SpoolLookup` closure. |

`pp-gateway` uses `moonraker`, and `moonraker_driver`'s read methods for the Rust-only printer-host routes. No binaries.

## Effects and flags

- Network: HTTP to printer hosts through `HttpGuard`. HTTPS is refused and redirects are not followed. Buffered replies are capped at 2 MiB, PrusaLink listings at 8 MiB; PrusaLink downloads and snapshots stream.
- DNS: `Watchers::new` uses `SystemResolver`, a 5 s request deadline and a 2 s poll interval.
- Tokio: `Watchers` spawns one polling task per enabled plain-HTTP host, up to `MAX_HOSTS`.
- Files: `UploadSource::Path` reads the file to upload.
- No SQLite, env vars or child processes.

Cargo features:

- `desktop_rebuild` (default off): compiles `farm_filament` and turns on `pp-api-types/desktop_rebuild`.

## Tests

- `tests/ticket_t_4_watcher_proof.rs`, `tests/ticket_t_4_watcher_proof_parity.rs`: Moonraker pollers and the shared status corpus in `fixtures/drivers/moonraker`.
- `tests/ticket_t_21_moonraker_driver.rs`, `tests/ticket_t_21_moonraker_upload.rs`: replay `fixtures/drivers/moonraker/driver`, including lost-connection uploads.
- `tests/ticket_t_22_prusalink.rs`, `tests/ticket_t_22_prusalink_reads.rs`: replay `fixtures/drivers/prusalink` through a loopback simulator. The reads file also covers downloads, snapshots, 8 MiB listings and the `object_list` prefix (GRE-1044).
- Unit tests in `src/moonraker.rs`, `src/moonraker_driver.rs` and `src/farm_filament.rs`.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-drivers
cargo nextest run --manifest-path rust/Cargo.toml -p pp-drivers --features desktop_rebuild
```
