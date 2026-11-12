use super::*;
use crate::import::copy::{confirm_copy, copy_verified};
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
    execute(
        &source,
        "INSERT INTO users (id, display_name, created_at) VALUES ('synthetic-a', 'Synthetic A', '2026-10-02');
         INSERT INTO sessions (id, user_id, expires_at) VALUES ('synthetic-session', 'synthetic-a', '2099-01-01');",
    );
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
    assert!(
        matches!(&error, ImportError::Installation(value) if value == "solo"),
        "{error:?}"
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
