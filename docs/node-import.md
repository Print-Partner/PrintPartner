# Node import

The import converts one Node installation into a new Rust-owned data folder, once. Rust owns its data formats ([ADR 0016](adr/0016-rust-owns-its-data-formats.md)), and `pp-storage::import` is the only Rust code that reads Node's stored shapes on purpose. The source is never opened for writing. Node and Rust never share a live data folder ([Docker image](docker.md#importing-node-data)).

The import lands in six steps. All six exist today.

| Step | What it adds | State |
| --- | --- | --- |
| 1 | The dry run and this inventory | in this release |
| 2 | The import: copy, schema upgrade, `projects.local_path` rebase, verification, report, ownership marker | in this release, see [Import](#import) |
| 3 | Printers and integrations | in this release, see [Printers and integrations](#printers-and-integrations) |
| 4 | Checkoff links, unmatched prints and the send queue | in this release, see [Checkoff and the send queue](#checkoff-and-the-send-queue) |
| 5 | Sources: metadata, import rules, categories, activity and custom filaments | in this release, see [Sources](#sources) |
| 6 | Builds and Plans; the report says `complete: true` | in this release, see [Builds and Plans](#builds-and-plans) |

Two decisions shape the later steps:

- Step 2 gives a usable copy before every converter exists. Its report says `complete: false` and names the unconverted rows, and such a copy is disposable. Confirmed by the owner, 2026-11-12 (GRE-412 Owner decisions).
- A Node send that was `queued` or `sending` arrives as `error` and is sent again by hand, so a copy never prints while Node is live (owner decision, 2026-11-12).

## Dry run

```sh
PP_DESKTOP_REBUILD=1 pp-server import --from <Node data folder or backup archive> [--data <destination>] --dry-run
```

The command exists in `desktop_rebuild` builds and needs `PP_DESKTOP_REBUILD=1`, which the Rust-only Docker image sets. `--from` is a stopped Node data folder, or a full backup archive that Node's `POST /backups` wrote: format 2 `full`, or format 1 from Print Partner v3.3.0 and earlier (see [Format 1 archives](#format-1-archives)). With `--data`, the dry run reads that folder and reports whether it is as the import needs (absent, or empty by the ownership-marker rule and without a `.desktop-lease` entry) or would be refused (it holds data, a lease entry, a Rust installation, or a `.pp-rust-owned.json` entry that is not a valid marker, or it cannot be read, for example because it is a file; in the last two cases the report gives the reason). It reads the folder's entries only: it never creates the folder, and it does not check the folder's owner or permissions, which the import and the server check when they open it.

What it does:

1. Refuses the source for any reason under [Refusals](#refusals).
2. Copies `print-partner.db`, its `-wal` and its `-journal` into a private directory under the system temporary directory (`TMPDIR`). From an archive it extracts the database and the single-file roots there, and only lists the other entries. SQLite never opens the source database, so no `-shm` or `-wal` file appears in the source. It does not follow links: a database file or `custom_filaments.json` that is a link fails the run, and a linked `repos` is not walked.
3. Checks the copy as startup checks a live database (schema version, `PRAGMA integrity_check`, table shapes), which makes a second short-lived copy in a `.pp-preflight-*` folder inside that private directory, and upgrades it to the current Rust schema. The upgrade keeps no rollback copies: the staged copy is discarded whole when a step fails.
4. Counts on the upgraded copy, prints the report, and removes the temporary directory. A run that is killed leaves `pp-import-dry-run-*` under `TMPDIR`; delete it.

The dry run refuses before it creates anything when `TMPDIR` is the source folder or the destination, or lies inside one: it must write nothing there.

The check in step 3 reads `print-partner.db`, its `-wal` and its `-journal` in 1 MiB chunks and reads them a second time to confirm they did not change, so it holds no database file in memory. Nothing checks memory before the run starts. The size of `repos/` does not count. A run killed during that check can leave `.pp-preflight-*` inside the dry run's directory under `TMPDIR`; deleting `pp-import-dry-run-*` removes it.

It checks free space in the temporary directory once and refuses with the bytes needed and the bytes free. For a folder the check comes before the copy and needs twice the database files plus 64 MB: the copy, and beside it the larger of the startup check's short-lived copy and the upgrade's write-ahead log. For an archive the check comes after extraction. The dry run extracts only the database and the single-file roots, so the archive's own size does not count; once they are extracted, it needs the database's size plus 64 MB more for the same two things. Nothing checks space before extraction, so an archive whose database does not fit fails during extraction with `import failed:`, not with a refusal. The temporary copy is removed and the archive is not touched. The documented Docker run mounts `/tmp` as a 1 GB tmpfs ([Docker image](docker.md#run)), enough for a database of about 450 MB. To stage elsewhere, set `TMPDIR` to a directory on a disk, for example `docker run -e TMPDIR=/stage -v <volume>:/stage ...`.

The counts describe the upgraded copy, not the source file. The upgrade adds again any starter slicer profile that was deleted, and applies Node's Source revision repair.

It exits 0 with the report on stdout. It exits 1 with `import refused: <reason>` on stderr when the source is refused, or `import failed: <reason>` when a read fails.

### Dry-running a large installation from a snapshot

A backup archive is refused with `Invalid backup archive: <reason>` when it breaks any of the archive limits:

| Limit | Reason in the message |
| --- | --- |
| More than 20 GiB expanded in total | `Backup archive decompressed byte limit exceeded (21474836480)` |
| One entry over 8 GiB, for example a large `print-partner.db` | `Backup entry exceeds decompressed size limit` |
| More than 100,000 entries | `Backup archive entry limit exceeded (100000)` |
| Expanded size over 200 times the archive's size | `Backup archive exceeds the decompression ratio limit (200)` |

An installation that breaks any of them is dry-run from a folder instead: a filesystem snapshot of the data folder, or a copy of it. The folder path copies the database files to a private folder under `TMPDIR` and lists `repos/` in place, and none of these limits applies to it.

```sh
PP_DESKTOP_REBUILD=1 pp-server import --from <copy> --dry-run
```

- Point `--from` at the snapshot or copy, never at the live `/data` while Node runs. The dry run writes nothing to its source, but it refuses a folder whose database files change while it copies them, and a copy taken while Node runs keeps any `.desktop-lease`, which is refused under [Refusals](#refusals).
- `TMPDIR` needs twice the size of the database files plus 64 MB free, and must lie outside the copy (see the free-space rule above). The size of `repos/` does not count: it is listed, not copied.
- The check streams those database files and does not hold them in memory. Disk on `TMPDIR` is the limit (see the free-space rule above). Check the database size first with `ls -l <copy>/print-partner.db*`.

The test `a_folder_over_the_archive_size_cap_is_dry_run_and_its_archive_is_refused` in `rust/crates/pp-storage/src/import/tests.rs` checks both paths with a lowered cap.

### Format 1 archives

Print Partner v3.3.0 and earlier wrote format 1: `formatVersion` 1 and no `scope`. Its roots are the five that Node's `LEGACY_BACKUP_ROOTS` names: `repos`, `sources`, `exports`, `thumbs` and `covers`. The dry run and the import read it as they read a format 2 `full` archive, with the same validation, and count the same tables. v3.3.0 also wrote its pre-update backups (`backups/print-partner-pre-update-to-*.tar.gz`) as format 1 with only the database. A format 1 archive with no entry under any of the five roots is one of those, and is refused as a database-only archive.

The six roots that format 2 added are not in it: `assistant-domain`, `manifests`, `custom_filaments.json`, `kit-catalog.json`, `kit-catalog.yaml` and `path-hints.yaml`. The report lists no entries for them, the shapes stored there count 0 (for example `file custom_filaments.json`), and the import writes none of them. A format 1 archive written before v3.3.0 can hold a schema below 31, which is refused as [Refusals](#refusals) describes.

### Report

The report holds keys, names and counts. The only stored values it prints are a printer id or a bed size that the conversion changed, and the `tenant_id` values, which it masks. On the synthetic installation (`npm run fixture:synthetic -- --seed 1138`, [Storage](storage.md#synthetic-installations)) it starts:

```text
Dry run of a Node data folder. Nothing was written.
Schema 34, upgraded to 50 on a temporary copy.
22 Builds, 10 Sources, 2 printers, 18 unmatched prints.
```

Then it lists:

- **Installation values and accounts:** one line with every `tenant_id` value that any table holds rows under and the number of accounts, and a line each when a value is not `default` or more than one account is stored. `default` and `anonymous` are printed as stored. Any other value can hold a sign-in name, so it is printed as `basic-***` when it starts with `basic-` and as `***` otherwise.
- **Database files:** the bytes of `print-partner.db`, its `-wal` and its `-journal`, for each the source holds, and their total. The startup check copies these under `TMPDIR` and streams them, so compare the total with the free space given above; the documented Docker run gives `/tmp` 1 GB.
- **Source paths:** four counts, "Source paths outside the installation", "Source paths whose folder is missing in the copy", "Sources whose current revision is missing in the copy" and "Sources whose snapshot name is not a plain path", by the rule under [Source paths](#source-paths). Paths are never printed.
- **Printers and integrations:** each id or size the conversion changed, with the stored value and the converted value, and a count for each field it dropped or moved. A dropped or moved field is named, never given its value. An address that holds a username or password is a count only. See [Printers and integrations](#printers-and-integrations).
- **Backup roots:** for each backup root the source holds, the number of its files and folders, counted as a backup counts entries, and for a folder source the bytes of its files; then the total for all roots. A backup is bounded by its number of entries, so this line says how far an installation is from that bound.
- **Tables with rows:** each table's row count, and its rows per `tenant_id` when it has that column, with the values masked the same way. Values are counted and compared as stored and masked only when printed, so two account ids print as two `***` entries.
- **Conversion inventory:** every row of the table below with its step, its status and the item count of each shape. The status is `converted` when the import rewrites the row, `checked` when it reads the row with today's reader and leaves the bytes, and `pending` when it does neither. An array setting counts its elements, a per-Build setting counts its rows, a column counts the rows that hold a value.
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
| `database-only` archive, or a format 1 archive with no entry under its roots | The import needs the roots a full backup holds. v3.3.0 wrote its pre-update backups as format 1 with only the database. |
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
| `TMPDIR` that is the source folder or the destination, or lies inside one | The dry run stages its copy of the database there. The import refuses the same setting, although its startup check copies into `<destination>/.pp-import/.pp-preflight-*`. Set `TMPDIR` to a directory outside both. |
| Backup roots that hold a symbolic link, a FIFO, a socket or a device | The import copies files and folders and skips nothing. The message gives the number of each kind under each root, never a file name. Find them with `find <folder> ! -type f ! -type d`, then remove or replace them. |
| Destination that gains a database, a root, a report or a marker while the import runs | The import replaces nothing. Nothing is moved in. |
| Stored JSON the inventory does not name | Every shape must be named as converted, copied unchanged, kept unused or dropped. The message lists the shapes as the dry run does. |
| Rows in any table under an installation value other than `default` | The Rust runtime reads only `default`. The message names the value and the number of rows under it. Send the dry-run report back so the case can be added; nothing re-assigns the data today. |
| More than one account | The Rust runtime signs in with one. Remove the extra accounts in Node first, or send the dry-run report back so a choice can be added. There is no option to pick an account. |
| A copied file that reads back differently from its source | The disk or the copy failed. |
| A Source whose current revision has no files under `repos/`, or whose snapshot name is not a plain path below `repos/` | The Source's files could not be found in the new folder. The message names the Source id. |
| Two Sources that would lead to the same folder in the copy | The mapping of stored paths is wrong for this installation. The message names both Source ids. Send the dry-run report back. |
| A Source whose rewritten path does not stay under the copy's `repos/` with links resolved, or whose path in the copy is not what its stored path maps to | The import checks every path it rewrote, apart from the code that wrote it. The message names the Source id. |
| A source row that is missing or different after the upgrade and the rebase | Every row of every table must arrive as the source stored it, except what [Verification](#verification) lists. The message names the table. |
| `printer.fleet` or `integrations` that is not the typed shape | The message names the field and a count. A key the typed shape has no place for is dropped and does not refuse the import. See [Printers and integrations](#printers-and-integrations). |

What it does:

1. Refuses unless the destination is apart from the source, then creates it, resolves its links, checks it a second time, and from then on writes only through that resolved folder. It creates `<destination>/.pp-import/` and stages everything there, never in the system temporary directory. The startup check makes its short-lived database copy in a `.pp-preflight-*` folder inside the stage, on the destination's filesystem.
2. From a folder, copies the database files and every backup root: regular files and directories, no links. The database files (`print-partner.db`, its `-wal` and its `-journal`) are read back like every other file, before anything opens the copy. Each copied file is read back and its SHA-256 compared with the bytes read from the source; modification times are kept. The read-back runs before the flush in step 4 and reads the page cache: it catches a wrong copy, and it is not a check of the disk. A folder has no cap on its size, on the size of one file or on the number of entries: the only size refusal for a folder is too little free space at the destination. From an archive, validates and extracts it with the checks a restore runs, under Node's archive limits (`archive::Limits::node()`: 100,000 entries, 20 GiB expanded, 8 GiB for one entry). A restore of a backup Rust wrote has no such byte limits; see [Storage](storage.md). An installation above those caps is imported from a folder: a filesystem snapshot or a copy of the stopped data folder (owner decision, 2026-11-12).
3. Checks and upgrades the staged database as the dry run does, and rewrites `printer.fleet` and `integrations` as under [Printers and integrations](#printers-and-integrations). It then rewrites Source paths by the rule under [Source paths](#source-paths). It deletes the rows of `sessions` and `password_reset_tokens`. Last, it compares every table with the row digests it took before the upgrade and stops if a row is missing or different; see [Verification](#verification). Lease files and `backups/` are not copied. Users and password hashes are.
4. Checkpoints and closes the staged database, so it is one file with no `-wal` or `-shm` beside it. Then it flushes everything it staged to disk in one call: `syncfs` on the destination's filesystem on Linux, which the Docker image runs on. Other platforms have no such call, so there it syncs every staged file and directory. The stage is inside the destination, so one flush covers both. A flush that fails ends the run with `import failed:` and no marker.
5. Moves the database and the roots into the destination with a rename that fails if the name exists, so it never replaces a file. It saves the report as `import-report.json`, syncs the destination directory, and removes the stage.
6. Writes `.pp-rust-owned.json` last, into the folder it resolved in step 1 and only while the given path still leads there, and syncs it and its directory.

Disk: the import has no size cap. It is bounded by free disk space. From a folder, the destination needs the size of the source (its database files and its backup roots) plus the size of the database files again, plus 64 MB: one copy of everything, and working space for the schema upgrade, whose write-ahead log can grow as large as the database. That is one times the data size plus one times the database size. A 50 GB installation with a 1 GB database needs about 51.1 GB free at the destination. The import checks this before it copies the roots and refuses with the bytes needed and the bytes available. The startup check then makes one more short-lived copy of the database files in a `.pp-preflight-*` folder inside the stage, on the destination, and refuses with the bytes needed and the bytes available when that copy does not fit. The check before the roots are copied does not include this extra copy. From an archive, the destination needs the archive's expanded size plus the same working space, and the same extra copy during the check.

Time: measured once, on 2026-11-13, on a synthetic 50 GB installation (50.09 GB in 54,498 files, a 14 MB database with 199,312 draft assignment rows): the dry run took 8 seconds and the import 24 minutes, on a debug build writing to the same disk it read from, with 73 MB of memory at its peak. Nearly all of that time is the copy and its read-back.

Memory: the check in step 3 is the startup check. It reads `print-partner.db`, its `-wal` and its `-journal` in 1 MiB chunks, so it holds no database file in memory. An import killed during the check leaves that folder inside the stage.

A destination with a marker is complete and durable; one without a marker is discarded. Two rules enforce the second half: the Rust runtime refuses to open a folder that holds data and no marker (`ownership_marker::admit_writer`), and the import refuses a destination that is not empty (`inspect_destination`).

### Source paths

Node stores each Source's folder in `projects.local_path` as an absolute path. The import rewrites one only when it can show where the files are in the copy:

- **A Source with a current revision** gets `<destination>/repos/<snapshot_locator>`. The snapshot must be in the copy; if it is not, the import stops.
- **A Source without one** is rewritten only when its stored path lies under the source installation's own `repos` folder and the copy holds that folder. Both separators are read first, so `..\..` cannot pass as a name. The stored path is compared part by part with the `--from` folder plus `repos`, and everything below that is kept, also when a lower part is again called `repos`. A part that is empty, `.`, `..` or a drive letter is never accepted.
- **Every other stored path is kept byte for byte.** A folder the user picked outside the installation, a path that climbs out of `repos`, or a folder the copy does not hold is never pointed at another Source's files. Such a Source keeps its record and loses nothing, but its files are not in the copy.

The dry run and the import report give two counts, never a path: "Source paths outside the installation" and "Source paths whose folder is missing in the copy". While either is above zero the copy says `complete: false`. The dry run gives two more counts, "Sources whose current revision is missing in the copy" and "Sources whose snapshot name is not a plain path", and says the import would refuse when either is above zero. A snapshot name that is empty, `.`, `..` or a drive letter is not a plain path. The import still refuses that installation; the dry run finishes and prints the rest of the report. For an archive the dry run reads the listing, since it does not extract `repos/`. It counts a snapshot as present only when the listing has a directory at `repos/<name>` or a file under that directory. A file stored at exactly `repos/<name>` is not the snapshot.

A stored path on a Windows share (`\\server\share\...`) is compared the same way. The check after writing accepts the leading `//` that those two separators become, and still refuses an empty segment anywhere else.

Two cases count as outside although the files may be in the copy, because the import cannot show it:

- **A snapshot or copy at another path than Node used.** The comparison is with the `--from` folder as given. If Node ran with its data at `/data` and the import reads a copy at `/snapshots/data`, the stored paths start with `/data` and count as outside. Sources with a current revision are not affected.
- **An archive.** It does not record which folder its installation lived in.

After writing, the import checks every Source's path in the copy against the path the source stored, without the code that wrote it: the path is either the stored one, or `<destination>/repos/` followed by the snapshot name or by what the stored path holds below the installation's `repos`.

### Verification

Before the upgrade, the import takes a SHA-256 of every row of every table in the staged database. After the upgrade and the rebase it takes them again, over the columns the source had, and refuses the import unless every table holds exactly the rows it held. The tables come from the database, not from a list, so a table this page does not know is compared too. The report gives the number of tables and rows compared.

It leaves out only what the import or the upgrade changes on purpose:

| Left out | Why |
| --- | --- |
| Tables `sessions` and `password_reset_tokens` | The import empties them. |
| Setting `schema_version` | The upgrade writes Rust's schema number. |
| Every shape an inventory row marks `converted`: `projects.local_path`, `printer.fleet`, `integrations`, `printer.checkoff_links`, `printer.unattributed_prints`, `printer.plan_bindings`, `printer.send_queue`, `projects.metadata_json`, `projects.imported_paths`, `source_categories`, `app_events.payload_json`, `custom_filaments.json`, `feature_toggles`, `feature_toggles:plan:`, `production_setup:`, `role_filaments_`, `plan_variant_selection_`, `kit_manifest_`, `plan_snapshots.payload_json` and `api_keys_v1` | A converter rewrites it, and that converter's own check covers it. For `projects.local_path` see [Source paths](#source-paths). For the printer settings see [Printers and integrations](#printers-and-integrations) and [Checkoff and the send queue](#checkoff-and-the-send-queue). For Sources see [Sources](#sources). For Builds and Plans see [Builds and Plans](#builds-and-plans). A row marked `checked` stays in the digest. |
| Rows added to `printer_profiles`, `process_profiles` and `filament_profiles` | The upgrade adds the starter profiles a table lacks, as every start does. The rows the source stored must still all be there, unchanged. |

A column or a table the upgrade adds is not compared, because the source has no row for it.

An import that fails removes its stage and leaves the destination without a marker. A refused import can leave the new destination folder behind, empty. One that is killed leaves `.pp-import/` behind, or the copy and `import-report.json` without a marker. The next import refuses that folder until it is emptied. `pp-server --data` on it exits 1 with `This data folder holds an import that stopped before it finished.` before it locks, marks or writes anything there, in every build.

An import that fails while moving its copy into place leaves the database, and any roots already moved. It leaves no stage, no report and no marker. First-run setup cannot tell that folder from a stopped Node installation. A build without the `test-unmarked-writer` feature, which includes every release build, refuses it at the writer's marker check, because it has no `.pp-rust-owned.json`. A test build with that feature, such as a workspace test build, opens it. Only a change inside the importer can make this folder detectable: durable evidence written before the first move.

Every import ends with a last line that says what the destination now holds: on stdout after the report, or on stderr after `import refused:` or `import failed:`. The exit codes stay 0 and 1.

| Last line | Next step |
| --- | --- |
| `destination: an imported installation, ready to start with --data` | Start `pp-server --data` on it. |
| `destination: absent or empty; import again into it or into another empty folder` | Fix the reason, then import again. |
| `destination: a stopped import; no Print Partner runtime opens it; import again into a new, empty folder` | Import into a new, empty folder. Nothing empties this one for you. |
| `destination: a database without an ownership marker: a Node installation, or an import that failed while moving its copy into place; not an import destination` | If the import just printed `import failed:`, this is that import's partial copy: import into a new, empty folder. Otherwise the folder held a Node installation and the import did not write to it. |
| Any other `destination: ... not an import destination` | The folder held a Rust installation, a Node installation that is running or did not shut down, or other data. The import did not write to it. |

The report holds keys, names and counts, plus the printer id and size changes described under [Printers and integrations](#printers-and-integrations). It says `complete: true` when every inventory row is `converted` or `checked` and every Source path leads into the copy. It says `complete: false` while any row is `pending`, or while a Source path is outside the installation or missing in the copy. A copy that says `complete: false` is disposable.

## Printers and integrations

After the upgrade, the import rewrites `printer.fleet` and `integrations` into the documents Settings reads, then reads them back with those readers. A document that does not come back as the typed shape is left as stored. The dry run counts the reason and says the import would refuse; the import refuses and writes no marker.

A printer is read the way Node reads a fleet row. The id is stored as text, so a numeric id becomes text. A missing name becomes `Printer`. A missing model becomes the name. Width, depth and height become whole millimetres, rounding down; the margin rounds up. A missing width, depth, margin or slot count takes Node's default (250, 210, 4 and 1). A missing height stays missing. Slots are held to 1 to 4. Each of those changes is listed with the field, the stored value and the converted value. A size that is not a number at or above zero, a slot count that is not a whole number, a row that is not an object, a missing or empty or repeated id, or a document that is not a list, is a refusal. The message names the field and a count.

Moonraker, PrusaLink and Spoolman rows become the typed integration. `baseUrl`, `apiKey`, `user` and `storage_path` move to `base_url`, `api_key`, `username` and `storage` only when that field is empty; the report names the move and does not print the value. A PrusaLink `api_key` stays `api_key`. Every other known kind is kept byte for byte. A type Rust does not know, a missing name, config, `base_url`, `created_at` or `updated_at`, or a repeated id, is a refusal.

A key the typed shape has no place for, including one inside `config` or a loaded-filament row, is dropped. The report names the field and a count, never the value. That alone does not refuse the import.

An address that holds a username or password (`user:pass@host`) is counted and kept as stored. The report gives the count only. Such an address has never connected on either runtime.

## Checkoff and the send queue

After the printer documents, the import rewrites `printer.checkoff_links`, `printer.unattributed_prints` and `printer.plan_bindings`, and moves `printer.send_queue` into `printer_send_queue`. A Checkoff link goes through `links::parse`. An unmatched print or a plan binding is kept when Node's reader would keep it. A send-queue item follows Node's `parseItem`. An item that reader discards is left out and named by its index and the reason. A value that reader would keep, and the typed shape or the table cannot store, refuses the import. The dry run counts the reason and says the import would refuse; the import refuses and writes no marker. The report names keys, indexes and counts. It does not print a stored value, a path or a secret.

A Checkoff link Node's reader drops (not an object, a state it does not know, or a missing id, integration, printer, filename or positive `profile_id`) is absent. `applied` is stored as `verified`. A `part_id` stored as text becomes a number when it is a positive integer, and the report says so. A key the typed link has no place for is dropped and named.

An unmatched print Node's reader drops (not an object, or an id or filename that is not text) is absent. A document that is not a list becomes an empty list, which is what that reader returns. A known field of another type, including a `claimed_profile_id` that is not a positive integer, refuses the import. Unknown keys are dropped and named. Plan bindings are stricter, as Node's reader is: empty text becomes an empty list, and invalid JSON, a non-list, or a row that is not a binding refuses the import. Extra keys are dropped and named.

Each send-queue item Node's reader keeps is inserted in stored order. Its unit list is stored in `printer_send_queue.checkoff_units`. The inventory names that column, so the JSON is not an unknown document. `queued` becomes `error` with the not-started text, and `sending` becomes `error` with the interrupted text, so a copy never prints while Node is live (owner decision, 2026-11-12). Nothing inserts a `printer-upload` job. The setting is deleted after the report has counted it. `FarmTasks::read` then reads the queue through `queue_list`, which uses the table once the setting is gone. A repeated id, an id longer than 128 characters, or an `artifact_path` that sits under the installation and then climbs out refuses the import. A path under the Node folder, with no empty, `.`, `..` or drive part below it, is rewritten to the same path below the destination. A path outside the folder, and every path from an archive, is kept. The report counts rebased paths and kept paths, and does not print them.

## Sources

After Checkoff, the import rewrites each Source's `metadata_json` and `imported_paths`, the `source_categories` setting, each `app_events.payload_json`, and `custom_filaments.json`.

`metadata_json` is stored as an object. A `category` string is trimmed with JavaScript's whitespace, which is what `GET /sources` shows. Keys the readers already use (`sync_required`, `sync_error`, `remote_update_status`, `remote_checked_at`, `naming`, `catalog_metadata_version`) stay as stored. Any other key stays in the object and is named in the report. Text that is not a JSON object is stored as `{"raw": <the original text>}`, which is what the catalog reader synthesises, and the report does not print that text. A `category` that is not text is kept as stored and named.

`imported_paths` is stored as a JSON array of strings. Each string is trimmed, backslashes become slashes, a leading slash is removed, and a rule that is not empty, does not end in `/` and does not end in `.stl` gains a trailing `/`. That is the normalization the four readers do, done once here. A document that is not a list, and an element that is not text or is blank, is left out and named by index. Those are the values the readers already skip. The report does not print a path.

`source_categories` is stored as the list the category reader keeps. Each entry is trimmed per segment. An empty path, or one with more than nine segments, is left out and named by index, as is an element that is not text. Each remaining path is stored with its parents, and a later entry that repeats a path in another case is not stored again.

An activity `payload_json` is stored as an object. `source_id` stays when it is a number, and `source_name` and `error` stay when they are text, which is what `GET /sources/activity` reads. Any other key stays and is named. A payload that is not an object becomes `{}`, which is what that reader uses. The report names the event id and the key, not the text.

`custom_filaments.json` is stored as `{"filaments": [...]}`. Each object keeps `id`, `display_name`, `product_line`, `hex` and `combo_label`, trimmed when they are text, and any other key. A file that is present and that the reader does not treat as a store (not JSON, or no `filaments` member) becomes `{"filaments":[]}`. A key on the store other than `filaments` is dropped and named, because the list reader does not return it. A missing file stays missing. A `null` file, a `filaments` value that is not a list, an entry that is not an object, or one of the five fields that is not text, refuses the import. The report names the index and the key. It does not print a name or a colour.

The dry run reads the filament file and does not write it. The import writes the converted file in the stage after the inventory has counted the copied file, and after that copy's bytes were checked.

## Builds and Plans

After Sources, the import rewrites feature toggles, each Build's production setup, role filaments, variant selection and kit manifest, each named snapshot, and `api_keys_v1`. It then reads the rows marked `checked` and leaves their bytes as stored.

`feature_toggles` and `feature_toggles:plan:{id}` are stored as `Toggles`. A known key must be a boolean. An unknown key is dropped and named. A plan key whose suffix is not the Build id is a refusal. A layer that is not an object, or a known key that is not a boolean, is a refusal: the reader treats that layer as unreadable, and the import does not replace it with `{}`.

`production_setup:{build}` is stored as the document `stored_setup` returns. A document that reader ignores is stored as that Build's default setup, and the report says `production_setup ignored`. A key whose suffix is not the Build id is a refusal. The report does not print the suffix.

`role_filaments_{build}` is stored as one object per role, with `filament_color_id`, `filament_custom_hex` and `spoolman_spool_id`. A spool id Node stored as a JSON number is stored as text, and the report says so without the id. A color or hex that is not text becomes null, which is what the reader already does. An extra key is dropped and named. Text that is not a JSON object becomes `{}`.

`plan_variant_selection_{build}` is stored as a record of strings. A number becomes text. Any other value refuses the import, because the loose reader would keep it and the record cannot. Text that is not a JSON object becomes `{}`.

`kit_manifest_{build}` is stored with `KitManifest::write_json`. A key the struct does not have is dropped and named. A document `from_stored` rejects is stored as the empty kit, which is what the reader returns, and the report says `kit_manifest ignored`.

`plan_snapshots.payload_json` is stored as `SnapshotPayload`. `layers`, when present, must be an array of objects. `recipe_summary` and `captured_at`, when present, must be text or null. `accepted_plan_revision_id` and `accepted_plan_version`, when present, must be null or an integer the restore reader can hold. Any other key stays. A payload that is not a JSON object, or that breaks one of those rules, refuses the import.

`api_keys_v1` is stored as the `StoredKey` collection `load` keeps. A legacy `keyHash` is re-hashed once, and the report gives the count, not the key or the hash. A collection `load` rejects refuses the import. A missing row stays missing.

The checked rows are `stl_naming_defaults`, `build_planning.v1.{build}`, `build_planning.create.{key}`, `plan_decisions.params_json`, `plan_decisions.result_json`, both reconciliation JSON columns, and `.printpartner-source-snapshot.json` under `repos/`. The naming reader and the decision reader accept the stored text, so those bytes stay. A planning brief the reader rejects refuses the import. A `build_planning.create` value that is not a Build id refuses the import. The report counts those keys and does not print them: the create key ends in free text. A reconciliation the reader rejects refuses the import. A snapshot file is left as stored. `{}` is the stub a synthetic installation writes, and it passes. Any other object must be version 1 and must carry `upstreamRevisionKey`, a 64-character lowercase `manifestDigest`, `files` and `selection.omittedFiles`. A folder dry run reads the files. An archive dry run only lists them, because it does not extract `repos/`; the import checks them after extraction. The report does not print a path.

## Conversion inventory

`rust/crates/pp-storage/src/import/inventory.rs` holds this table as data; a test fails when a shape there is not named here. `ps/` is `rust/crates/pp-storage/src/`. A later step that finds a new shape adds its row here and there.

| Step | Stored shape | Rust reader today | Typed target | Status |
| --- | --- | --- | --- | --- |
| 2 | `projects.local_path`, absolute | `ps/working_drafts/source_reads.rs`, `ps/catalog.rs` | `<data>/repos/<snapshot_locator>`, or `<data>/repos/` and the stored path below the installation's `repos`; any other path is kept | converted |
| 3 | setting `printer.fleet`; ids and sizes may be numbers or strings | `ps/printer_hosts.rs`, `ps/plates.rs`, `ps/checkoff_progress/print_file.rs`, `ps/read_model/farm.rs` | typed fleet document | converted |
| 3 | setting `integrations`; `port` and `username` loosely typed | `ps/printer_hosts.rs`, `ps/read_model/stock.rs`, `ps/app_settings.rs`, `pp-drivers/src/bambu.rs` | typed integrations document; unknown kinds kept as they are | converted |
| 4 | setting `printer.checkoff_links` | `ps/checkoff_progress/links.rs`, `burst.rs`, `ps/plan_publication.rs` | `Vec<CheckoffLink>` with typed units, file identity and enums | converted |
| 4 | setting `printer.send_queue` | `ps/send_queue.rs` adopts it at every open; `ps/read_model/farm.rs`, `history.rs` | rows in `printer_send_queue`; setting deleted; `artifact_path` rebased | converted |
| 4 | settings `printer.unattributed_prints`, `printer.plan_bindings` | `ps/checkoff_progress/unattributed.rs` | `Vec<UnattributedPrint>`, `Vec<PlanBinding>` | converted |
| 5 | `projects.metadata_json` | `ps/catalog.rs`, `ps/source_scan/node_json.rs` | `SourceMetadata`; unread keys kept in a map | converted |
| 5 | `projects.imported_paths` | four readers that trim JavaScript whitespace, such as `ps/manifest_options/observation.rs` | `Vec<String>`, normalized once | converted |
| 5 | setting `source_categories`; `app_events.payload_json` | `ps/catalog/categories.rs`, `ps/catalog/activity.rs` | `Vec<String>`; `SourceEventPayload` | converted |
| 5 | file `custom_filaments.json` | `ps/admin.rs`, `ps/filaments/runtime.rs` | `Vec<CustomFilament>` | converted |
| 6 | settings `feature_toggles`, `feature_toggles:plan:{id}` | `ps/features.rs` | `pp_domain::features::Toggles` | converted |
| 6 | setting `production_setup:{build}` | `ps/production/setup.rs` | `ProductionSetup` | converted |
| 6 | settings `role_filaments_{build}`, `plan_variant_selection_{build}`, `kit_manifest_{build}` | `ps/working_drafts/preparation.rs`, `ps/build_graph/variants.rs`, `ps/manifest_options/model.rs` | one struct each; spool id one type | converted |
| 6 | `plan_snapshots.payload_json` | `ps/working_drafts/restore.rs`, `ps/catalog.rs` | `SnapshotPayload` | converted |
| 6 | setting `api_keys_v1` | `ps/auth/keys.rs`, re-hashes a legacy `keyHash` on load | `StoredKey`, re-hashed once | converted |
| 6 | Checked, not rewritten: settings `stl_naming_defaults`, `build_planning.v1.{build}`, `build_planning.create.{key}` (a Build id as text, read by `ps/build_graph/create_with_brief.rs`; its key ends in free text, so the report counts these keys and does not print them); `plan_decisions.params_json`, `plan_decisions.result_json`; `plan_draft_required_unit_reconciliations.selection_basis_json`, `plan_draft_required_unit_reconciliations.result_json`; files `.printpartner-source-snapshot.json` under `repos/` | already typed, or stored without reading | must pass today's typed reader | checked |

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
