# pp-server

The headless `pp-server` binary. It starts `pp_core::CoreRuntime` from command-line paths, prints the origin, and shuts down on a signal.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- The headless launcher for Docker and CI: argument parsing, Unix signals, and the process lifecycle around `CoreRuntime`.
- `pp-server healthcheck`: the liveness and readiness probe the Rust-only Docker image runs ([Docker image](../../../docs/docker.md#health)).
- `pp-server bootstrap-url`: headless first sign-in. Asks the running server for a fresh single-use sign-in URL over an owner-only Unix socket in the data directory ([Docker image](../../../docs/docker.md#sign-in)).
- Prints the loopback origin as its first stdout line when ready, then the `ShutdownReceipt` as JSON on exit.

## Not responsible for

- Startup, workers, the storage lease, and bounded shutdown: `pp-core`.
- Listeners, sessions, and assets: `pp-gateway`. The Node child: `pp-compat`.
- The desktop window, menus, and tray: `pp-desktop`.

## Interface

Binary only (`src/main.rs`, `src/args.rs`, `src/health.rs`, `src/bootstrap.rs`, `src/import.rs`); no library. Every flag takes a value; a repeated flag keeps the last one. Bad input exits non-zero before startup with `Unknown argument`, `Argument value missing`, or `--<flag> is required`.

| Argument | What it does |
| --- | --- |
| `--data DIR` | Required. Data directory passed to `DesktopLaunch::data_dir`. |
| `--web DIR` | Required. Web workspace root. Reads `package.json` for the version (`<version>-web`), the default runtime runs `apps/server/dist/current/desktop.js` and serves `apps/web/dist`. With both rebuild gates on, serves verified `apps/ui/dist` when installed, otherwise verified `apps/web/dist`, without Node. |
| `--node PATH` | Node executable for the compatibility child. Required unless the start is Rust-only (`desktop_rebuild` compiled in and `PP_DESKTOP_REBUILD` exactly `1` or `true`). |
| `--commit SHA` | Required. Must match the commit in the embedded bundle manifest. |
| `--status-file PATH` | Optional. Keeps a JSON status file with `pid`, `state` (`starting`, `ready`, `stopping`) and, once ready, `origin`. Written atomically, removed on exit. `pp-server healthcheck` reads it. |
| `--test-credential-file PATH` | Test only. Creates a new file, mode `0600` on Unix (on Windows it inherits its directory's ACL), with `bootstrap_url`, `child_pid`, and `runtime_dir`. `pp_core::perf_seed` uses it. The URL is a sign-in secret. |

`pp-server healthcheck <liveness|readiness> --status-file PATH [--timeout-ms N]` reads the status file, checks the recorded process is alive, and once ready sends one cookie-less `GET /health` to the loopback origin, which must answer below 500 within the timeout (default 2000 ms). Readiness fails while starting or stopping. Exits 0 or 1. `--help` prints usage.

`pp-server bootstrap-url --data DIR` (Unix only) prints a fresh sign-in URL from the `pp-server` running on `DIR`: single use, valid 60 seconds, minted by the gateway as the desktop's Open in browser does. A Rust-only server listens on `DIR/.pp-server-bootstrap.sock`, mode 0600, from startup until shutdown, and answers only a peer whose UID equals its own; a Node-backed start creates no socket. The command refuses a `DIR` another account owns or other accounts can write, a socket that is not owner-only, and a socket whose server runs as another account. With no server running it exits 1 and writes nothing. The server never prints or logs the URL. `--help` prints usage. On Windows the command exits non-zero.

`pp-server import --from PATH --dry-run` (`desktop_rebuild` builds, with `PP_DESKTOP_REBUILD` on) reports what importing a stopped Node data folder or a full backup archive would convert, through `pp_core::node_import`. It writes nothing to `PATH`, accepts and ignores `--data`, and exits 0 with the report, or 1 with `import refused: <reason>` (or `import failed: <reason>` for a read error) on stderr. Without `--dry-run` it exits non-zero: the import itself is not written yet ([Node import](../../../docs/node-import.md)). `--help` prints usage.

```sh
pp-server --data /tmp/pp-data --web ./web --node "$(command -v node)" --commit "$(git rev-parse HEAD)"
```

## Effects and flags

- Starts `CoreRuntime`, which takes the storage lease, binds a loopback port, and spawns Node (see `pp-core`).
- Registers `SIGTERM` and `SIGINT` before printing the origin; on Windows it registers Ctrl+C, Ctrl+Break, console close, logoff and system shutdown instead. Stops on any of them, or, with the Node-backed runtime, on a gateway shutdown request (`POST /auth/logout`). In Rust-only mode a sign-out revokes the account session and the server keeps serving. Exits non-zero when the receipt is incomplete.
- On Windows, Ctrl+C and Ctrl+Break wait for the whole shutdown, as SIGTERM and SIGINT do. A console close starts the same shutdown, but Windows ends the process when its grace period runs out, about 5 seconds. Windows delivers logoff and system shutdown events only to services: it ends an interactive `pp-server` at logoff or shutdown before any event arrives, and a `pp-server` that a service wrapper starts as a child never sees logoff or shutdown either; only the wrapper does. `pp-server` has no service integration of its own today, so those handlers are not reached yet. Either way the grace period is shorter than the shutdown can take: the gateway's mutation drain alone allows 5 seconds and the Node child gets up to 10 seconds after its stop request. If Windows ends `pp-server` first, the Node child ends with it because it runs in a kill-on-close Job Object, the storage lease is released by the operating system, and the next start recovers as it does after a crash.
- Reads `PP_DESKTOP_REBUILD` through `pp_core::DesktopRebuild` only to decide whether `--node` is required. `pp-core` reads it to select the runtime; `pp-compat` passes `PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_ALL`, and `TZ` to the Node child.
- Cargo features: `desktop_rebuild` (default off) enables `pp-core/desktop_rebuild`. Without it, `PP_DESKTOP_REBUILD` has no effect on a `-p pp-server` build.
- The released `Dockerfile` still runs Node directly. `Dockerfile.rust` runs `pp-server` Rust-only ([Docker image](../../../docs/docker.md)).

## Tests

- Unit tests for argument parsing in `src/args.rs`.
- `tests/cli.rs` runs the built binary with bad arguments and checks it fails without printing an origin. It borrows `pp-storage/tests/common/process.rs`. With `desktop_rebuild` it also checks the `import` arguments and runs `import --dry-run` on a synthetic Node folder (exit 0 with the report), with the runtime switch off, on a Rust-owned folder and on a missing path (exit 1 with the reason). The dry run itself is tested in `pp-storage` (`src/import/tests.rs`).
- `tests/rust_only_health.rs` (Unix, `desktop_rebuild`) starts a Rust-only `pp-server` without `--node`, checks both probes pass while it serves, then sends SIGTERM and checks the status file is gone.
- `src/bootstrap.rs` unit tests cover a fresh URL per request, only the exact request (`bootstrap-url\n` followed by the end of the client's writes) getting one, a silent client cut off at 5 s while another is answered, a failed mint, a panic unwinding past the listener, a symlink at the socket name, the 0600 socket, a peer with another UID, a malformed request, a stopping runtime, a stale socket and a foreign entry at the socket name, no server running, a directory another account owns or others can write, a socket open to other users, and a socket peer of another account.
- `tests/bootstrap_url.rs` (Unix, `desktop_rebuild`) starts a Rust-only `pp-server`, signs in once through `pp-server bootstrap-url`, checks the reused URL and a 61-second-old URL get 401, checks another directory and a stopped server fail without writing, and checks no URL or token reached the server's stdout, stderr or status file. It also checks `/` is refused for a non-root user. The expiry wait makes it take about 75 seconds.
- `src/health.rs` unit tests cover the probe states, a 503, a silent gateway, a non-loopback origin, malformed and stale status files, and the timeout bound.
- `tests/stop_signal.rs` starts the built binary, sends SIGTERM on Unix or Ctrl+Break on Windows (the binary runs in its own process group so the event does not reach the test), and checks that it exits 0 with a complete shutdown receipt. Console close, logoff and system shutdown cannot be raised from a test without ending the test's own console session.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-server
```
