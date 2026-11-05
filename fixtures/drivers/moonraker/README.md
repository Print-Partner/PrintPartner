# Moonraker status fixtures

These saved Moonraker response payloads are simulator fixtures based on the
existing TypeScript adapter examples and PrinterHostStatus contract. They are
not hardware captures. Controlled physical-printer status-only evidence remains
pending at GRE-335.

- `moonraker-success.json`, `moonraker-error.json`, `moonraker-offline.json`:
  bodies for `GET /printer/objects/query`. The Rust watcher simulators in
  `pp-drivers` and the Node/gateway test in `pp-core/tests/runtime_m0.rs`
  replay them.
- `status-parity.json` (GRE-449): HTTP 200 query bodies with the
  PrinterHostStatus that the TS adapter returns for each. A case either names
  one of the files above in `fixture` or carries its `body` inline. Two tests
  read it:
  - `web/apps/server/src/integrations/adapters/moonraker.fixtures.test.ts` runs
    every case through `moonrakerAdapter` and checks that status polling sends
    only the two status GETs.
  - `rust/crates/pp-drivers/tests/ticket_t_4_watcher_proof_parity.rs` runs every
    case through `recorded_status`.

- `host-snapshot-parity.json` (GRE-449): stored `integrations` settings and
  the Moonraker host list each projects to (`null` for a setting that fails
  closed). `web/apps/server/src/moonraker-host-snapshot.test.ts` runs every
  case through Node's `moonrakerHostSnapshot`, and
  `pp-storage` `printer_hosts` tests run them through `project`, which feeds
  the Rust-only runtime's watchers.

A case with `rust_rejects` names a deliberate Rust difference: `recorded_status`
refuses the body and the poller reports offline instead. The Node test still
asserts `expected`.

The Node adapter also reads `uptime_seconds` from `/machine/proc_stats`. The
Rust watcher does not make that second request, so the Node test answers it
with 404 and no case expects uptime.

Only the cases listed here are checked. When either mapping changes, add the
new edge case here.

```sh
(cd web && npm run test -w @print-partner/server -- src/integrations/adapters/moonraker.fixtures.test.ts --reporter=verbose)
cargo nextest run --locked --manifest-path rust/Cargo.toml -p pp-drivers --no-tests=fail -E 'test(ticket_t_4_watcher_proof)'
```
