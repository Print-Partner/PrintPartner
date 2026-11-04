# pp-compat

Supervises the Node compatibility server as a child process and forwards requests to it over a private Unix socket. It retires with Node.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Verifies the measured release, frontend and backend, against the embedded bundle manifest once during core startup (`verify_bundle`). Before each child restart the supervisor rechecks only the backend files (`release::verify_backend`).
- Spawns, watches, restarts with backoff, crash-guards, and stops the Node child (`Supervisor`).
- Forwards HTTP to the child over a per-generation Unix socket with a private key (`Endpoint::forward`).
- Owns the local log files, the one redaction policy, and the table of Node environment settings.

## Not responsible for

- Listeners, sessions, routing, and ownership decisions: `pp-gateway`.
- The storage lease, startup order, and shutdown receipts: `pp-core`. No SQL; only `pp-storage` writes SQL.
- Product behavior. Node owns the operations it still serves.

## Interface

| Item | What it does |
| --- | --- |
| `Supervisor::start`, `start_with_config`, `disarm_restarts`, `shutdown` | Child lifecycle. `SpawnSpec` and `SupervisorConfig` configure it; `shutdown` returns `CompatExit`. |
| `CompatHandle` | Clonable handle: `status` (`watch` of `Status`), `endpoint()`, `recover()` after the crash guard trips. |
| `Endpoint` | `forward`, `rate_limit_bucket`, `bound_to`. |
| `Bundle`, `MeasuredBundle`, `verify_bundle` | Release identity and the proof that its files match the manifest. |
| `drain` | Process-group stop helpers: `drive_shutdown_group`, `reap_process_group`, `SIGTERM_GRACE`. |
| `redact` | `Scrubber`, `scrub`, `REDACTED`: the scrub policy for logs, the Logs view, and opt-in Sentry. |
| `settings` | `ENVIRONMENT` maps each Node env name to an `Owner`; `fixed_env`. |
| `CoreLog`, `log_folder`, `recent_logs`, `recent_diagnostics` | Log writing and bounded reads. |
| `DesktopAccountPin` | Hash of `desktop-account.json`; `Disabled` unless built with `desktop_rebuild` and enabled at runtime. |

## Effects and flags

- Spawns Node in its own process group with a parent-death pipe and the inherited storage lock descriptor. The child gets a cleared environment plus `PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_ALL`, `TZ`, `PP_COMMIT`, `settings::fixed_env`, and the desktop settings `pp-core` passes in.
- Creates `<runtime_dir>/<generation>.sock` and removes it on stop. Writes logs under `<data>/logs/`. Hashes release files on a blocking thread.
- `build.rs` copies the bundle manifest into `OUT_DIR`, and `release::manifest` embeds it. Source: `PP_DESKTOP_RELEASE_MANIFEST`, else `rust/bundle-manifest.json`. The build fails if the file is missing; generate it with `rust/scripts/build-desktop-manifest.py` (`just ci --only web-assets`). A change to the file or variable rebuilds the crate.
- Cargo feature `desktop_rebuild` (default off): turns on `DesktopAccountPin` capture. `pp-core` enables it with its own `desktop_rebuild` feature.

## Tests

- Unit tests in `src/lib.rs`, `src/logs.rs`, `src/redact.rs`, `src/release.rs`, `src/settings.rs`, `src/account_pin.rs`, and `src/ticket_t_2.rs` (with `src/tests/built_startup.rs`). Node fixtures are in `src/fixtures/` and `tests/fixtures/`. Child tests use `PP_TEST_NODE`, else `node`.
- `tests/ticket_t_10_draftreads_bundle.rs`: `Bundle::community_manifests_dir`. `tests/ticket_t_11_packaging.rs`: runs `rust/scripts/precompress-spa.py`. `tests/installed_bundle_lifecycle.rs`: macOS only.
- `tests/process-group-cleanup.md` records why one drain fixture runs in its own process.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-compat
cargo nextest run --manifest-path rust/Cargo.toml -p pp-compat --features desktop_rebuild
```
