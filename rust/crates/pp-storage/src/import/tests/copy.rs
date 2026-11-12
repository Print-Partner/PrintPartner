use super::*;
use crate::import::copy::{confirm_copy, copy_verified, ensure_room, import_with, sync_tree};
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

#[test]
fn a_folder_imports_into_a_new_rust_owned_folder() {
    let source = node_folder();
    let before = state(source.path());
    let parent = tempfile::tempdir().unwrap();
    let destination = parent.path().join("rust");
    let report = import(source.path(), &destination).unwrap();
    assert!(
        before == state(source.path()),
        "the import changed its source"
    );

    assert!(!report.complete, "no converter exists yet");
    assert_eq!(
        (report.source_schema, report.schema),
        (34, versions::RUST_SCHEMA)
    );
    assert_eq!(
        (report.builds, report.sources),
        (synthetic::BUILDS, synthetic::SOURCES)
    );
    // The Source snapshot's two files and the custom filament list.
    assert_eq!(report.files, 3);
    assert_eq!(report.rows.len(), inventory::ROWS.len());
    assert!(report.rows.iter().all(|row| row.status == "pending"));

    let names = [
        "custom_filaments.json",
        REPORT_FILE,
        "print-partner.db",
        "repos",
        ownership_marker::MARKER_FILE,
    ];
    let mut expected = names.map(str::to_owned).to_vec();
    expected.sort();
    assert_eq!(entries(&destination), expected);
    assert!(matches!(
        ownership_marker::inspect(&destination),
        ownership_marker::MarkerState::Valid(_)
    ));
    let database = destination.join("print-partner.db");
    let stored = |sql| setting(&database, sql);
    assert_eq!(
        stored("SELECT value FROM app_settings WHERE key = 'schema_version'"),
        versions::RUST_SCHEMA.to_string()
    );
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
    assert_eq!(
        std::fs::read(destination.join(manifest)).unwrap(),
        std::fs::read(source.path().join(manifest)).unwrap()
    );

    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(destination.join(REPORT_FILE)).unwrap()).unwrap();
    assert_eq!(saved["complete"], false);
    assert_eq!(saved["builds"], synthetic::BUILDS);
    assert_eq!(
        saved["rows"][0]["shapes"][0]["shape"],
        "projects.local_path"
    );
    let text = std::fs::read_to_string(destination.join(REPORT_FILE)).unwrap();
    assert!(
        !text.contains(synthetic::INTEGRATION_SECRET),
        "the saved report holds a stored value"
    );
    assert!(
        report
            .to_string()
            .contains("complete: false. 16 inventory rows are not converted yet.")
    );

    // A second import refuses the folder it just wrote, and changes nothing in it.
    let imported = state(&destination);
    let again = import(source.path(), &destination).unwrap_err();
    assert!(
        matches!(again, ImportError::Destination(Destination::Marked)),
        "{again:?}"
    );
    assert!(imported == state(&destination));
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
        let error = import_with(archive.path(), &refused, limits, flush).unwrap_err();
        assert!(matches!(error, ImportError::InvalidArchive(_)), "{error:?}");
        assert_eq!(entries(&refused), [] as [String; 0], "{error}");
        let destination = parent.path().join(format!("folder-{index}"));
        let report = import_with(source.path(), &destination, limits, flush).unwrap();
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
    let error = import_with(source.path(), &destination, limits, failing).unwrap_err();
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
}
