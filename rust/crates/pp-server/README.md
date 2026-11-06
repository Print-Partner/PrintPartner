# pp-server

The headless `pp-server` binary. It starts `pp_core::CoreRuntime` from command-line paths, prints the origin, and shuts down on a signal.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- The headless launcher for Docker and CI: argument parsing, Unix signals, and the process lifecycle around `CoreRuntime`.
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
| `--node PATH` | Required. Node executable for the compatibility child. |
| `--commit SHA` | Required. Must match the commit in the embedded bundle manifest. |
| `--test-credential-file PATH` | Test only. Creates a new `0600` file with `bootstrap_url`, `child_pid`, and `runtime_dir`. `pp_core::perf_seed` uses it. The URL is a sign-in secret. |

```sh
pp-server --data /tmp/pp-data --web ./web --node "$(command -v node)" --commit "$(git rev-parse HEAD)"
```

## Effects and flags

- Starts `CoreRuntime`, which takes the storage lease, binds a loopback port, and spawns Node (see `pp-core`).
- Registers `SIGTERM` and `SIGINT` (on Windows, Ctrl+C, Ctrl+Break and console close) before printing the origin. Stops on any of them or on a gateway shutdown request (`POST /auth/logout`). Exits non-zero when the receipt is incomplete.
- Reads no environment variables itself. `pp-core` reads `PP_DESKTOP_REBUILD`; `pp-compat` passes `PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_ALL`, and `TZ` to the Node child.
- Cargo features: none. It does not enable `pp-core/desktop_rebuild`, so `PP_DESKTOP_REBUILD` has no effect on a `-p pp-server` build.
- The current `Dockerfile` still runs Node directly, not `pp-server`.

## Tests

- Unit tests for argument parsing in `src/args.rs`.
- `tests/cli.rs` runs the built binary with bad arguments and checks it fails without printing an origin. It borrows `pp-storage/tests/common/process.rs`.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-server
```
