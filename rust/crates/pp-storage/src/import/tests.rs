use super::*;
use inventory::Status;
use pp_testkit::{Fixture, Installation, node_installation as synthetic, node_schema};
use std::time::SystemTime;

fn node_folder() -> Installation {
    let installation = Installation::create(Fixture::NodeSchema34).unwrap();
    synthetic::populate(installation.path()).unwrap();
    installation
}

/// Every entry under `root` with its size and modification time.
fn listing(root: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let (path, metadata) = (
                entry.as_ref().unwrap().path(),
                entry.unwrap().metadata().unwrap(),
            );
            if metadata.is_dir() {
                pending.push(path.clone());
            }
            entries.push((path, metadata.len(), metadata.modified().unwrap()));
        }
    }
    entries.sort();
    entries
}

/// Runs the dry run with its own temporary directory, and checks the run
/// left the source's listing, sizes, times and bytes alone and removed its
/// temporary copy.
fn run(source: &Path) -> Result<DryRunReport, ImportError> {
    let temporary = tempfile::tempdir().unwrap();
    let before = source
        .is_dir()
        .then(|| (listing(source), pp_testkit::snapshot(source).unwrap()));
    let result = dry_run_in(source, temporary.path());
    let after = source
        .is_dir()
        .then(|| (listing(source), pp_testkit::snapshot(source).unwrap()));
    assert!(before == after, "the dry run changed its source");
    assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 0);
    result
}

fn execute(folder: &Installation, sql: &str) {
    Connection::open(folder.database_path())
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

/// A backup archive of `folder` as Node writes it: the database, the
/// metadata and, for a full backup, the roots.
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
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        file.reopen().unwrap(),
        flate2::Compression::default(),
    ));
    let metadata = serde_json::to_vec(&metadata).unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_size(metadata.len() as u64);
    header.set_mode(0o644);
    builder
        .append_data(&mut header, archive::METADATA_FILE, &metadata[..])
        .unwrap();
    for root in [archive::DATABASE_FILE].into_iter().chain(roots) {
        let path = folder.join(root);
        if path.is_dir() {
            builder.append_dir_all(root, path).unwrap();
        } else if path.is_file() {
            builder.append_path_with_name(path, root).unwrap();
        }
    }
    builder.into_inner().unwrap().finish().unwrap();
    file
}

#[test]
fn a_node_folder_reports_its_counts_and_is_left_unchanged() {
    let folder = node_folder();
    let report = run(folder.path()).unwrap();
    assert_eq!(report.source, SourceKind::Directory);
    assert_eq!(
        (report.schema_version, report.upgraded_schema),
        (34, versions::RUST_SCHEMA)
    );
    assert_eq!(
        (
            report.builds,
            report.sources,
            report.printers,
            report.unmatched_prints
        ),
        (
            synthetic::BUILDS,
            synthetic::SOURCES,
            synthetic::PRINTERS,
            synthetic::UNMATCHED_PRINTS
        )
    );
    let builds = report
        .tables
        .iter()
        .find(|table| table.table == "build_profiles")
        .unwrap();
    assert_eq!(
        builds.by_tenant,
        BTreeMap::from([("default".to_owned(), synthetic::BUILDS)])
    );

    assert_eq!(report.rows.len(), inventory::ROWS.len());
    let items: BTreeMap<String, u64> = (report.rows.iter())
        .inspect(|(row, _)| assert_eq!(row.status, Status::Pending))
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
    assert_eq!(
        report.scalar_settings,
        BTreeSet::from([
            "build_planning.create.synthetic-key-#".to_owned(),
            "date_format".to_owned()
        ])
    );
    assert!(report.unnamed.is_empty(), "{:?}", report.unnamed);

    let text = report.to_string();
    assert!(
        text.contains("3 Builds, 2 Sources, 2 printers, 2 unmatched prints."),
        "{text}"
    );
    assert!(
        text.contains("step 3 pending: setting printer.fleet = 2"),
        "{text}"
    );
    assert!(
        text.contains("Stored JSON the inventory does not name:\n  none"),
        "{text}"
    );
    assert!(!text.contains(synthetic::INTEGRATION_SECRET) && !text.contains("Synthetic Printer"));
}

#[test]
fn a_full_archive_gives_the_same_counts() {
    let folder = node_folder();
    let from_folder = run(folder.path()).unwrap();
    let archive = archive_of(folder.path(), 2, "full");
    let from_archive = run(archive.path()).unwrap();
    assert_eq!(from_archive.source, SourceKind::Archive);
    assert_eq!(
        DryRunReport {
            source: SourceKind::Directory,
            ..from_archive
        },
        from_folder
    );
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

/// The refusal of a synthetic folder after `change`, whose text holds `reason`.
fn refused(change: impl FnOnce(&Installation), reason: &str) -> ImportError {
    let folder = node_folder();
    change(&folder);
    let error = run(folder.path()).unwrap_err();
    assert!(error.to_string().contains(reason), "{reason}: {error}");
    error
}

#[test]
fn a_folder_that_cannot_be_imported_is_refused_unchanged() {
    let stamp = |version| {
        move |folder: &Installation| {
            node_schema::restamp_schema_version(&folder.database_path(), version).unwrap()
        }
    };
    let error = refused(stamp(30), "older than schema 31");
    assert!(matches!(error, ImportError::Schema(refusal) if refusal.found == 30));
    let error = refused(stamp(35), "newer than Node's schema 34");
    assert!(matches!(error, ImportError::Schema(refusal) if refusal.found == 35));
    let mark = |folder: &Installation| folder.write_marker(&pp_testkit::Marker::default()).unwrap();
    assert!(matches!(
        refused(mark, "Rust installation"),
        ImportError::RustOwned
    ));
    let lease = |folder: &Installation| std::fs::create_dir(folder.path().join(LEASE)).unwrap();
    assert!(matches!(refused(lease, LEASE), ImportError::Leased));
    let damage = |folder: &Installation| {
        node_schema::break_check_constraint(&folder.database_path()).unwrap()
    };
    assert!(matches!(
        refused(damage, "integrity_check"),
        ImportError::Database(_)
    ));
    let split = |folder: &Installation| {
        execute(
            folder,
            "INSERT INTO build_profiles (tenant_id, name) VALUES ('other', 'Synthetic Build 04')",
        )
    };
    let error = refused(split, "more than one tenant (default, other)");
    assert!(matches!(error, ImportError::SplitTenants(_)));
    let empty = |folder: &Installation| std::fs::remove_file(folder.database_path()).unwrap();
    assert!(matches!(
        refused(empty, "has no print-partner.db"),
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
fn the_documented_inventory_names_every_shape() {
    let page = include_str!("../../../../../docs/node-import.md");
    for shape in inventory::named() {
        let name = match shape {
            Shape::Setting(name)
            | Shape::SettingPrefix(name)
            | Shape::File(name)
            | Shape::SourceFile(name) => (*name).to_owned(),
            Shape::Column(table, column) => format!("{table}.{column}"),
            Shape::Table(table) => (*table).to_owned(),
        };
        assert!(
            page.contains(&format!("`{name}")),
            "docs/node-import.md does not name {shape}"
        );
    }
}
