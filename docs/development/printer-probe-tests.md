# First-run printer probe tests

The probe tests use `pp-testkit::moonraker::RecordedPrinter` on ephemeral
loopback ports, a resolver that never completes and an injected HTTP port.
They must never contact a real printer or send LAN discovery traffic.
Address-policy refusal cases must stop before opening a connection.

```sh
cargo nextest run --manifest-path rust/Cargo.toml --locked -p pp-api -p pp-drivers --features desktop_rebuild --test printer_probe
npm --prefix web run test --workspace @print-partner/rust-client
```

The mounted route contract is covered by
`rebuild_api::tests::client_routes_match_registrations`. Its probe request uses
an unsupported kind so it opens no outbound connection. The API test compares
database and WAL bytes after authenticated probes and checks admission,
request limits and an unresponsive port's eight-second deadline.

See [Printer-host reads](../printer-host-reads.md#first-run-probe) for the contract.
Run the full `just ci` before submitting this sensitive network change.
