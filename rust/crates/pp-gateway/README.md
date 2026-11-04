# pp-gateway

The loopback HTTP front door for one running app. It admits browser sessions, serves the React assets, and forwards each route to its owner, which today is the Node compatibility child.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Listener admission: loopback-only bind, exact `Host` match, `Origin` checks on effects, single-use 60-second bootstrap links that become a `pp_desktop_<port>` session cookie.
- Serves `apps/web/dist` with gzip negotiation (`src/assets.rs`) and forwards admitted routes through `pp_compat::Endpoint`.
- Answers a few routes itself: `/__desktop/bootstrap`, `/__desktop/rate-limit`, `/__runtime`, `POST /auth/logout`, and `/legal/summary` once that route is Rust-owned. It refuses `/mcp`.
- Defines `RustStatus`, which `/__runtime` reports for a runtime started with `Gateway::start_rust_only`. `pp-core` owns the sender.
- Runs Moonraker status watchers from `pp_drivers::moonraker` and the request drain used at shutdown.
- Temporary ownership code (`ownership`): which runtime owns each operation participant, and the cutover record that moves one from Node to Rust.

## Not responsible for

- Authentication, API keys, and MCP handlers: `pp-api`. This crate does not depend on `pp-api`.
- SQL of any kind: `pp-storage`. Printer protocols: `pp-drivers`, which is not send authority.
- Spawning or restarting Node: `pp-compat`. Startup order, the storage lease, and shutdown receipts: `pp-core`.

## Interface

| Item | What it does |
| --- | --- |
| `Gateway::start(listener, assets, compat, owners)` | Validates `operations.json` against `OperationOwners` and starts serving. |
| `Gateway` methods | `origin`, `take_launch_target`, `bootstrap_minter`, `monitoring_control`, `shutdown_requested`, `begin_drain`, `wait_idle`, `finish_drain`, `stop`. |
| `LaunchTarget`, `BootstrapMinter`, `MonitoringControl`, `ShutdownRequested` | First sign-in URL, new browser links, watcher suspend/resume, logout-triggered shutdown. |
| `MUTATION_DRAIN` | 5-second bound for in-flight mutations at shutdown. |
| `ownership` | Source Sync admission: `admit_bundled_source_sync`, `boot_bundled_source_sync`, `Owner`, `AdmissionError`. |
| `ownership::manifest` | `admit_bundled_manifest` builds the `OwnershipIndex` (`lookup`, `resources_for`, `admit_rollback`). |
| `ownership::cutover` | `recover`, `plan`, `CutoverPlan::commit`, `append_evidence`, `OperationOwners::owner_of`. |
| `ownership::resources`, `ownership::live_formats` | Table and file resource sets per operation, and the data formats each side can read. |

### Bundled manifests

Both JSON files are compiled in with `include_str!`. Do not edit them by hand.

- `operations.json`: the HTTP route registry. Each route has `owner` (`compat`), `effect`, and `operation`; `denied` lists refused routes. `rust/scripts/gateway-operations.py` writes it from `fixtures/http/routes-inventory.json`, which `rust/scripts/inspect-node-routes.mjs` produces from the built Node server. The pp-core test `ticket_t_7_ownership_http_registry_matches_fastify_union` runs the script with `--check`.
- `ownership-manifest.json` (schema `pp-ownership-manifest-v2`): `operations`, `node_participants`, `resources`, and `live_formats`. `admit_bundled_manifest` admits it, `tests/ticket_t_7_*.rs` check it, and `web/apps/server/src/assistant/operation-resources.test.ts` checks its resource sets against Node. The repo has no generator script for it.
- `scripts/operation-inventory.py` reads both to write `docs/parity/operation-inventory.md`.

```sh
python3 rust/scripts/gateway-operations.py fixtures/http/routes-inventory.json          # rewrite
python3 rust/scripts/gateway-operations.py fixtures/http/routes-inventory.json --check  # verify
```

## Effects and flags

- Accepts TCP on the loopback listener the caller binds; spawns Tokio tasks on the caller's runtime.
- Forwards requests over the Node child's Unix socket. Moonraker watchers open connections to configured printers.
- Writes in the data directory: `source-sync-admission.json`, `operation-cutover.json`, and `operation-cutover-evidence.jsonl`.
- Embeds `web/apps/server/src/data/source-sync-ownership.json`, `node-fenced-participants.json`, and `legal/LICENSE-SUMMARY.md` at compile time.
- Reads no environment variables. Cargo features: none.

## Tests

- Unit tests in `src/lib.rs`, `src/ticket_t_2.rs`, `src/ticket_t_3.rs`, and `src/ticket_t_11.rs`. They borrow scratch and cleanup helpers from `pp-storage/tests/common/`.
- `tests/ticket_t_19.rs` (bootstrap links, logout) and `tests/ticket_t_68.rs` (`Host` and `Origin` rejections).
- `tests/ticket_t_7_ownership*.rs`, `ticket_t_7_operation_resources.rs`, `ticket_t_7_live_formats.rs`, and `ticket_t_3_drain_cutover.rs` cover the manifests and cutover.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-gateway
```
