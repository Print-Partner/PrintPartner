# Bambu LAN driver (GRE-362)

`pp_drivers::bambu` reads a Bambu Lab printer's status over its LAN MQTT broker, as Node's `integrations/adapters/bambu.ts` does. It is status-only: it never uploads a file or controls a print. No route or watcher calls it yet. GRE-1245 adds certificate pinning on Settings → Test connection and the reset route, and GRE-1176 adds the status watcher, all behind `desktop_rebuild` and `PP_DESKTOP_REBUILD`.

## One status read

`Driver::status(trust)` does what Node's `getStatus` does, in one connection:

1. `HttpGuard::connect` opens the TCP connection with the same checks as every Rust HTTP request: an IP literal or one DNS lookup, every answer `permitted` (private and loopback addresses yes; link-local, documentation and other special ranges no), connections only to those checked addresses, and no proxy. A refused host never gets a packet.
2. TLS 1.2 or 1.3 with rustls. The verifier checks the certificate as described below.
3. MQTT 3.1.1 CONNECT as user `bblp` with the access code, clean session, 30 s keep-alive.
4. SUBSCRIBE to `device/<serial>/report`, then one PUBLISH of `{"pushing":{"sequence_id":"0","command":"pushall"}}` to `device/<serial>/request`.
5. The first PUBLISH on the report topic whose `print.gcode_state` is a non-blank string is mapped to a `PrinterHostStatus`. Other packets and reports are skipped.
6. DISCONNECT and close.

Steps 1 to 3 have 8 s (Node's `CONNECT_TIMEOUT_MS`); the whole read has 10 s (`STATUS_TIMEOUT_MS`). A packet over 1 MiB fails the read before its body is read. Every failure is an `offline` status with Node's message, for example `Bambu MQTT connection closed (LAN mode off or connection refused?)` or `Connection refused: Not authorized`. The access code never appears in a status. Nothing is retried.

The MQTT code (`bambu::mqtt`) can build only CONNECT, one SUBSCRIBE, the pushall PUBLISH and DISCONNECT, so no print command can leave the driver.

`Config::from_stored` reads an integration's `config` as Node's `resolveConnection` does: `host`/`hostname`/`ip`, `access_code`/`accessCode` (`****` counts as missing), `serial`/`device_id`/`deviceId` matching `^[A-Za-z0-9_-]{6,64}$`, and `port` (default 8883), with Node's error messages.

## Certificate pinning

Node turns TLS verification off for private addresses. Rust never does. Every read names a `Trust`:

- `Trust::Pinned(sha256)` accepts only the end-entity certificate whose SHA-256 over its DER is the stored pin. Any other certificate fails the TLS handshake with `Printer certificate changed. If you replaced or reset the printer, reset its certificate in Settings.`, so MQTT CONNECT and the access code never reach that peer. The printer's own self-signed chain and host name are not checked, because the pin identifies the certificate exactly.
- `Trust::FirstUse` accepts the presented certificate. Only an explicit user action may use it (Settings → Test connection, GRE-1245), never a watcher.

Both still verify the TLS handshake signatures, so the peer must hold the certificate's private key. `Read::certificate` reports the fingerprint the printer presented, so the caller can store it after a first-use read or show it after a mismatch.

## Status mapping

`status_from_print` ports Node's `statusFromBambuPrint` and `mapBambuGcodeState`, including JavaScript's `Number()` for string fields. `fixtures/drivers/bambu/driver/prints.json` holds the four recorded fixtures and edge cases. `bambu.driver-oracle.test.ts` runs them through Node and checks `node-oracle-status.json` (`UPDATE_BAMBU_ORACLE=1` rewrites it); `ticket_t_23_bambu.rs` checks the Rust mapping against the same file.

## Tests

`pp_testkit::bambu::Broker` is a loopback MQTT broker over TLS. Each test generates its certificate with rcgen, so the repository holds no key material. The broker logs each connection's steps (TCP, TLS or TLS refused, CONNECT with whether the access code matched, SUBSCRIBE, PUBLISH, DISCONNECT) and can switch to a new certificate to act as a replaced printer. The tests in `rust/crates/pp-drivers/tests/ticket_t_23_bambu.rs` cover the exchange, checked DNS to a pinned connection, a changed certificate refused before CONNECT, hosts the guard refuses, and broker failures. No test talks to a real printer.
