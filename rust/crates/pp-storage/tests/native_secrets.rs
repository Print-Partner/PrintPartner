#![cfg(feature = "desktop_rebuild")]

use pp_storage::{
    Limits, Setting, SettingCommand, WriterOwner,
    native_secrets::{MigrationPreparation, StorageFailure},
};
use rusqlite::Connection;
use std::os::unix::fs::DirBuilderExt;
use std::{
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

fn directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "pp-storage-native-secret-{name}-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)
        .unwrap();
    path
}

fn set_legacy(owner: &WriterOwner, value: &str) {
    owner
        .client()
        .submit(
            SettingCommand::Set(Setting {
                tenant: "default".into(),
                key: "github_pat".into(),
                value: value.into(),
            }),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
}

fn legacy(root: &Path) -> Option<String> {
    Connection::open(root.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='github_pat'",
            [],
            |row| row.get(0),
        )
        .ok()
}

fn assert_legacy_matches(root: &Path, expected: &str) {
    assert!(
        legacy(root).is_some_and(|value| value.as_bytes() == expected.as_bytes()),
        "legacy value did not match"
    );
}

#[test]
fn interrupted_prepared_state_reopens_with_the_same_locator_and_generation() {
    let root = directory("prepared-reopen");
    let sentinel = format!(
        "owned-test-token-{}",
        hex::encode(rand::random::<[u8; 16]>())
    );
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    set_legacy(&owner, &sentinel);
    let first = match owner
        .native_secret_migration()
        .prepare(&AtomicBool::new(false), Duration::from_secs(5))
        .unwrap()
    {
        MigrationPreparation::Prepared(prepared) => prepared,
        MigrationPreparation::Absent | MigrationPreparation::LegacyRowRemoved(_) => {
            panic!("expected prepared migration")
        }
    };
    let account = first.locator().account().to_owned();
    let generation = first.locator().generation();
    assert!(first.material().matches_bytes(sentinel.as_bytes()));
    drop(first);
    owner.shutdown().unwrap();

    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    let reopened = match owner
        .native_secret_migration()
        .prepare(&AtomicBool::new(false), Duration::from_secs(5))
        .unwrap()
    {
        MigrationPreparation::Prepared(prepared) => prepared,
        MigrationPreparation::Absent | MigrationPreparation::LegacyRowRemoved(_) => {
            panic!("expected prepared migration")
        }
    };
    assert_eq!(reopened.locator().account(), account);
    assert_eq!(reopened.locator().generation(), generation);
    assert!(reopened.material().matches_bytes(sentinel.as_bytes()));
    drop(reopened);
    owner.shutdown().unwrap();
}

#[test]
fn prepared_value_and_physical_owner_are_revalidated_before_removal() {
    let first_root = directory("first-owner");
    let second_root = directory("second-owner");
    let sentinel = format!(
        "owned-test-token-{}",
        hex::encode(rand::random::<[u8; 16]>())
    );
    let replacement = format!("replacement-{}", hex::encode(rand::random::<[u8; 16]>()));
    let (first_owner, _) = WriterOwner::open(&first_root, Limits::default()).unwrap();
    let (second_owner, _) = WriterOwner::open(&second_root, Limits::default()).unwrap();
    set_legacy(&first_owner, &sentinel);
    let prepared = match first_owner
        .native_secret_migration()
        .prepare(&AtomicBool::new(false), Duration::from_secs(5))
        .unwrap()
    {
        MigrationPreparation::Prepared(prepared) => prepared,
        MigrationPreparation::Absent | MigrationPreparation::LegacyRowRemoved(_) => {
            panic!("expected prepared migration")
        }
    };
    let error = second_owner
        .native_secret_migration()
        .confirm(prepared, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(error.failure(), StorageFailure::Superseded);
    assert_legacy_matches(&first_root, &sentinel);

    let prepared = match first_owner
        .native_secret_migration()
        .prepare(&AtomicBool::new(false), Duration::from_secs(5))
        .unwrap()
    {
        MigrationPreparation::Prepared(prepared) => prepared,
        MigrationPreparation::Absent | MigrationPreparation::LegacyRowRemoved(_) => {
            panic!("expected prepared migration")
        }
    };
    set_legacy(&first_owner, &replacement);
    let error = first_owner
        .native_secret_migration()
        .confirm(prepared, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(error.failure(), StorageFailure::LegacyChanged);
    assert_legacy_matches(&first_root, &replacement);
    first_owner.shutdown().unwrap();
    second_owner.shutdown().unwrap();
}

#[test]
fn superseded_generation_rejects_an_old_prepared_value() {
    let root = directory("superseded");
    let sentinel = format!(
        "owned-test-token-{}",
        hex::encode(rand::random::<[u8; 16]>())
    );
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    set_legacy(&owner, &sentinel);
    let client = owner.native_secret_migration();
    let prepared = match client
        .prepare(&AtomicBool::new(false), Duration::from_secs(5))
        .unwrap()
    {
        MigrationPreparation::Prepared(prepared) => prepared,
        MigrationPreparation::Absent | MigrationPreparation::LegacyRowRemoved(_) => {
            panic!("expected prepared migration")
        }
    };
    Connection::open(root.join("print-partner.db"))
        .unwrap()
        .execute(
            "UPDATE native_secret_migrations SET generation=2,migration_id=?1",
            ["b".repeat(64)],
        )
        .unwrap();
    let error = client
        .confirm(prepared, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(error.failure(), StorageFailure::Superseded);
    assert_legacy_matches(&root, &sentinel);
    owner.shutdown().unwrap();
}

#[test]
fn confirmation_removes_only_the_legacy_secret_and_survives_reopen() {
    let root = directory("confirmed-reopen");
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    let db = Connection::open(root.join("print-partner.db")).unwrap();
    let table_exists = || {
        db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='native_secret_migrations')",
            [],
            |row| row.get::<_, bool>(0),
        )
        .unwrap()
    };
    assert!(!table_exists());
    let client = owner.native_secret_migration();
    assert!(
        matches!(client.prepare(&AtomicBool::new(true), Duration::ZERO),
        Err(error) if error.failure() == StorageFailure::Cancelled)
    );
    assert!(!table_exists());
    assert!(matches!(
        client
            .prepare(&AtomicBool::new(false), Duration::from_secs(5))
            .unwrap(),
        MigrationPreparation::Absent
    ));
    set_legacy(&owner, "owned-test-token");
    let prepare = || match client
        .prepare(&AtomicBool::new(false), Duration::from_secs(5))
        .unwrap()
    {
        MigrationPreparation::Prepared(value) => value,
        _ => panic!("expected prepared migration"),
    };
    let first = prepare();
    let duplicate = prepare();
    let locator = first.locator().clone();
    client
        .confirm(first, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap();
    client
        .confirm(duplicate, &AtomicBool::new(false), Duration::from_secs(5))
        .unwrap();
    assert!(legacy(&root).is_none());
    drop(db);
    owner.shutdown().unwrap();
    assert!(
        matches!(client.prepare(&AtomicBool::new(false), Duration::ZERO),
        Err(error) if error.failure() == StorageFailure::Stopped)
    );
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    match owner
        .native_secret_migration()
        .prepare(&AtomicBool::new(false), Duration::from_secs(5))
        .unwrap()
    {
        MigrationPreparation::LegacyRowRemoved(actual) => assert_eq!(actual, locator),
        _ => panic!("expected durable removal receipt"),
    }
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn changed_legacy_value_cannot_reuse_a_prepared_locator_even_after_reopen() {
    let root = directory("changed-preparation");
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    set_legacy(&owner, "original-test-token");
    let client = owner.native_secret_migration();
    let prepared = match client
        .prepare(&AtomicBool::new(false), Duration::from_secs(5))
        .unwrap()
    {
        MigrationPreparation::Prepared(value) => value,
        _ => panic!("expected prepared migration"),
    };
    let db = Connection::open(root.join("print-partner.db")).unwrap();
    let rowid = || {
        db.query_row(
            "SELECT rowid FROM app_settings WHERE tenant_id='default' AND key='github_pat'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
    };
    let original_rowid = rowid();
    set_legacy(&owner, "replacement-test-token");
    assert_eq!(rowid(), original_rowid);
    assert!(matches!(
        client.prepare(&AtomicBool::new(false), Duration::from_secs(5)),
        Err(error) if error.failure() == StorageFailure::LegacyChanged
    ));
    assert_eq!(
        client
            .confirm(prepared, &AtomicBool::new(false), Duration::from_secs(5))
            .unwrap_err()
            .failure(),
        StorageFailure::LegacyChanged
    );
    drop(db);
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    assert!(matches!(
        owner.native_secret_migration().prepare(&AtomicBool::new(false), Duration::from_secs(5)),
        Err(error) if error.failure() == StorageFailure::LegacyChanged
    ));
    assert_legacy_matches(&root, "replacement-test-token");
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
