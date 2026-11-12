# pp-gateway

The HTTP front door for one running app. It admits browser sessions, serves the React assets, and sends each route to its selected Rust or Node owner. Desktop startup uses loopback; an explicit headless listener can accept network peers.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Listener admission: loopback by default with an exact configured `Host`; explicit headless mode validates a literal-IP `Host` and matching browser `Origin`. Both modes check `Origin` on effects and use single-use 60-second bootstrap links that become a `pp_desktop_<port>` session cookie.
- Serves `apps/web/dist` with gzip negotiation (`src/assets.rs`) and forwards admitted routes through `pp_compat::Endpoint`.
- Answers a few routes itself: `/__desktop/bootstrap`, `/__desktop/rate-limit`, `/__runtime`, `POST /auth/logout`, and `/legal/summary` once that route is Rust-owned. It refuses `/mcp`.
- Defines `RustStatus`, which `/__runtime` reports for a runtime started with `Gateway::start_rust_only`. `pp-core` owns the sender.
- Closes a connection when a write stays pending because the peer accepts no bytes for 30 seconds (`stall::WRITE_STALL`). A peer that stops reading while socket buffers still have room is not detected. At shutdown, `text/event-stream` product responses stop counting toward the drain as soon as draining begins.
- Runs Moonraker status watchers from `pp_drivers::moonraker` and the request drain used at shutdown. With Node, watchers take hosts from Node's `/__desktop/moonraker-hosts` and the gateway answers cached statuses itself. In a Rust-only runtime they take hosts from the caller's `MoonrakerHosts` source, and an authenticated product route answers through `MoonrakerStatus`.
- Runs on-demand Moonraker and PrusaLink reads for the Rust-only runtime's printer-host routes (`printer_host::HostReads`: connection test, print-file browse, camera list) through `pp_drivers::moonraker_driver` and `pp_drivers::prusalink`. Read-only; no upload or print start.
- Temporary ownership code (`ownership`): which runtime owns each operation participant, and the cutover record that moves one from Node to Rust.

## Not responsible for

- Authentication, API keys, and MCP handlers: `pp-api`. This crate does not depend on `pp-api`.
- SQL of any kind: `pp-storage`. Printer protocols: `pp-drivers`, which is not send authority.
- Spawning or restarting Node: `pp-compat`. Startup order, the storage lease, and shutdown receipts: `pp-core`.

## Interface

| Item | What it does |
| --- | --- |
| `Gateway::start(listener, assets, compat, owners)` | Validates `operations.json` against `OperationOwners` and starts serving. |
| `Gateway::start_rust_only(listener, assets, ui, product, status, hosts)` | No Node child. `ServedUi::Legacy` serves the old build from disk with its existing CSP and gzip negotiation. `product` builds Rust routes from a `MoonrakerStatus` lookup; `hosts` supplies the printer status source. |
| `Gateway::start_rust_only_with_assets(listener, assets, files, product, status, hosts)` | Serves the replacement bytes retained by release verification. Requests use only that immutable inventory, without disk reads or gzip sibling lookup. Sends `src/replacement-csp.txt` and templates each document with a fresh style nonce; see [Content security policy](../../../docs/network-sharing.md#content-security-policy). |
| `Gateway::start_rust_only_with_network(listener, network, assets, (ui, snapshot), product, status, hosts)` | Explicit headless admission when `network` is true. `(ServedUi::Replacement, Some(snapshot))` serves the verified replacement bytes as `start_rust_only_with_assets` does; `(ServedUi::Legacy, None)` serves the legacy build as `start_rust_only` does. Preserves the real `ConnectInfo` peer and validates the browser-visible IP authority, including a published Docker port. Bootstrap admission remains required; `pp-core` keeps the product's account-login guards. MCP stays disabled. See [Docker admission](../../../docs/docker.md#run). |
| `Gateway` methods | `origin`, `take_launch_target`, `bootstrap_minter`, `monitoring_control`, `printer_observations`, `shutdown_requested`, `begin_drain`, `wait_idle`, `finish_drain`, `stop`. |
| `LaunchTarget`, `BootstrapMinter`, `MonitoringControl`, `ShutdownRequested` | First sign-in URL, new browser links, watcher suspend/resume, logout-triggered shutdown. |
| `printer_host::HostReads` | `new()` (8 s deadline, 2 MiB replies), `test`, `browse`, `cameras` for one `HostTarget` (`Moonraker { base_url, api_key }` or `PrusaLink { base_url, username, password, storage }`); JSON values in Node's shapes. Read-only. |
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

- Accepts TCP on the caller's listener: loopback by default, or an explicit headless IP listener through `start_rust_only_with_network`. Spawns Tokio tasks on the caller's runtime.
- Forwards requests over the Node child's Unix socket. Moonraker watchers open connections to configured printers.
- Writes in the data directory: `source-sync-admission.json`, `operation-cutover.json`, and `operation-cutover-evidence.jsonl`.
- Embeds `web/apps/server/src/data/source-sync-ownership.json`, `node-fenced-participants.json`, and `legal/LICENSE-SUMMARY.md` at compile time.
- Reads no environment variables. Cargo features: none.

## Tests

- Unit tests in `src/lib.rs`, `src/ticket_t_2.rs`, `src/ticket_t_3.rs`, `src/ticket_t_11.rs`, and `src/ticket_t_4_watcher_proof.rs` (Rust-only watchers against recorded Moonraker bodies). They borrow scratch and cleanup helpers from `pp-storage/tests/common/`.
- `tests/ticket_t_19.rs` (bootstrap links, logout) and `tests/ticket_t_68.rs` (`Host` and `Origin` rejections).
- `tests/ticket_t_7_ownership*.rs`, `ticket_t_7_operation_resources.rs`, `ticket_t_7_live_formats.rs`, and `ticket_t_3_drain_cutover.rs` cover the manifests and cutover.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-gateway
```

Replacement document navigation uses SPA fallback for extensionless paths such
as `/today`, `/farm/log` and `/builds/12/history`. `/api`, `/ws`, `/health`,
`/assets` and `/icons` remain service or asset namespaces; missing files return
404. Legacy client-path selection is unchanged.
