//! Stored schemas outside the supported range stop the open with a typed,
//! terminal refusal, and the refused database stays byte for byte unchanged.

use pp_storage::{
    Limits, WriterOwner, ownership_marker,
    versions::{self, IncompatibleSchema, NODE_SCHEMA, OLDEST_UPGRADABLE, RUST_SCHEMA},
};
use pp_testkit::{Fixture, Installation};
use std::path::Path;

/// A copied Node schema 34 installation, restamped to `version`.
fn installation_at(version: u64) -> Installation {
    let installation = Installation::create(Fixture::NodeSchema34).unwrap();
    pp_testkit::node_schema::restamp_schema_version(&installation.database_path(), version)
        .unwrap();
    installation
}

/// The bytes of every SQLite file of the database.
fn database_files(root: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .map(|suffix| {
            let name = format!("print-partner.db{suffix}");
            let bytes = std::fs::read(root.join(&name)).ok();
            (name, bytes)
        })
        .collect()
}

fn refusal(error: &anyhow::Error) -> IncompatibleSchema {
    *error
        .downcast_ref::<IncompatibleSchema>()
        .unwrap_or_else(|| panic!("not an incompatible-storage refusal: {error:#}"))
}

#[test]
fn writer_refuses_unsupported_schemas_without_changing_the_database() {
    for found in [1, OLDEST_UPGRADABLE - 1, RUST_SCHEMA + 1, 999] {
        let installation = installation_at(found);
        let before = database_files(installation.path());
        let error = WriterOwner::open(installation.path(), Limits::default())
            .err()
            .unwrap();
        let refused = refusal(&error);
        assert_eq!(
            refused,
            IncompatibleSchema {
                found,
                oldest: OLDEST_UPGRADABLE,
                newest: RUST_SCHEMA
            }
        );
        println!("schema {found}: {error}");
        assert_eq!(error.to_string(), refused.to_string());
        assert_eq!(
            database_files(installation.path()),
            before,
            "schema {found}"
        );
    }
}

#[test]
fn writer_refusal_text_names_version_range_and_recovery() {
    let older = refusal(
        &WriterOwner::open(installation_at(30).path(), Limits::default())
            .err()
            .unwrap(),
    );
    assert_eq!(
        older.to_string(),
        format!(
            "Incompatible storage: cannot upgrade database schema version 30 (this release opens schema 31 to \
             {RUST_SCHEMA}). Install Print Partner v3.3.0, start it once so it upgrades the database, then install \
             this release. The database was not changed."
        )
    );
    let newer = refusal(
        &WriterOwner::open(installation_at(RUST_SCHEMA + 1).path(), Limits::default())
            .err()
            .unwrap(),
    );
    assert_eq!(
        newer.to_string(),
        format!(
            "Incompatible storage: database schema version {} is newer than supported version {RUST_SCHEMA} \
             (this release opens schema 31 to {RUST_SCHEMA}). Install the Print Partner release that last opened \
             this data folder, or restore a backup made by this release. The database was not changed.",
            RUST_SCHEMA + 1
        )
    );
}

/// The Node-backed runtime checks its lease before starting the Node child:
/// Node opens 31 to 34, so a Rust-upgraded, newer or older database is refused
/// there, even when a later Rust validation fails, and the refusal leaves the
/// database unchanged.
#[test]
fn node_lease_refuses_schemas_node_cannot_open() {
    let node = Installation::create(Fixture::NodeSchema34).unwrap();
    let lease = ownership_marker::lease_unmarked(node.path()).unwrap();
    assert_eq!(versions::node_refusal(&lease).unwrap(), None);
    drop(lease);

    let upgraded = Installation::create(Fixture::NodeSchema34).unwrap();
    let (writer, ready) = WriterOwner::open(upgraded.path(), Limits::default()).unwrap();
    assert_eq!(ready.version, RUST_SCHEMA);
    writer.shutdown().unwrap();

    // A schema 34 shape stamped 35 fails the Rust durable-job check after
    // the version read. The writer keeps that message; the Node check still
    // refuses version 35 instead of handing the database to Node.
    let mislabeled = installation_at(35);
    let error = WriterOwner::open(mislabeled.path(), Limits::default())
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "Durable job schema mismatch");

    let cases = [
        (installation_at(30), 30),
        (mislabeled, 35),
        (upgraded, RUST_SCHEMA),
        (installation_at(999), 999),
        (installation_at(u64::MAX), u64::MAX),
    ];
    for (installation, found) in cases {
        let before = database_files(installation.path());
        let lease = ownership_marker::lease_unmarked(installation.path()).unwrap();
        assert_eq!(
            versions::node_refusal(&lease).unwrap(),
            Some(IncompatibleSchema {
                found,
                oldest: OLDEST_UPGRADABLE,
                newest: NODE_SCHEMA
            })
        );
        drop(lease);
        assert_eq!(
            database_files(installation.path()),
            before,
            "schema {found}"
        );
    }
}

#[test]
fn node_schema_matches_the_node_source() {
    let schema =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../web/apps/server/src/db/schema.ts");
    let source = std::fs::read_to_string(schema).unwrap();
    assert!(source.contains(&format!(
        "export const currentSchemaVersion = {NODE_SCHEMA};"
    )));
    let guard = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../web/apps/server/src/db/upgrade-guard.ts");
    let source = std::fs::read_to_string(guard).unwrap();
    assert!(source.contains(&format!(
        "export const minimumUpgradeSchemaVersion = {OLDEST_UPGRADABLE};"
    )));
}
