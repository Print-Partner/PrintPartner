//! The pp-testkit fixtures behave like real installations under the Rust writer.

use pp_storage::{Limits, WriterOwner};
use pp_testkit::{Fixture, Installation, LeaseForm, Spec};
use rusqlite::Connection;

fn schema_version(installation: &Installation) -> String {
    Connection::open(installation.database_path())
        .unwrap()
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn node_schema_34_fixture_upgrades_under_the_rust_writer() {
    let installation = Installation::create(Fixture::NodeSchema34).unwrap();
    let (owner, ready) = WriterOwner::open(installation.path(), Limits::default()).unwrap();
    assert_eq!(ready.previous_version, 34);
    assert!(ready.version > 34);
    owner.shutdown().unwrap();
    assert_eq!(schema_version(&installation), ready.version.to_string());
}

#[test]
fn node_lease_directory_leftover_blocks_the_rust_writer() {
    let installation = Installation::create(Fixture::EmptyWithLeaseLeftovers).unwrap();
    let error = WriterOwner::open(installation.path(), Limits::default())
        .err()
        .expect("a Node lease directory must refuse Rust");
    assert!(
        format!("{error:#}").contains("standalone Node"),
        "{error:#}"
    );
    assert!(!installation.database_path().exists());
}

#[test]
fn rust_lease_file_leftover_is_reused_by_the_rust_writer() {
    let installation = Installation::create(Spec {
        lease_leftovers: Some(LeaseForm::RustFile),
        ..Spec::default()
    })
    .unwrap();
    let (owner, ready) = WriterOwner::open(installation.path(), Limits::default()).unwrap();
    assert_eq!(ready.previous_version, 0);
    owner.shutdown().unwrap();
    assert!(installation.database_path().is_file());
}
