use super::*;
use crate::import::copy::{confirm_copy, copy_verified, ensure_room};
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
    execute(
        &source,
        "INSERT INTO users (id, display_name, created_at) VALUES ('synthetic-a', 'Synthetic A', '2026-10-02');
         INSERT INTO sessions (id, user_id, expires_at) VALUES ('synthetic-session', 'synthetic-a', '2099-01-01');",
    );
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
        refusal.starts_with(
            "3 Builds and 2 Sources are stored under the installation value \"solo\"."
        ),
        "{refusal}"
    );
    assert!(
        refusal.contains("Send the dry-run report back"),
        "{refusal}"
    );

    let accounts = node_folder();
    execute(
        &accounts,
        "INSERT INTO users (id, display_name, created_at) VALUES
           ('synthetic-a', 'Synthetic A', '2026-10-02'), ('synthetic-b', 'Synthetic B', '2026-10-02');",
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
fn a_changed_accepted_row_is_named() {
    let folder = node_folder();
    execute(
        &folder,
        "PRAGMA foreign_keys = OFF;
         INSERT INTO print_progress (part_id, unit_index, completed) VALUES ('synthetic-part', 0, 1)",
    );
    let connection = Connection::open(folder.database_path()).unwrap();
    let stored = verify::digests(&connection, None).unwrap();
    assert!(
        verify::changed(
            &stored,
            &verify::digests(&connection, Some(&stored)).unwrap()
        )
        .is_empty()
    );
    // A column the upgrade adds does not count; a changed value does.
    connection
        .execute_batch("ALTER TABLE print_progress ADD COLUMN synthetic_added TEXT DEFAULT 'x'")
        .unwrap();
    assert!(
        verify::changed(
            &stored,
            &verify::digests(&connection, Some(&stored)).unwrap()
        )
        .is_empty()
    );
    connection
        .execute_batch("UPDATE print_progress SET completed = 0")
        .unwrap();
    let changed = verify::changed(
        &stored,
        &verify::digests(&connection, Some(&stored)).unwrap(),
    );
    assert_eq!(changed, ["print_progress"]);
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
