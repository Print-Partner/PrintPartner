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
| 35 to 47, `desktop_rebuild` builds only | `RustUpgrade`: run only the later Rust steps. |
| The build's newest: 35 without `desktop_rebuild`, 48 with it | `Current`: nothing to migrate. |
| 1 to 30, or above the newest (a value above `u64::MAX` counts as `u64::MAX`) | Refused with `IncompatibleSchema`. |

`ticket_t_7_migrations` opens a fresh directory and Node-built schema 31, 32, 33 and 34 databases through the writer and checks each against its named path.

The refusal is terminal. The version is read from the preflight copy in the temporary directory, before the integrity check, so a refused database, its `-wal` and its `-journal` keep their bytes. The message names the version found, the supported range and the recovery step:

- Too old: `Incompatible storage: cannot upgrade database schema version 30 (this release opens schema 31 to 48). Install Print Partner v3.3.0, start it once so it upgrades the database, then install this release. The database was not changed.`
- Too new: `Incompatible storage: database schema version 49 is newer than supported version 48 (this release opens schema 31 to 48). Install the Print Partner release that last opened this data folder, or restore a backup made by this release. The database was not changed.`

Each runtime stops on it instead of reopening the database:

- **Node-backed start.** Node opens 31 to 34. Right after `lease_unmarked`, and before cutover recovery or the Node child, `pp-core` runs the same preflight (`versions::node_refusal`) and refuses any other version against the range 31 to 34. Preflight reads and checks the version first and records it, before `PRAGMA integrity_check` and the Rust schema validations. So an unsupported version is refused here even when the file also fails its integrity check or does not match the Rust schema; those errors themselves are unchanged. The child never starts, so the supervisor never backs off and restarts it. A failure before the version is read, such as a file SQLite cannot open or a non-numeric version, logs `node_schema_preflight_unchecked` and leaves the decision to Node, as before. The check copies the database to the temporary directory. For a version this build's Rust schema accepts, it runs `PRAGMA integrity_check` once per start, including versions 35 up to the Rust schema ceiling, which Node then refuses. A nonzero version below 31, or a version newer than the Rust schema ceiling, returns before that check.
- **Rust-only start.** `WriterOwner::open` refuses during its preflight, and startup returns the error once; nothing retries it.
- **Desktop.** The startup dialog is titled "Print Partner cannot open this data folder" and shows the message. Stderr gets `desktop_startup_failed: core_startup -> incompatible_storage: found=49 supported=31..=48 direction=newer`. `core.log` does not: it attaches only after core startup admits the data directory.
- **Headless `pp-server`.** It writes `incompatible_storage: found=<n> supported=<oldest>..=<newest> direction=<older|newer>` and the message to stderr and exits with code 78 (`EX_CONFIG`). Other start failures exit with 1. Docker restart policies restart on any non-zero code, so a container with a restart policy starts again, and each start refuses again without writing. Stop the container before you recover the data folder.

A downgrade refuses the live schema before restore cleanup. It preserves the database, all SQLite sidecars (`-wal`, `-shm`, `-journal`), saved backup archives, and pending restore uploads and stages. Repeated refused starts do not prune or replace these copies. This does not restore older data automatically; rollback after new writes still needs informed owner approval (ADR 0010).

## Ownership today

- `pp-storage` owns the SQLite connection, migrations, the bounded settings writer, the read-only pool, the storage lease and safe backups.
- The Node server still runs the application SQL, startup migrations and jobs.
- The desktop rebuild slices move these operations into `pp-storage` behind `desktop_rebuild`. See [Desktop rebuild flag](desktop-rebuild.md).
- `pp-storage::import` is the only Rust code that reads Node's stored shapes on purpose. Today it holds the dry run and the conversion inventory. See [Node import](node-import.md).

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

With the `desktop_rebuild` feature, `pp_storage::restore` stages a whole-installation restore: the database and every backup root in `archive::FULL_ROOTS`. Staging never changes live files. Activation, which swaps the staged copy into place, and the HTTP routes are not built yet (GRE-404, GRE-1161). Build snapshot restore is a separate Plan operation.

Staging (`WriterOwner::installation`, then `InstallationClient::begin_upload` and `stage`) takes no filesystem path from the caller:

1. Both steps need a valid `.pp-rust-owned.json`; an unmarked data directory refuses with `Refusal::Unmarked` before writing anything.
2. The upload is written to `<data>/.pp-restore/incoming-<32 hex>.tar.gz`, opened with `create_new`. `begin_upload` takes the size the caller declares as its byte limit. There is no fixed cap: it refuses with `Refusal::NoSpace`, before creating the file, when that many bytes would leave less than the reserve free. A write that would pass the limit refuses with `Refusal::TooLarge` and writes nothing past it. The first failed write fails the upload for good: later writes and `stage` refuse it. A `.pp-restore` that is a symlink or not a directory refuses.
3. A first pass over the archive (`archive::declared_bytes`) totals the sizes its tar headers declare and counts its entries, extracting nothing. If that total would leave less than the reserve free, `stage` refuses with `Refusal::NoSpace` and nothing is extracted. `archive::extract` then runs the same entry, scope, ratio and `integrity_check` validation as `archive::validate` and writes every entry under `<data>/.pp-restore/staged-<32 hex>/`. It stops with `Refusal::LargerThanDeclared`, and the stage is removed, before it writes more than the first pass counted, so an archive cannot fill the disk past what was checked. The second pass decompresses the archive again, which takes minutes at 50 GB; restores are rare. The entry guard admits only regular files and directories under the core files and declared roots, with no absolute, `.` or `..` component, so a hostile entry cannot write outside the stage or create a link. After the tar end-of-archive block, validation reads the gzip stream to its end, so a damaged CRC or length trailer refuses the archive, and so does anything after the tar end other than zero padding, or more than 1 MiB of padding. Both exporters (Rust's `archive::write` and Node's `tar.c({ gzip: true })`) write exactly one gzip member, so the file must end where that member ends. Any byte after it is refused, including padding, a second member, and the rest of a tar split across members, whose entries tar iteration would never see. `archive::validate` shares this code, so it refuses the same archives. Only a `full` scope is accepted.
4. `schema::preflight` admits the staged database as startup admits a live one: a schema newer than this build, older than 31, missing (version 0), or failing a job-table check refuses with `Refusal::UnsupportedSchema`.

The upload file is removed whether staging succeeds or not. A refusal removes only its own staged copy and keeps an earlier successful stage. Earlier `staged-<32 hex>` directories are removed only after the new stage is complete, and nothing else is removed. If that removal fails, `stage` still returns the new stage, with `Staged::cleanup_error` describing the failure, and the next open removes the leftover. Staging never removes `.pp-restore` itself, so an upload starting at the same time keeps its directory; the next open removes it. Stages for one owner run one at a time. While a stage runs, the data volume holds the upload, the new extraction and the earlier stage at once.

Restore work belongs to the `WriterOwner` that created the client. Shutdown first closes all admission at once: new storage writes, reader checkouts, uploads, upload writes and stages are refused from then on. It then waits for reader checkouts and restore work against one shared 5 s deadline, so the drain takes at most 5 s in total before the writer thread is joined. If a checkout or restore work is still running at the deadline, shutdown fails and the owner keeps its lease until the process exits, so no other owner can clear `.pp-restore` under it.

Stages are ephemeral until activation. A stage lasts only as long as the `WriterOwner` that made it. The next schema-admitted open removes every stage, complete or not, and the client has to upload the backup again. An incompatible build refuses first and leaves those stages alone. Schema admission holds only for the build that ran it, and a restart can be an upgrade or a downgrade. Without a completion marker, `recover` also can't tell a complete stage from a half-extracted one, and a forgotten stage as large as the installation would otherwise stay on the data volume. Activation (GRE-1285) runs in the same process as its stage.

A `full` archive need not contain every root in `includedRoots`. Both the Rust (`archive::plan`) and Node (`backup-restore.ts`) exporters declare all eleven roots and skip a root that was missing when the backup was taken. Three roots are files, which have no directory entry. A declared root with no entries therefore means it was absent at backup time, and an archive holding only the metadata and the database is a valid backup of an installation with no files. Validation still refuses any entry outside the declared roots. Telling an absent root apart from a stripped archive needs a format change, tracked in GRE-1348.

`WriterOwner::open` runs `restore::recover` under the directory lock, after the ownership marker decision and both read-only schema preflights, before opening the live database for writes. Unsupported schemas refuse before cleanup, so their recovery uploads and staged copies remain available to the release that created them. It checks every entry before it removes any: `incoming-<id>.tar.gz` must be a regular file and `staged-<id>` a real directory, both read with `symlink_metadata`. It then removes them and `.pp-restore` itself. Any other entry, a link or a wrong type under one of those names, or a `.pp-restore` that is not a real directory, refuses the open and removes nothing, because this build cannot tell whether it holds the only copy of an installation.

Threat model for `.pp-restore`: staging checks the work directory with `symlink_metadata` and then uses its pathname for the upload, the extraction, schema admission and cleanup. A process that can replace `.pp-restore` between that check and that use could redirect those writes or removals. Only the effective user or root can do that, because store admission requires the data directory to belong to the effective user and to deny group and other write (see [Desktop rebuild](desktop-rebuild.md)). Print Partner is single-user, so this is in scope only for a hostile process running as that user, which could already change the data directory directly. A `.pp-restore`, `staged-*` or `incoming-*` that is already a link or has the wrong type is refused. Moving every restore path onto `O_NOFOLLOW` directory handles is tracked in GRE-1347.

Tests: `pp-storage/src/restore/tests.rs` (`ticket_t_8_restore_*`, on pp-testkit installations with a copied schema-34 database).

## Whole-installation backup export

With the `desktop_rebuild` feature, `InstallationClient::export_backup(app_version)` writes a full backup archive (format 2, every root in `archive::FULL_ROOTS`) for the `WriterOwner` that made the client. The caller names no path. The archive is written on the writer thread, as `WriterOwner::create_archive` writes one, so the database snapshot and the files are read while no other write runs.

Each export has its own folder, `<data>/backups/export-<32 hex>/`, which holds the archive and the working copies made for it (the database snapshot and the copy validation checks). Nothing is written under the system temporary directory, which Docker mounts as a 1 GB tmpfs. The returned `BackupFile` holds the archive path, its metadata, its size in bytes, its SHA-256 and its warnings; dropping it removes the folder. `backups` is not a backup root, so no export contains another export.

`backups` also holds the schema upgrade's rollback copies (`pre-schema*.db`, `pre-schema-run`) and, in a folder Node once ran, Node's `.tar.gz` archives. Export touches none of them, and backup retention still matches only `pre-schema*.db`.

A killed process leaves its export folder, with a full copy of the database, behind. `WriterOwner::open` removes those folders in `restore::recover`: after both schema preflights, under the held lease, before any client exists. It removes only real directories named `export-<32 hex>` inside a real `backups` directory and follows no link. A folder it cannot remove is logged and does not stop the open. The sweep runs only at open, so a second export never removes an earlier one that a `BackupFile` still holds.

### Size: free disk space, not fixed limits

The backups Rust writes and restores have no fixed limit on total size or on one entry. `archive::Limits` has two named cases and no default, so every caller says which it means. `Limits::backup()`, for these backups, keeps the 200:1 expansion ratio and bounds the entry count at 5,000,000, a guard against damaged metadata and not a product limit. `Limits::node()`, for the Node archives the import reads, keeps Node's limits: 100,000 entries, 20 GiB in total, 8 GiB for one entry.

Each operation checks the free space on the data volume first and keeps a reserve of 1 GiB free (`archive::RESERVE_BYTES`, until the reserve setting of GRE-1445 replaces it). Too little space is a typed refusal that carries the bytes needed, reserve included, and the bytes available: `RestoreError::NoSpace` from `export_backup`, `Refusal::NoSpace` from `begin_upload` and `stage`. Nothing is written or extracted.

| Operation | Free space it needs on the data volume |
| --- | --- |
| Backup | Three times the database plus once the files under the backup roots, plus the reserve: about 1× the data folder when most of it is files. |
| Restore, upload | The declared upload size plus the reserve. |
| Restore, stage | The extracted size plus the reserve, while the upload is still there: up to about 2× the data folder for upload and stage together. |

A backup archive over 20 GiB succeeds and carries `BackupWarning::Large`.

The operation returns `restore::RestoreError`: `NoSpace`, or the shared `WriterFailure` (`Stopped` once the owner is closing, `QueueFull` when the writer queue stayed full for 5 s, `Busy` when another process held the database past the writer deadline, and `Internal` for a file or SQLite failure). The HTTP routes, which stream the download, are GRE-1371.

Tests: `pp-storage/src/restore/tests/export.rs`.

## To be written

The storage milestone fills in these sections:

- Schema versions and how migrations run on startup.
- Restore activation, and downgrade blocking.
- Durable jobs and recovery after an interrupted external effect. An interrupted printer send is never retried automatically.

[Operations](../OPERATIONS.md) describes backups for the current Node server.

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
