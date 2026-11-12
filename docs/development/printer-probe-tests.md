# First-run printer probe tests

The probe tests use `pp-testkit::moonraker::RecordedPrinter` on ephemeral
loopback ports, a resolver that never completes and an injected HTTP port.
They must never contact a real printer or send LAN discovery traffic.
Address-policy refusal cases must stop before opening a connection.

```sh
cargo nextest run --manifest-path rust/Cargo.toml --locked -p pp-api -p pp-drivers --features desktop_rebuild --test printer_probe
npm --prefix web run check --workspace @print-partner/rust-client
(cd web && node --conditions=development --import tsx scripts/probe-connection-oracle.mjs)
```

The mounted route contract is covered by
`rebuild_api::tests::client_routes_match_registrations`. Its probe request uses
an unsupported kind for the inventory pass, then classifies a recorded
Moonraker through the composed router with a real authenticated HTTP request. The API test compares
database and WAL bytes after authenticated probes and checks admission,
request limits and an unresponsive port's eight-second deadline. The API
clock is paused only after authentication. The driver deadline test uses a
30-second transport deadline and proves the probe stops at eight seconds; both
deadline tests check that the request is still pending at seven seconds.

See [Printer-host reads](../printer-host-reads.md#first-run-probe) for the contract.
Run the full `just ci` before submitting this sensitive network change.

The Node oracle reads `fixtures/drivers/probe/connection-cases.json`, replaces
fetch with recorded replies, and checks `node-connections.json`. Add `--write`
to regenerate the oracle. The Rust replay uses pp-testkit printers and compares
both the saved adapter result and request sequence with Node before asserting
the documented stricter unsaved result. The 302 case proves redirects stay
unfollowed, with exactly one GET.
