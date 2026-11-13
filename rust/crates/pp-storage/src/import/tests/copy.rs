use super::*;
use crate::import::copy::{
    confirm_copy, copy_verified, damage, ensure_room, import_with, publish, sync_tree,
};
use crate::import::{REPORT_FILE, import};
use sha2::{Digest, Sha256};

fn setting(database: &Path, sql: &str) -> String {
    let connection = Connection::open(database).unwrap();
    let text = format!("SELECT CAST(({sql}) AS TEXT)");
    connection.query_row(&text, [], |row| row.get(0)).unwrap()
}

/// The entries of `folder`, sorted.
fn entries(folder: &Path) -> Vec<String> {
    let names = std::fs::read_dir(folder).unwrap();
    let mut names: Vec<String> =
        (names.map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())).collect();
    names.sort();
    names
}

/// A synthetic folder with one account and a live session, imported into a
/// new folder. Checks on the way that the import left its source alone.
fn imported() -> (Installation, tempfile::TempDir, ImportReport) {
    let source = node_folder();
    let before = state(source.path());
    let parent = tempfile::tempdir().unwrap();
    let report = import(source.path(), &parent.path().join("rust")).unwrap();
    assert!(
        before == state(source.path()),
        "the import changed its source"
    );
    (source, parent, report)
}

#[test]
fn a_folder_imports_into_a_new_rust_owned_folder() {
    let (source, parent, _) = imported();
    let destination = parent.path().join("rust");
    let marker = ownership_marker::MARKER_FILE;
    let names = [
        "custom_filaments.json",
        REPORT_FILE,
        "print-partner.db",
        "repos",
        marker,
    ];
    let mut expected = names.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(entries(&destination), expected);
    let marked = ownership_marker::inspect(&destination);
    assert!(
        matches!(marked, ownership_marker::MarkerState::Valid(_)),
        "{marked:?}"
    );

    let database = destination.join("print-partner.db");
    let stored = |sql: &str| setting(&database, sql);
    let version = stored("SELECT value FROM app_settings WHERE key = 'schema_version'");
    assert_eq!(version, versions::RUST_SCHEMA.to_string());
    assert_eq!(
        stored("SELECT COUNT(*) FROM build_profiles"),
        synthetic::BUILDS.to_string()
    );
    assert_eq!(
        stored("SELECT COUNT(*) FROM users"),
        "1",
        "accounts are carried"
    );
    assert_eq!(
        stored("SELECT COUNT(*) FROM sessions"),
        "0",
        "sessions are not carried"
    );
    let manifest = "repos/1/revisions/synthetic-a/.printpartner-source-snapshot.json";
    let copied = std::fs::read(destination.join(manifest)).unwrap();
    assert_eq!(copied, std::fs::read(source.path().join(manifest)).unwrap());

    // Every Source path now leads into the new folder, to files that are there.
    for source in 1..=synthetic::SOURCES {
        let path = stored(&format!(
            "SELECT local_path FROM projects WHERE id = {source}"
        ));
        assert!(
            Path::new(&path).starts_with(destination.join("repos")),
            "{path}"
        );
        assert!(
            Path::new(&path).join("synthetic-part.stl").is_file(),
            "{path}"
        );
    }

    // A second import refuses the folder it just wrote, and changes nothing in it.
    let written = state(&destination);
    let again = import(source.path(), &destination).unwrap_err();
    assert!(
        matches!(again, ImportError::Destination(Destination::Marked)),
        "{again:?}"
    );
    assert!(written == state(&destination));
}

#[test]
fn the_import_report_is_saved_without_stored_values() {
    let (_source, parent, report) = imported();
    assert!(!report.complete, "most converters do not exist yet");
    assert_eq!(
        (report.source_schema, report.schema),
        (34, versions::RUST_SCHEMA)
    );
    assert_eq!(
        (report.builds, report.sources),
        (synthetic::BUILDS, synthetic::SOURCES)
    );
    // The Source snapshot's two files and the custom filament list.
    assert_eq!(
        (report.files, report.rebased_sources),
        (3, synthetic::SOURCES)
    );
    assert_eq!(report.rows.len(), inventory::ROWS.len());
    let statuses: Vec<&str> = report.rows.iter().map(|row| row.status.as_str()).collect();
    assert_eq!(statuses[0], "converted");
    assert!(statuses[1..].iter().all(|status| *status == "pending"));
    let summary = report.to_string();
    assert!(
        summary.contains("complete: false. 15 inventory rows are not converted yet."),
        "{summary}"
    );

    let text = std::fs::read_to_string(parent.path().join("rust").join(REPORT_FILE)).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (&saved["complete"], &saved["builds"]),
        (&false.into(), &synthetic::BUILDS.into())
    );
    assert_eq!(
        saved["rows"][0]["shapes"][0]["shape"],
        "projects.local_path"
    );
    assert!(
        !text.contains(synthetic::INTEGRATION_SECRET),
        "the saved report holds a stored value"
    );
}

#[test]
fn a_full_archive_imports_the_same_data() {
    let source = node_folder();
    let archive = archive_of(source.path(), 2, "full");
    let parent = tempfile::tempdir().unwrap();
    let from_archive = import(archive.path(), &parent.path().join("a")).unwrap();
    let from_folder = import(source.path(), &parent.path().join("b")).unwrap();
    assert_eq!(from_archive, from_folder);
    assert_eq!(
        entries(&parent.path().join("a")),
        entries(&parent.path().join("b"))
    );
}

/// A folder is copied whatever its size: the caps on total bytes, entry
/// bytes and entry count apply to an archive only (owner decision,
/// 2026-11-12). Lowered caps stand in for an installation above them.
#[test]
fn a_folder_over_the_archive_caps_imports_and_its_archive_is_refused() {
    let source = node_folder();
    let database = std::fs::metadata(source.database_path()).unwrap().len();
    let parent = tempfile::tempdir().unwrap();
    let caps = [
        archive::Limits {
            max_total_bytes: database / 2,
            ..archive::Limits::default()
        },
        archive::Limits {
            max_entry_bytes: database / 2,
            ..archive::Limits::default()
        },
        archive::Limits {
            max_entries: 4,
            ..archive::Limits::default()
        },
    ];
    let archive = archive_of(source.path(), 2, "full");
    let uncapped = import(source.path(), &parent.path().join("uncapped")).unwrap();
    for (index, limits) in caps.into_iter().enumerate() {
        let refused = parent.path().join(format!("archive-{index}"));
        let flush = pp_os::sync_filesystem;
        let error =
            import_with(archive.path(), &refused, parent.path(), limits, flush).unwrap_err();
        assert!(matches!(error, ImportError::InvalidArchive(_)), "{error:?}");
        assert_eq!(entries(&refused), [] as [String; 0], "{error}");
        let destination = parent.path().join(format!("folder-{index}"));
        let report =
            import_with(source.path(), &destination, parent.path(), limits, flush).unwrap();
        assert_eq!(report, uncapped);
    }
}

/// The marker is written only after the staged files were flushed. A flush
/// that fails ends the import with its own error and leaves a folder the
/// next import accepts.
#[test]
fn a_failed_flush_leaves_no_marker_and_is_reported() {
    let source = node_folder();
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let limits = archive::Limits::default();
    let failing = |_: &Path| Err(std::io::Error::other("synthetic flush failure"));
    let error =
        import_with(source.path(), &destination, parent.path(), limits, failing).unwrap_err();
    let text = error.to_string();
    let ImportError::Flush { path, .. } = error else {
        panic!("a failed flush was reported as: {text}");
    };
    assert_eq!(path, destination);
    assert!(text.contains("synthetic flush failure"), "{text}");
    assert_eq!(entries(&destination), Vec::<String>::new());
    import(source.path(), &destination).unwrap();
    assert!(destination.join(ownership_marker::MARKER_FILE).is_file());

    // The walk that platforms without one flush call use reaches every
    // staged file and directory.
    sync_tree(&destination).unwrap();
    assert!(sync_tree(&destination.join("absent")).is_err());
}

#[test]
fn a_refused_import_leaves_no_marker_and_no_stage() {
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let refused = |source: &Installation| {
        let error = import(source.path(), &destination).unwrap_err();
        assert_eq!(entries(&destination), Vec::<String>::new(), "{error}");
        error
    };
    let unnamed = node_folder();
    execute(
        &unnamed,
        r#"INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'synthetic_unknown', '{"a":1}')"#,
    );
    let error = refused(&unnamed);
    assert!(
        matches!(&error, ImportError::Unnamed(shapes) if shapes == &["setting synthetic_unknown"]),
        "{error:?}"
    );

    // A value in a table other than Builds and Sources counts too.
    let setting = node_folder();
    execute(
        &setting,
        "INSERT INTO app_settings (tenant_id, key, value) VALUES ('solo', 'synthetic_scalar', '1')",
    );
    let error = refused(&setting);
    assert!(
        matches!(&error, ImportError::Installation { value, rows: 1 } if value == "solo"),
        "{error:?}"
    );

    let other = node_folder();
    execute(
        &other,
        "UPDATE build_profiles SET tenant_id = 'solo'; UPDATE projects SET tenant_id = 'solo';",
    );
    let error = refused(&other);
    let refusal = error.to_string();
    assert!(
        matches!(&error, ImportError::Installation { value, .. } if value == "solo"),
        "{error:?}"
    );
    assert!(
        refusal.starts_with("Rows stored under the installation value \"***\": 5."),
        "{refusal}"
    );
    assert!(
        refusal.contains("Send the dry-run report back"),
        "{refusal}"
    );

    let accounts = node_folder();
    execute(
        &accounts,
        "INSERT INTO users (id, display_name, created_at) VALUES ('synthetic-b', 'Synthetic B', '2026-10-02');",
    );
    let error = refused(&accounts);
    assert!(matches!(error, ImportError::Accounts(2)), "{error:?}");
    assert!(
        error
            .to_string()
            .contains("Remove the extra accounts in Node first")
    );

    // A stage left by an interrupted import makes the folder non-empty.
    std::fs::create_dir(destination.join(".pp-import")).unwrap();
    let error = import(node_folder().path(), &destination).unwrap_err();
    let leftover = Destination::NotEmpty(".pp-import".into());
    assert!(
        matches!(&error, ImportError::Destination(found) if found == &leftover),
        "{error:?}"
    );
    std::fs::remove_dir(destination.join(".pp-import")).unwrap();
    import(node_folder().path(), &destination).expect("the emptied folder imports");
}

#[test]
fn a_source_whose_files_cannot_be_placed_stops_the_import() {
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let revision = |locator: &str| {
        format!(
            "INSERT INTO source_revisions (project_id, upstream_revision_key, manifest_digest, snapshot_locator, synced_at)
               VALUES (1, 'synthetic-a', 'synthetic-digest', '{locator}', '2026-10-02T00:00:00.000Z');
             UPDATE projects SET current_source_revision_id = last_insert_rowid() WHERE id = 1;"
        )
    };
    let missing = node_folder();
    execute(&missing, &revision("1/revisions/synthetic-gone"));
    let error = import(missing.path(), &destination).unwrap_err();
    assert!(
        matches!(error, ImportError::MissingRevision(1)),
        "{error:?}"
    );
    assert_eq!(entries(&destination), Vec::<String>::new());

    let elsewhere = node_folder();
    execute(
        &elsewhere,
        "UPDATE projects SET local_path = '/synthetic/elsewhere' WHERE id = 2",
    );
    let error = import(elsewhere.path(), &destination).unwrap_err();
    assert!(matches!(error, ImportError::SourcePath(2)), "{error:?}");

    // A current revision decides the path, whatever Node stored beside it.
    let current = node_folder();
    execute(&current, &revision("1/revisions/synthetic-a"));
    execute(
        &current,
        "UPDATE projects SET local_path = 'C:\\node\\repos\\old' WHERE id = 1",
    );
    import(current.path(), &destination).unwrap();
    let stored = setting(
        &destination.join("print-partner.db"),
        "SELECT local_path FROM projects WHERE id = 1",
    );
    assert_eq!(
        Path::new(&stored),
        destination.join("repos/1/revisions/synthetic-a")
    );
}

#[test]
fn a_changed_row_in_any_table_is_named() {
    let folder = node_folder_33();
    let connection = Connection::open(folder.database_path()).unwrap();
    let stored = verify::digests(&connection, None).unwrap();
    let changed = || {
        let found = verify::digests(&connection, Some(&stored)).unwrap();
        verify::changed(&stored, &found)
    };
    assert!(changed().is_empty());
    // Every table of the source is compared, less the two the import empties.
    let tables = rows(folder.path());
    assert_eq!(stored.len(), tables.len() - 2);
    assert!(!stored.contains_key("sessions") && !stored.contains_key("password_reset_tokens"));
    let compared: usize = stored.values().map(|(_, rows)| rows.len()).sum();
    let held: i64 = (tables.iter())
        .filter(|(table, _)| stored.contains_key(*table))
        .map(|(_, count)| count)
        .sum();
    // All rows but the schema number, which the upgrade writes.
    assert_eq!(compared as i64, held - 1);

    // What the upgrade does on purpose changes nothing: a new column, a new
    // table, the schema number and a rebased Source path.
    connection
        .execute_batch(
            "ALTER TABLE source_notes ADD COLUMN synthetic_added TEXT DEFAULT 'x';
             CREATE TABLE synthetic_added (id INTEGER);
             INSERT INTO synthetic_added VALUES (1);
             UPDATE app_settings SET value = '48' WHERE key = 'schema_version';
             UPDATE projects SET local_path = '/synthetic/elsewhere';",
        )
        .unwrap();
    assert!(changed().is_empty());

    // A changed value, a missing row, an added row and a dropped column are
    // each named, in tables no list holds.
    connection
        .execute_batch(
            "UPDATE source_notes SET updated_at = '2026-10-03';
             DELETE FROM app_events;
             INSERT INTO source_docs (project_id, path, kind, updated_at) VALUES (1, 'ADDED.md', 'markdown', '2026-10-02');
             UPDATE app_settings SET value = 'synthetic' WHERE key = 'filename_grouping:1';
             ALTER TABLE plan_snapshots DROP COLUMN name;",
        )
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        changed(),
        [
            "app_events",
            "app_settings",
            "plan_snapshots",
            "source_docs",
            "source_notes"
        ]
    );
}

#[test]
fn starter_rows_may_be_added_and_stored_profiles_may_not_change() {
    let row = |byte: u8| [byte; 32];
    let table = |name: &str, rows: &[u8]| {
        let rows = rows.iter().copied().map(row).collect();
        (name.to_owned(), (vec!["name".to_owned()], rows))
    };
    let stored = verify::Digests::from([table("filament_profiles", &[2, 2, 5])]);
    let found = |rows: &[u8]| verify::Digests::from([table("filament_profiles", rows)]);
    assert!(verify::changed(&stored, &found(&[1, 2, 2, 3, 5, 9])).is_empty());
    for lost in [&[1, 2, 3, 5][..], &[2, 2], &[]] {
        assert_eq!(
            verify::changed(&stored, &found(lost)),
            ["filament_profiles"]
        );
    }
    // Any other table must hold exactly its stored rows.
    let stored = verify::Digests::from([table("plan_revisions", &[2, 5])]);
    let added = verify::Digests::from([table("plan_revisions", &[1, 2, 5])]);
    assert_eq!(verify::changed(&stored, &added), ["plan_revisions"]);
    assert_eq!(
        verify::changed(&stored, &verify::Digests::new()),
        ["plan_revisions"]
    );
}

#[test]
fn a_copy_that_reads_back_differently_stops_the_import() {
    let folder = tempfile::tempdir().unwrap();
    let (from, to) = (
        folder.path().join("source.stl"),
        folder.path().join("copy.stl"),
    );
    std::fs::write(&from, "solid synthetic").unwrap();
    assert_eq!(copy_verified(&from, &to).unwrap(), 15);
    let stored = Sha256::digest("solid synthetic");
    confirm_copy(&to, &stored).expect("an intact copy is confirmed");
    // One byte of the copy changes on disk after it was written.
    std::fs::write(&to, "solid synthetiC").unwrap();
    let error = confirm_copy(&to, &stored).unwrap_err();
    assert!(
        matches!(&error, ImportError::CopyMismatch(copy) if *copy == to),
        "{error:?}"
    );
    assert!(error.to_string().contains("does not match its source"));
}

#[test]
fn the_write_step_refuses_a_destination_without_room() {
    let folder = tempfile::tempdir().unwrap();
    ensure_room(folder.path(), 1).expect("one byte fits");
    let error = ensure_room(folder.path(), u64::MAX).unwrap_err();
    let ImportError::DestinationSpace { needed, available } = error else {
        panic!("not a free-space refusal: {error:?}");
    };
    assert!(needed == u64::MAX && available < needed);
    let refusal = ImportError::DestinationSpace { needed, available }.to_string();
    assert!(
        refusal.contains(&format!("needs {needed} bytes free")),
        "{refusal}"
    );
    assert!(
        refusal.contains(&format!("{available} are available")),
        "{refusal}"
    );
}

/// Rows per table of the database in `folder`.
fn rows(folder: &Path) -> BTreeMap<String, i64> {
    let connection = Connection::open(folder.join("print-partner.db")).unwrap();
    let tables = strings(
        &connection,
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        None,
    )
    .unwrap();
    let count = |table: &String| {
        connection
            .query_row(
                &format!("SELECT COUNT(*) FROM {}", quoted(table)),
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    tables
        .iter()
        .map(|table| (table.clone(), count(table)))
        .collect()
}

#[test]
fn a_schema_33_folder_imports_with_every_table_it_holds() {
    let source = node_folder_33();
    let stored = rows(source.path());
    for table in synthetic::LONG_LIVED_TABLES {
        assert!(stored[table] >= 1, "the fixture has no row in {table}");
    }
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let report = import(source.path(), &destination).unwrap();
    assert_eq!(
        (report.source_schema, report.schema),
        (33, versions::RUST_SCHEMA)
    );

    // Every table of the source arrives with its rows. The import empties
    // the sign-in state, and the upgrade may add starter rows, never drop any.
    let imported = rows(&destination);
    for (table, count) in &stored {
        let arrived = imported.get(table).copied().unwrap_or(-1);
        match table.as_str() {
            "sessions" | "password_reset_tokens" => assert_eq!(arrived, 0, "{table}"),
            _ => assert!(
                arrived >= *count,
                "{table}: {count} stored, {arrived} imported"
            ),
        }
        assert_eq!(
            report.tables[table],
            arrived.unsigned_abs(),
            "{table} in the report"
        );
    }
    // Verification covered every source table but the two emptied ones.
    assert_eq!(report.verified_tables as usize, stored.len() - 2);
    assert!(report.to_string().contains(&format!(
        "{} rows in {} tables arrived as the source stored them.",
        report.verified_rows, report.verified_tables
    )));
}

/// Every entry of `folder`, with the SHA-256 of each file: what a refused
/// import must leave exactly as it found it.
fn fingerprint(folder: &Path) -> BTreeMap<PathBuf, String> {
    let (mut found, mut pending) = (BTreeMap::new(), vec![folder.to_owned()]);
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let kind = path.symlink_metadata().unwrap().file_type();
            let value = if kind.is_dir() {
                pending.push(path.clone());
                "folder".to_owned()
            } else if kind.is_file() {
                hex::encode(Sha256::digest(std::fs::read(&path).unwrap()))
            } else {
                "other".to_owned()
            };
            found.insert(path, value);
        }
    }
    found
}

/// Imports into `destination`, which must be refused for where it is. The
/// source keeps every file byte for byte, gains no entry and no marker.
fn refused_for_its_place(source: &Installation, destination: &Path) -> Destination {
    let before = fingerprint(source.path());
    let error = import(source.path(), destination).unwrap_err();
    // The advice must never be to empty a folder that may be the source.
    let text = error.to_string();
    assert!(
        text.ends_with("Choose another folder") && !text.contains("Empty it"),
        "{text}"
    );
    assert!(
        before == fingerprint(source.path()),
        "the refused import changed its source: {error}"
    );
    assert!(!source.path().join(ownership_marker::MARKER_FILE).exists());
    assert!(!source.path().join(".pp-import").exists());
    match error {
        ImportError::Destination(found) => found,
        other => panic!("refused for another reason: {other}"),
    }
}

// The four tests below are the record of the path that let an import write
// into its own source: `--data <source>/new/..` read as absent, was created
// as `<source>/new`, and then named the source itself.

#[test]
fn a_destination_given_through_a_parent_step_is_refused() {
    let source = node_folder();
    let stepped = source.path().join("new").join("..");
    let found = refused_for_its_place(&source, &stepped);
    assert_eq!(found, Destination::Indirect);
    assert!(found.to_string().contains("\"..\" component"), "{found}");
    // A step through a folder that exists, and a `.`, are refused the same.
    let parent = tempfile::tempdir().unwrap();
    for stepped in ["a/../rust", "./rust", "rust/."] {
        let stepped = parent.path().join(stepped);
        assert_eq!(
            refused_for_its_place(&source, &stepped),
            Destination::Indirect
        );
    }
    assert_eq!(entries(parent.path()), Vec::<String>::new());
}

#[test]
fn the_source_as_its_own_destination_is_refused() {
    let source = node_folder();
    let found = refused_for_its_place(&source, source.path());
    assert_eq!(found, Destination::Overlaps(Overlap::Same));
}

#[test]
fn a_destination_inside_the_source_is_refused() {
    let source = node_folder();
    let inside = Destination::Overlaps(Overlap::DestinationInside);
    for child in ["child", "sources/child", "repos/a/b"] {
        let child = source.path().join(child);
        assert_eq!(refused_for_its_place(&source, &child), inside);
    }
    // The dry run says so too, and creates nothing.
    let temporary = tempfile::tempdir().unwrap();
    let child = source.path().join("child");
    let report = dry_run_in(source.path(), Some(&child), temporary.path()).unwrap();
    assert_eq!(report.destination, Some(inside));
    let text = report.to_string();
    assert!(text.contains("lies inside the source folder"), "{text}");
    assert!(!child.exists());
}

#[cfg(unix)]
#[test]
fn a_destination_linked_into_the_source_is_refused() {
    let source = node_folder();
    let beside = tempfile::tempdir().unwrap();
    let link = beside.path().join("link");
    std::os::unix::fs::symlink(source.path(), &link).unwrap();
    assert_eq!(
        refused_for_its_place(&source, &link),
        Destination::Overlaps(Overlap::Same)
    );
    let inside = Destination::Overlaps(Overlap::DestinationInside);
    assert_eq!(refused_for_its_place(&source, &link.join("child")), inside);
    let deep = beside.path().join("deep");
    std::os::unix::fs::symlink(source.path().join("repos"), &deep).unwrap();
    assert_eq!(refused_for_its_place(&source, &deep), inside);
}

#[test]
fn a_destination_that_holds_the_source_is_refused() {
    let around = tempfile::tempdir().unwrap();
    let source = around.path().join("node");
    std::fs::create_dir(&source).unwrap();
    let before = fingerprint(around.path());
    let error = import(&source, around.path()).unwrap_err();
    assert!(
        matches!(
            error,
            ImportError::Destination(Destination::Overlaps(Overlap::SourceInside))
        ),
        "{error}"
    );
    assert!(before == fingerprint(around.path()));
}

#[test]
fn the_import_keeps_the_startup_copy_out_of_the_source() {
    let source = node_folder();
    let inside = source.path().join("synthetic-tmp");
    std::fs::create_dir(&inside).unwrap();
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let before = fingerprint(source.path());
    let (limits, flush) = (archive::Limits::default(), pp_os::sync_filesystem);
    for temporary in [source.path(), inside.as_path()] {
        let error = import_with(source.path(), &destination, temporary, limits, flush).unwrap_err();
        assert!(
            matches!(error, ImportError::TemporaryInside { .. }),
            "{error}"
        );
        assert!(before == fingerprint(source.path()), "{error}");
        assert!(!destination.exists(), "the refusal created the destination");
    }
}

#[test]
fn a_changed_byte_in_a_database_copy_stops_the_import() {
    let source = node_folder();
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    damage::FILE.set(Some(pp_testkit::DATABASE_FILE));
    let error = import(source.path(), &destination).unwrap_err();
    damage::FILE.set(None);
    let text = error.to_string();
    let ImportError::CopyMismatch(copy) = error else {
        panic!("a damaged database copy was reported as: {text}");
    };
    assert!(copy.ends_with(Path::new(".pp-import").join(pp_testkit::DATABASE_FILE)));
    // No marker and no stage: the folder is as an import needs it.
    assert_eq!(entries(&destination), Vec::<String>::new());
    import(source.path(), &destination).unwrap();
}

#[cfg(unix)]
#[test]
fn an_entry_that_is_no_file_or_folder_stops_the_import() {
    let source = node_folder();
    let repos = source.path().join("repos");
    std::os::unix::fs::symlink("/synthetic/elsewhere", repos.join("synthetic-link.stl")).unwrap();
    std::os::unix::fs::symlink(&repos, source.path().join("thumbs")).unwrap();
    let link = |root| Unsupported {
        root,
        kind: EntryKind::Link,
        entries: 1,
    };
    let expected = vec![link("repos"), link("thumbs")];

    // The dry run counts them by root and kind, and leaves them out of the
    // files it counts.
    let report = run(source.path()).unwrap();
    assert_eq!(report.unsupported, expected);
    let text = report.to_string();
    let line = "Entries that are neither files nor folders: 1 symbolic links under repos, 1 symbolic links under thumbs.";
    assert!(text.contains(line), "{text}");
    assert!(!text.contains("synthetic-link"), "{text}");

    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let before = fingerprint(source.path());
    let error = import(source.path(), &destination).unwrap_err();
    let text = error.to_string();
    assert!(
        matches!(&error, ImportError::NotRegular(found) if found == &expected),
        "{text}"
    );
    assert!(!text.contains("synthetic-link"), "{text}");
    assert!(!destination.exists(), "the refusal created the destination");
    assert!(before == fingerprint(source.path()));
}

#[test]
fn publishing_never_replaces_what_the_destination_holds() {
    let (source, folder) = (node_folder(), tempfile::tempdir().unwrap());
    let report = import(source.path(), &folder.path().join("imported")).unwrap();
    let (stage, destination) = (folder.path().join("stage"), folder.path().join("rust"));
    for directory in [&stage, &destination] {
        std::fs::create_dir(directory).unwrap();
    }
    let staged = stage.join(pp_testkit::DATABASE_FILE);
    std::fs::write(&staged, "staged").unwrap();
    std::fs::create_dir(stage.join("repos")).unwrap();
    for name in [
        pp_testkit::DATABASE_FILE,
        "repos",
        ownership_marker::MARKER_FILE,
    ] {
        let held = destination.join(name);
        std::fs::write(&held, "held").unwrap();
        let error = publish(&stage, &destination, &report).unwrap_err();
        let holds = Destination::NotEmpty(name.to_owned());
        assert!(
            matches!(&error, ImportError::Destination(found) if found == &holds),
            "{error}"
        );
        // Nothing was moved in and nothing was replaced.
        assert_eq!(std::fs::read(&held).unwrap(), b"held");
        assert_eq!(std::fs::read(&staged).unwrap(), b"staged");
        assert_eq!(entries(&destination), [name]);
        std::fs::remove_file(&held).unwrap();
    }
    // The rename itself refuses a name that appears after the check.
    let taken = destination.join("repos");
    std::fs::write(&taken, "held").unwrap();
    let error = pp_os::rename_new(&stage.join("repos"), &taken).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&taken).unwrap(), b"held");
}
