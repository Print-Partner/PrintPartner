# First-run printer probe tests

The probe tests use `pp-testkit::moonraker::RecordedPrinter` on ephemeral
loopback ports, a resolver that never completes and an injected HTTP port.
They must never contact a real printer or send LAN discovery traffic.
Address-policy refusal cases must stop before opening a connection.

```sh
cargo nextest run --manifest-path rust/Cargo.toml --locked -p pp-api -p pp-drivers --features desktop_rebuild --test printer_probe
cargo nextest run --manifest-path rust/Cargo.toml --locked -p pp-core --features desktop_rebuild --test printer_probe_http
npm --prefix web run test --workspace @print-partner/rust-client
(cd web && node --conditions=development --import tsx scripts/probe-connection-oracle.mjs)
```

`pp-api` compiles `desktop_rebuild` by default. The flag stays because `pp-drivers` does not.

The mounted route contract is covered by
`rebuild_api::tests::client_routes_match_registrations`. Its probe request uses
an unsupported kind so it opens no outbound connection. `pp-core`'s
`printer_probe_http` test sends an authenticated probe through the composed
`product_router` and checks the real classification of a recorded Moonraker,
with exactly one GET to `/server/info`.

The API test compares database and WAL bytes after authenticated probes and
checks admission and request limits. Its deadline test pauses the Tokio clock
after authentication, checks that the request is still pending at seven
seconds and times out at eight. A timer created before the pause can round up
by one millisecond, so the test expects 8.001 seconds. The driver deadline test
gives the network guard a 30-second transport deadline, so the probe's own
eight-second deadline is the one that fires. The driver suite also replays a
302 and checks that the redirect is not followed: one GET, an unidentified
result.

The Node oracle reads `fixtures/drivers/probe/connection-cases.json`, replaces
`fetch` with the recorded replies, and checks
`fixtures/drivers/probe/node-connections.json`. Add `--write` to regenerate the
oracle file. The driver suite replays the same cases through pp-testkit
printers, compares the saved adapter's connection-test result and request
sequence with Node's, and then checks the stricter unsaved probe result.

See [Printer-host reads](../printer-host-reads.md#first-run-probe) for the contract.
Run the full `just ci` before submitting this sensitive network change.
