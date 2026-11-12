# Node import

The import converts one Node installation into a new Rust-owned data folder, once. Rust owns its data formats, and `pp-storage::import` is the only Rust code that reads Node's stored shapes on purpose. The source is never opened for writing. Node and Rust never share a live data folder ([Docker image](docker.md#importing-node-data)).

The import lands in six steps. Only step 1 exists today.

| Step | What it adds | State |
| --- | --- | --- |
| 1 | The dry run and this inventory | in this release |
| 2 | The import: copy, schema upgrade, `projects.local_path` rebase, verification, report, ownership marker | not written |
| 3 | Printers and integrations | not written; needs the typed fleet and integrations documents |
| 4 | Checkoff links, unmatched prints and the send queue | not written |
| 5 | Sources | not written |
| 6 | Builds and Plans; the report says `complete: true` | not written |

Two decisions shape the later steps:

- Step 2 gives a usable copy before every converter exists. Its report says `complete: false` and names the unconverted rows, and such a copy is disposable. This is provisional until the owner confirms it.
- A Node send that was `queued` or `sending` arrives as `error` and is sent again by hand, so a copy never prints while Node is live (owner decision, 2026-11-12).

## Dry run

```sh
PP_DESKTOP_REBUILD=1 pp-server import --from <Node data folder or backup archive> --dry-run
```

The command exists in `desktop_rebuild` builds and needs `PP_DESKTOP_REBUILD=1`, which the Rust-only Docker image sets. `--from` is a stopped Node data folder, or a format 2 `full` backup archive that Node's `POST /backups` wrote. `--data` is accepted and not read.

What it does:

1. Refuses the source for any reason under [Refusals](#refusals).
2. Copies `print-partner.db`, its `-wal` and its `-journal` into a private directory under the system temporary directory (`TMPDIR`). From an archive it extracts the database and the single-file roots there, and only lists the other entries. SQLite never opens the source database, so no `-shm` or `-wal` file appears in the source.
3. Checks the copy as startup checks a live database (schema version, `PRAGMA integrity_check`, table shapes) and upgrades it to the current Rust schema. The upgrade keeps a copy of the database per schema step until the run ends, so the temporary directory needs about 14 times the database size. The documented Docker run mounts `/tmp` as a 1 GB tmpfs ([Docker image](docker.md#run)), enough for a database of about 70 MB; for a larger one, set `TMPDIR` to a directory on a disk.
4. Counts on the upgraded copy, prints the report, and removes the temporary directory.

It exits 0 with the report on stdout. It exits 1 with `import refused: <reason>` on stderr when the source is refused, or `import failed: <reason>` when a read fails.

### Report

The report holds keys, names and counts, never a stored value. On the synthetic installation (`npm run fixture:synthetic -- --seed 1138`, [Storage](storage.md#synthetic-installations)) it starts:

```text
Dry run of a Node data folder. Nothing was written.
Schema 34, upgraded to 48 on a temporary copy.
22 Builds, 10 Sources, 2 printers, 18 unmatched prints.
```

Then it lists:

- **Tables with rows:** each table's row count, and its rows per `tenant_id` when it has that column.
- **Conversion inventory:** every row of the table below with its step, its status and the item count of each shape. The status is `pending` until the row's converter exists. An array setting counts its elements, a per-Build setting counts its rows, a column counts the rows that hold a value.
- **Copied unchanged:** the shapes under [Copied unchanged](#copied-unchanged) that hold data, and the keys of settings whose value is not a JSON object or array. Digits in a key print as `#`, so per-Build keys group.
- **Stored JSON the inventory does not name:** every other setting or column that holds a JSON object or array, as `setting <key>` or `table.column` with its count. The inventory below was built by reading code and may be incomplete. Report every line here before importing: each one needs a row or a place under Copied unchanged.

"Unmatched prints" counts entries of `printer.unattributed_prints` with no `claimed_at` time and no `dismissed: true`.

### Refusals

Each refusal exits 1 and writes nothing to the source.

| Source | Reason |
| --- | --- |
| Folder with a `.pp-rust-owned.json` entry | It is a Rust installation. |
| Folder with a `.desktop-lease` entry | Print Partner runs or crashed there. |
| Folder without `print-partner.db` | Not a data folder. |
| Database files that change during the copy | Node is still running. |
| Archive that fails validation | Unsafe path, unexpected entry, size limit, bad metadata or failed integrity check. |
| `database-only` or format 1 archive | The import needs the roots a `full` backup holds. |
| Schema below 31 | Start Print Partner v3.3.0 on the Node installation once so it upgrades. |
| Schema above 34 | Not a Node database, for example a backup a Rust installation made. |
| Failed `PRAGMA integrity_check` or table-shape check | The database is damaged. |
| Builds or Sources in more than one tenant | The import does not merge tenants. |

A single tenant other than `default` is reported, not refused. Whether the import moves it to `default` is an open decision.

## Conversion inventory

`rust/crates/pp-storage/src/import/inventory.rs` holds this table as data; a test fails when a shape there is not named here. `ps/` is `rust/crates/pp-storage/src/`. A later step that finds a new shape adds its row here and there.

| Step | Stored shape | Rust reader today | Typed target | Status |
| --- | --- | --- | --- | --- |
| 2 | `projects.local_path`, absolute | `ps/working_drafts/source_reads.rs`, `ps/catalog.rs` | `<data>/repos/<snapshot_locator>` | pending |
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
| 6 | Checked, not rewritten: settings `stl_naming_defaults`, `build_planning.v1.{build}`; `plan_decisions.params_json`, `plan_decisions.result_json`; `plan_draft_required_unit_reconciliations.selection_basis_json`, `plan_draft_required_unit_reconciliations.result_json`; files `.printpartner-source-snapshot.json` under `repos/` | already typed, or stored without reading | must pass today's typed reader | pending |

### Copied unchanged

The import copies these byte for byte:

- Accepted Plan, Required-unit and Plate rows. Their digests are checked against the source.
- Settings whose value is not a JSON object or array.
- Slicer profile text that Rust keeps opaque (`ps/profiles.rs`): `printer_profiles.raw_json`, `printer_profiles.resolved_flat_config`, `process_profiles.resolved_flat_config`, `filament_profiles.raw_json`, `filament_profiles.resolved_flat_config`, `filament_profiles.retraction`. The dry run on the synthetic installation found these; the first inventory did not name them.
- Tables Rust does not read: `board_posts`, `plan_shares`, `printer_telemetry`, `slicer_instances`.

Not carried: `sessions`, `password_reset_tokens`, lease files and `backups/`. Users and password hashes are carried.
