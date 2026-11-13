use super::*;
use inventory::Status;
use pp_testkit::{Fixture, Installation, node_installation as synthetic, node_schema};
use std::time::SystemTime;

mod copy;

fn node_folder() -> Installation {
    let installation = Installation::create(Fixture::NodeSchema34).unwrap();
    synthetic::populate(installation.path()).unwrap();
    installation
}

/// The same synthetic installation as Print Partner v3.3.0 stored it, at
/// schema 33.
fn node_folder_33() -> Installation {
    let installation = Installation::create(Fixture::Empty).unwrap();
    node_schema::write_schema_33_database(&installation.database_path()).unwrap();
    synthetic::populate(installation.path()).unwrap();
    installation
}

/// The source and every entry under it, each with its size, modification
/// time and, for a file, its bytes.
fn state(source: &Path) -> Vec<(PathBuf, u64, SystemTime, Option<Vec<u8>>)> {
    let mut entries = Vec::new();
    let mut pending = vec![source.to_owned()];
    while let Some(path) = pending.pop() {
        let metadata = path.symlink_metadata().unwrap();
        if metadata.is_dir() {
            pending.extend(
                std::fs::read_dir(&path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        }
        let bytes = metadata.is_file().then(|| std::fs::read(&path).unwrap());
        entries.push((path, metadata.len(), metadata.modified().unwrap(), bytes));
    }
    entries.sort();
    entries
}

/// Runs the dry run with its own temporary directory, and checks that the
/// run changed nothing in the source and removed its temporary copy.
fn run(source: &Path) -> Result<DryRunReport, ImportError> {
    let temporary = tempfile::tempdir().unwrap();
    let before = state(source);
    let result = dry_run_in(source, None, temporary.path());
    assert!(before == state(source), "the dry run changed its source");
    assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 0);
    result
}

fn execute(folder: &Installation, sql: &str) {
    Connection::open(folder.database_path())
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

/// A backup archive of `folder` as Node writes it: the metadata, the
/// database and, for a full backup, the two roots the synthetic folder holds.
fn archive_of(folder: &Path, format: u64, scope: &str) -> tempfile::NamedTempFile {
    let roots: Vec<&str> = match scope {
        "full" => archive::FULL_ROOTS.iter().map(|(root, _)| *root).collect(),
        _ => Vec::new(),
    };
    let mut metadata = serde_json::json!({
        "version": format.to_string(), "createdAt": "2026-11-08T00:00:00.000Z",
        "appVersion": "synthetic", "formatVersion": format,
    });
    if format == 2 {
        metadata["scope"] = serde_json::json!({"kind": scope, "includedRoots": roots});
    }
    let metadata = serde_json::to_vec(&metadata).unwrap();
    let file = tempfile::NamedTempFile::new().unwrap();
    let gzip =
        flate2::write::GzEncoder::new(file.reopen().unwrap(), flate2::Compression::default());
    let mut builder = tar::Builder::new(gzip);
    let mut header = tar::Header::new_gnu();
    header.set_size(metadata.len() as u64);
    header.set_mode(0o644);
    builder
        .append_data(&mut header, archive::METADATA_FILE, &metadata[..])
        .unwrap();
    let database = archive::DATABASE_FILE;
    builder
        .append_path_with_name(folder.join(database), database)
        .unwrap();
    if scope == "full" {
        builder
            .append_dir_all("repos", folder.join("repos"))
            .unwrap();
        let filaments = "custom_filaments.json";
        builder
            .append_path_with_name(folder.join(filaments), filaments)
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
    file
}

/// The rows per tenant of `table` in `report`.
fn by_tenant(report: &DryRunReport, table: &str) -> Vec<(String, u64)> {
    let table = report.tables.iter().find(|count| count.table == table);
    table.unwrap().by_tenant.clone().into_iter().collect()
}

#[test]
fn a_node_folder_reports_its_counts_and_is_left_unchanged() {
    let folder = node_folder();
    let report = run(folder.path()).unwrap();
    assert_eq!(report.source, SourceKind::Directory);
    assert_eq!(report.schema_version, 34);
    assert_eq!(report.upgraded_schema, versions::RUST_SCHEMA);
    assert_eq!(
        (report.builds, report.sources),
        (synthetic::BUILDS, synthetic::SOURCES)
    );
    assert_eq!(report.printers, synthetic::PRINTERS);
    assert_eq!(report.unmatched_prints, synthetic::UNMATCHED_PRINTS);
    let default = |rows| vec![("default".to_owned(), rows)];
    assert_eq!(
        by_tenant(&report, "build_profiles"),
        default(synthetic::BUILDS)
    );

    assert_eq!(report.rows.len(), inventory::ROWS.len());
    let items: BTreeMap<String, u64> = (report.rows.iter())
        .inspect(|(row, _)| assert_eq!(row.status == Status::Pending, row.step > 2))
        .flat_map(|(_, shapes)| shapes)
        .map(|(shape, items)| (shape.to_string(), *items))
        .collect();
    for (shape, expected) in [
        ("projects.local_path", 2),
        ("setting printer.fleet", 2),
        ("setting integrations", 1),
        ("setting printer.unattributed_prints", 4),
        ("setting printer.send_queue", 0),
        ("projects.metadata_json", 1),
        ("projects.imported_paths", 1),
        ("file custom_filaments.json", 2),
        ("setting feature_toggles", 1),
        ("setting feature_toggles:plan:*", 1),
        ("setting production_setup:*", 1),
        ("setting kit_manifest_*", 1),
        ("file repos/**/.printpartner-source-snapshot.json", 1),
    ] {
        assert_eq!(items[shape], expected, "{shape}");
    }
    let scalars: Vec<&String> = report.scalar_settings.iter().collect();
    assert_eq!(scalars, ["date_format"]);
    // Settings no Rust code reads are named, so they are kept and not flagged.
    // A setting no Rust code reads is named, so it is kept and not flagged.
    let kept = (Shape::SettingPrefix("filename_grouping:"), 1);
    assert!(report.unused.contains(&kept), "{:?}", report.unused);
    assert_eq!(items["setting build_planning.create.*"], 1);
    assert_eq!(report.installations, BTreeSet::from(["default".to_owned()]));
    assert_eq!(report.users, 1);
    assert_eq!(report.destination, None);
    assert!(report.unnamed.is_empty(), "{:?}", report.unnamed);

    let text = report.to_string();
    let holds = |line: &str| assert!(text.contains(line), "{line} is not in:\n{text}");
    holds("3 Builds, 2 Sources, 2 printers, 2 unmatched prints.");
    holds("step 3 pending: setting printer.fleet = 2");
    holds("Stored JSON the inventory does not name:\n  none");
    holds("Installation value \"default\"; accounts stored: 1.");
    holds("Kept, not used by Rust:\n  setting filename_grouping:* = 1");
    // No stored value: not the integration's key, a printer name or a free-text key.
    let stored = [
        synthetic::INTEGRATION_SECRET,
        "Synthetic Printer",
        "synthetic-key",
    ];
    assert_eq!(stored.map(|value| text.contains(value)), [false; 3]);
}

#[test]
fn a_full_archive_gives_the_same_counts() {
    let folder = node_folder();
    let from_folder = run(folder.path()).unwrap();
    let archive = archive_of(folder.path(), 2, "full");
    let from_archive = run(archive.path()).unwrap();
    assert_eq!(from_archive.source, SourceKind::Archive);
    // Both count the same entries under each root. Only a folder has the
    // bytes: an archive is listed, not read.
    assert!(from_folder.roots.iter().all(|count| count.bytes.is_some()));
    let listed = |report: &DryRunReport| -> Vec<(&'static str, u64)> {
        (report.roots.iter().map(|count| (count.root, count.entries))).collect()
    };
    assert_eq!(listed(&from_archive), listed(&from_folder));
    // The synthetic `repos/` is itself, four folders and the snapshot's two
    // files; `custom_filaments.json` is one file.
    let expected = [("repos", 7), ("custom_filaments.json", 1)];
    assert_eq!(listed(&from_folder), expected);
    let text = from_folder.to_string();
    let lines = "\nBackup roots (files and folders, then bytes of the files):\n  repos 7 38\n  custom_filaments.json 1 52\n  all roots: 8 entries\n";
    assert!(text.contains(lines), "{text}");
    let lines = "\n  repos 7\n  custom_filaments.json 1\n  all roots: 8 entries\n";
    assert!(from_archive.to_string().contains(lines));
    // The size of each database file the source holds, from its own bytes.
    let database = std::fs::metadata(folder.database_path()).unwrap().len();
    assert_eq!(
        from_folder.database_files[0],
        ("print-partner.db", database)
    );
    assert_eq!(from_archive.database_files[0].0, "print-partner.db");
    let lines = format!("\nDatabase files (bytes):\n  print-partner.db {database}\n");
    assert!(from_folder.to_string().contains(&lines));
    let total: u64 = from_folder
        .database_files
        .iter()
        .map(|(_, bytes)| bytes)
        .sum();
    let line = format!("  all database files: {total} bytes\n");
    assert!(from_folder.to_string().contains(&line));
    // An archive names no installation folder, so it cannot show that a
    // Source without a current revision lies inside it.
    let inside = SourcePaths {
        rebased: synthetic::SOURCES,
        ..SourcePaths::default()
    };
    assert_eq!(from_folder.source_paths, inside);
    let outside = SourcePaths {
        outside: synthetic::SOURCES,
        ..SourcePaths::default()
    };
    assert_eq!(from_archive.source_paths, outside);
    let source = SourceKind::Directory;
    assert_eq!(
        DryRunReport {
            source,
            roots: from_folder.roots.clone(),
            database_files: from_folder.database_files.clone(),
            source_paths: from_folder.source_paths,
            ..from_archive
        },
        from_folder
    );
}

#[test]
fn an_account_without_builds_or_sources_is_reported_under_its_own_value() {
    // No Build and no Source: the values come from the other tables.
    let folder = Installation::create(Fixture::NodeSchema34).unwrap();
    let report = run(folder.path()).unwrap();
    assert_eq!((report.builds, report.sources), (0, 0));
    assert_eq!(report.installations, BTreeSet::from(["default".to_owned()]));
    assert!(!report.to_string().contains("reads only data under"));
    execute(
        &folder,
        "INSERT INTO users (id, display_name, created_at) VALUES ('synthetic-alice', 'Synthetic Alice', '2026-10-02');
         INSERT INTO app_settings (tenant_id, key, value) VALUES
           ('basic-alice', 'date_format', 'iso'), ('synthetic-account-7', 'date_format', 'iso');",
    );
    let report = run(folder.path()).unwrap();
    let values = ["basic-alice", "default", "synthetic-account-7"].map(str::to_owned);
    assert_eq!(report.installations, values.into());
    let text = report.to_string();
    for line in [
        "Installation values \"basic-***\", \"default\", \"***\"; accounts stored: 1.",
        "reads only data under \"default\", so the import would refuse this installation.",
        "  app_settings ",
    ] {
        assert!(text.contains(line), "{line} is not in:\n{text}");
    }
    assert!(
        !text.contains("alice") && !text.contains("account-7"),
        "{text}"
    );
}

#[test]
fn one_tenant_of_another_name_is_reported_and_a_fleet_that_is_no_array_has_no_printers() {
    let folder = node_folder();
    execute(
        &folder,
        "UPDATE build_profiles SET tenant_id = 'basic-solo'; UPDATE projects SET tenant_id = 'basic-solo';
         UPDATE app_settings SET value = '{}' WHERE key = 'printer.fleet';
         INSERT INTO users (id, display_name, created_at) VALUES ('synthetic-b', 'Synthetic B', '2026-10-02');",
    );
    let report = run(folder.path()).unwrap();
    assert_eq!((report.builds, report.printers), (synthetic::BUILDS, 0));
    // The value is counted as stored and masked when printed: it can hold a
    // sign-in name.
    let solo = vec![("basic-solo".to_owned(), synthetic::SOURCES)];
    assert_eq!(by_tenant(&report, "projects"), solo);
    let values = ["basic-solo", "default"].map(str::to_owned);
    assert_eq!((&report.installations, report.users), (&values.into(), 2));
    let text = report.to_string();
    assert!(!text.contains("solo"), "{text}");
    for line in [
        "Installation values \"basic-***\", \"default\"; accounts stored: 2.",
        "reads only data under \"default\", so the import would refuse this installation.",
        "signs in only when one account is stored, so the import would refuse this installation.",
    ] {
        assert!(text.contains(line), "{line} is not in:\n{text}");
    }
}

#[test]
fn a_destination_is_inspected_and_never_created() {
    let parent = tempfile::tempdir().unwrap();
    let absent = parent.path().join("absent");
    assert_eq!(inspect_destination(&absent), Destination::Free);
    assert!(!absent.exists(), "inspecting created the destination");
    let empty = Installation::create(Fixture::Empty).unwrap();
    assert_eq!(inspect_destination(empty.path()), Destination::Free);
    let marked = Installation::create(Fixture::RustMarked).unwrap();
    assert_eq!(inspect_destination(marked.path()), Destination::Marked);
    // A damaged marker is refused for what it is, not as a Rust installation.
    let damaged = Installation::create(Fixture::MarkedCorrupt).unwrap();
    let found = inspect_destination(damaged.path());
    assert!(matches!(found, Destination::BadMarker(_)), "{found:?}");
    assert!(found.to_string().contains("is no valid marker"), "{found}");
    // The writer refuses a folder with a Node lease directory, so the import does too.
    let leased = Installation::create(Fixture::EmptyWithLeaseLeftovers).unwrap();
    let lease = Destination::NotEmpty(LEASE.into());
    assert_eq!(inspect_destination(leased.path()), lease);
    let node = Installation::create(Fixture::NodeSchema34).unwrap();
    let before = state(node.path());
    let holds = Destination::NotEmpty(pp_testkit::DATABASE_FILE.into());
    assert_eq!(inspect_destination(node.path()), holds);
    assert!(
        before == state(node.path()),
        "inspecting changed the destination"
    );
    // A destination that cannot be read is a refusal with its reason, not a
    // failed run. A file is one that every platform has.
    let file = parent.path().join("file");
    std::fs::write(&file, "synthetic").unwrap();
    let found = inspect_destination(&file);
    assert!(matches!(found, Destination::Unreadable(_)), "{found:?}");
    assert!(found.to_string().contains("cannot be read ("), "{found}");
    let report = dry_run_in(node.path(), Some(&file), parent.path()).unwrap();
    assert_eq!(report.destination, Some(found));
}

#[test]
fn a_temporary_directory_inside_the_source_or_the_destination_is_refused() {
    let folder = node_folder();
    let inside = folder.path().join("synthetic-tmp");
    std::fs::create_dir(&inside).unwrap();
    let destination = tempfile::tempdir().unwrap();
    let before = state(folder.path());
    let refused = |target: Option<&Path>, temporary: &Path, named: &Path| {
        let error = dry_run_in(folder.path(), target, temporary).unwrap_err();
        let text = error.to_string();
        let ImportError::TemporaryInside { folder: found, .. } = error else {
            panic!(
                "{} was not refused as a temporary directory: {text}",
                temporary.display()
            );
        };
        assert_eq!(found, named);
        assert!(text.contains("Set TMPDIR"), "{text}");
        assert!(
            before == state(folder.path()),
            "the refusal wrote into the source"
        );
        assert_eq!(std::fs::read_dir(destination.path()).unwrap().count(), 0);
    };
    refused(None, folder.path(), folder.path());
    refused(None, &inside, folder.path());
    refused(
        Some(destination.path()),
        destination.path(),
        destination.path(),
    );
    // A link that leads into the source is resolved first.
    #[cfg(unix)]
    {
        let beside = tempfile::tempdir().unwrap();
        let link = beside.path().join("link");
        std::os::unix::fs::symlink(&inside, &link).unwrap();
        refused(None, &link, folder.path());
    }
    // A directory beside both is used.
    let beside = tempfile::tempdir().unwrap();
    let report = dry_run_in(folder.path(), Some(destination.path()), beside.path()).unwrap();
    assert_eq!(report.destination, Some(Destination::Free));
}

#[test]
fn staging_refuses_with_the_bytes_needed_and_free() {
    // The staged copy, plus the larger of the startup check's copy and the
    // upgrade's write-ahead log, plus the reserve.
    assert_eq!(stage_bytes(100, 1), 200 + STAGE_RESERVE);
    assert_eq!(stage_bytes(100, 0), 100 + STAGE_RESERVE);
    assert_eq!(stage_bytes(u64::MAX, 1), u64::MAX);
}

/// Linux only: the test needs a sparse file larger than any disk.
#[cfg(target_os = "linux")]
#[test]
fn a_database_that_cannot_fit_is_refused_before_it_is_copied() {
    let folder = node_folder();
    let database = std::fs::OpenOptions::new()
        .write(true)
        .open(folder.database_path())
        .unwrap();
    database.set_len(1 << 43).unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let error = dry_run_in(folder.path(), None, temporary.path()).unwrap_err();
    let staged = std::fs::read_dir(temporary.path()).unwrap().count();
    assert_eq!(staged, 0, "the refusal left its stage behind");
    let refusal = error.to_string();
    let ImportError::NoSpace { needed, free, .. } = error else {
        panic!("a sparse 8 TiB database was not refused for space: {refusal}");
    };
    assert!(needed > (1 << 44) && free < needed);
    assert!(
        refusal.contains(&format!("needs {needed} bytes")) && refusal.contains("set TMPDIR"),
        "{refusal}"
    );
}

#[test]
fn the_staged_upgrade_keeps_no_rollback_copies() {
    // The import's own upgrade of a staged folder writes no copy beside it.
    let (folder, stage) = (node_folder(), tempfile::tempdir().unwrap());
    stage_directory(folder.path(), stage.path()).unwrap();
    let (connection, ready, ()) = upgrade(stage.path(), |_| Ok(())).unwrap();
    drop(connection);
    assert_eq!(
        (ready.previous_version, ready.version),
        (34, versions::RUST_SCHEMA)
    );
    assert!(ready.backup.is_none() && !stage.path().join("backups").exists());
    // The writer's upgrade of the same database still keeps its copies.
    let now = crate::auth::catalog_timestamp();
    let (connection, ready) = schema::initialize(&folder.database_path(), 34, &now).unwrap();
    drop(connection);
    assert!(ready.backup.is_some() && folder.path().join("backups").is_dir());
}

#[test]
fn stored_json_outside_the_inventory_is_marked() {
    let folder = node_folder();
    execute(
        &folder,
        r#"INSERT INTO app_settings (tenant_id, key, value) VALUES
             ('default', 'synthetic_unknown:12', '{"a":1}'), ('default', 'synthetic_unknown:13', '[]');
           UPDATE build_profiles SET special_request = '["x"]' WHERE id = 1;
           UPDATE build_profiles SET special_request = 'not json' WHERE id = 2;"#,
    );
    let report = run(folder.path()).unwrap();
    let unnamed = |shape: &str, items| Unnamed {
        shape: shape.into(),
        items,
    };
    assert_eq!(
        report.unnamed,
        [
            unnamed("build_profiles.special_request", 1),
            unnamed("setting synthetic_unknown:#", 2)
        ]
    );
    assert!(
        report
            .to_string()
            .contains("  setting synthetic_unknown:# = 2\n")
    );
}

/// One folder takes each refusal in turn and is put back between them:
/// building the schema 34 database is the slow part of these tests.
#[test]
fn a_folder_that_cannot_be_imported_is_refused_unchanged() {
    let folder = node_folder();
    let refused = |reason: &str| {
        let error = run(folder.path()).unwrap_err();
        assert!(error.to_string().contains(reason), "{reason}: {error}");
        error
    };
    let stamp =
        |version| node_schema::restamp_schema_version(&folder.database_path(), version).unwrap();
    stamp(30);
    let error = refused("older than schema 31");
    assert!(matches!(error, ImportError::Schema(refusal) if refusal.found == 30));
    stamp(35);
    let error = refused("newer than Node's schema 34");
    assert!(matches!(error, ImportError::Schema(refusal) if refusal.found == 35));
    stamp(34);

    execute(
        &folder,
        "INSERT INTO build_profiles (tenant_id, name) VALUES ('other', 'Synthetic Build 04')",
    );
    let error = refused("more than one tenant (default, ***)");
    assert!(matches!(error, ImportError::SplitTenants(_)));
    // Two values that print alike are still two: masking happens only when
    // the message is written.
    execute(
        &folder,
        "UPDATE build_profiles SET tenant_id = 'basic-a' WHERE tenant_id = 'default';
         UPDATE build_profiles SET tenant_id = 'basic-b' WHERE tenant_id = 'other';
         UPDATE projects SET tenant_id = 'basic-a';",
    );
    let error = refused("more than one tenant (basic-***, basic-***)");
    assert!(
        matches!(&error, ImportError::SplitTenants(values) if values == &["basic-a", "basic-b"])
    );
    execute(
        &folder,
        "DELETE FROM build_profiles WHERE tenant_id = 'basic-b';
         UPDATE build_profiles SET tenant_id = 'default'; UPDATE projects SET tenant_id = 'default';",
    );

    std::fs::create_dir(folder.path().join(LEASE)).unwrap();
    assert!(matches!(refused(LEASE), ImportError::Leased));
    std::fs::remove_dir(folder.path().join(LEASE)).unwrap();

    folder.write_marker(&pp_testkit::Marker::default()).unwrap();
    assert!(matches!(
        refused("Rust installation"),
        ImportError::RustOwned
    ));
    folder
        .write_corrupt_marker(pp_testkit::Corruption::Truncated)
        .unwrap();
    assert!(matches!(
        refused("is no valid marker"),
        ImportError::SourceMarker(_)
    ));
    folder.remove_marker().unwrap();
    run(folder.path()).expect("the folder imports again once each cause is undone");

    node_schema::break_check_constraint(&folder.database_path()).unwrap();
    assert!(matches!(
        refused("integrity_check"),
        ImportError::Database(_)
    ));
    std::fs::remove_file(folder.database_path()).unwrap();
    assert!(matches!(
        refused("has no print-partner.db"),
        ImportError::NoDatabase
    ));
}

#[test]
fn only_a_full_format_2_archive_is_read() {
    let folder = node_folder();
    for (format, scope, reason) in [(2, "database-only", "database-only"), (1, "", "format 1")] {
        let error = run(archive_of(folder.path(), format, scope).path()).unwrap_err();
        assert!(matches!(error, ImportError::PartialArchive(_)), "{error:?}");
        assert!(error.to_string().ends_with(reason), "{error}");
    }
    let not_an_archive = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(not_an_archive.path(), "synthetic text").unwrap();
    let error = run(not_an_archive.path()).unwrap_err();
    assert!(matches!(error, ImportError::InvalidArchive(_)), "{error:?}");

    node_schema::restamp_schema_version(&folder.database_path(), 48).unwrap();
    let error = run(archive_of(folder.path(), 2, "full").path()).unwrap_err();
    assert!(
        matches!(error, ImportError::Schema(refusal) if refusal.found == 48),
        "{error:?}"
    );
}

#[test]
fn the_two_credentials_are_copied_unchanged_and_reported_by_key_only() {
    let folder = node_folder();
    execute(
        &folder,
        "INSERT INTO app_settings (tenant_id, key, value) VALUES
           ('default', 'github_pat', 'synthetic-token-value'),
           ('default', 'discord_notify_webhook_url', 'https://synthetic.invalid/hook-value');",
    );
    let report = run(folder.path()).unwrap();
    for key in ["github_pat", "discord_notify_webhook_url"] {
        assert!(report.copied.contains(&(Shape::Setting(key), 1)), "{key}");
        assert!(!report.scalar_settings.contains(key), "{key}");
    }
    assert!(report.unnamed.is_empty(), "{:?}", report.unnamed);
    let text = report.to_string();
    assert!(text.contains("  setting github_pat = 1"), "{text}");
    assert!(
        !text.contains("token-value") && !text.contains("hook-value"),
        "{text}"
    );
}

#[test]
fn a_destination_that_cannot_be_resolved_is_reported_and_the_dry_run_goes_on() {
    let folder = node_folder();
    let parent = tempfile::tempdir().unwrap();
    let file = parent.path().join("file");
    std::fs::write(&file, "synthetic").unwrap();
    let below = file.join("child");
    let temporary = tempfile::tempdir().unwrap();
    let report = dry_run_in(folder.path(), Some(&below), temporary.path()).unwrap();
    let found = report.destination.as_ref().unwrap();
    assert!(matches!(found, Destination::Unreadable(_)), "{found:?}");
    assert!(report.to_string().contains("cannot be read ("), "{report}");
}

#[test]
fn a_disk_or_filesystem_failure_in_the_upgrade_is_a_failed_run_not_a_refused_database() {
    use rusqlite::ffi;
    let database = Path::new("print-partner.db");
    let sorted = |cause: &(dyn std::error::Error + 'static)| {
        upgrade_failure(database, "synthetic detail".into(), [cause].into_iter())
    };
    let sqlite = |code| rusqlite::Error::SqliteFailure(ffi::Error::new(code), None);
    for code in [
        ffi::SQLITE_FULL,
        ffi::SQLITE_IOERR,
        ffi::SQLITE_READONLY,
        ffi::SQLITE_CANTOPEN,
    ] {
        let error = sorted(&sqlite(code));
        assert!(matches!(error, ImportError::Io { .. }), "{code}: {error:?}");
        assert_eq!(error.to_string(), "print-partner.db: synthetic detail");
    }
    let full = std::io::Error::from(ErrorKind::StorageFull);
    let error = sorted(&full);
    assert!(
        matches!(&error, ImportError::Io { source, .. } if source.kind() == ErrorKind::StorageFull),
        "{error:?}"
    );
    // A damaged or unexpected database is still a refusal.
    for code in [
        ffi::SQLITE_CORRUPT,
        ffi::SQLITE_CONSTRAINT,
        ffi::SQLITE_NOTADB,
    ] {
        let error = sorted(&sqlite(code));
        assert!(
            matches!(error, ImportError::Database(_)),
            "{code}: {error:?}"
        );
    }
    let none = upgrade_failure(
        database,
        "Invalid database schema version".into(),
        [].into_iter(),
    );
    assert_eq!(
        none.to_string(),
        "The database failed its checks: Invalid database schema version"
    );
}

#[test]
fn the_documented_inventory_names_every_shape() {
    let page = include_str!("../../../../../docs/node-import.md");
    for shape in inventory::named() {
        // A prefix is documented with its placeholder, as in `kit_manifest_{build}`.
        let name = match shape {
            Shape::Setting(name) | Shape::File(name) | Shape::SourceFile(name) => {
                format!("`{name}`")
            }
            Shape::SettingPrefix(prefix) => format!("`{prefix}{{"),
            Shape::Column(table, column) => format!("`{table}.{column}`"),
            Shape::Table(table) => format!("`{table}`"),
        };
        assert!(
            page.contains(&name),
            "docs/node-import.md does not name {shape}"
        );
    }
}
