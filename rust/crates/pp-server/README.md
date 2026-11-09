# pp-server

The headless `pp-server` binary. It starts `pp_core::CoreRuntime` from command-line paths, prints the origin, and shuts down on a signal.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- The headless launcher for Docker and CI: argument parsing, Unix signals, and the process lifecycle around `CoreRuntime`.
- `pp-server healthcheck`: the liveness and readiness probe the Rust-only Docker image runs ([Docker image](../../../docs/docker.md#health)).
- Prints the loopback origin as its first stdout line when ready, then the `ShutdownReceipt` as JSON on exit.

## Not responsible for

- Startup, workers, the storage lease, and bounded shutdown: `pp-core`.
- Listeners, sessions, and assets: `pp-gateway`. The Node child: `pp-compat`.
- The desktop window, menus, and tray: `pp-desktop`.

## Interface

Binary only (`src/main.rs`, `src/args.rs`); no library. Every flag takes a value; a repeated flag keeps the last one. Bad input exits non-zero before startup with `Unknown argument`, `Argument value missing`, or `--<flag> is required`.

| Argument | What it does |
| --- | --- |
| `--data DIR` | Required. Data directory passed to `DesktopLaunch::data_dir`. |
| `--web DIR` | Required. Web workspace root. Reads `package.json` for the version (`<version>-web`), runs `apps/server/dist/current/desktop.js`, serves `apps/web/dist`. |
| `--node PATH` | Node executable for the compatibility child. Required unless the start is Rust-only (`desktop_rebuild` compiled in and `PP_DESKTOP_REBUILD` exactly `1` or `true`). |
| `--commit SHA` | Required. Must match the commit in the embedded bundle manifest. |
| `--status-file PATH` | Optional. Keeps a JSON status file with `pid`, `state` (`starting`, `ready`, `stopping`) and, once ready, `origin`. Written atomically, removed on exit. `pp-server healthcheck` reads it. |
| `--test-credential-file PATH` | Test only. Creates a new file, mode `0600` on Unix (on Windows it inherits its directory's ACL), with `bootstrap_url`, `child_pid`, and `runtime_dir`. `pp_core::perf_seed` uses it. The URL is a sign-in secret. |

`pp-server healthcheck <liveness|readiness> --status-file PATH [--timeout-ms N]` reads the status file, checks the recorded process is alive, and once ready sends one cookie-less `GET /health` to the loopback origin, which must answer below 500 within the timeout (default 2000 ms). Readiness fails while starting or stopping. Exits 0 or 1. `--help` prints usage.

```sh
pp-server --data /tmp/pp-data --web ./web --node "$(command -v node)" --commit "$(git rev-parse HEAD)"
```

## Effects and flags

- Starts `CoreRuntime`, which takes the storage lease, binds a loopback port, and spawns Node (see `pp-core`).
- Registers `SIGTERM` and `SIGINT` before printing the origin; on Windows it registers Ctrl+C, Ctrl+Break, console close, logoff and system shutdown instead. Stops on any of them or on a gateway shutdown request (`POST /auth/logout`). Exits non-zero when the receipt is incomplete.
- On Windows, Ctrl+C and Ctrl+Break wait for the whole shutdown, as SIGTERM and SIGINT do. A console close starts the same shutdown, but Windows ends the process when its grace period runs out, about 5 seconds. Windows delivers logoff and system shutdown events only to services: it ends an interactive `pp-server` at logoff or shutdown before any event arrives, and a `pp-server` that a service wrapper starts as a child never sees logoff or shutdown either; only the wrapper does. `pp-server` has no service integration of its own today, so those handlers are not reached yet. Either way the grace period is shorter than the shutdown can take: the gateway's mutation drain alone allows 5 seconds and the Node child gets up to 10 seconds after its stop request. If Windows ends `pp-server` first, the Node child ends with it because it runs in a kill-on-close Job Object, the storage lease is released by the operating system, and the next start recovers as it does after a crash.
- Reads `PP_DESKTOP_REBUILD` through `pp_core::DesktopRebuild` only to decide whether `--node` is required. `pp-core` reads it to select the runtime; `pp-compat` passes `PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_ALL`, and `TZ` to the Node child.
- Cargo features: `desktop_rebuild` (default off) enables `pp-core/desktop_rebuild`. Without it, `PP_DESKTOP_REBUILD` has no effect on a `-p pp-server` build.
- The released `Dockerfile` still runs Node directly. `Dockerfile.rust` runs `pp-server` Rust-only ([Docker image](../../../docs/docker.md)).

## Tests

- Unit tests for argument parsing in `src/args.rs`.
- `tests/cli.rs` runs the built binary with bad arguments and checks it fails without printing an origin. It borrows `pp-storage/tests/common/process.rs`.
- `tests/rust_only_health.rs` (Unix, `desktop_rebuild`) starts a Rust-only `pp-server` without `--node`, checks both probes pass while it serves, then sends SIGTERM and checks the status file is gone.
- `src/health.rs` unit tests cover the probe states, a 503, a silent gateway, a non-loopback origin, malformed and stale status files, and the timeout bound.
- `tests/stop_signal.rs` starts the built binary, sends SIGTERM on Unix or Ctrl+Break on Windows (the binary runs in its own process group so the event does not reach the test), and checks that it exits 0 with a complete shutdown receipt. Console close, logoff and system shutdown cannot be raised from a test without ending the test's own console session.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-server
```
