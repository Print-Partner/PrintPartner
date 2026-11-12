# pp-core

Starts and stops one running app. It takes the storage lease, verifies the release, starts the Node child and the gateway, and shuts them down in a bounded order.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Composes `pp-storage`, `pp-compat`, and `pp-gateway` into `CoreRuntime`, run on a dedicated owner thread.
- Node-backed startup: release verification, storage lease (refusing any ownership marker), cutover recovery, Source Sync admission, loopback port, Node readiness (32-second budget), then the gateway.
- Rust-only startup (`start_for_launcher` with `desktop_rebuild` and `PP_DESKTOP_REBUILD=1|true`, in `rust_startup`): the storage writer takes the lease and makes the ownership marker decision first. Release verification, cutover recovery (no ownership index) and Source Sync admission run after it, only on an accepted directory; a failure in any of them shuts the writer down. Then the loopback port and the gateway with the `pp-api` routers. No Node child.
- Shutdown: rejects new work, drains within `MUTATION_DRAIN`, stops Node and the gateway, releases the lease, and reports a `ShutdownReceipt`. It can commit one operation cutover during that drain.
- Keeps the desktop state files: saved port, browser preferences, and desktop settings.
- Owns process launch for performance captures; `pp-perf` owns the records.
- Behind `desktop_rebuild`: provides `pp-api` router composition (`rebuild_api`) and the Source import and acquisition worker types. `CoreRuntime::start` never mounts those routers; `start_for_launcher` with both gates on serves them without Node.

## Not responsible for

- SQL, migrations, and the lease itself: `pp-storage`. Core calls its typed handles.
- Listeners, sessions, assets, and the ownership manifests: `pp-gateway`.
- Spawning and supervising Node: `pp-compat`.
- HTTP and MCP handlers: `pp-api`. Immutable Source bytes: `pp-source`.
- Windows, menus, and tray: `pp-desktop`. CLI and signals: `pp-server`.
- Measurement records and regression comparison: `pp-perf`.

## Interface

| Item | What it does |
| --- | --- |
| `CoreRuntime::start(DesktopLaunch)` | Node-backed entry point. `start_with_compat_config` takes a `SupervisorConfig`. `start_for_launcher` is the launchers' entry point: Rust-only with both gates on, otherwise `start`. |
| `CoreRuntime` methods | `origin`, `handle`, `take_launch_target`, `shutdown_requested`, `shutdown`, `shutdown_with_cutover`, `origin_change`, `runtime_dir`, `desktop_rebuild`. |
| `DesktopLaunch`, `VerifiedBundle`, `CoreStatus` | Launch input (`data_dir`, `bundle`, `assets`) and child status, re-exported from `pp-compat`. |
| `RuntimeStatus`, `StatusReceiver`, `RustStatus` | What `CoreHandle::subscribe` returns: the Node child's `CoreStatus`, or the Rust-only runtime's `RustStatus` (no child `generation` or `pid`). |
| `CoreHandle` | Clonable adapter handle: preferences and desktop settings, `suspend_monitoring`/`resume_monitoring`, `browser_launch_target`, `subscribe`, `compat_status`, `recover_compat`, `track_shutdown_work`. |
| `ShutdownReceipt`, `ShutdownGate`, `TrackedShutdownWork` | Shutdown evidence (`complete()`) and work that must finish before stop. |
| `DesktopRebuild` | `from_env` reads the runtime switch once at start. |
| `BrowserPreference(s)`, `Theme`, `PrinterPollInterval`, `DesktopSetting(s)`, `OriginChange` | Preference and settings types. |
| Re-exports | `pp_compat` drain helpers, `redact`, `CoreLog`, log readers; `pp_gateway::MUTATION_DRAIN`. |
| `perf_launch`, `perf_seed` | `capture_run`, `with_ready_app`, `write_capture`; `seed_printers` with `SimulatedPrinters`. |

Gated by `#[cfg(feature = "desktop_rebuild")]`:

- `rebuild_api`: `clients`, `router`, `read_router`, `draft_router`, `job_router`.
- `uploads` (`SourceImports`) and `source_acquisition` (`SourceAcquisitions`).
- `profile_interpreter` (`interpret_profile`), `review_observations` (`SnapshotReviewObserver`), `spoolman` (`SpoolmanInventory`, the shortfall route's inventory reader), `printer_status` (`StoredMoonrakerHosts` feeds the Rust-only gateway's watchers from storage; `StatusRoutes` mounts the authenticated status route over the gateway's lookup, and the printer-host reads over storage and the gateway's `HostReads`).
- `production::geometry` (`parse_accepted_stl`, `dimensions_um`), `layout` (`DirectoryLayout::resolve`).

Binary `pp-perf-capture` (`src/bin/pp-perf-capture.rs`, Linux only): `--kind --name --artifact --out` are required; `--package --runs --idle-secs --ready-timeout-secs --noise-pct --printers --rustc --node` are optional; the command follows `--`.

## Effects and flags

- Threads: one `pp-core-owner` thread with a current-thread Tokio runtime per `CoreRuntime`.
- Network: binds `127.0.0.1` on the port saved in `desktop.toml`, or a new port when that one is in use (`OriginChange::PersistedPortInUse`).
- Files in the data directory: `desktop.toml`, `browser-preferences.json`, `desktop-settings.json`, plus what `pp-storage`, `pp-compat`, and `pp-gateway` write. Core reads its files through `pp-os` without following links and publishes them by rename, or by hard link for a first publication so a concurrent winner is never replaced. On Windows, where a directory cannot be flushed, both go through `pp-os`'s write-through moves (`publish_rename`, `publish_new`), as do the gateway's admission receipt and cutover record.
- Child processes: the Node child through `pp-compat`. `perf_launch` spawns the measured command and reads `/proc`; `perf_seed` runs loopback Moonraker simulators on threads.
- Env: `PP_DESKTOP_REBUILD` turns the rebuild on only when it is exactly `1` or `true` and the crate is built with `desktop_rebuild`. `layout` reads `PP_PORTABLE_DIR`, `APPDATA`, `LOCALAPPDATA`, and `XDG_*_HOME`. Desktop settings pass `PP_EXCHANGE_DIR`, `PP_COMPOSE_FILE`, and other paths to the Node child.
- With the rebuild on: admits `ownership-manifest.json`, and `review_observations` makes outbound HTTP checked by `pp_net::outbound`, and `spoolman` makes Spoolman GETs through `pp_net::outbound::HttpGuard`.
- Cargo feature `desktop_rebuild` (default off): enables the gated modules, the optional `pp-api`, `pp-source`, `pp-net`, `reqwest`, and `axum` dependencies, and `desktop_rebuild` in `pp-compat`, `pp-storage`, `pp-source`, and `pp-api`. See [Desktop rebuild flag](../../../docs/desktop-rebuild.md).

## Tests

`send_worker::tests::recovery` runs the production send worker and checked driver
against `pp-testkit`'s loopback fault printer. The fake reads the complete upload,
then waits while the test verifies committed effect intent before accepting it.
Lost upload/start replies and a worker aborted before local confirmation retain
uncertainty across storage reopen. Tests check exact request counts, retained
staged bytes, no Checkoff completion and refused redispatch. An aborted future
and reopened writer model an interrupted attempt; this is not a whole-process
power-loss test. Other job kinds, route/worker cutover and final parent receipts
remain in the GRE-409 follow-ups.

- Unit tests in `src/` (`origin_tests.rs`, `review_observations_tests.rs`, `rust_startup/tests.rs` (Rust-only startup order on pp-testkit installations, feature only), `preferences/tests.rs`, `desktop_settings/tests.rs`, and others).
- Lifecycle: `runtime_m0.rs` (with `runtime_m0/`), `single_owner.rs`, `process_group_drain.rs`, `shutdown_gate_wait_idle.rs`, `source_sync_process_admission.rs`, `app_launch_capture.rs`.
- Feature-only: `production_geometry.rs`, `profile_interpreter*.rs`, `source_acquisition_*.rs`, `source_import_*.rs`, and `rebuild_*_http.rs` (empty stubs without the feature).
- Fixtures: `tests/fixtures/drain_child.rs` (compiled with `RUSTC`), `tests/fixtures/profile-parser/node-capture.json`, `tests/support/scratch.rs`, `tests/support/process.rs` (kill, stop and resume by PID: Unix signals, or `TerminateProcess`, `NtSuspendProcess` and `NtResumeProcess` on Windows), `tests/support/process_tree.rs` (descendants from `/proc`, `ps` or a Windows ToolHelp snapshot) and `src/tests.rs` (unit-test links, special entries, file access and self-termination per platform).
- The tests run on Linux, macOS and Windows. A test that checks a Unix-only fact names its Windows counterpart in a comment. Runtime tests need built web assets and `rust/bundle-manifest.json`, and use `PP_TEST_NODE` (default `/usr/bin/node`).

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-core
cargo nextest run --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild
```
