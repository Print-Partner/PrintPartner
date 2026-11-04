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

Only one Print Partner runtime may own a data directory at a time. The Rust core and standalone Rust writer acquire an exclusive OS advisory lock on `<data-dir>/printpartner.lock` with [fs4](https://docs.rs/fs4/1.1.0/fs4/). They take it before database preflight, migrations, or any SQLite open. A second instance refuses with `Data directory already owned; stop the other Print Partner process before starting another instance`. Rust creates `printpartner.lock` and `.desktop-lease` with `O_EXCL` and opens an existing entry without `O_CREAT`. On macOS, racing `O_CREAT` opens relative to a directory descriptor can fail with `ENOENT` (GRE-673).

The runtime holds the lock while it can access storage, including compatibility-child restarts. Graceful shutdown releases it after database closure and child reaping. If writer closure or cleanup cannot be proved, the runtime retains ownership until a successful cleanup retry or process exit. The kernel releases the lock when its last owning descriptor closes, including after a crash or SIGKILL. The lock file remains on disk. Its presence does not mean the directory is owned, and startup does not delete it.

The supervised Node compatibility child inherits the locked descriptor at fd 198. The core sets `PP_STORAGE_CORE_LOCK=1`. Private parent setup validates the inherited descriptor against `printpartner.lock` and verifies the parent PID and canonical data directory. The child then opens only that directory under the core's lock. Setting the environment flag alone does not bypass standalone ownership checks. Supervised descendants that access storage retain the descriptor. Detached Bambu Connect launchers replace fd 198 with `/dev/null` before execution, so those external applications cannot retain the lock after a core crash.

Until standalone Node retires, Rust reserves the `.desktop-lease` name with a regular file while holding the advisory lock. Standalone Node publishes a directory at that same name. Neither publication can replace the other file type, so Rust and standalone Node exclude each other before opening the database. Rust refuses an existing directory without reading its contents, checking PIDs, or recovering stale leases. Standalone Node, including today's Docker image, keeps its existing directory lease behavior.

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

The writer creates `.pp-rust-owned.json.tmp-<16 hex>` in the data directory with mode `0600` and `O_EXCL`, writes and fsyncs it, renames it onto `.pp-rust-owned.json`, then fsyncs the directory. Reads, creation, removal, the rename (`pp_os::publish_rename`) and the directory sync (`pp_os::sync_directory`) go through `pp-os`, on the directory handle the lock holds. If the write fails, it removes the temporary file and opens nothing. After a successful write it removes older temporary files.

"Empty" means only these top-level entries: `printpartner.lock`, `.desktop-lease` (a Rust file, or a Node directory holding only `owner.json` and `previous-marker.json`), `.desktop-lease-candidate-*`, `.desktop-lease-release-*`, `.desktop-lease-retired-*`, `.desktop-owner.json`, `.desktop-owner-release-*.json`, `lost+found`, and the marker writer's own `.pp-rust-owned.json.tmp-*` crash leftover. A `.desktop-owner.json` whose pid is running and whose `process_identity` still matches (`<boot id>:<start time>` from `/proc`, as Node computes it) makes the directory non-empty. So does an unreadable record, or a running pid whose identity cannot be looked up. The test `lease_names_in_rust_and_node_lease_code_and_the_marker_name_match_the_allowlist` reads `pp-storage/src/lease.rs`, `web/apps/server/src/storage-lease.ts` and `web/apps/server/src/desktop-context.ts` and fails if their lease names and the allowlist differ.

A standalone Node lease is a `.desktop-lease` directory. The Rust lock refuses it before creating `printpartner.lock`, so Rust writes nothing while Node holds the lease. A Node lease published after that check still fails the existing `.desktop-lease` reservation.

The Node-backed runtime (`pp-core` supervising the Node child) calls `ownership_marker::refuse_marked` right after taking the lease. Any marker entry, valid or not, refuses the start before cutover recovery, Source Sync admission or the Node child. Unmarked directories start as before. Node itself reads schema 34 and refuses newer databases (`web/apps/server/src/db/client.ts`).

The test-only Cargo feature `pp-storage/test-unmarked-writer` turns the decision off for `WriterOwner::open` in tests that open unmarked fixture directories. Only `[dev-dependencies]` entries enable it (pp-storage on itself, pp-api and pp-core), and `rust/scripts/check-boundaries.py` refuses it in any feature table, default, or normal dependency, so release builds never contain it. With it on, `WriterOwner::open_with_gate` takes an explicit gate; the marker tests in `rust/crates/pp-storage/tests/ownership_marker.rs` use it on pp-testkit installations and copied Node schema 34 databases. A manual run of a pp-storage fixture binary in a `desktop_rebuild` build adds `--features desktop_rebuild,test-unmarked-writer`.

## Ownership today

- `pp-storage` owns the SQLite connection, migrations, the bounded settings writer, the read-only pool, the storage lease and safe backups.
- The Node server still runs the application SQL, startup migrations and jobs.
- The desktop rebuild slices move these operations into `pp-storage` behind `desktop_rebuild`. See [Desktop rebuild flag](desktop-rebuild.md).

## Synthetic installations

`web/scripts/synthetic-dataset/generate.mts` writes a synthetic Node schema-34 data directory shaped like a long-lived installation, for invariant tests to run against. Owner UX checks and the cutover rehearsal can fall back to it when no production backup copy is available. Its Source revisions are plain STL folders without a `.printpartner-source-snapshot.json`, so saving a Source manifest on it fails. Its tests cover the stored shape and STL dimensions, not Plate initialization or 3MF export. It is not the representative copy that step 2 of [Whole-runtime cutover](architecture.md#whole-runtime-cutover-and-rollback) requires, and evidence built on it must say so.

From `web/`, with Node 24.21 and `better-sqlite3` built for it:

```sh
npm run fixture:synthetic -- --seed 1138 /tmp/pp-synthetic-1138
```

The output directory must be new or empty. The generator refuses anything else, so it never writes into an existing data directory.

### How it is built

Node's own `SqliteDatabase.connect` creates the schema-34 database. The repository then builds every row: `createSource`, `recordSourceRevision`, `activateSourceRevision`, `createProfile`, `addAddonLayer`, the accept and edit test helpers in `apps/server/src/test/accept-plan.ts`, `publishAcceptedPlates`, `deleteProfile` and `saveUnattributedPrint`. No SQL is written by hand. The clock starts at `2026-10-02T00:00:00.000Z` and advances one minute per operation. Required-unit tokens come from a seeded token factory. Plate IDs use the same `plate_<sha256 prefix>` form as the Node Plate workspace.

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

Names are `Synthetic Source NN`, `Synthetic Build NN` and `synthetic-sNN-part-NN.stl`. Every part is a closed 20 × 20 × 10 mm ASCII STL box that matches the stored unit size, so Plate initialization and 3MF export accept it. URLs and printer hosts use `.invalid`. Nothing is copied from a real data directory.

### Manifest and digest

Every run writes `SYNTHETIC-DATASET.json` at the root of the output with:

- `label`: `synthetic: not the architecture's step-2 rehearsal`
- `synthetic: true`, the `seed`, the `clock` and the Node version
- `generator`: name, `version` (currently 1), script path and script SHA-256
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
- Node opens a copy and reads accepted Plans, the anchor's Plates and the 18 prints.

Node connect re-seeds the starter slicer profiles with `INSERT OR IGNORE`, which advances their `sqlite_sequence` rows on every open, so that check compares the measured shape rather than the digest.

Set `PP_STORAGE_FIXTURE_BIN` to run the Rust check too. It opens a copy with `pp-storage`'s `storage-fixture initialize`, which migrates it from schema 34 to 35 and takes the `pre-schema35.db` backup. The shape must survive. Build the binary with `cargo build --manifest-path rust/Cargo.toml --locked -p pp-storage --bin storage-fixture`.

The generator does not produce open drafts, Checkoff progress, printer sends, exports or historical schema versions. Tests that need those build them separately.

## To be written

The storage milestone fills in these sections:

- Schema versions and how migrations run on startup.
- Backup, restore and downgrade blocking.
- Durable jobs and recovery after an interrupted external effect. An interrupted printer send is never retried automatically.

[Operations](../OPERATIONS.md) describes backups for the current Node server.
