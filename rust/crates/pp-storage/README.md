# pp-storage

The SQLite storage layer for the Rust backend. It owns the data directory lease, one writer thread, a read-only connection pool, schema migrations, and the typed handles that every operation family uses to read and write.

Current ownership and allowed dependencies: [Architecture](../../../docs/architecture.md#crates-and-allowed-edges) and [ADR 0009](../../../docs/adr/0009-keep-crates-and-deepen-modules.md). [ADR 0007](../../../docs/adr/0007-rust-crate-layout.md) is historical dependency evidence only.

## Purpose

- All production SQL in Rust: transactions, the bounded writer queue, and the read-only pool. The defaults are 32 queued writes and 4 readers (`Limits::default`).
- Schema setup and forward migrations, pre-upgrade copies, and refusal of a newer schema.
- The data directory lease that keeps Node and a second Rust process out.
- Durable jobs: claims, fences, retention, and restart recovery.
- Family transactions: auth, settings and Product Modules, catalog, scan, sync, uploads, drafts, build graph, publication, Required units, Checkoff, and read projections.
- Database backups and Node-compatible backup archives.

## Not responsible for

- HTTP, MCP, and event adapters: `pp-api`. Listeners and session admission: `pp-gateway`.
- Running workers, startup order, and shutdown: `pp-core`.
- Immutable Source revision bytes and extraction: `pp-source`.
- Platform file mechanics: `pp-os` supplies no-follow opens and file status, private directory creation, advisory locks (`try_lock`, `unlock`), and durable rename and sync. Storage decides the paths and policy. A path that storage writes to a row, returns, or compares with a stored path is in `pp_os::display_path` spelling, so Windows stores `C:\…` as Node does and never the verbatim `\\?\C:\…` of the canonical data directory; files still open through the canonical path.
- Sync policy and slicer operations: `pp-services`.
- Printer and provider protocols: `pp-drivers`. Storage records queue and effect state; it never sends to a printer.
- Archive restore and activation. The crate writes and validates archives (`archive::validate`) but does not restore them yet.

## Interface

Start with `WriterOwner::open(directory, limits)`. It takes the lease, migrates the database, recovers jobs, starts the writer thread, and returns the owner plus `SchemaReady`. `open_at` takes a fixed seed timestamp for tests. Every family handle is a method on `WriterOwner`.

| Item | What it does |
| --- | --- |
| `WriterOwner` | Owns the lease, writer thread and pools. `client`, `backup`, `create_archive`, `database_pages`, `admit_disk_quota`, `shutdown`. |
| `SettingsClient` | Cloneable handle. `submit` queues a `SettingCommand` (`Set` or `CompareAndSet`); `reader` checks out a `ReaderCheckout`. |
| `ReaderCheckout` | A pooled read-only connection. `snapshot`, `get_setting`. Returns to the pool on drop. |
| `Limits`, `SchemaReady`, `Setting`, `SettingSnapshot`, `WriteReply` | Open limits, migration result, and settings values. |

Modules available in every build:

| Module | Main items |
| --- | --- |
| `auth` | `WriterOwner::auth`, `auth_with_policy` return `AuthClient`. `AuthPolicy` and its parts select registration and tenant rules. Users, sessions and API keys. `auth::rebuild` is gated. |
| `jobs` | `WriterOwner::jobs` (`AtomicJobClient`), `job_worker` (`ServerWorkerClient`), `job_physical_owner`, `retain_jobs`. `WorkerAdmission` caps concurrent claims per kind and resource and sets the lease length; a claim carries an `AttemptLease`. |
| `catalog` | `source_catalog`, `source_catalog_with_policy`, `local_source_catalog` return `SourceCatalogClient`. |
| `printer_hosts` (`desktop_rebuild`) | `read(client, wait)` and `project(raw)`: the Moonraker hosts in the `integrations` setting, projected as Node's `/__desktop/moonraker-hosts`. `printer_link(client, wait, printer_id)` and `integration(client, wait, id)` resolve a fleet printer or an integration id as Node's printer-host routes do. Read-only. |
| `read_model` | `accepted_reads`, `accepted_reads_with_policy` return `ReadClient`. Submodules `views`, `workflow`, and `stock` (`ReadClient::read_stock`, `shortfalls`: Spoolman shortfalls from each send's grams in `print_link_grams`, `measured` or `estimated`, read-only). With `desktop_rebuild`, `build_history(policy)` returns `history::HistoryClient`, a paged read-only Build history ([Build history](../../../docs/build-history.md)). `build_workflow(policy, inputs)` returns `build_workflow::WorkflowClient`, the Build workflow and task feed ([Build workflow](../../../docs/build-workflow.md)). `build_planning(policy, SourceReads)` returns `planning::PlanningClient`, the read-only Build planning and decisions reads ([Build planning and decisions](../../../docs/build-planning.md)). `farm_reads(policy)` returns `farm::FarmClient`, the read-only printers, Checkoff results, unmatched prints, match suggestions (`read_for_suggestions`, `Farm::suggestions`) and send queue ([Farm reads](../../../docs/farm-reads.md)). |
| `archive` | `Request`, `Metadata`, `Scope`, `Limits`, and `validate` for Node's tar.gz backup format. |
| `lease` | `StorageLease`, `LeaseRelease`, `prepare_directory`. |

Modules behind `#[cfg(feature = "desktop_rebuild")]`:

| Module | Main items |
| --- | --- |
| `working_drafts` | `working_drafts_with_policy`, `working_drafts_with_observations` (`WorkingDraftClient`), `source_reads`. |
| `build_graph` | `build_graph_with_policy`, `build_graph_with_observations` (`BuildGraphClient`); `wire`, `manifest_options`. |
| `plan_publication` | `publication_with_policy` (`PublicationClient`): the acceptance transaction. |
| `required_units` | `required_units_with_policy` (`ReconciliationClient`). |
| `checkoff_progress` | `checkoff_progress`, `checkoff_progress_with_policy` (`ProgressClient`). With `desktop_rebuild`, `Request::FarmLog` and `Request::UndoFarmLogEntry` serve the farm log (`farm_log`, [Farm log](../../../docs/farm-log.md)). `Request::FileRequest` and `Request::AssignFile` back the print-file assignment routes, and `print_file::SnapshotStore` holds files/content snapshots ([Print-file assignment routes](../../../docs/desktop-rebuild.md#print-file-assignment-routes-gre-1220)). |
| `domain_events` | `domain_events`, `domain_events_with_policy` (`DomainEventsClient`): the transactional domain event log, cursor reads and live wake-ups ([Domain events](../../../docs/domain-events.md)). |
| `features` | `features_with_policy` (`FeatureClient`), `View`, `Scope`. |
| `send_queue` | The `printer_send_queue` table (schema 44). `WriterOwner::open` moves a clean legacy `printer.send_queue` setting into it and marks `sending` items interrupted; readers read the table, or the setting while it exists ([Send queue storage](../../../docs/send-queue.md)). `INTERRUPTED_ERROR`. |
| `print_grams` | The Rust-only `print_link_grams` table (schema 46): grams per Printer Checkoff link. `record_estimate` stores the slicer's estimate and grams per millimetre; `record_measured` converts the printer's measured length (`Measurement`). `Basis` is `Estimated` or `Measured` ([Desktop rebuild](../../../docs/desktop-rebuild.md#filament-grams-per-send-gre-1165-part-a-gre-1237)). |
| `uploads` | `imports` (`ImportClient`), `import_repos_root`. |
| `source_scan`, `source_sync` | Local scan and sync job state, driven through `ServerWorkerClient` (`observe_local_scan`, `open_source_sync`, ...). |
| `native_secrets` | `native_secret_migration` (`NativeSecretMigrationClient`). |
| `admin` | `admin_files` (`AdminFiles`), `custom_filaments`: Node-owned admin files, read only. |
| `profiles`, `manifest_options`, `manifest_text`, `production` | Profile import shapes, manifest models and UTF-16 text, plate layout and packing (`production::layout`). |

Other gated entry points: `WriterOwner::open_with_job_feature_admission`, `job_worker_with_policy`, `catalog_access`, `catalog_key_access`, `rebuild_auth_with_policy`.

Binaries are test fixtures that the parity and process tests run: `storage-fixture`, `auth-fixture`, `jobs-fixture`, `catalog-fixture`, `plan-read-fixture`, and `required-unit-fixture` (needs `desktop_rebuild`). Each takes a data directory argument.

## Effects and flags

- Files in the data directory: `print-partner.db` (WAL, `synchronous=FULL`, foreign keys, 5 s busy timeout), the locks `printpartner.lock` and `.desktop-lease`, the empty `repos`, `sources`, `exports`, `thumbs` and `covers` directories, and pre-upgrade copies in `backups/pre-schema*.db`.
- A runtime directory `pp-<pid>-<random>` under the system temp directory, removed on shutdown.
- Threads: one writer thread per `WriterOwner` and a process-wide pool of two auth threads. No Tokio runtime, network, or child processes.
- `backup` and `create_archive` write a new file outside managed storage and refuse a destination inside it.
- Env: the catalog metadata version guard reads `PP_DESKTOP_REBUILD` (`1` or `true`). The fixture binaries read `PP_READ_SESSION`, `PP_REVIEW_OBSERVATIONS` and `PP_RECONCILIATION_SESSION`.
- Embedded data: `data/schema.json`, `data/filament-catalog.json`, and pinned Node schemas in `data/historical`.

Cargo features:

- `desktop_rebuild` (default off): enables the gated modules, the optional `pp-domain`, `pp-contracts` and `saphyr-parser` dependencies, and schema version 42 instead of 35. Runtime callers must also check `PP_DESKTOP_REBUILD`.
- `codegen`: adds `schemars` schemas for the TypeScript generator without turning on the gated edges.

## Tests

- Unit tests: `src/tests.rs` and per-module `tests` modules.
- Integration tests in `tests/`: auth, jobs (`durable_jobs`, `job_*`, `jobs_*`), catalog, imports and uploads, `working_draft_*`, Required units, Checkoff, archive and retention, writer and lease, and Node parity (`*_parity`). Shared helpers live in `tests/common`.
- Fixtures: `tests/fixtures` holds Node generators (`*-node.mjs`, `*-node.mts`) and their recorded output. `tests/fixtures/required-units/README.md` explains how to regenerate them.
- Dev-dependencies: `pp-testkit` (disposable installations, see `tests/testkit_installations.rs`) and `tempfile`.
- Some parity tests run Node. Set `PP_SCHEMA_PARITY_NODE` to the Node 22.23.3 binary; otherwise they use `node` from `PATH`.

```sh
cargo nextest run --manifest-path rust/Cargo.toml -p pp-storage
cargo nextest run --manifest-path rust/Cargo.toml -p pp-storage --features desktop_rebuild
```
