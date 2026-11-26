# Storage

Print Partner keeps its SQLite database and local files in one data directory.

## Data directory

| Runtime | Location |
| --- | --- |
| Desktop app | `~/.print-partner` by default |
| Docker image | `/data` inside the container (the Node image today) |
| Node server | `PRINT_PARTNER_DATA_DIR` |

The Rust core uses SQLite only. It does not support Postgres.

## One writer

Only one Print Partner runtime may own a data directory at a time. The Rust core and standalone Rust writer acquire an exclusive OS advisory lock on `<data-dir>/printpartner.lock` through `pp_os::try_lock`: `flock` via [fs4](https://docs.rs/fs4/1.1.0/fs4/) on Unix, and on Windows a `LockFileEx` lock on the one byte at offset `i64::MAX`, so other handles can still read the file (see "Lock range on Windows" in `rust/crates/pp-os/src/lib.rs`). They take it before database preflight, migrations, or any SQLite open. A second instance refuses with `Data directory already owned; stop the other Print Partner process before starting another instance`. Rust creates `printpartner.lock` and `.desktop-lease` with `O_EXCL` and opens an existing entry without `O_CREAT`. On macOS, racing `O_CREAT` opens relative to a directory descriptor can fail with `ENOENT` (GRE-673).

When lease acquisition fails, Rust removes a `printpartner.lock` or `.desktop-lease` that this call created, but only when no other holder can exist and the name still refers to that file. Acquisition here means `AdmittedDirectory::lock` (the directory and lock identity checks, `try_lock`, the reservation open, for example `EISDIR` when standalone Node publishes its `.desktop-lease` directory first) and `StorageLease::acquire_locked`; the ownership-marker refusals and both database preflights in `WriterOwner::open` use the same rule: the preflight under the lock (`LockedDirectory::abandon`) and the one after the lease is held (`StorageLease::abandon`, which also removes the private runtime directory it created). Rust removes the file through the held directory descriptor while the file is still open, while it holds that file's advisory lock, and only if the name still has the opened file's device and inode. When the failure came before Rust held the lock, it tries to take the lock first. The created file stays in place when `try_lock` reports `WouldBlock` (another owner holds it), when taking the lock fails with any other error (the holder is unknown), and when the name now refers to a different file (a replaced lock or reservation, which stays too). A lease file that existed before the attempt is never removed by this cleanup and keeps its bytes and modification time. On Windows the removal is `pp_os::Dir::remove_file`; a failed removal leaves the file, and the original error is still returned. Failures after both preflights pass (creating the storage directories, `schema::initialize`, job and send-queue recovery, and opening the domain-event feed and readers) still release the lease normally, which removes `.desktop-lease`, including a reused leftover.

The runtime holds the lock while it can access storage, including compatibility-child restarts. Graceful shutdown releases it after database closure and child reaping. If writer closure or cleanup cannot be proved, the runtime retains ownership until a successful cleanup retry or process exit. The kernel releases the lock when its last owning descriptor closes, including after a crash or SIGKILL. The lock file remains on disk. Its presence does not mean the directory is owned, and startup does not delete it.

The supervised Node compatibility child inherits the locked descriptor at fd 198. The core sets `PP_STORAGE_CORE_LOCK=1`. Private parent setup validates the inherited descriptor against `printpartner.lock` and verifies the parent PID and canonical data directory. The child then opens only that directory under the core's lock. Setting the environment flag alone does not bypass standalone ownership checks. Supervised descendants that access storage retain the descriptor. Detached Bambu Connect launchers replace fd 198 with `/dev/null` before execution, so those external applications cannot retain the lock after a core crash.

Until standalone Node retires, Rust reserves the `.desktop-lease` name with a regular file while holding the advisory lock. Standalone Node publishes a directory at that same name. Neither publication can replace the other file type, so Rust and standalone Node exclude each other before opening the database. Rust refuses an existing directory without reading its contents, checking PIDs, or recovering stale leases. Standalone Node, including today's Docker image, keeps its existing directory lease behavior.

Standalone Node refuses a data directory that holds the [ownership marker](#ownership-marker-format) `.pp-rust-owned.json`, so a mistaken rollback cannot write into Rust-owned data. Any entry at that name refuses: a valid marker, an unparseable file, an unknown version, a directory or a symlink. Node checks only that the name exists and does not read the file. The check runs in `acquireDataDirectory` after Node takes its `.desktop-lease` directory and before it inspects `.desktop-owner.json`. On refusal Node releases the lease and changes nothing else, then fails with `Data directory is owned by the Rust runtime (.pp-rust-owned.json); standalone Node will not open it`. Every standalone entrypoint goes through that function: the server (`node dist/current/index.js`), MCP stdio, `db:migrate`, ownership migration and `SqliteDatabase.connect()`. The supervised compatibility child runs under the Rust core's lock and skips this check. Without a marker, Node's lease and owner-record handling is unchanged. `web/apps/server/src/rust-ownership-marker.test.ts` covers each entrypoint.

A Rust crash can leave the regular `.desktop-lease` file. The next Rust instance reuses it after acquiring `printpartner.lock` and removes it on orderly shutdown before releasing the advisory lock. Cleanup compares the current entry with the acquired reservation by device and inode through the held directory descriptor. A replacement entry is left untouched, and cleanup retains the advisory lock until the acquired reservation is restored or the entry is removed. Standalone Node conservatively refuses that file until a Rust instance completes shutdown. A lease directory left by standalone Node also blocks Rust. Stop every instance before manually clearing a legacy Node lease.

Rust does not create or inspect owner markers, marker directories, or `.desktop.lock`. Legacy artifacts have no authority over Rust admission beyond the `.desktop-lease` compatibility name. Pre-GRE-636 Rust executables do not cooperate with this locking model.

Deleting, renaming, moving, or replacing the data directory, `printpartner.lock`, or `.desktop-lease` while any instance runs is unsupported. Stop every instance before moving the directory. Advisory locks require cooperating processes and a filesystem that supports them.

Storage admission and preflight I/O failures identify the filesystem operation and entry. The subprocess ownership proofs preserve the complete error chain in refusal events and print the debug error to stderr.

The shutdown receipt's legacy `marker_removed` field now reports removal of the `.desktop-lease` compatibility file. `CoreRuntime::runtime_dir()` exposes the private socket directory for lifecycle inspection without an owner marker. The existing launcher test receipt also carries `child_pid` and `runtime_dir`. These fields replace lifecycle fixtures' marker reads and are emitted only with `--test-credential-file`.

The compatibility backend serves only on that private socket. `rust/tests/built_backend_listeners.py` runs the built backend in desktop mode on Linux and macOS CI. It checks that the socket has mode `0600` inside a `0700` directory. It reads the kernel's system-wide table of listening TCP and bound, unconnected UDP endpoints before launch. It then compares the table about every 100 ms from launch through a 4-second window after readiness and through shutdown. Ownership also comes from the kernel, so a process whose file descriptors cannot be inspected still counts. On Linux, `/proc/net/{tcp,tcp6,udp,udp6}` gives each socket's uid, and the inode is matched in `/proc/<pid>/fd` to name the process. On macOS, `netstat -anv -p tcp` and `-p udp` give the owning pid and epid, and `ps` gives their uid. A new endpoint fails the proof when its owner is the current user or cannot be determined, whichever process opened it. The failure prints each owner's full command line and whether it is in the backend's process group, or `unattributed`. New endpoints owned by other users, such as root system daemons, are logged in the receipt as `ignored_other_users` and do not fail the proof. A missing or unparseable table, including a macOS `netstat` header without a pid column, also fails the proof. Before the run, a self-test checks both parsers against fixed samples. The backend must stay alive through the window and then stop on stdin EOF. Four negative controls must fail the same check: a child listener, a detached grandchild listener, a backend listener opened 1 second after readiness and, on Linux only, a detached listener that disables its dumpability so its fds cannot be read. Every process that inherited the run's environment token is killed after shutdown.

## Test installations

`rust/crates/pp-testkit` creates disposable data directories for tests, so tests never open a real one. It is a dev-dependency only: `rust/scripts/check-boundaries.py` refuses it as a normal or build dependency of any crate, so no release binary contains it. Each `Installation` is a new temporary directory, deleted on drop. `Installation::create_in(parent, ..)` places it under a chosen parent instead.

| `Fixture` | Contents |
| --- | --- |
| `Empty` | Nothing. |
| `EmptyWithLeaseLeftovers` | Every lease leftover that still counts as empty, with a Node `.desktop-lease` directory. |
| `NodeSchema34` | `print-partner.db` at Node schema 34 and nothing else. No marker. |
| `RustMarked` | A valid ownership marker only. |
| `MarkedCorrupt` | A truncated ownership marker only. |

A `Spec` combines the same parts: `lease_leftovers` (`RustFile` or `NodeDirectory` for `.desktop-lease`), `node_database`, and `marker` (`Absent`, `Valid`, or one of the `Corruption` cases: `Empty`, `Truncated`, `NotJson`, `WrongOwner`, `UnsupportedVersion`, `MissingField`, `Directory`). `write_marker`, `write_corrupt_marker` and `remove_marker` change the marker of an installation the kit created. The kit has no function that writes a marker into any other path.

"Empty" allows only these top-level entries: `printpartner.lock`, `.desktop-lease` (a Rust regular file, or a Node directory holding only `owner.json` and `previous-marker.json`), `.desktop-lease-candidate-*`, `.desktop-lease-release-*`, `.desktop-lease-retired-*`, `.desktop-owner.json`, `.desktop-owner-release-*.json`, and `lost+found`. `allowed_in_empty` and `allowed_in_node_lease` encode that rule for tests. The leftover owner records name PID 2147483647, which no process can hold, so they describe a dead owner.

The schema 34 database replays `schemaMigrations` from the pinned Node source in `rust/crates/pp-storage/data/historical/schema-34.ts` (commit `71e25e77`, SHA-256 checked), opens it in WAL mode with foreign keys on, and stamps `app_settings.schema_version = 34`. It skips Node's post-migration repairs and starter seeds and holds no user rows. The `ticket_t_7_migrations_historical_order` test uses the same parser.

### Ownership marker format

The durable ownership marker is `<data-dir>/.pp-rust-owned.json`. The fixtures write it with mode `0600` as one compact JSON object with exactly these keys, in this order, followed by a newline:

```json
{"version":1,"owner":"rust","created_at":"2026-11-03T00:00:00Z"}
```

`version` is the integer 1. `owner` is the string `rust`. `created_at` is UTC in the form `YYYY-MM-DDTHH:MM:SSZ`. A reader refuses any other key, version, owner, or timestamp shape. `Marker::parse` in the kit is the test oracle for these rules. The product reader and writer are `pp_storage::ownership_marker`; its tests compare bytes with the kit.

### Ownership marker decision

The marker is separate from the `.desktop-owner.json` process lease record. `pp_storage::ownership_marker` reads it without following symlinks and refuses a marker that is not a regular file, is over 1,024 bytes, is empty or truncated, is not JSON, has an unknown or missing key, has a version other than 1, has an owner other than `rust`, or has a `created_at` that is not a real UTC time.

In a `desktop_rebuild` build, `WriterOwner::open`, `open_at` and `open_with_job_feature_admission` take the directory lock first. While the lock is held, and before the schema preflight, storage directories or any schema write, they apply this decision:

| `PP_DESKTOP_REBUILD` | Marker | Directory | Result |
| --- | --- | --- | --- |
| `1` or `true` | valid | any | Open. The marker is not rewritten. |
| `1` or `true` | absent | empty | Write the marker, then open. |
| `1` or `true` | absent | not empty | Refuse. Nothing changes. |
| `1` or `true` | present but invalid | any | Refuse. Nothing changes. |
| unset or anything else | any | any | Refuse before any write. |

In Rust-only startup only `prepare_data_dir` runs before this decision. It creates the data directory if it is missing (`pp_storage::lease::prepare_directory`) and finishes a lease release that this process failed earlier (`StorageLease::retry_failed_release_if_present`). It creates no schema, marker or other file in an existing directory. Release verification, `cutover::recover` and source-sync admission run after it, and only on an accepted directory; see [Rust-only product entry point](desktop-rebuild.md#rust-only-product-entry-point). `pp-core`'s `rust_startup` tests prove the order on pp-testkit installations, including a copied Node schema 34 database.

Taking the lock can create `printpartner.lock` and the `.desktop-lease` reservation. On any refusal, and when the marker write fails, the lock removes the lease files this open created, while it still holds the lock and only if each name still refers to the file it opened. Lease files that already existed stay with the same bytes and modification time, so the directory listing after a refusal matches the one before.

The writer creates `.pp-rust-owned.json.tmp-<16 hex>` in the data directory with mode `0600` and `O_EXCL`, writes and fsyncs it, renames it onto `.pp-rust-owned.json`, then fsyncs the directory. Reads, creation, removal, the rename (`pp_os::publish_rename`) and the directory sync (`pp_os::sync_directory`) go through `pp-os`, on the directory handle the lock holds. Listing entries for the emptiness check and the leftover cleanup uses `std::fs::read_dir` on the canonical path, because `pp_os::Dir` has no listing call yet. Adding one needs a Unix and a Windows adapter in `pp-os`, so it is left to a `pp-os` change. If the write fails, it removes the temporary file and opens nothing. After a successful write it removes older temporary files.

"Empty" means only these top-level entries: `printpartner.lock`, `.desktop-lease` (a Rust file, or a Node directory holding only `owner.json` and `previous-marker.json`), `.desktop-lease-candidate-*`, `.desktop-lease-release-*`, `.desktop-lease-retired-*`, `.desktop-owner.json`, `.desktop-owner-release-*.json`, `lost+found`, and the marker writer's own `.pp-rust-owned.json.tmp-*` crash leftover. A `.desktop-owner.json` whose pid is running and whose `process_identity` still matches (`<boot id>:<start time>` from `/proc`, as Node computes it) makes the directory non-empty. So does an unreadable record, or a running pid whose identity cannot be looked up. On Unix the probe is `kill(pid, 0)`, where only `ESRCH` means not running. On Windows it is `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` plus `GetExitCodeProcess` returning `STILL_ACTIVE`; Node's Windows identity cannot be looked up, so a running pid counts as live there. Both probes live in `ownership_marker/{unix,windows}.rs`. The test `lease_names_in_rust_and_node_lease_code_and_the_marker_name_match_the_allowlist` reads `pp-storage/src/lease.rs`, `web/apps/server/src/storage-lease.ts` and `web/apps/server/src/desktop-context.ts` and fails if their lease names and the allowlist differ.

A standalone Node lease is a `.desktop-lease` directory. The Rust lock refuses it before creating `printpartner.lock`, so Rust writes nothing while Node holds the lease. A Node lease published after that check still fails the existing `.desktop-lease` reservation.

The Node-backed runtime (`pp-core` supervising the Node child) takes its lease with `ownership_marker::lease_unmarked`, which runs `refuse_marked` while the lock is held and, on refusal, removes the lease files it created. Any marker entry, valid or not, refuses the start before cutover recovery, Source Sync admission or the Node child. Unmarked directories start as before. Node itself reads schema 34 and refuses newer databases (`web/apps/server/src/db/client.ts`).

The test-only Cargo feature `pp-storage/test-unmarked-writer` turns the decision off for `WriterOwner::open` in tests that open unmarked fixture directories. Only `[dev-dependencies]` entries enable it (pp-storage on itself, pp-api and pp-core), and `rust/scripts/check-boundaries.py` refuses it in any feature table, default, or normal dependency, so release builds never contain it. pp-storage also fails to compile with a `compile_error!` if the feature is enabled without `debug_assertions`, so an optimized build with `--features` or `--all-features` cannot ship it. With it on, `WriterOwner::open_with_gate` takes an explicit gate; the marker tests in `rust/crates/pp-storage/tests/ownership_marker.rs` use it on pp-testkit installations and copied Node schema 34 databases. A manual run of a pp-storage fixture binary in a `desktop_rebuild` build adds `--features desktop_rebuild,test-unmarked-writer`.

## Schema versions

`app_settings.schema_version` decides the path an open takes. `pp_storage::versions::classify` names it:

| Stored version | Path |
| --- | --- |
| No database, an empty file, or no `schema_version` row | `Fresh`: install the full schema. |
| 31 to 34 (Node; 31 shipped in Print Partner v3.3.0) | `NodeUpgrade`: replay Node's migrations, then the Rust steps. |
| 35 to 50, `desktop_rebuild` builds only | `RustUpgrade`: run only the later Rust steps. |
| The build's newest: 35 without `desktop_rebuild`, 51 with it | `Current`: nothing to migrate. |
| 1 to 30, or above the newest (a value above `u64::MAX` counts as `u64::MAX`) | Refused with `IncompatibleSchema`. |

`ticket_t_7_migrations` opens a fresh directory and Node-built schema 31, 32, 33 and 34 databases through the writer and checks each against its named path.

The refusal is terminal. The version is read from the preflight copy, made in a private `.pp-preflight-<id>` folder inside the data folder, before the integrity check, so a refused database, its `-wal` and its `-journal` keep their bytes. The message names the version found, the supported range and the recovery step:

- Too old: `Incompatible storage: cannot upgrade database schema version 30 (this release opens schema 31 to 51). Install Print Partner v3.3.0, start it once so it upgrades the database, then install this release. The database was not changed.`
- Too new: `Incompatible storage: database schema version 52 is newer than supported version 51 (this release opens schema 31 to 51). Install the Print Partner release that last opened this data folder, or restore a backup made by this release. The database was not changed.`

Each runtime stops on it instead of reopening the database:

- **Node-backed start.** Node opens 31 to 34. Right after `lease_unmarked`, and before cutover recovery or the Node child, `pp-core` runs the same preflight (`versions::node_refusal`) and refuses any other version against the range 31 to 34. Preflight reads and checks the version first and records it, before `PRAGMA integrity_check` and the Rust schema validations. So an unsupported version is refused here even when the file also fails its integrity check or does not match the Rust schema; those errors themselves are unchanged. The child never starts, so the supervisor never backs off and restarts it. A failure before the version is read, such as a file SQLite cannot open or a non-numeric version, logs `node_schema_preflight_unchecked` and leaves the decision to Node, as before. The check copies the database into a private `.pp-preflight-<id>` folder in the data folder. Too little free disk for that copy is a failure before the version is read, so it logs `node_schema_preflight_unchecked` and leaves the decision to Node; only the Rust-only start refuses it. For a version this build's Rust schema accepts, it runs `PRAGMA integrity_check` once per start, including versions 35 up to the Rust schema ceiling, which Node then refuses. A nonzero version below 31, or a version newer than the Rust schema ceiling, returns before that check.
- **Rust-only start.** `WriterOwner::open` refuses during its preflight, and startup returns the error once; nothing retries it.
- **Desktop.** The startup dialog is titled "Print Partner cannot open this data folder" and shows the message. Stderr gets `desktop_startup_failed: core_startup -> incompatible_storage: found=52 supported=31..=51 direction=newer`. `core.log` does not: it attaches only after core startup admits the data directory.
- **Headless `pp-server`.** It writes `incompatible_storage: found=<n> supported=<oldest>..=<newest> direction=<older|newer>` and the message to stderr and exits with code 78 (`EX_CONFIG`). Other start failures exit with 1. Docker restart policies restart on any non-zero code, so a container with a restart policy starts again, and each start refuses again without writing. Stop the container before you recover the data folder.

A downgrade refuses the live schema before restore cleanup. It preserves the database, all SQLite sidecars (`-wal`, `-shm`, `-journal`), saved backup archives, and pending restore uploads and stages. Repeated refused starts do not prune or replace these copies. This does not restore older data automatically; rollback after new writes still needs informed owner approval (ADR 0010).

### Unsupported-schema acceptance evidence

GRE-405 builds on two merged changes. GRE-1362 (#1666) added named schema
paths, terminal version refusals, desktop recovery text and headless exit 78.
GRE-1363 (#1699) moved restore cleanup after schema admission so a refused
start preserves the database, SQLite sidecars and recovery copies.

| Requirement | Existing verification |
| --- | --- |
| Fresh and Node schemas 31–34 | `ticket_t_7_migrations` checks the named paths and historical migration inputs. |
| Version range and actionable recovery text | `schema_versions` checks the typed writer refusal and the Node lease refusal without changing database bytes. |
| Downgrade preserves recovery copies | `downgrade_recovery` checks repeated refusal, an uncheckpointed WAL version, saved archives and staged copies. |
| No headless reopen loop or partial startup | `pp-server/tests/unsupported_schema.rs` starts `pp-server` three times for each of schema 30 and 51 in each runtime mode, 12 starts in all: on the workspace test build in the default run, and on the release build `Dockerfile.rust` ships in an ignored test you run by hand (below). The Rust-only starts use the image's arguments and `PP_DESKTOP_REBUILD=1` on a Rust-owned directory; the Node-backed starts use an unmarked Node directory. Each start gets only the variables it sets. Each must exit 78 within 30 seconds, print the `incompatible_storage:` diagnostic line once and the recovery line once on stderr, print nothing on stdout, and leave no status file or test credential file after it exits. The first start may add only the empty `printpartner.lock` lease file; every other file in the data directory, the database and its SQLite sidecars included, stays byte for byte the same, and the second and third starts change nothing. The comparison is by content; modification times are not compared. |

Schema admission changes no routes or workers. A refused startup never announces
an origin or publishes launch credentials; it cannot activate a second mutation
owner or fall back to the other runtime. Existing migration and restore parity
fixtures remain the gates for those operations.

Run the ticket's acceptance selector; it must run a nonzero count:

```sh
cargo nextest run --locked --manifest-path rust/Cargo.toml --no-tests=fail -E 'test(ticket_t_8_versions)' --success-output immediate
```

It runs two tests: the storage writer refusal in `pp-storage` and the 12
headless starts on the workspace test build of `pp-server`. `pp-server` builds
with `desktop_rebuild` by default (GRE-1646), so the headless test runs without
naming the feature. The test build turns on pp-storage's `test-unmarked-writer`
feature, which skips the ownership marker gate but not schema admission, so the
refusal is the same one the shipped binary makes.

#### Release binary proof

`ticket_t_8_versions_release_binary_refuses_without_restarts` repeats the 12
starts on the binary the image ships. It is `#[ignore]`d, because it first
builds the release `pp-server`, so neither the selector above nor CI runs it.
On a fleet VM at two jobs the test took 21 minutes. Expect that on every run:
each build gets a new Cargo home, and both runs so far rebuilt every crate. Run it from the
repository root after `just ci --only web-assets`:

```sh
cargo test --locked --manifest-path rust/Cargo.toml -p pp-server --test unsupported_schema -- --ignored --exact ticket_t_8_versions_release_binary_refuses_without_restarts --nocapture
```

`cargo test` is used because nextest's default profile stops a test after 10
minutes. Set `PP_SERVER_RELEASE_BINARY` to a binary built with `cargo build
--locked --release -p pp-server` to skip the build. Before the 12 starts the
test checks that the binary refuses an unmarked Node directory in Rust-only
mode with exit 1, which a build without the ownership gate would not do. It
prints the runtime mode, attempt, exit code and version diagnostic for every
start. These are disposable installations; no real library or printer is used.

The test runs `cargo build --locked --offline --release -p pp-server` into
`<target>/pp-server-release`, isolated so that no credential reaches the build
or the test's output:

- The build starts from an empty environment. It gets `PATH`, `RUSTUP_HOME`,
  `PP_DESKTOP_RELEASE_MANIFEST` and a numeric `CARGO_BUILD_JOBS`, the channel
  in `rust-toolchain.toml` as `RUSTUP_TOOLCHAIN`, and empty `RUSTC_WRAPPER` and
  `RUSTC_WORKSPACE_WRAPPER`, so sccache never starts. It gets no `HOME`.
- Each build gets a new, empty `CARGO_HOME` under the target directory that
  only links to the real Cargo home's `registry` and `git` caches.
- Cargo runs in `/`, so it reads no `.cargo/config.toml` in the checkout or in
  a home directory above it. If `/.cargo/config` or `/.cargo/config.toml`
  exists, the test refuses to build.
- `--offline` keeps Cargo off the network and away from registry credentials.
- A failed build reports only its exit status and its count of `error` lines.

`ticket_t_8_release_build_keeps_credentials_out_of_the_build_and_its_output`,
in the default run, checks this with a small probe package. It puts fake
credentials in the parent environment and in three Cargo config files, and
fails if any reaches the probe's build script or the failure message.

## Ownership today

- `pp-storage` owns the SQLite connection, migrations, the bounded settings writer, the read-only pool, the storage lease and safe backups.
- The Node server still runs the application SQL, startup migrations and jobs.
- The desktop rebuild slices move these operations into `pp-storage` behind `desktop_rebuild`. See [Desktop rebuild flag](desktop-rebuild.md).
- `pp-storage::import` is the only Rust code that reads Node's stored shapes on purpose. Today it holds the dry run, the import into a new Rust-owned folder (`import(source, destination)`), and the conversion inventory. See [Node import](node-import.md).

## Synthetic installations

`web/scripts/synthetic-dataset/generate.mts` writes a synthetic Node schema-34 data directory shaped like a long-lived installation, for invariant tests to run against. Owner UX checks and the cutover rehearsal can fall back to it when no production backup copy is available. Its tests cover the stored shape, STL dimensions, saving a Source manifest, Plate initialization and unit transfer, not 3MF export. It is not the representative copy that step 2 of [Whole-runtime cutover](architecture.md#whole-runtime-cutover-and-rollback) requires, and evidence built on it must say so.

From `web/`, with Node 24.21 and `better-sqlite3` built for it:

```sh
npm run fixture:synthetic -- --seed 1138 /tmp/pp-synthetic-1138
```

The output directory must be new or empty. The generator refuses anything else, so it never writes into an existing data directory.

### Disposable copies

Node stores each active Source's snapshot as an absolute path in `projects.local_path`, so a plain `cp -r` of the output still reads Source files from the original directory. Copy with the helper instead:

```sh
npm run fixture:synthetic:copy -- /tmp/pp-synthetic-1138 /tmp/pp-synthetic-1138-copy
```

It refuses a source without `SYNTHETIC-DATASET.json` and a non-empty target. After copying, it opens the copy with Node and calls `activateSourceRevision` again for each Source's current revision, which resolves the stored snapshot locator against the copy's `repos` directory. The original directory can then be removed. Node connect re-seeds the starter profiles on that open, so the copy's digest differs from the manifest in `sqlite_sequence`; the measured shape does not change.

### How it is built

Node's own `SqliteDatabase.connect` creates the schema-34 database. `saveFleet` writes the two synthetic printers to the `printer.fleet` setting. Each Source revision is written by `LocalSourceSnapshotStore.materialize`, the snapshot store Node sync uses, so it holds a `.printpartner-source-snapshot.json` and the revision row stores the digest and locator `materialize` returns. The repository then builds every row: `createSource`, `recordSourceRevision`, `activateSourceRevision`, `createProfile`, `addAddonLayer`, the accept and edit test helpers in `apps/server/src/test/accept-plan.ts`, `publishAcceptedPlates`, `deleteProfile` and `saveUnattributedPrint`. No SQL is written by hand. The clock starts at `2026-10-02T00:00:00.000Z` and advances one minute per operation. Required-unit tokens come from a seeded token factory. Plate IDs use the same `plate_<sha256 prefix>` form as the Node Plate workspace.

### Shapes

It produces only these shapes:

| Shape | What the output holds |
| --- | --- |
| ID density | 22 live Builds and 7 deleted Builds, interleaved, so Build IDs run 1 to 29 with gaps. The deletions also leave gaps in Plan revision, Part and layer IDs. |
| Immutable IDs | Plan, Plate and Required-unit IDs created by Node, kept through later Plan and Plate revisions |
| Plan size | 10 Sources with 3 to 8 STL files each. Builds have 1 to 5 layers and 1 to 4 accepted Plan revisions. |
| Plate size | Two anchor Builds: `Synthetic Build Anchor 100u15p` (100 units on 15 Plates) and `Synthetic Build Anchor 47u6p` (47 units on 6 Plates). Other Builds have up to 8 Plates or none. |
| Accepted history | Builds with Plates and 3 or more Plan revisions keep a Plate revision on an earlier Plan revision. Each anchor has two Plate revisions on its current Plan. |
| Unmatched prints | 18 open entries in the `printer.unattributed_prints` setting |
| Printers | `synthetic-printer-1` and `synthetic-printer-2` in `printer.fleet`: 250 × 220 × 220 mm beds with a 5 mm margin, matching the stored Plates. Neither has an `integration_id` or `device_id`, so nothing can send to them. |

Names are `Synthetic Source NN`, `Synthetic Build NN` and `synthetic-sNN-part-NN.stl`. Every part is a closed 20 × 20 × 10 mm ASCII STL box that matches the stored unit size, so Plate initialization and 3MF export accept it. URLs and printer hosts use `.invalid`. Nothing is copied from a real data directory.

### Manifest and digest

Every run writes `SYNTHETIC-DATASET.json` at the root of the output with:

- `label`: `synthetic: not the architecture's step-2 rehearsal`
- `synthetic: true`, the `seed`, the `clock` and the Node version
- `generator`: name, `version` (currently 2), script path and script SHA-256
- `digest`: the installation digest
- `shape`: the measured counts above, the deleted Build IDs and the Build ID density

The digest is a SHA-256 over every table's schema and rows, sorted by all columns, plus the hash of every file except the manifest and the database files. Node stores absolute snapshot paths in `projects.local_path`, so the digest replaces the output directory with `$DATA`. The same seed gives the same digest in any directory.

### Readers and tests

```sh
npm run test:synthetic-dataset
npx tsc -p scripts/synthetic-dataset/tsconfig.json
```

The test generates seed 7 twice and seed 8 once, then checks:

- the digests match for seed 7 and differ for seed 8;
- the label and the shape counts;
- Node opens a copy and reads accepted Plans, the anchor's Plates and the 18 prints;
- each Source revision's digest matches its `.printpartner-source-snapshot.json`;
- `publishSourceManifestRevision` saves a Source manifest on a copy;
- a copy made by the helper serves `/sources/:id/stl-tree` for every Source after the original directory is removed;
- the Plate routes initialize the `Synthetic Build Anchor 47u6p` Plates on `synthetic-printer-1` and transfer a unit to `synthetic-printer-2`.

Node connect re-seeds the starter slicer profiles with `INSERT OR IGNORE`, which advances their `sqlite_sequence` rows on every open, so that check compares the measured shape rather than the digest.

Set `PP_STORAGE_FIXTURE_BIN` to run the Rust check too. It opens a copy with `pp-storage`'s `storage-fixture initialize`, which migrates it from schema 34 to 35 and takes the `pre-schema35.db` backup. The shape must survive. Build the binary with `cargo build --manifest-path rust/Cargo.toml --locked -p pp-storage --bin storage-fixture`.

The generator does not produce open drafts, Checkoff progress, printer sends, exports or historical schema versions. Tests that need those build them separately.

## Whole-installation restore: staging

With the `desktop_rebuild` feature, `pp_storage::restore` stages a whole-installation restore: the database and every backup root in `archive::FULL_ROOTS`. Staging never changes live files. Confirmed activation, described below, swaps the staged copy into place under the same owner. Create and download are the routes in [Whole-installation backup export](#whole-installation-backup-export) (GRE-1371). Stage and restore are the routes in [Stage and restore routes](#stage-and-restore-routes) (GRE-1161). Build snapshot restore is a separate Plan operation.

Staging (`WriterOwner::installation`, then `InstallationClient::begin_upload` and `stage`) takes no filesystem path from the caller:

1. Both steps need a valid `.pp-rust-owned.json`; an unmarked data directory refuses with `Refusal::Unmarked` before writing anything.
2. The upload is written to `<data>/.pp-restore/incoming-<32 hex>.tar.gz`, created new without following a link, relative to the `.pp-restore` handle (see the threat model below). `begin_upload` takes the size the caller declares as its byte limit. There is no fixed cap: it refuses with `Refusal::NoSpace`, before creating the file, when that many bytes would leave less than the reserve free. A write that would pass the limit refuses with `Refusal::TooLarge` and writes nothing past it. The first failed write fails the upload for good: later writes and `stage` refuse it. A `.pp-restore` that is a symlink or not a directory refuses.
3. A first pass over the archive (`archive::declared_bytes`) totals the sizes its tar headers declare and counts its entries, extracting nothing. If that total would leave less than the reserve free, `stage` refuses with `Refusal::NoSpace` and nothing is extracted. `archive::extract` then runs the same entry, scope, ratio and `integrity_check` validation as `archive::validate` and writes every entry under `<data>/.pp-restore/staged-<32 hex>/`. It stops with `Refusal::LargerThanDeclared`, and tries to remove the stage, before it writes more than the first pass counted, so an archive cannot fill the disk past what was checked. The second pass decompresses the archive again, which takes minutes at 50 GB; restores are rare. The entry guard admits only regular files and directories under the core files and declared roots, with no absolute, `.` or `..` component, so a hostile entry cannot write outside the stage or create a link. After the tar end-of-archive block, validation reads the gzip stream to its end, so a damaged CRC or length trailer refuses the archive, and so does anything after the tar end other than zero padding, or more than 1 MiB of padding. Both exporters (Rust's `archive::write` and Node's `tar.c({ gzip: true })`) write exactly one gzip member, so the file must end where that member ends. Any byte after it is refused, including padding, a second member, and the rest of a tar split across members, whose entries tar iteration would never see. `archive::validate` shares this code, so it refuses the same archives. Only a `full` scope is accepted.
4. `schema::preflight` admits the staged database as startup admits a live one: a schema newer than this build, older than 31, missing (version 0), or failing a job-table check refuses with `Refusal::UnsupportedSchema`.

Both staging passes and standalone validation check each GNU long-name, GNU long-link, and local or global PAX header before tar reads its payload. Each extension payload may hold up to 64 KiB. A larger extension refuses the archive as invalid. The tar parser still resolves names and sizes from supported extensions within the limit.

Staging tries to remove the upload file whether it succeeds or not. A refusal tries to remove only its own staged copy and keeps an earlier successful stage. Those two removals ignore errors: a file that could not be removed stays until the next open tries again. Earlier `staged-<32 hex>` directories are removed only after the new stage is complete, and nothing else is removed. If that removal fails, the earlier stage stays, and `stage` still returns the new stage, with `Staged::cleanup_error` describing the failure; the next open tries again. Staging never removes `.pp-restore` itself, so an upload starting at the same time keeps its directory; the next open removes it. Stages for one owner run one at a time. While a stage runs, the data volume holds the upload, the new extraction and the earlier stage at once.

Restore work belongs to the `WriterOwner` that created the client. Shutdown first closes all admission at once: new storage writes, reader checkouts, uploads, upload writes and stages are refused from then on. It then waits for reader checkouts and restore work against one shared 5 s deadline, so the drain takes at most 5 s in total before the writer thread is joined. If a checkout or restore work is still running at the deadline, shutdown fails and the owner keeps its lease until the process exits, so no other owner can clear `.pp-restore` under it.

Stages do not survive a restart. Only the `WriterOwner` that made a stage can activate it: shutdown leaves the stage on disk, and the next schema-admitted open removes every stage, complete or not, before any client exists (or refuses to open if a removal fails), so the client has to upload the backup again. Within one owner, a stage also ends when a later stage replaces it or a restore uses it or rolls it back; [Stage and restore routes](#stage-and-restore-routes) lists every case. An incompatible build refuses first and leaves those stages alone. Schema admission holds only for the build that ran it, and a restart can be an upgrade or a downgrade. Without a completion marker, `recover` also can't tell a complete stage from a half-extracted one, and a forgotten stage as large as the installation would otherwise stay on the data volume. Activation runs in the same process as its stage. After a restart, an unknown restore id tells the client to upload the backup again.

A `full` archive need not contain every root in `includedRoots`. Both the Rust (`archive::plan`) and Node (`backup-restore.ts`) exporters declare all eleven roots and skip a root that was missing when the backup was taken. Three roots are files, which have no directory entry. A declared root with no entries therefore means it was absent at backup time, and an archive holding only the metadata and the database is a valid backup of an installation with no files. Validation still refuses any entry outside the declared roots. Telling an absent root apart from a stripped archive needs a format change, tracked in GRE-1348.

`WriterOwner::open` runs `restore::recover` under the directory lock, after the ownership marker decision and both read-only schema preflights, before opening the live database for writes. Unsupported schemas refuse before cleanup, so their recovery uploads and staged copies remain available to the release that created them. It checks every entry before it removes any: `incoming-<id>.tar.gz` and `journal.json.tmp` must be regular files and `staged-<id>` a real directory, all read without following a link through the `.pp-restore` handle. With an activation journal, `journal.json` must be a regular file, and the `prior-<id>` and `staged-<id>` it names must be real directories where they exist; either may be missing. Open then rolls back or finishes the interrupted activation, as described under confirmed activation. It then removes the remaining entries and `.pp-restore` itself. If a removal fails, the open refuses with "could not be cleared" and storage does not start; each later open tries again. Any other entry, a link or a wrong type under one of those names, or a `.pp-restore` that is not a real directory, refuses the open and removes nothing, because this build cannot tell whether it holds the only copy of an installation.

Threat model for `.pp-restore` (GRE-1347). Staging, activation and `recover` open `<data>/.pp-restore` once, relative to a handle on the data directory, without following a link (`openat` with `O_NOFOLLOW | O_DIRECTORY` on Unix). That open is the check: a link, a regular file or any other non-directory at that name refuses, and nothing is written or removed. After the check, every create, open, read, rename and removal of a `.pp-restore` entry goes through the held handle: the upload file, extraction of each stage entry (one no-follow directory open per path component), the schema preflight's reads of stage files, the journal and its temporary file, the `prior-<id>` copy, and removal of stages and leftovers. Live entries move only through the data-directory handle. If another process replaces `.pp-restore` with a link after the check, none of those operations follows it.

Not covered yet, tracked in GRE-1802:

- The archive `integrity_check` opens the staged database by pathname (`archive.rs`), because SQLite has no handle-based open. An identity check that the path still names the handle runs just before it, but a swap between that check and the open is not ruled out.
- The schema preflight opens its working copy by pathname (`schema.rs`), after the same identity check, with the same gap.
- The free-space check (`fs4::available_space`, `schema/working_copy.rs`) reads the stage path with no check before it. It only reads.
- On Windows, handle-based removal refuses an entry whose name is not valid Unicode. A non-Unicode name inside a journal's `prior-<id>` copy would leave that copy and `journal.json` behind, and every later open refuses with "Restore journal cleanup failed". Unix removes such names.

Only the effective user or root can replace `.pp-restore`, because store admission requires the data directory to belong to the effective user and to deny group and other write (see [Desktop rebuild](desktop-rebuild.md)). Print Partner is single-user, so the remaining gaps matter only for a hostile process running as that user, which could already change the data directory directly.

Tests: `pp-storage/src/restore/tests.rs` (`ticket_t_8_restore_*`, on pp-testkit installations with a copied schema-34 database).

## Whole-installation restore: confirmed activation

`InstallationClient::activate(restore_id, confirmation)` swaps a staged restore into place. It takes a stage id, never a filesystem path, and the exact `restore::CONFIRMATION` text, which says that replacing the database and files cannot reverse prints already sent to printers or filament usage already recorded in Spoolman. It refuses, with nothing changed, a wrong confirmation (`Refusal::Confirmation`), an id with no stage (`Refusal::UnknownRestore`) and an unmarked data directory (`Refusal::Unmarked`). It admits the staged schema again before it queues the swap. Staging, activation and backup export share the owner's restore activity, and staging and activation share one gate.

Activation runs on the writer thread, so no write runs beside it:

1. It refuses with `Refusal::Busy`, and changes nothing, while a pending MCP action is `applying`: the action's apply is a separate write after its claim, and would otherwise land in the restored database. It then pauses admission: new writer, authentication and restore work is refused from then on.
2. It closes idle readers and waits up to 5 s for reader checkouts to return. A reader still out refuses the activation with `Refusal::Busy` and changes nothing: the database and the archive keep their bytes, work queued behind the activation still runs, and job streams stay open.
3. Once the readers have drained, it closes job streams and fails the work queued behind it. It checkpoints and closes the writer's connection.
4. It syncs the stage and publishes `.pp-restore/journal.json` before the first rename: it writes and syncs `journal.json.tmp`, renames it over `journal.json`, and syncs `.pp-restore`. Then it creates `prior-<32 hex>`. The journal names the staged directory, the `prior-<id>` directory, and the managed names present on each side. The managed names are the four SQLite files (`print-partner.db`, `-wal`, `-shm`, `-journal`) and `archive::FULL_ROOTS`.
5. It moves each live managed name into `prior-<id>`, then each staged one into the data directory, one rename at a time, syncing both directories after each. The ownership marker, the lease files, `backups/` and everything else stay where they are.
6. It opens the restored database as startup does (schema admission, migrations, recovery of durable jobs and sends), invalidates every open `pending_actions` row in the restored database before committing the journal, and refreshes every consumer of the old database: Source work leases, publication tokens, the import epoch, event cursors, print-file previews and the reader connections.
7. It marks the journal `committed`, tries to remove the prior and staged copies and then the journal, installs the new readers and reopens admission. Those removals ignore errors. If one fails, the restore still succeeds and the journal stays with whatever copies remain. Until a restart, activating a stage that still exists answers `RestartRequired`; an id whose stage is gone answers `UnknownRestore`. The next open retries the cleanup; if it fails again, the open refuses with "Restore journal cleanup failed" and storage does not start. Work that a family queued while this ran fails then; it does not run against the restored database.

The journal stays where the next open looks for it. The request opens `.pp-restore` when it admits the stage, and activation uses that handle later, so another process could move the directory in between. Before it publishes the journal (both times) and before each live rename, and at each step until the commit, activation checks that `<data>/.pp-restore` still names, without following a link, the directory it holds open. If not, it moves nothing more and rolls back through its handles, which works wherever the directory now is. The journal is removed only after the prior and staged copies are gone. So a crash leaves a journal outside `.pp-restore` only if another process moves the directory after one check and the process dies before the next one; that is the same as another process deleting the journal.

Three rules hold whatever fails:

- **A failure rolls back.** Any error before the commit moves the prior copies back, reopens the previous database and its consumers, and returns `RestoreError::RolledBack`. What happens to the stage depends on when the error came. After the journal was published, the rollback tries to remove the stage and the prior copy and then the journal; those removals ignore errors, and anything left stays, with the journal, until the next open retries as in step 7. Before that (for example, a stage that fails to sync), nothing had moved and nothing is removed: the stage stays in `.pp-restore` until a later stage replaces it or the next open removes it.
- **A failed rollback stops storage.** The rollback fails if moving the prior copies back fails, or if reopening the previous database, refreshing its consumers or reopening the readers fails. That can happen before the journal was published too. `activate` then returns `RestoreError::RestartRequired`, and until a restart: storage writes and authentication work answer `Stopped` (the writer queue is closed and still marked restoring), reader checkouts answer `Stopped` (the pool stays closed), and every restore operation (upload, stage, activate, backup export) answers `Refusal::Busy`, because restore work stays paused. If the move back failed, the published journal stays for the next start. If the reopen failed, the rollback's cleanup has already removed the journal with the copies, unless that cleanup failed too. With no journal (also when the failure came before publication), the next open finds the previous installation's live files and removes the leftovers.
- **A restart finishes the job.** At open, after both schema preflights and under the held lease, a `swapping` journal rolls back and a `committed` journal finishes its cleanup. If the prior copy exists and holds a database, that database's schema is admitted before a replay; a missing prior copy or one with no database is not checked. The recovered database's schema is read again before it opens for writes. The journal is removed only after both copies are gone, so an interrupted cleanup repeats. If the replay or cleanup fails, the open refuses (for cleanup, "Restore journal cleanup failed") and storage does not start; each later open tries again. A corrupt journal, an unexpected entry in `.pp-restore` or a link where a recovery copy should be refuses the open and removes nothing. An older build that does not know the journal refuses the same way: `journal.json` and `prior-<id>` are not names that cleanup deletes.

Shutdown is permanent: an activation that finishes after shutdown began does not reopen admission. `activate` returns `restore::RestoreError`: `Refused`, `RolledBack`, `RestartRequired`, or the shared `WriterFailure`. The cause of a rollback or a failed rollback is logged, not returned.

Activation needs no free space beyond the stage, which staging already checked: the swap renames files within the data volume, and the commit then tries to remove the prior copies. If that removal fails, they stay on the data volume with the journal until an open removes them.

Tests: `pp-storage/src/restore/tests/activation.rs`. They observe stopped admission and drained readers during the swap, fail and then crash at every step of it, and compare the four SQLite files and the archive bytes around every refusal and rollback. One fails before and after the journal is published and checks that only the second rollback removes the stage. `pp-storage/src/restore/tests/swap.rs` replaces `.pp-restore` with a link after the check for staging, open-time cleanup and journal replay, refuses a regular file at `.pp-restore`, and moves `.pp-restore` aside before activation and at each activation step, crashes at the next step, and checks that the next open finds the old or the restored installation and no journal outside `.pp-restore`.

### Stage and restore routes

With `PP_DESKTOP_REBUILD` exactly `1` or `true`, `backup_router` also mounts two restore routes behind the same gate (GRE-1161, `pp_api::restore`). They are absent otherwise. Both require a session or an API key, and the loopback Host and Origin checks. The client sends no path. One stage or restore runs at a time; a second answers 503 `A restore is already running` with code `restore_busy`. Answers send `Cache-Control: private, no-store`, and refusals are problem documents ([Problem responses](problem-responses.md)) with fixed text: a staging or schema error can name a path, so its detail is logged, not sent.

`POST /api/v2/installation/restore/stage` takes the backup archive as the request body (`application/gzip`, not a form) and needs `Content-Length`, which becomes `begin_upload`'s limit. The route passes the body to the upload in 1 MiB writes, then calls `stage`, and answers `RestorePreview`: `restore_id`, the archive's `created_at` and `app_version`, its `schema_version`, and the `confirmation` text to repeat. The stage stays until the first of these:

- A later stage completes. `stage` then tries to remove every earlier stage.
- A restore with it succeeds. The swap moves its contents into place and the commit tries to remove the rest.
- A restore with it fails after it published its journal and rolls back. The rollback tries to remove the stage.
- The next open of the data directory, after a restart or by a new `WriterOwner`, once it admits the live schema. Open removes every stage, or refuses to open.

Each of the first three removals can fail without failing the operation; the stage's files then stay until the next open. A restore that fails before it publishes its journal leaves the stage in place. After a rollback that itself fails (`RestartRequired`), the stage may or may not remain: the rollback's cleanup removes it before the previous database is reopened, so a failed move back leaves it but a failed reopen does not. The next open removes every leftover or, if a removal fails, refuses to open and reports it (see the open-time cleanup above). Once the `staged-<id>` directory is gone, its `restore_id` answers `Refusal::UnknownRestore` (404), and the backup must be uploaded again.

`POST /api/v2/installation/restore` takes `RestoreRequest` (`restore_id`, `confirmation`; unknown members are refused) and calls `activate`. It answers `RestoreOutcome` (`restored_schema_version`, `schema_version`) once the restored database is serving. The restored database brings its own accounts and sessions, so a session the backup does not hold signs out.

| Refusal | Answer |
| --- | --- |
| No `Content-Length` | 411 `Content-Length is required` |
| Body ends early or cannot be read | 400 `The upload body could not be read` |
| Body longer than its `Content-Length` (`Refusal::TooLarge`) | 413 |
| No valid `.pp-rust-owned.json` (`Refusal::Unmarked`) | 409 `Restore needs a Rust-owned installation` |
| `Refusal::NoSpace`, `Refusal::InsufficientSpace` | 503 `Not enough free disk space`, with `needed` and `available` |
| `Refusal::InvalidArchive`, `Refusal::LargerThanDeclared` | 422 `Invalid backup archive` |
| `Refusal::UnsupportedSchema` | 422 `This release cannot restore the backup's database` |
| `RestoreRequest` not readable | 400 `Request is invalid`, with field errors |
| `Refusal::Confirmation` | 400 `Restore confirmation does not match` |
| `Refusal::UnknownRestore` (also any id that is not 32 lowercase hex) | 404, code `restore_not_found`: upload the backup again |
| `Refusal::Busy` | 503 `Storage work is still active; try again`, code `restore_busy`: this request changed nothing |
| `RestoreError::RolledBack` | 500, code `restore_rolled_back`: the previous installation is kept and serving |
| `RestoreError::RestartRequired` | 503, code `restart_required`: restart Print Partner. After a failed rollback (with or without a published journal), writes and reads answer `Stopped` and restore operations `Busy` until then. When an earlier activation left its journal, storage keeps serving and only activating a stage that still exists answers this |

The generated client names the routes `stageInstallationRestore` (no request DTO; post the archive bytes) and `restoreInstallation`.

Tests: `pp-core/tests/installation_restore_http.rs` runs on `pp_testkit::Installation::with_builds_drafts_and_history` (copied schema-34 database, valid marker, the same seed the storage restore tests use). It makes and downloads a backup through the mounted routes, adds a Build and changes the files, stages and restores the download, and checks that Build IDs, drafts, History tables and the files under `repos` and `exports` match the backup, and that the Build list, drafts and History routes answer as before. It also covers the auth, Origin, archive, confirmation, unknown-id and one-at-a-time refusals. `pp-core/tests/installation_backup_http.rs` checks that the routes are absent with the flag off. `pp_api::restore`'s unit tests pin every refusal's body.

### Backup and restore from Settings

The replacement Settings screen has a Backups section (GRE-1340, `web/apps/ui/src/settings/BackupsSection.tsx`). Nothing is sent when the section opens or when a file is chosen. Only the confirmation dialog calls the restore route.

**Create backup** posts `createInstallationBackup` once, with `redirect: "error"`: a 307 or 308 is never followed, so one click never sends a second POST, and the refused redirect is read as not confirmed. While it runs, the section's status line says `Creating backup` and the button is off; the same mounted status line then says `Backup made`. Offline, the button is off and names the offline reason. The answer shows the archive's file name, time, app version, size (with the exact byte count) and SHA-256. The page keeps only the backup it made; after a reload, create one again.

A failed create reads the answer this way:

- The core's own problem keeps the backup already shown and says `The backup was not made`, with the core's reason, such as `A backup is already running`. The core's own problem means the body is `application/problem+json`, valid against the generated `Problem` schema, and its `status` equals the HTTP status (`CoreError.problem` in `web/apps/ui/src/core/client.ts`). The core keeps a new archive only when it answers 200, so its refusal changed nothing.
- Anything else drops the shown backup and says `The core did not confirm the backup. Create backup again.` That covers a redirect of any kind (whatever the destination would answer), a proxy page, a cut-off body, a 200 that fails the contract, a lost answer, and a problem with a malformed member such as `"code": null`. The core may have made a new archive and dropped the one shown. Creating again only replaces the kept archive.

**Download** (GRE-1865) sits next to the shown file name. It first sends `HEAD /api/v2/installation/backup/archive?sha256=<shown digest>` with `redirect: "error"`. Only a 200 starts a browser download of the same URL, so the file saved is the one whose size and SHA-256 the page shows. The `GET` carries the digest too, so it never sends bytes other than the shown archive's. A HEAD answer has no body, so the section reads its status alone:

- 404 or 409: the core no longer keeps the shown archive. The section drops it, saves nothing, and says to create a backup again.
- 503: a create or another download holds the core's backup slot. Nothing is saved, the shown backup stays, and the section says `The core is busy with another backup or download. Try Download again in a moment.`
- Any other answer, another 2xx such as 204 included, a redirect or a lost answer: nothing is saved, the shown backup stays, and the section says the download did not start. Trying again cannot save a different file.

HEAD and the browser's `GET` are two requests, so the slot can change between them. If a create from any window is still running when the `GET` arrives, the core answers 503. If a create has finished and replaced the archive, the core answers 409. In neither case does it send other bytes. The section has already said `Download started`, and the browser's own download shows the failure. While the HEAD is pending, the Download button is off (`aria-disabled`), and a second press sends nothing. A press after it resolves starts a second download.

Download streams the archive through a browser `<a download>` navigation on purpose, so the page never holds a backup of any size in memory, as `fetch` and a `Blob` would. That navigation's `GET` can follow a redirect if one appears after the HEAD. The HEAD before it asks the same URL with `redirect: "error"`, so a layer that redirects that URL stops the download before it starts. Every `fetch` in the section refuses redirects (`createInstallationBackup`, the archive HEAD, `stageInstallationRestore`). The owner accepted the streaming `GET` as a scope exception to GRE-1865 on 2026-11-20.

**Restore from a backup** has a file field and **Check backup** (GRE-1865). Check backup posts the chosen file once as the body of `stageInstallationRestore` (`application/gzip`, with `redirect: "error"`). The section's second status line says `Checking backup`, then `Backup checked`, and the section shows the backup's time, app version and schema version. Staging never changes this installation. A new stage replaces the earlier one, so checking again is safe. A failed check drops any preview already shown:

- The core's own problem, such as 422 `Invalid backup archive` for a truncated file, shows `The backup was not accepted: <reason>. This installation is unchanged.`
- Anything else (a redirect, a proxy page, a malformed problem, a 200 that fails the contract, a lost answer) shows `The core did not confirm the check. This installation is unchanged. Check backup again.`

**Restore this backup** appears only after a successful check. It opens a dialog that shows the core's confirmation text as sent. Focus starts on Cancel, and Cancel sends nothing. Only **Replace this installation** posts `restoreInstallation`, once, with the `restore_id` and that text and with `redirect: "error"`, so a 307 or 308 from a layer in front of the core is a network error and is never followed with a second POST. On success the section says `Restore complete` and offers Reload; the restored database brings its own sessions, so the page may then ask for sign-in.

One pure function, `restoreVerdict` in `web/apps/ui/src/settings/restoreAnswer.ts`, decides what a failed restore shows. A retry is allowed only for an answer that proves this request changed nothing. Each row needs the core's own problem (`CoreError.problem`: `application/problem+json`, valid against the generated `Problem` schema, and `status` equal to the HTTP status) and, when a code is present, exactly the code listed:

| Answer | Where the core decides it | Shown |
| --- | --- | --- |
| Request never sent (fails the generated request schema) | In the page | Unchanged; may confirm again |
| 400, no code | Unreadable or invalid body, or a wrong confirmation | Unchanged; may confirm again |
| 401, no code | Gate: no credential, or the credential fails | Unchanged; may confirm again |
| 403, no code | Gate: loopback, Host, Origin, owner mapping | Unchanged; may confirm again |
| 409, no code | `activate`: installation has no Rust marker | Unchanged; may confirm again |
| 422, no code | `activate`: the staged database fails the schema preflight | Unchanged; may confirm again |
| 404 `restore_not_found` | `activate`: unknown stage id | Unchanged; the file field empties, choose the file again |
| 503 `restore_busy` | Route semaphore, the activity gate, or the writer when readers did not drain, before it swaps any file | Unchanged; may confirm again |
| 500 `restore_rolled_back` | Writer: rollback put the previous files back | Unchanged; the file field empties, choose the file again |

After a rollback the stage may or may not be removed (cleanup is best effort), so the section asks for a fresh check rather than resending the same `restore_id`.

Everything else is an unknown outcome: every other status, every 3xx, a 2xx whose body fails the contract, any other code or a listed code on the wrong status, an empty or unknown code string, a `code` member that is not a string (`null`, a number, an object), a body that is not the core's problem (other content type, cut off, unreadable, or a `status` that disagrees), `restart_required`, `outcome_unknown`, any code-less 503 (`activate` answers `WriterFailure::Stopped` both before it queues the restore and when the writer's reply is lost after queueing), a refused redirect, and a network error or abort after the request left. An unknown outcome shows `The answer was lost. Reload to see which installation is open.` with Reload as the only action. `restoreAnswer.test.ts` runs `restoreVerdict` over every status from 100 to 599 with and without each code and the core's problem body, and reads malformed raw bodies through `fetchCore`; `Backups.test.tsx` drives the same bodies through Settings and asserts one POST.

The restore's state lives for the page load in `web/apps/ui/src/settings/restoreOutcome.ts`, outside the Settings screen. Once a restore leaves and is not refused, Restore this backup is gone and Check backup, the file field and Create backup are off. Leaving Settings and coming back, choosing a file again, or a restore that answers after Settings closed keeps that lock and its Reload button. Only a page reload clears it.

## Source thumbnail cache

`GET /sources/{id}/thumbnail?path=` (GRE-430) renders a 256 x 256 PNG of one STL in a Source's current revision and caches it at `thumbs/sources/<revision id>/<SHA-256 of the path>-v<renderer version>.png`. Rust writes nothing else under `thumbs/`. Node writes only flat files directly in `thumbs/`: part thumbnails and previews as `thumbs/<16-hex digest>.png` (`globalThumbnailPath`, `globalPreviewPath` in `web/apps/server/src/lib/thumbnails.ts`) and the accepted-media cache as `thumbs/<basis>.png` (`web/apps/server/src/lib/accepted-media-cache.ts`). Node never creates a folder there, so its files and `thumbs/sources/` cannot collide.

The cache is regenerable: a revision never changes, and the renderer version is in the name, so any file there can be deleted, or the whole `thumbs/sources` folder, and the next request renders it again. A file is written under a temporary name, synced, then renamed into place, so a crash leaves at most a `.tmp` file, which is also safe to delete. A cached file that does not start with the PNG signature is rendered again and replaced.

### Backups leave the cache out

GRE-1538. A full backup written by Rust leaves out `thumbs/sources` and everything under it (`archive::SOURCE_THUMBNAIL_CACHE`). `archive::plan` skips that one path while it walks the `thumbs` root. Node's flat files in `thumbs/` are still backed up.

The exclusion is in the walk, not in `archive::FULL_ROOTS`. `thumbs` holds Node's part thumbnails and accepted-media cache too, so it stays a root. The metadata still declares all eleven roots, so the format is unchanged and Node and Rust both validate the archive. A declared root may already be missing from an archive (see [Whole-installation restore: staging](#whole-installation-restore-staging)), so a `thumbs` root without `sources` is valid.

Restoring a backup that still holds `thumbs/sources`, such as one Rust wrote before this change, is harmless. Staging accepts it, because every entry is under the declared `thumbs` root. Activation puts the cache folders back. Their names are the revision ids of the database in that same backup, so a restored cache file still matches its revision. A full restore replaces the whole `thumbs` root, so the live cache never survives next to a restored database whose revision ids it does not match.

Entry count. The cache adds one entry for `thumbs/sources`, one per cached revision, and one per STL viewed in that revision. A test on a disposable data folder with two revisions of 500 STLs each (Node's per-Source snapshot cap, `DEFAULT_LOCAL_SNAPSHOT_STL_LIMIT`) counts 1,003 cache entries, and the walk takes none of them (`archive::tests::the_walk_leaves_out_the_source_thumbnail_cache`). The owner's STL count therefore does not change a backup's entry count, and no owner number is needed. Rust's own limit is 5,000,000 entries (`Limits::backup()`); Node's 100,000-entry limit applies only to archives Node reads.

### Superseded revisions are pruned

When the thumbnail route creates a revision's cache folder, which happens on the first thumbnail of a Source's new current revision, it then prunes the cache:

1. It opens `thumbs/sources` without following a link and lists the revision folders: real folders whose name is a revision id as Rust writes it (decimal, no leading zero). Links, files and other names are left alone.
2. In one read transaction it checks the request's credential and reads every Source's current revision, in any tenant.
3. It removes each listed folder whose revision is not current: first the files in it, without following a link, then the empty folder. A folder that holds a subfolder is left in place.

This removes the folder of a revision superseded by a Rust or a Node sync, of a Source that was deleted, and of a revision restored from an older backup. The folders are listed before the read, so a folder made for a revision that becomes current after the read is not on the list. A request that read the old revision before an activation can still write into the old folder. That folder is removed by the next prune, which runs as soon as any revision's folder is created.

A failed prune is logged as `source_thumbnail_cache_prune_failed` and the thumbnail is still answered. A write into a folder that already exists prunes nothing. Nothing else removes cache folders: no timer and no startup sweep. The cache holds at most the folders of the revisions that were current at the last prune, plus the folders created for them since.

Tests: `pp-storage/src/source_files/tests.rs` (prune), `pp-storage/src/archive/tests.rs` (walk), and `pp-storage/src/restore/tests/export.rs` (backup and restore).

## Whole-installation backup export

With the `desktop_rebuild` feature, `InstallationClient::export_backup(app_version)` writes a full backup archive (format 2, every root in `archive::FULL_ROOTS`, without the [Source thumbnail cache](#backups-leave-the-cache-out)) for the `WriterOwner` that made the client. The caller names no path. The archive is written on the writer thread, as `WriterOwner::create_archive` writes one, so the database snapshot and the files are read while no other write runs.

Each export has its own folder, `<data>/backups/export-<32 hex>/`, which holds the archive and the working copies made for it (the database snapshot and the copy validation checks). Nothing is written under the system temporary directory, which Docker mounts as a 1 GB tmpfs. The returned `BackupFile` holds the archive path, its metadata, its size in bytes, its SHA-256 and its warnings; dropping it removes the folder. `backups` is not a backup root, so no export contains another export.

`backups` also holds the schema upgrade's rollback copies (`pre-schema*.db`, `pre-schema-run`) and, in a folder Node once ran, Node's `.tar.gz` archives. Export touches none of them, and backup retention still matches only `pre-schema*.db`.

A killed process leaves its export folder, with a full copy of the database, behind. `WriterOwner::open` removes those folders in `restore::recover`: after both schema preflights, under the held lease, before any client exists. It removes only real directories named `export-<32 hex>` inside a real `backups` directory and follows no link. It runs after the restore checks, so an open that refuses removes nothing. A folder it cannot remove is logged and does not stop the open. The sweep runs only at open, so a second export never removes an earlier one that a `BackupFile` still holds.

### Size: free disk space, not fixed limits

The backups Rust writes and restores have no fixed limit on total size or on one entry. `archive::Limits` has two named cases and no default, so every caller says which it means. `Limits::backup()`, for these backups, keeps the 200:1 expansion ratio and bounds the entry count at 5,000,000, a guard against damaged metadata and not a product limit. `Limits::node()`, for the Node archives the import reads, keeps Node's limits: 100,000 entries, 20 GiB in total, 8 GiB for one entry.

Each operation checks the free space on the data volume first and keeps a reserve of 1 GiB free (`archive::RESERVE_BYTES`, until the reserve setting of GRE-1445 replaces it). Too little space is a typed refusal that carries the bytes needed, reserve included, and the bytes available: `RestoreError::Refused(Refusal::NoSpace)` from `export_backup`, and `Refusal::NoSpace` from `begin_upload` and `stage`. Nothing is written or extracted.

| Operation | Free space it needs on the data volume |
| --- | --- |
| Backup | Three times the database plus once the files under the backup roots, plus the reserve: about 1× the data folder when most of it is files. |
| Restore, upload | The declared upload size plus the reserve. |
| Restore, stage | The extracted size plus the reserve, while the upload is still there: up to about 2× the data folder for upload and stage together. |

A backup archive over 20 GiB succeeds and carries `BackupWarning::Large`.

The operation returns `restore::RestoreError`. A backup that would leave less than the reserve free is `Refused(Refusal::NoSpace)`. Activation adds `RolledBack` and `RestartRequired`. Other failures are the shared `WriterFailure` (`Stopped` once the owner is closing, `QueueFull` when the writer queue stayed full for 5 s, `Busy` when another process held the database past the writer deadline, and `Internal` for a file or SQLite failure).

### Create and download

With `PP_DESKTOP_REBUILD` exactly `1` or `true`, the product router mounts two routes (GRE-1371). They are absent otherwise. Both require a session or an API key, and the loopback Host and Origin checks. The client sends no path. One request runs at a time; a second answers 503 `A backup is already running`. Responses send `Cache-Control: private, no-store`.

`POST /api/v2/installation/backup` calls `export_backup` with this build's version and answers the archive file name, `created_at`, app version, size in bytes, SHA-256 and warnings. The name is `print-partner-backup.tar.gz`. The router keeps that one archive. A later create drops the previous one, which removes its folder. Not enough free disk answers 503 `Not enough free disk space`, with `needed` and `available`. A restore in progress answers 503 `A restore is in progress`.

`GET /api/v2/installation/backup/archive` streams the kept archive as `application/gzip`. Nothing has been created answers 404 `No backup has been created`. The optional `sha256` query names the archive the client wants by the digest the create answered. When a later create, from any window or client, has replaced it, the route answers 409 `A newer backup replaced this one; create a backup again` and sends no bytes (GRE-1865). A repeated `sha256` keeps its last value. `HEAD` runs the same checks as `GET` in the same order and answers the same status and headers with no body: 503 `A backup is already running` while a create or a download holds the one-request slot, then 404, 409, or 200 with the archive's `Content-Length`. `HEAD` only looks at the slot and never takes it, so HEADs never refuse each other and a `GET` right after a HEAD is not refused as busy. The two answers agree for one state of the slot. Between a HEAD and a `GET`, a create still running makes the `GET` answer 503, and a create that has finished and replaced the archive makes it answer 409; neither sends other bytes. The generated client names the create route `createInstallationBackup`. The download is a file, so it is not in the client.

Tests: `pp-storage/src/restore/tests/export.rs` and `pp-core/tests/installation_backup_http.rs`.

## To be written

The storage milestone fills in these sections:

- Schema versions and how migrations run on startup.
- Durable jobs and recovery after an interrupted external effect. An interrupted printer send is never retried automatically.

[Operations](../OPERATIONS.md) describes backups for the current Node server.

## Spoolman deduction intent

Schema 49, compiled only with `desktop_rebuild`, adds `spoolman_deductions` and
`spoolman_deduction_units` (GRE-411). A confirmed Checkoff burst writes, in its
own transaction, one `pending` row per bed and pinned spool (link grams divided
by link units, times the confirmed units whose Part pins that spool) or one
`skipped` row per bed and reason. Each `pending` row claims its units in
`spoolman_deduction_units`, keyed by link, Part and unit, so a unit is deducted
at most once. Undoing the burst deletes its `pending` and `skipped` rows and
their claims. Nothing is written with the Spoolman Module off. The migration
takes `backups/pre-schema49.db` and validates both tables and their indexes
before stamping version 49, as earlier steps do.

## Spoolman deduction send states

Schema 50, compiled only with `desktop_rebuild`, adds three columns to
`spoolman_deductions` and does not rebuild the table (GRE-1522). `failure` is
one of `unreachable`, `refused`, `reply_lost`, `interrupted`,
`integration_missing` or `integration_disabled`, and only on a `failed`,
`unknown` or `dismissed` row. `failure_status` is an integer from 100 to 599,
and only when `failure` is `refused`. `spool_grams_before` is the grams Spoolman
reported before the send, stored when a row is claimed, and may be null.

A claim moves one `pending` row to `sending` and stores that before value. It
applies only while the row is still `pending`, and it commits before any
request. The send worker's claim also requires the stored integration to equal
the one it read before the spool read; otherwise the row stays `pending`. A finish applies only while the row is `sending`. Opening the database
marks every `sending` row `unknown` with failure `interrupted`, before seeding
and before job recovery, and does not send it. Nothing in this step returns
`sending`, `unknown` or `failed` to `pending`. Reconcile is the only later
write that returns `unknown` or `failed` to `pending`. Undoing a burst still
removes only its `pending` and `skipped` rows.

With the Spoolman Module off at send time the row stays `pending` and is sent
once the Module is on again. That is not a new skip reason, and this step does
not change the reason check. The worker that sends, and the notify a burst
commit uses to wake it (with a 30 second recheck, as the printer send worker
does), are GRE-411 part 2b. The migration takes `backups/pre-schema50.db` and
validates the columns before stamping version 50. A failed step leaves the
database at 49.

## Pending MCP actions

Schema 51, compiled only with `desktop_rebuild`, adds the `pending_actions` table
and `PendingActionsClient` (GRE-475). Rust MCP uses this queue for staging,
listing, confirming, and dismissing proposals. Each row stores an action's id, type,
Build id, label, summary, and opaque JSON parameters, along with `staged_by`
(the display label passed by the caller of `stage`, normalized and never empty),
`depends_on` (a JSON array of action ids), `created_at`, and `expires_at`
(8 hours after creation).

A stored row is in state `pending`, `applying`, `applied`, `dismissed`,
`unknown`, or `invalidated_by_restore`. Calling `claim` moves a `pending` row to
`applying` in its own write transaction before any domain write runs. It refuses
a row whose `expires_at` has passed, reported as `expired` and never stored, and
refuses a row with any dependency that is not `applied`, naming the first action
to apply. A successful `claim` returns a `Claim` that carries the row and an
opaque `ClaimToken` minted from 32 random bytes. Storage persists only the
token's 32-byte SHA-256 digest in `claim_digest` while the row is `applying`.

Calling `settle` requires that `ClaimToken` and needs no MCP key, so revoking a
key during an apply cannot leave a row stuck in `applying`. Settlement updates
the row only when `id`, `state='applying'`, and `claim_digest` all match, and
clears `claim_digest` on exit. A stale or repeated settlement changes nothing
and returns `false`. A caller settles as `End::Applied`, as `End::Released`
(moving the row back to `pending` only for a failure positively known to have
written nothing), or as `End::Unknown` (for any ambiguous outcome where a write
may have committed).
If settlement cannot be recorded, MCP reports uncertainty and keeps retrying
the settlement in the background without rerunning the operation.

Opening the database moves every `applying` row to `unknown` and clears its
`claim_digest` before seeding and job recovery. Nothing returns `unknown` to
`pending`. Restoring a backup moves every open row in that backup (`pending`,
`applying`, or `unknown`) to `invalidated_by_restore` and clears `claim_digest`
before committing the restore journal. Invalidated rows stay in `list` until
dismissed, cannot be claimed or used as dependencies, and must be staged again
under a new id after checking whether earlier effects survived the restore.
Rows that were `applied` or `dismissed` in the backup keep their state.

A `pending`, `expired`, `unknown`, or `invalidated_by_restore` row can be
dismissed. Staging accepts only dependencies that exist and can still apply, and
`depends_on` never changes, so dependencies cannot form a cycle. Rows are never
deleted. The migration writes `backups/pre-schema51.db` and validates the table
and index before stamping version 51. A failed step leaves the database at 50.

## Experiment ancestry

Schema 48, compiled only with `desktop_rebuild`, adds `build_experiments`.
Its `build_id` is the experiment Build's primary key and references
`build_profiles(id)` with `ON DELETE CASCADE`. Its `parent_build_id` references
the same table with `ON DELETE RESTRICT`; an index supports parent lookups.
Existing Builds have no ancestry row and are unchanged. This separate table
leaves Node's schema untouched. The migration takes `backups/pre-schema48.db`
before its immediate transaction and validates the table and index before
stamping version 48, using the same backup and restart path as earlier steps.

The coordinator call on GRE-1284 (2026-11-10) keeps ancestry intact: deletion
refuses while experiments reference the Build. The API checks before deleting
settings or rows and names the experiments in a 409 problem. The foreign key
also protects the link from a direct SQL delete. Archiving does not remove the
Build or its ancestry. See [Experiments](build-workflow.md#experiments).
