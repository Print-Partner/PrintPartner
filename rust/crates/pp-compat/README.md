# pp-compat

Supervises the Node compatibility server as a child process and forwards requests to it over a private channel: a Unix socket, or a named pipe on Windows. It retires with Node.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- Verifies the measured release, frontend and backend, against the embedded bundle manifest once during core startup (`verify_bundle`). Before each child restart the supervisor rechecks only the backend files (`release::verify_backend`).
- Spawns, watches, restarts with backoff, crash-guards, and stops the Node child (`Supervisor`).
- Forwards HTTP to the child over a per-generation private channel with a private key (`Endpoint::forward`).
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
| `drain` | Process-group stop helpers: `drive_shutdown_group`, `reap_process_group`, `SIGTERM_GRACE`. On Windows the group is the child's Job Object. |
| `redact` | `Scrubber`, `scrub`, `REDACTED`: the scrub policy for logs, the Logs view, and opt-in Sentry. |
| `settings` | `ENVIRONMENT` maps each Node env name to an `Owner`; `fixed_env`. |
| `CoreLog`, `log_folder`, `recent_logs`, `recent_diagnostics` | Log writing and bounded reads. |
| `spawn_child` | Starts any other child process of the host (`pp-core` re-exports it). It shares one lock with the compat spawn; see Effects. |
| `DesktopAccountPin` | Hash of `desktop-account.json`; `Disabled` unless built with `desktop_rebuild` and enabled at runtime. The file must be a regular file owned by this user. On Unix its mode must be exactly 0600; on Windows its DACL may grant only this user, SYSTEM and Administrators, because the profile folder gives the file inherited ACEs for both and both are root-equivalent. |

## Effects and flags

- Platform code lives in `src/unix.rs` and `src/windows.rs`; shared code calls `platform::` and never names an OS.
- Unix: spawns Node in its own process group with a parent-death pipe and the inherited storage lock descriptor. A planned stop sends SIGTERM to the group after closing stdin, then SIGKILL after `SIGTERM_GRACE`.
- Windows: creates Node inside a new Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` through `CreateProcessW` and `PROC_THREAD_ATTRIBUTE_JOB_LIST`, so Node never exists outside the job, even if the core dies during the spawn. Node inherits only its three stdio pipe ends (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`) and gets no console window. Those ends are inheritable only from just before `CreateProcessW` until just after it, on every path, and only while the spawn holds the lock that `spawn_child` also takes. A std or tokio spawn passes `bInheritHandles=TRUE` with no handle list, so a child started outside that lock in the window would keep Node's pipes open: Node would never see stdin EOF and the log readers would never reach EOF. Every Windows-reachable spawner outside tests goes through `spawn_child`: the desktop's open-data, reveal-logs and Open in browser actions (`pp-desktop/src/recovery.rs`) and the sponsor opener (`native_actions.rs`, Linux and macOS only). `pp-core`, `pp-gateway` and `pp-server` start no other process in the host; `pp-core`'s `perf_launch` runs only in the separate `pp-perf-capture` binary. Spawns inside third-party crates (Tauri, WebView2, `rfd`) cannot take the lock. Every process Node starts joins the job. Only the core holds the job handle, so when the core closes it, by exiting or being killed, the kernel terminates the whole job; Node exiting does not end the job; this replaces both the process group and the parent-death watcher. A planned stop closes stdin, which is the only stop request Node gets, then terminates the job after `SIGTERM_GRACE` with exit code `0x5050_4b4c`. Because Node runs with `CREATE_NO_WINDOW`, the job also holds the conhost.exe that serves Node's hidden console, and conhost exits a moment after Node. So when Node exits on its own, the drain waits up to 500 ms (`GROUP_SETTLE`) for the job's `JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO` completion-port message before it reaps; without that wait a clean exit could be reported as forced. A process still in the job after the 500 ms is terminated and the stop is reported as forced, as before. The core keeps the storage lock: a Windows byte-range lock belongs to the process that took it, and the job guarantees Node never outlives the core.
- The child gets a cleared environment plus the inherited names in `platform::INHERITED_ENV` (Unix: `PATH`, `HOME`, `TMPDIR`, `LANG`, `LC_ALL`, `TZ`; Windows: `PATH`, `SystemRoot`, `TEMP`, `TMP`, `USERPROFILE`, `LOCALAPPDATA`, `APPDATA`, `TZ`), `PP_COMMIT`, `settings::fixed_env`, and the desktop settings `pp-core` passes in.
- Unix: Node binds `<runtime_dir>/<generation>.sock`, and the core removes it on stop. Windows: the core creates `\\.\pipe\printpartner-compat-<generation>` as the first instance, with a DACL that grants only the current user and `PIPE_REJECT_REMOTE_CLIENTS`, and always keeps one instance listening; Node dials in and serves one request per connection. Node keeps four dials outstanding, each connecting or connected and not yet used, and replaces one as soon as the core sends a request on it rather than when it closes, so long-lived WebSocket streams cannot use them up; a core request that still finds no dial within 5 seconds fails instead of waiting forever. There is no TCP fallback. Writes logs under `<data>/logs/`. Hashes release files on a blocking thread.
- Log files are private to the current user. On Unix the directory gets mode 0700 and each file 0600, and entries are opened relative to the directory descriptor. On Windows (`src/logs/windows.rs`) the directory and each file get a protected DACL that grants only the current user (the checks accept no ACE for any other account, SYSTEM and Administrators included), the directory stays open without `FILE_SHARE_DELETE` so it cannot be renamed or replaced while logs are open, every entry operation (append open, exclusive create, the archive security open, and the replacing rotation rename) runs through `pp_os::Dir` relative to that handle, never by path, and every open except the archive security open refuses a reparse point (that open cannot read attributes, so a reparse point is restricted, refused by the log checks, and given its saved DACL back; its target is never written). The read-only tail readers (`recent_logs`, `recent_diagnostics`) still open by path and are pinned only while some other `Logs` in the process holds the directory open. The Unix side still calls `openat` and `renameat` through libc. Both refuse hard-linked logs.
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
