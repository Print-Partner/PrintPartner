# Bambu LAN driver (GRE-362)

`pp_drivers::bambu` reads a Bambu Lab printer's status over its LAN MQTT broker, as Node's `integrations/adapters/bambu.ts` does. It is status-only: it never uploads a file or controls a print. The Rust-only runtime's Test connection uses it to pin a printer's certificate ([Pinning in Settings](#pinning-in-settings)), and the status watchers keep a pinned session open to each Bambu printer ([Status watcher](#status-watcher)). Both run only behind `desktop_rebuild` and `PP_DESKTOP_REBUILD`.

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

- `POST /api/v1/integrations/{id}/test` (Settings → Test connection) is the only writer. It first applies Node's tenant admission: the `bambu` feature choice off or unset answers 403 `FEATURE_DISABLED`, a corrupt choice 500 `FEATURE_TOGGLES_INVALID`, both before any network. With no pin it reads with `Trust::FirstUse` and stores the presented certificate's hash only after a status report arrived, so a peer that never speaks Bambu MQTT is not pinned. The insert never replaces a row. If two tests race, the stored pin wins: the other test succeeds when it saw the same certificate and answers the certificate-changed failure when it saw a different one. With a pin it reads with `Trust::Pinned`, so a changed certificate fails before MQTT CONNECT and nothing is stored.
- A pin counts only for the host and port it was taken for. After the user edits either, Test connection answers `Printer address changed since its certificate was pinned. Reset its certificate in Settings.` without connecting. Nothing re-pins silently.
- `GET /api/v1/integrations/{id}/tls-pin` answers `{"pinned": false}` or `{"pinned": true, "sha256_prefix": "<12 hex>", "pinned_at": "<RFC 3339>"}`, never the whole hash. `POST /api/v1/integrations/{id}/tls-pin/reset` deletes the row and answers `{"reset": true}` (`false` when there was none). Both answer 404 `Integration not found` for an unknown id, sit behind the same authentication and loopback admission as the other integration routes, and are limited to 60 and 5 requests a minute.
- Settings → Printers shows a Bambu row's pin (`Certificate pinned · ab12cd34ef56 · <date>` or `Not pinned. Test connection to pin.`) and a **Reset certificate** button with a confirmation that the printer must be the same device. The row renders only when health advertises the desktop rebuild and the runtime serves the pin route. Node advertises the rebuild too but has no `tls-pin` route, so there the GET answers 404 and the row is hidden.

Bambu is a `Host::Status` target in `pp-core`: capabilities read `{files: false, cameras: false, status: true}` for a row the watchers take, and the files and camera routes answer unsupported. No Rust route uploads to a Bambu printer.

## Status watcher

GRE-1176 puts Bambu under the same `Watchers` lifecycle as Moonraker and PrusaLink: one task per enabled host, inside the 128-host cap, cancelled and joined on a config change, suspend or shutdown, and started again on resume. Storage takes Bambu rows the way it takes PrusaLink rows (stored order, after every Moonraker row, first row per id), and a row with no address (`host`, `hostname` or `ip`) still takes a cap slot and its id, but the watchers give it no task. `pp_storage::printer_hosts::read` adds each row's stored pin from `printer_tls_pins`.

- The watcher never pins. With no pin it shows offline `Certificate not pinned yet. Run Test connection in Settings.` and makes no connection. A pin taken at another host or port shows the address-changed message, also without connecting. A row whose settings the driver refuses shows the driver's message. The watchers re-read the setting every second, so a Test connection that stores a pin, or a reset, restarts the host's task.
- `Driver::watch` opens one session with `Trust::Pinned` only, so a changed certificate fails before CONNECT. It subscribes, sends pushall, and publishes a status for every report: the first full report (one with a non-blank `gcode_state`), then each later report's `print` fields merged into it, since the printer sends only what changed. Every session starts from nothing, since changes made while no session was open never arrive as changes: until its first full report the host reads `Awaiting status`.
- It sends PINGREQ every 15 s and treats 30 s without a packet as a dropped session. A dropped session reads offline with the driver's message and reconnects with the pollers' backoff from a 5 s interval: 10 s after a session that had reported, doubling with each attempt that did not, up to 60 s, each wait up to a fifth shorter for jitter.
- pushall goes out at most once every five minutes per host task, across reconnects. A session that reconnects inside that window reads `Awaiting status` until the printer sends a full report on its own, and sends the deferred pushall once the five minutes have passed. `last_heard` moves, and the backoff resets, only on a report from the printer.
- The status route, farm reads, `/ws/printers` and capabilities treat Bambu as watched, like PrusaLink ([Farm reads](farm-reads.md)).

One gap: a Test connection while the watcher holds a session opens a second MQTT client, and a printer that limits concurrent clients may refuse it. The recorded broker accepts both.

## Status mapping

`status_from_print` ports Node's `statusFromBambuPrint` and `mapBambuGcodeState`, including JavaScript's `Number()` for string fields. `fixtures/drivers/bambu/driver/prints.json` holds the four recorded fixtures and edge cases. `bambu.driver-oracle.test.ts` runs them through Node and checks `node-oracle-status.json` (`UPDATE_BAMBU_ORACLE=1` rewrites it); `ticket_t_23_bambu.rs` checks the Rust mapping against the same file.

## Tests

`pp_testkit::bambu::Broker` is a loopback MQTT broker over TLS. Each test generates its certificate with rcgen, so the repository holds no key material. The broker logs each connection's steps (TCP, TLS or TLS refused, CONNECT with whether the access code matched, SUBSCRIBE, PUBLISH, DISCONNECT) and can switch to a new certificate to act as a replaced printer. The tests in `rust/crates/pp-drivers/tests/ticket_t_23_bambu.rs` cover the exchange, checked DNS to a pinned connection, a changed certificate refused before CONNECT, v1 and v3 certificates over TLS 1.2 and 1.3, a matching pin served without its private key, a report before SUBACK, hosts the guard refuses, and broker failures. `rust/crates/pp-core/tests/runtime_m0/bambu_pin.rs` runs the Rust-only launcher against the broker: the three routes refuse an unauthenticated request before the printer sees anything, Test connection pins, a replaced certificate fails without a second CONNECT, and a reset lets the next Test connection pin the new certificate. No test talks to a real printer.
