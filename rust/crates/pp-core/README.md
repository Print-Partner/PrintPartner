# pp-core

Starts and stops one running app. It takes the storage lease, verifies the release, starts the Node child and the gateway, and shuts them down in a bounded order.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Composes `pp-storage`, `pp-compat`, and `pp-gateway` into `CoreRuntime`, run on a dedicated owner thread.
- Startup: storage lease, release verification, cutover recovery, Source Sync admission, loopback port, Node readiness (32-second budget), then the gateway.
- Shutdown: rejects new work, drains within `MUTATION_DRAIN`, stops Node and the gateway, releases the lease, and reports a `ShutdownReceipt`. It can commit one operation cutover during that drain.
- Keeps the desktop state files: saved port, browser preferences, and desktop settings.
- Owns process launch for performance captures; `pp-perf` owns the records.
- Behind `desktop_rebuild`: provides `pp-api` router composition (`rebuild_api`) and the Source import and acquisition worker types. `CoreRuntime` startup does not mount those routers or start those workers yet; integration tests and explicit callers do. A launch with both gates on still serves the Node compatibility runtime.

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
| `CoreRuntime::start(DesktopLaunch)` | Entry point. `start_with_compat_config` takes a `SupervisorConfig`. |
| `CoreRuntime` methods | `origin`, `handle`, `take_launch_target`, `shutdown_requested`, `shutdown`, `shutdown_with_cutover`, `origin_change`, `runtime_dir`, `desktop_rebuild`. |
| `DesktopLaunch`, `VerifiedBundle`, `CoreStatus` | Launch input (`data_dir`, `bundle`, `assets`) and child status, re-exported from `pp-compat`. |
| `CoreHandle` | Clonable adapter handle: preferences and desktop settings, `suspend_monitoring`/`resume_monitoring`, `browser_launch_target`, `subscribe`, `recover_compat`, `track_shutdown_work`. |
| `ShutdownReceipt`, `ShutdownGate`, `TrackedShutdownWork` | Shutdown evidence (`complete()`) and work that must finish before stop. |
| `DesktopRebuild` | `from_env` reads the runtime switch once at start. |
| `BrowserPreference(s)`, `Theme`, `PrinterPollInterval`, `DesktopSetting(s)`, `OriginChange` | Preference and settings types. |
| Re-exports | `pp_compat` drain helpers, `redact`, `CoreLog`, log readers; `pp_gateway::MUTATION_DRAIN`. |
| `perf_launch`, `perf_seed` | `capture_run`, `with_ready_app`, `write_capture`; `seed_printers` with `SimulatedPrinters`. |

Gated by `#[cfg(feature = "desktop_rebuild")]`:

- `rebuild_api`: `clients`, `router`, `read_router`, `draft_router`, `job_router`.
- `uploads` (`SourceImports`) and `source_acquisition` (`SourceAcquisitions`).
- `profile_interpreter` (`interpret_profile`), `review_observations` (`SnapshotReviewObserver`).
- `production::geometry` (`parse_accepted_stl`, `dimensions_um`), `layout` (`DirectoryLayout::resolve`).

Binary `pp-perf-capture` (`src/bin/pp-perf-capture.rs`, Linux only): `--kind --name --artifact --out` are required; `--package --runs --idle-secs --ready-timeout-secs --noise-pct --printers --rustc --node` are optional; the command follows `--`.

## Effects and flags

- Threads: one `pp-core-owner` thread with a current-thread Tokio runtime per `CoreRuntime`.
- Network: binds `127.0.0.1` on the port saved in `desktop.toml`, or a new port when that one is in use (`OriginChange::PersistedPortInUse`).
- Files in the data directory: `desktop.toml`, `browser-preferences.json`, `desktop-settings.json`, plus what `pp-storage`, `pp-compat`, and `pp-gateway` write.
- Child processes: the Node child through `pp-compat`. `perf_launch` spawns the measured command and reads `/proc`; `perf_seed` runs loopback Moonraker simulators on threads.
- Env: `PP_DESKTOP_REBUILD` turns the rebuild on only when it is exactly `1` or `true` and the crate is built with `desktop_rebuild`. `layout` reads `PP_PORTABLE_DIR`, `APPDATA`, `LOCALAPPDATA`, and `XDG_*_HOME`. Desktop settings pass `PP_EXCHANGE_DIR`, `PP_COMPOSE_FILE`, and other paths to the Node child.
- With the rebuild on: admits `ownership-manifest.json`, and `review_observations` makes outbound HTTP checked by `pp_net::outbound`.
- Cargo feature `desktop_rebuild` (default off): enables the gated modules, the optional `pp-api`, `pp-source`, `pp-net`, `reqwest`, and `axum` dependencies, and `desktop_rebuild` in `pp-compat`, `pp-storage`, `pp-source`, and `pp-api`. See [Desktop rebuild flag](../../../docs/desktop-rebuild.md).

## Tests

- Unit tests in `src/` (`origin_tests.rs`, `review_observations_tests.rs`, `preferences/tests.rs`, `desktop_settings/tests.rs`, and others).
- Lifecycle: `runtime_m0.rs` (with `runtime_m0/`), `ticket_t_30_single_instance.rs`, `ticket_t_3_drain.rs`, `shutdown_gate_wait_idle.rs`, `source_sync_process_admission.rs`, `ticket_t_53_capture.rs`.
- Feature-only: `production_geometry.rs`, `profile_interpreter*.rs`, `source_acquisition_*.rs`, `source_import_*.rs`, and `rebuild_*_http.rs` (empty stubs without the feature).
- Fixtures: `tests/fixtures/drain_child.rs` (compiled with `RUSTC`), `tests/fixtures/profile-parser/node-capture.json`, `tests/support/scratch.rs`. Runtime tests need built web assets and `rust/bundle-manifest.json`, and use `PP_TEST_NODE` (default `/usr/bin/node`).

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-core
cargo nextest run --manifest-path rust/Cargo.toml -p pp-core --features desktop_rebuild
```
