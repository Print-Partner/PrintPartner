use super::*;
use rusqlite::{Connection, OpenFlags};

fn table_present(connection: &Connection, name: &str) -> bool {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .is_ok()
}

fn assert_only_lease_leftovers(installation: &Installation) {
    for name in installation.entries().unwrap() {
        assert!(allowed_in_empty(&name), "{name} is not a lease leftover");
    }
    let lease = installation.path().join(".desktop-lease");
    if lease.is_dir() {
        for entry in fs::read_dir(lease).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            assert!(allowed_in_node_lease(&name), "{name} inside .desktop-lease");
        }
    }
}

#[test]
fn empty_has_no_entries() {
    let installation = Installation::create(Fixture::Empty).unwrap();
    assert!(installation.entries().unwrap().is_empty());
}

#[test]
fn lease_leftovers_cover_every_allowed_name_and_nothing_else() {
    let names: BTreeSet<String> = [
        "printpartner.lock",
        ".desktop-lease",
        ".desktop-owner.json",
        "lost+found",
        ".desktop-lease-candidate-cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        ".desktop-lease-release-cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        ".desktop-lease-retired-cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        ".desktop-owner-release-abababababababababababababababab.json",
    ]
    .map(str::to_owned)
    .into();
    for (form, lease_is_dir) in [
        (LeaseForm::NodeDirectory, true),
        (LeaseForm::RustFile, false),
    ] {
        let installation = Installation::create(Spec {
            lease_leftovers: Some(form),
            ..Spec::default()
        })
        .unwrap();
        assert_eq!(installation.entries().unwrap(), names);
        assert_only_lease_leftovers(&installation);
        let lease = installation.path().join(".desktop-lease");
        assert_eq!(lease.is_dir(), lease_is_dir);
        if lease_is_dir {
            assert!(lease.join("owner.json").is_file());
            assert!(lease.join("previous-marker.json").is_file());
        } else {
            assert_eq!(fs::metadata(&lease).unwrap().len(), 0);
        }
        assert!(!installation.marker_path().exists());
    }
    assert_eq!(
        Fixture::EmptyWithLeaseLeftovers.spec().lease_leftovers,
        Some(LeaseForm::NodeDirectory)
    );
}

#[test]
fn empty_name_rules_refuse_everything_else() {
    for name in [
        MARKER_FILE,
        DATABASE_FILE,
        "print-partner.db-wal",
        "repos",
        ".desktop-lease-candidate-",
        ".desktop-owner-release-.json",
        ".desktop-owner-release-x",
        ".desktop.lock",
        "printpartner.lock.bak",
    ] {
        assert!(!allowed_in_empty(name), "{name}");
    }
    assert!(!allowed_in_node_lease("stray"));
}

#[test]
fn node_schema_34_is_a_real_node_database_without_a_marker() {
    let installation = Installation::create(Fixture::NodeSchema34).unwrap();
    assert_eq!(
        installation.entries().unwrap(),
        BTreeSet::from([DATABASE_FILE.to_owned()])
    );
    let connection = Connection::open_with_flags(
        installation.database_path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let version: String = connection
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "34");
    let journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal, "wal");
    for table in [
        "projects",
        "build_profiles",
        "source_revisions",
        "board_posts",
    ] {
        assert!(table_present(&connection, table), "{table}");
    }
    let check: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    assert!(!installation.marker_path().exists());
}

#[test]
fn schema_34_replay_matches_the_pinned_statement_count() {
    assert_eq!(node_schema::schema_34_statements().unwrap().len(), 180);
}

#[test]
fn schema_34_database_refuses_an_existing_path() {
    let installation = Installation::create(Fixture::NodeSchema34).unwrap();
    let error = node_schema::write_schema_34_database(&installation.database_path()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
}

/// Creates `fixture`, checks the marker is its only entry, and returns its bytes.
fn marker_only(fixture: Fixture) -> (Installation, Vec<u8>) {
    let installation = Installation::create(fixture).unwrap();
    assert_eq!(
        installation.entries().unwrap(),
        BTreeSet::from([MARKER_FILE.to_owned()])
    );
    let bytes = fs::read(installation.marker_path()).unwrap();
    (installation, bytes)
}

#[test]
fn rust_marked_holds_only_the_documented_marker() {
    let (installation, bytes) = marker_only(Fixture::RustMarked);
    assert!(installation.marker_path().is_file());
    assert_eq!(
        bytes,
        b"{\"version\":1,\"owner\":\"rust\",\"created_at\":\"2026-11-03T00:00:00Z\"}\n"
    );
    assert_eq!(Marker::parse(&bytes).unwrap(), Marker::default());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(installation.marker_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn marked_corrupt_holds_a_truncated_marker() {
    let (_installation, bytes) = marker_only(Fixture::MarkedCorrupt);
    assert_eq!(Marker::parse(&bytes), Err(MarkerError::NotJson));
}

#[test]
fn every_corruption_is_refused_by_the_oracle() {
    let expected = [
        (Corruption::Empty, MarkerError::NotJson),
        (Corruption::Truncated, MarkerError::NotJson),
        (Corruption::NotJson, MarkerError::NotJson),
        (Corruption::WrongOwner, MarkerError::WrongOwner),
        (
            Corruption::UnsupportedVersion,
            MarkerError::UnsupportedVersion,
        ),
        (
            Corruption::MissingField,
            MarkerError::MissingField("created_at"),
        ),
    ];
    assert_eq!(expected.len() + 1, Corruption::ALL.len());
    let installation = Installation::create(Fixture::RustMarked).unwrap();
    for (corruption, error) in expected {
        installation.write_corrupt_marker(corruption).unwrap();
        let bytes = fs::read(installation.marker_path()).unwrap();
        assert_eq!(Marker::parse(&bytes), Err(error), "{corruption:?}");
    }
    installation
        .write_corrupt_marker(Corruption::Directory)
        .unwrap();
    assert!(installation.marker_path().is_dir());
    installation.write_marker(&Marker::default()).unwrap();
    assert!(installation.marker_path().is_file());
    installation.remove_marker().unwrap();
    assert!(installation.entries().unwrap().is_empty());
}

#[test]
fn oracle_refuses_extra_fields_and_bad_timestamps() {
    assert_eq!(
        Marker::parse(br#"{"version":1,"owner":"rust","created_at":"2026-11-03T00:00:00Z","x":1}"#),
        Err(MarkerError::UnknownField("x".into()))
    );
    assert_eq!(Marker::parse(b"[]"), Err(MarkerError::NotObject));
    for created_at in [
        "2026-11-03",
        "2026-11-03T00:00:00+00:00",
        "2026-11-03T00:00:00.000Z",
    ] {
        assert_eq!(
            Marker::parse(&Marker::new(created_at).to_bytes()),
            Err(MarkerError::BadCreatedAt),
            "{created_at}"
        );
    }
}

#[test]
fn every_fixture_is_disposable_and_isolated() {
    let parent = tempfile::tempdir().unwrap();
    for fixture in Fixture::ALL {
        let installation = Installation::create_in(parent.path(), fixture).unwrap();
        let path = installation.path().to_owned();
        assert_eq!(path.parent().unwrap(), parent.path());
        assert_eq!(installation.spec(), fixture.spec());
        assert_eq!(
            installation.marker_path().exists(),
            fixture.spec().marker != MarkerState::Absent,
            "{fixture:?}"
        );
        drop(installation);
        assert!(!path.exists(), "{fixture:?} was not removed");
    }
    assert_eq!(fs::read_dir(parent.path()).unwrap().count(), 0);
    let default = Installation::create(Fixture::Empty).unwrap();
    assert!(default.path().starts_with(std::env::temp_dir()));
}

#[test]
fn lease_leftovers_do_not_share_the_live_lease_instance() {
    // Node's stale-lease recovery renames the dead lease to
    // .desktop-lease-retired-<instance>; a leftover with that same name would
    // make recovery collide instead of succeed.
    let installation = Installation::create(Fixture::EmptyWithLeaseLeftovers).expect("fixture");
    let owner = std::fs::read_to_string(installation.path().join(".desktop-lease/owner.json"))
        .expect("owner record");
    let live = owner
        .split("\"instance\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("instance");
    assert!(
        !installation
            .path()
            .join(format!(".desktop-lease-retired-{live}"))
            .exists()
    );
}
