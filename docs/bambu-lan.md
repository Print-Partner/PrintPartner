# Bambu LAN driver (GRE-362)

`pp_drivers::bambu` reads a Bambu Lab printer's status over its LAN MQTT broker, as Node's `integrations/adapters/bambu.ts` does. It is status-only: it never uploads a file or controls a print. The Rust-only runtime's Test connection uses it to pin a printer's certificate ([Pinning in Settings](#pinning-in-settings)). GRE-1176 adds the status watcher. Both run only behind `desktop_rebuild` and `PP_DESKTOP_REBUILD`.

## One status read

`Driver::status(trust)` does what Node's `getStatus` does, in one connection:

1. `HttpGuard::connect` opens the TCP connection with the same checks as every Rust HTTP request: an IP literal or one DNS lookup, every answer `permitted` (private and loopback addresses yes; link-local, documentation and other special ranges no), connections only to those checked addresses, and no proxy. A refused host never gets a packet.
2. TLS 1.2 or 1.3 with rustls. The verifier checks the certificate as described below.
3. MQTT 3.1.1 CONNECT as user `bblp` with the access code, clean session, 30 s keep-alive.
4. SUBSCRIBE to `device/<serial>/report`, then one PUBLISH of `{"pushing":{"sequence_id":"0","command":"pushall"}}` to `device/<serial>/request`.
5. The first PUBLISH on the report topic whose `print.gcode_state` is a non-blank string is mapped to a `PrinterHostStatus`. Other packets and reports are skipped. A usable report that arrives before SUBACK (a retained one, say) answers the read without a pushall, as Node's message listener does.
6. DISCONNECT and close.

Steps 1 to 3 have 8 s (Node's `CONNECT_TIMEOUT_MS`); the whole read has 10 s (`STATUS_TIMEOUT_MS`). A packet over 1 MiB fails the read before its body is read. Every failure is an `offline` status. Where Node has a message it is Node's, for example `Bambu MQTT connection closed (LAN mode off or connection refused?)` or `Connection refused: Not authorized`. Rust adds `Bambu MQTT TLS handshake failed`, `Bambu MQTT packet is too large`, the guard's `Prohibited DNS answer` and `Host unavailable`, and the certificate-changed message. A connect phase that runs out of its 8 s reports the closed message; mqtt.js would report `connack timeout`. `ip_address` is set once the host check passes, as in Node. The access code never appears in a status. Nothing is retried.

The MQTT code (`bambu::mqtt`) can build only CONNECT, one SUBSCRIBE, the pushall PUBLISH and DISCONNECT, so no print command can leave the driver.

`Config::from_stored` reads an integration's `config` as Node's `resolveConnection` does: `host`/`hostname`/`ip`, `access_code`/`accessCode` (`****` counts as missing), `serial`/`device_id`/`deviceId` matching `^[A-Za-z0-9_-]{6,64}$`, and `port` (default 8883), with Node's error messages.

## Certificate pinning

Node turns TLS verification off for private addresses. Rust never does. Every read names a `Trust`:

- `Trust::Pinned(sha256)` accepts only the end-entity certificate whose SHA-256 over its DER is the stored pin. Any other certificate fails the TLS handshake with `Printer certificate changed. If you replaced or reset the printer, reset its certificate in Settings.`, so MQTT CONNECT and the access code never reach that peer. The printer's own self-signed chain and host name are not checked, because the pin identifies the certificate exactly.
- `Trust::FirstUse` accepts the presented certificate. Only an explicit user action may use it (Settings → Test connection, GRE-1245), never a watcher.

Both still verify the TLS 1.2 or 1.3 handshake signature against the certificate's public key, so the peer must hold its private key. The driver reads that key from the certificate itself instead of through webpki, which accepts only X.509 v3, because Node's adapter notes that Bambu printers present self-signed v1 certificates. `Read::certificate` reports the fingerprint the printer presented, so the caller can store it after a first-use read or show it after a mismatch.

## Pinning in Settings

The Rust-only runtime stores one pin per Bambu integration in `printer_tls_pins` (schema step 47, `pp_storage::tls_pins`): the host and port it was taken for, the certificate's SHA-256 and `pinned_at`. Node has no such table and never reads it.

- `POST /api/v1/integrations/{id}/test` (Settings → Test connection) is the only writer. It first applies Node's tenant admission: the `bambu` feature choice off or unset answers 403 `FEATURE_DISABLED`, a corrupt choice 500 `FEATURE_TOGGLES_INVALID`, both before any network. With no pin it reads with `Trust::FirstUse` and stores the presented certificate's hash only after a status report arrived, so a peer that never speaks Bambu MQTT is not pinned. The insert never replaces a row. Test connection and **Reset certificate** take a per-integration lock in the runtime, held from reading the pin through the network test to storing the pin. Once a pin write or reset is queued, only the writer's answer (applied or refused) or a stopped writer releases the lock, never elapsed time or a dropped request: the writer runs a queued command even after its caller stopped waiting. The request answers Busy 10.5 seconds after its pin write or reset starts, and a request that cannot take the lock within 30 seconds answers Busy without connecting. A writer that stops answering while the write is still queued keeps the lock, so that integration's Test connections fail closed. A second Test connection on the same integration waits, then reads the first one's pin, so it never sends the access code to a different certificate under `Trust::FirstUse`. A reset during a first-use test runs after that test, so the pin the test stores does not outlive the reset. With a pin it reads with `Trust::Pinned`, so a changed certificate fails before MQTT CONNECT and nothing is stored.
- A pin counts only for the host and port it was taken for. After the user edits either, Test connection answers `Printer address changed since its certificate was pinned. Reset its certificate in Settings.` without connecting. Nothing re-pins silently.
- `GET /api/v1/integrations/{id}/tls-pin` answers `{"pinned": false}` or `{"pinned": true, "sha256_prefix": "<12 hex>", "pinned_at": "<RFC 3339>"}`, never the whole hash. `POST /api/v1/integrations/{id}/tls-pin/reset` deletes the row and answers `{"reset": true}` (`false` when there was none). Both answer 404 `Integration not found` for an unknown id, sit behind the same authentication and loopback admission as the other integration routes, and are limited to 60 and 5 requests a minute.
- Settings → Printers shows a Bambu row's pin (`Certificate pinned · ab12cd34ef56 · <date>` or `Not pinned. Test connection to pin.`) and a **Reset certificate** button with a confirmation that the printer must be the same device. The row renders only when health advertises the desktop rebuild and the runtime serves the pin route. Node advertises the rebuild too but has no `tls-pin` route, so there the GET answers 404 and the row is hidden.

Bambu is a `Host::Status` target in `pp-core`: capabilities read all false and the files and camera routes answer unsupported. No Rust route uploads to a Bambu printer.

## Status mapping

`status_from_print` ports Node's `statusFromBambuPrint` and `mapBambuGcodeState`, including JavaScript's `Number()` for string fields. `fixtures/drivers/bambu/driver/prints.json` holds the four recorded fixtures and edge cases. `bambu.driver-oracle.test.ts` runs them through Node and checks `node-oracle-status.json` (`UPDATE_BAMBU_ORACLE=1` rewrites it); `ticket_t_23_bambu.rs` checks the Rust mapping against the same file.

## Tests

`pp_testkit::bambu::Broker` is a loopback MQTT broker over TLS. Each test generates its certificate with rcgen, so the repository holds no key material. The broker logs each connection's steps (TCP, TLS or TLS refused, CONNECT with whether the access code matched, SUBSCRIBE, PUBLISH, DISCONNECT) and can switch to a new certificate to act as a replaced printer. The tests in `rust/crates/pp-drivers/tests/ticket_t_23_bambu.rs` cover the exchange, checked DNS to a pinned connection, a changed certificate refused before CONNECT, v1 and v3 certificates over TLS 1.2 and 1.3, a matching pin served without its private key, a report before SUBACK, hosts the guard refuses, and broker failures. `rust/crates/pp-core/tests/runtime_m0/bambu_pin.rs` runs the Rust-only launcher against the broker: the three routes refuse an unauthenticated request before the printer sees anything, Test connection pins, a replaced certificate fails without a second CONNECT, and a reset lets the next Test connection pin the new certificate. Its race test holds one Test connection at the broker: a second first-use test on the same integration stays blocked, then fails on the other certificate with no CONNECT, and a reset waits for the first-use test and leaves no pin. `pp-core` unit tests in `printer_host/pin_lock.rs` and `printer_host/tests/stalled_pin.rs` hold a pin write unanswered: a later Test connection against another certificate stays blocked and sends no CONNECT, the lock survives a dropped request, and an hour of paused-clock time, until the writer applies, refuses or stops, and a cancelled waiter leaves no lock entry behind. No test talks to a real printer.
