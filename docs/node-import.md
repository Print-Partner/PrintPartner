# Node import

The import converts one Node installation into a new Rust-owned data folder, once. Rust owns its data formats ([ADR 0016](adr/0016-rust-owns-its-data-formats.md)), and `pp-storage::import` is the only Rust code that reads Node's stored shapes on purpose. The source is never opened for writing. Node and Rust never share a live data folder ([Docker image](docker.md#importing-node-data)).

The import lands in six steps. Steps 1 and 2 exist today.

| Step | What it adds | State |
| --- | --- | --- |
| 1 | The dry run and this inventory | in this release |
| 2 | The import: copy, schema upgrade, `projects.local_path` rebase, verification, report, ownership marker | in this release, see [Import](#import) |
| 3 | Printers and integrations | not written; needs the typed fleet and integrations documents |
| 4 | Checkoff links, unmatched prints and the send queue | not written |
| 5 | Sources | not written |
| 6 | Builds and Plans; the report says `complete: true` | not written |

Two decisions shape the later steps:

- Step 2 gives a usable copy before every converter exists. Its report says `complete: false` and names the unconverted rows, and such a copy is disposable.
- A Node send that was `queued` or `sending` arrives as `error` and is sent again by hand, so a copy never prints while Node is live (owner decision, 2026-11-12).

## Dry run

```sh
PP_DESKTOP_REBUILD=1 pp-server import --from <Node data folder or backup archive> [--data <destination>] --dry-run
```

The command exists in `desktop_rebuild` builds and needs `PP_DESKTOP_REBUILD=1`, which the Rust-only Docker image sets. `--from` is a stopped Node data folder, or a format 2 `full` backup archive that Node's `POST /backups` wrote. With `--data`, the dry run reads that folder and reports whether it is as the import needs (absent, or empty by the ownership-marker rule and without a `.desktop-lease` entry) or would be refused (it holds data, a lease entry, a Rust installation, or a `.pp-rust-owned.json` entry that is not a valid marker, or it cannot be read, for example because it is a file; in the last two cases the report gives the reason). It reads the folder's entries only: it never creates the folder, and it does not check the folder's owner or permissions, which the import and the server check when they open it.

What it does:

1. Refuses the source for any reason under [Refusals](#refusals).
2. Copies `print-partner.db`, its `-wal` and its `-journal` into a private directory under the system temporary directory (`TMPDIR`). From an archive it extracts the database and the single-file roots there, and only lists the other entries. SQLite never opens the source database, so no `-shm` or `-wal` file appears in the source. It does not follow links: a database file or `custom_filaments.json` that is a link fails the run, and a linked `repos` is not walked.
3. Checks the copy as startup checks a live database (schema version, `PRAGMA integrity_check`, table shapes), which makes a second short-lived copy under `TMPDIR`, and upgrades it to the current Rust schema. The upgrade keeps no rollback copies: the staged copy is discarded whole when a step fails.
4. Counts on the upgraded copy, prints the report, and removes the temporary directory. A run that is killed leaves `pp-import-dry-run-*` under `TMPDIR`; delete it.

The dry run refuses before it creates anything when `TMPDIR` is the source folder or the destination, or lies inside one: it must write nothing there.

It checks free space in the temporary directory once and refuses with the bytes needed and the bytes free. For a folder the check comes before the copy and needs twice the database files plus 64 MB: the copy, and beside it the larger of the startup check's short-lived copy and the upgrade's write-ahead log. For an archive the check comes after extraction. The dry run extracts only the database and the single-file roots, so the archive's own size does not count; once they are extracted, it needs the database's size plus 64 MB more for the same two things. Nothing checks space before extraction, so an archive whose database does not fit fails during extraction with `import failed:`, not with a refusal. The temporary copy is removed and the archive is not touched. The documented Docker run mounts `/tmp` as a 1 GB tmpfs ([Docker image](docker.md#run)), enough for a database of about 450 MB. To stage elsewhere, set `TMPDIR` to a directory on a disk, for example `docker run -e TMPDIR=/stage -v <volume>:/stage ...`.

The counts describe the upgraded copy, not the source file. The upgrade adds again any starter slicer profile that was deleted, and applies Node's Source revision repair.

It exits 0 with the report on stdout. It exits 1 with `import refused: <reason>` on stderr when the source is refused, or `import failed: <reason>` when a read fails.

### Report

The report holds keys, names and counts, never a stored value. On the synthetic installation (`npm run fixture:synthetic -- --seed 1138`, [Storage](storage.md#synthetic-installations)) it starts:

```text
Dry run of a Node data folder. Nothing was written.
Schema 34, upgraded to 48 on a temporary copy.
22 Builds, 10 Sources, 2 printers, 18 unmatched prints.
```

Then it lists:

- **Installation values and accounts:** one line with every `tenant_id` value that any table holds rows under and the number of accounts, and a line each when a value is not `default` or more than one account is stored. `default` and `anonymous` are printed as stored. Any other value can hold a sign-in name, so it is printed as `basic-***` when it starts with `basic-` and as `***` otherwise.
- **Backup roots:** for each backup root the source holds, the number of its files and folders, counted as a backup counts entries, and for a folder source the bytes of its files; then the total for all roots. A backup is bounded by its number of entries, so this line says how far an installation is from that bound.
- **Tables with rows:** each table's row count, and its rows per `tenant_id` when it has that column, with the values masked the same way. Values are counted and compared as stored and masked only when printed, so two account ids print as two `***` entries.
- **Conversion inventory:** every row of the table below with its step, its status and the item count of each shape. The status is `pending` until the import has the row's converter, then `converted`. An array setting counts its elements, a per-Build setting counts its rows, a column counts the rows that hold a value.
- **Copied unchanged:** the shapes under [Copied unchanged](#copied-unchanged) that hold data, and the keys of the other settings whose value is not a JSON object or array. Digits in a key print as `#`, so per-Build keys group.
- **Kept, not used by Rust:** the shapes under [Kept, not used by Rust](#kept-not-used-by-rust) that hold data.
- **Stored JSON the inventory does not name:** every other setting or column that holds a JSON object or array, as `setting <key>` or `table.column` with its count. The inventory below was built by reading code and may be incomplete. The dry run lists them and still exits 0. The import itself will refuse while any line is here: every stored JSON shape must be named as converted, copied unchanged, kept unused or dropped. So run the dry run on a copy of the real data, report every line, and import after the inventory names them.

"Unmatched prints" counts entries of `printer.unattributed_prints` with no `claimed_at` time and no `dismissed: true`. "Printers" counts the elements of `printer.fleet`; a value that is not an array counts as none.

### Refusals

Each refusal exits 1 and writes nothing to the source.

| Source | Reason |
| --- | --- |
| Folder with a `.pp-rust-owned.json` entry | It is a Rust installation. When the entry is not a valid marker, the message gives the reason instead. |
| Folder with a `.desktop-lease` entry | Print Partner runs or crashed there. |
| Folder without `print-partner.db` | Not a data folder. |
| Database files that change during the copy | Node is still running. |
| Archive that fails validation | Unsafe path, unexpected entry, size limit, bad metadata or failed integrity check. |
| `database-only` or format 1 archive | The import needs the roots a `full` backup holds. |
| Schema below 31 | Start Print Partner v3.3.0 on the Node installation once so it upgrades. |
| Schema above 34 | Not a Node database, for example a backup a Rust installation made. |
| Failed `PRAGMA integrity_check` or table-shape check | The database is damaged. |
| Builds or Sources in more than one tenant | The import does not merge tenants. |
| Too little free space in the temporary directory | The message gives the bytes needed and the bytes free. For an archive this check runs after extraction; a database that does not fit while it is extracted ends the run with `import failed:` instead. |
| `TMPDIR` that is the source folder or the destination, or lies inside one | A dry run writes nothing into either. Set `TMPDIR` to a directory outside both. |

With `--data`, the report also says when the destination is given with a `.` or `..` component, or is the source folder, lies inside it or holds it; the import refuses each. A folder whose backup roots hold a symbolic link, a FIFO, a socket or a device is listed by root and kind with a count, and the report says the import would refuse it. Such entries are not counted as files.

A value other than `default` in any table, and more than one account, are reported by the dry run and do not fail it. The import will refuse both. See [Installation value and accounts](#installation-value-and-accounts).

## Import

```sh
PP_DESKTOP_REBUILD=1 pp-server import --from <Node data folder or backup archive> --data <destination>
```

`--data` must be absent, or empty by the ownership-marker rule. Run the dry run first. The import is strict, and it is widened only when a real dry run shows the need (decisions of 2026-11-12). It refuses everything the dry run refuses, and also:

| Source or destination | Reason |
| --- | --- |
| Destination that holds data, a `.desktop-lease` entry or a Rust installation | The import writes only into a new folder. |
| Destination given with a `.` or `..` component | Such a path can name one folder before the import creates it and another after. Give the folder's own path. |
| Destination that is the source folder, lies inside it, or holds it | The paths are compared with links resolved, before anything is created. The import never writes into its source. |
| `TMPDIR` that is the source folder or the destination, or lies inside one | The startup check copies the database there. Set `TMPDIR` to a directory outside both. |
| Backup roots that hold a symbolic link, a FIFO, a socket or a device | The import copies files and folders and skips nothing. The message gives the number of each kind under each root, never a file name. Find them with `find <folder> ! -type f ! -type d`, then remove or replace them. |
| Destination that gains a database, a root, a report or a marker while the import runs | The import replaces nothing. Nothing is moved in. |
| Stored JSON the inventory does not name | Every shape must be named as converted, copied unchanged, kept unused or dropped. The message lists the shapes as the dry run does. |
| Rows in any table under an installation value other than `default` | The Rust runtime reads only `default`. The message names the value and the number of rows under it. Send the dry-run report back so the case can be added; nothing re-assigns the data today. |
| More than one account | The Rust runtime signs in with one. Remove the extra accounts in Node first, or send the dry-run report back so a choice can be added. There is no option to pick an account. |
| A copied file that reads back differently from its source | The disk or the copy failed. |
| A Source whose current revision has no files under `repos/`, or a Source without a revision whose stored path does not lead below a `repos` folder | The Source's files could not be found in the new folder. The message names the Source id. |
| A source row that is missing or different after the upgrade and the rebase | Every row of every table must arrive as the source stored it, except what [Verification](#verification) lists. The message names the table. |

What it does:

1. Refuses unless the destination is apart from the source, then creates it, resolves its links, checks it a second time, and from then on writes only through that resolved folder. It creates `<destination>/.pp-import/` and stages everything there, never in the system temporary directory. The startup check still makes its short-lived database copy under `TMPDIR`.
2. From a folder, copies the database files and every backup root: regular files and directories, no links. The database files (`print-partner.db`, its `-wal` and its `-journal`) are read back like every other file, before anything opens the copy. Each copied file is read back and its SHA-256 compared with the bytes read from the source; modification times are kept. The read-back runs before the flush in step 4 and reads the page cache: it catches a wrong copy, and it is not a check of the disk. A folder has no cap on its size, on the size of one file or on the number of entries: the only size refusal for a folder is too little free space at the destination. From an archive, validates and extracts it as a restore does, under the restore caps (100,000 entries, 20 GiB expanded, 8 GiB for one entry). An installation above those caps is imported from a folder: a filesystem snapshot or a copy of the stopped data folder (owner decision, 2026-11-12).
3. Checks and upgrades the staged database as the dry run does. It then points every Source at its files in the new folder: a Source with a current revision gets `<destination>/repos/<snapshot_locator>`, and one without keeps the part of its stored path below Node's `repos`. It deletes the rows of `sessions` and `password_reset_tokens`. Last, it compares every table with the row digests it took before the upgrade and stops if a row is missing or different; see [Verification](#verification). Lease files and `backups/` are not copied. Users and password hashes are.
4. Checkpoints and closes the staged database, so it is one file with no `-wal` or `-shm` beside it. Then it flushes everything it staged to disk in one call: `syncfs` on the destination's filesystem on Linux, which the Docker image runs on. Other platforms have no such call, so there it syncs every staged file and directory. The stage is inside the destination, so one flush covers both. A flush that fails ends the run with `import failed:` and no marker.
5. Moves the database and the roots into the destination with a rename that fails if the name exists, so it never replaces a file. It saves the report as `import-report.json`, syncs the destination directory, and removes the stage.
6. Writes `.pp-rust-owned.json` last, into the folder it resolved in step 1 and only while the given path still leads there, and syncs it and its directory.

Disk: the import has no size cap. It is bounded by free disk space. From a folder, the destination needs the size of the source (its database files and its backup roots) plus the size of the database files again, plus 64 MB: one copy of everything, and working space for the schema upgrade, whose write-ahead log can grow as large as the database. That is one times the data size plus one times the database size. A 50 GB installation with a 1 GB database needs about 51.1 GB free at the destination. The import checks this before it copies the roots and refuses with the bytes needed and the bytes available. `TMPDIR` needs one more copy of the database files for the startup check. From an archive, the destination needs the archive's expanded size plus the same working space.

Time: measured once, on 2026-11-13, on a synthetic 50 GB installation (50.09 GB in 54,498 files, a 14 MB database with 199,312 draft assignment rows): the dry run took 8 seconds and the import 24 minutes, on a debug build writing to the same disk it read from, with 73 MB of memory at its peak. Nearly all of that time is the copy and its read-back.

Memory: the check in step 3 is the startup check, and it holds the staged database in memory. It reads `print-partner.db`, its `-wal` and its `-journal` whole, and reads them a second time to confirm they did not change. So the import needs free memory of about twice the size of `print-partner.db` plus its `-wal`, beside the disk space at the destination and one short-lived copy of the database under `TMPDIR`. Nothing checks memory before the import starts, and the size of `repos/` does not count. An import that is killed during that check can leave `pp-preflight-*` under `TMPDIR`; delete it.

A destination with a marker is complete and durable; one without a marker is discarded. Two rules enforce the second half: the Rust runtime refuses to open a folder that holds data and no marker (`ownership_marker::admit_writer`), and the import refuses a destination that is not empty (`inspect_destination`).

### Verification

Before the upgrade, the import takes a SHA-256 of every row of every table in the staged database. After the upgrade and the rebase it takes them again, over the columns the source had, and refuses the import unless every table holds exactly the rows it held. The tables come from the database, not from a list, so a table this page does not know is compared too. The report gives the number of tables and rows compared.

It leaves out only what the import or the upgrade changes on purpose:

| Left out | Why |
| --- | --- |
| Tables `sessions` and `password_reset_tokens` | The import empties them. |
| Setting `schema_version` | The upgrade writes Rust's schema number. |
| Every shape an inventory row marks `converted`, today `projects.local_path` | A converter rewrites it, and that converter's own check covers it. |
| Rows added to `printer_profiles`, `process_profiles` and `filament_profiles` | The upgrade adds the starter profiles a table lacks, as every start does. The rows the source stored must still all be there, unchanged. |

A column or a table the upgrade adds is not compared, because the source has no row for it.

An import that fails removes its stage and leaves the destination without a marker. One that is killed leaves `.pp-import/` behind; no Rust runtime opens that folder, and the next import refuses it until it is emptied.

The report holds keys, names and counts, never a stored value. It says `complete: false` while any inventory row is `pending`. Such a copy is disposable: import again once every converter exists. One more limit applies to a copy made today:

- Node's send queue arrives as stored. The Rust runtime turns `sending` items into `error` when it opens the folder and never starts an upload on its own, so nothing prints without a Send action. Step 4 will turn `queued` items into `error` too.

## Conversion inventory

`rust/crates/pp-storage/src/import/inventory.rs` holds this table as data; a test fails when a shape there is not named here. `ps/` is `rust/crates/pp-storage/src/`. A later step that finds a new shape adds its row here and there.

| Step | Stored shape | Rust reader today | Typed target | Status |
| --- | --- | --- | --- | --- |
| 2 | `projects.local_path`, absolute | `ps/working_drafts/source_reads.rs`, `ps/catalog.rs` | `<data>/repos/<snapshot_locator>` | converted |
| 3 | setting `printer.fleet`; ids and sizes may be numbers or strings | `ps/printer_hosts.rs`, `ps/plates.rs`, `ps/checkoff_progress/print_file.rs`, `ps/read_model/farm.rs` | typed fleet document | pending |
| 3 | setting `integrations`; `port` and `username` loosely typed | `ps/printer_hosts.rs`, `ps/read_model/stock.rs`, `ps/app_settings.rs`, `pp-drivers/src/bambu.rs` | typed integrations document; unknown kinds kept as they are | pending |
| 4 | setting `printer.checkoff_links` | `ps/checkoff_progress/links.rs`, `burst.rs`, `ps/plan_publication.rs` | `Vec<CheckoffLink>` with typed units, file identity and enums | pending |
| 4 | setting `printer.send_queue` | `ps/send_queue.rs` adopts it at every open; `ps/read_model/farm.rs`, `history.rs` | rows in `printer_send_queue`; setting deleted; `artifact_path` rebased | pending |
| 4 | settings `printer.unattributed_prints`, `printer.plan_bindings` | `ps/checkoff_progress/unattributed.rs` | `Vec<UnattributedPrint>`, `Vec<PlanBinding>` | pending |
| 5 | `projects.metadata_json` | `ps/catalog.rs`, `ps/source_scan/node_json.rs` | `SourceMetadata`; unread keys kept in a map | pending |
| 5 | `projects.imported_paths` | four readers that trim JavaScript whitespace, such as `ps/manifest_options/observation.rs` | `Vec<String>`, normalized once | pending |
| 5 | setting `source_categories`; `app_events.payload_json` | `ps/catalog/categories.rs`, `ps/catalog/activity.rs` | `Vec<String>`; `SourceEventPayload` | pending |
| 5 | file `custom_filaments.json` | `ps/admin.rs`, `ps/filaments/runtime.rs` | `Vec<CustomFilament>` | pending |
| 6 | settings `feature_toggles`, `feature_toggles:plan:{id}` | `ps/features.rs` | `pp_domain::features::Toggles` | pending |
| 6 | setting `production_setup:{build}` | `ps/production/setup.rs` | `ProductionSetup` | pending |
| 6 | settings `role_filaments_{build}`, `plan_variant_selection_{build}`, `kit_manifest_{build}` | `ps/working_drafts/preparation.rs`, `ps/build_graph/variants.rs`, `ps/manifest_options/model.rs` | one struct each; spool id one type | pending |
| 6 | `plan_snapshots.payload_json` | `ps/working_drafts/restore.rs`, `ps/catalog.rs` | `SnapshotPayload` | pending |
| 6 | setting `api_keys_v1` | `ps/auth/keys.rs`, re-hashes a legacy `keyHash` on load | `StoredKey`, re-hashed once | pending |
| 6 | Checked, not rewritten: settings `stl_naming_defaults`, `build_planning.v1.{build}`, `build_planning.create.{key}` (a Build id as text, read by `ps/build_graph/create_with_brief.rs`; its key ends in free text, so the report counts these keys and does not print them); `plan_decisions.params_json`, `plan_decisions.result_json`; `plan_draft_required_unit_reconciliations.selection_basis_json`, `plan_draft_required_unit_reconciliations.result_json`; files `.printpartner-source-snapshot.json` under `repos/` | already typed, or stored without reading | must pass today's typed reader | pending |

### Copied unchanged

The import will copy these byte for byte; step 2 writes the copy and its checks:

- Accepted Plan, Required-unit and Plate rows. Step 2 compares their digests with the source's.
- Settings whose value is not a JSON object or array.
- Two credentials, by name: the settings `github_pat` and `discord_notify_webhook_url`. They stay plain `app_settings` rows on every platform, with no converter and no keychain (architect's ruling, 2026-11-13, on GRE-412). A later converter that rewrites `app_settings` leaves them alone. The report prints their keys, never their values.
- Slicer profile text, which Rust reads and keeps opaque (`ps/profiles.rs`): `printer_profiles.raw_json`, `printer_profiles.resolved_flat_config`, `process_profiles.resolved_flat_config`, `filament_profiles.raw_json`, `filament_profiles.resolved_flat_config`, `filament_profiles.retraction`.

The import will not carry `sessions`, `password_reset_tokens`, lease files or `backups/`. It will carry users and password hashes.

### Kept, not used by Rust

No Rust code reads these. The import keeps them byte for byte, also for features the product has cut:

- Tables: `board_posts`, `plan_shares`, `printer_telemetry`, `slicer_instances`.
- Settings: `filename_grouping:{build}`, `manifest_v2_{id}`, `integration_webhooks_v1`, `printer.print_outcomes`, `assistant_feedback`, `assistant_chat_history`, `assistant_daily_usage`. Rust's export job has a `filename_grouping` field of its own and does not read the setting.

## Installation value and accounts

Node stores every row under a `tenant_id` value. It writes `default` when it runs without accounts or with single-user sign-in, and for the first account otherwise. A later account's id, `basic-<login>` or `anonymous` appear in other setups. The Rust-only runtime maps every session to `default` (`ps/auth/policy.rs`), so it cannot see data under another value, and it refuses to sign in when `users` holds more than one row.

The dry run reads the values from every table that has the column, not only from Builds and Sources: an account with no Build and no Source still stores its settings and events under its own value. It prints them masked (see [Report](#report)), and the number of accounts as a count with no email address. The import will handle only data under `default` with at most one account, and will refuse anything else (decision of 2026-11-12). If the dry run shows another value or a second account, send the report back: a choice for that case is added only when it is needed.
