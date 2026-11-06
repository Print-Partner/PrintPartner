use super::*;
use pp_testkit::node_schema::{
    self, Statement as HistoricalStatement, execute as execute_historical, parse_schema_migrations,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::mpsc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SettingAttempts {
    pub executions: u32,
    pub inserts: u32,
    pub commits: u32,
}
thread_local! {
    static SETTING_ATTEMPTS: std::cell::Cell<SettingAttempts> = const {
        std::cell::Cell::new(SettingAttempts { executions: 0, inserts: 0, commits: 0 })
    };
}
pub(super) fn observe_setting(observe: impl FnOnce(&mut SettingAttempts)) {
    SETTING_ATTEMPTS.with(|counts| {
        let mut attempts = counts.get();
        observe(&mut attempts);
        counts.set(attempts);
    });
}

#[test]
fn ticket_t_7_writer_business_and_commit_errors_are_not_replayed() {
    let path = directory("no-replay");
    let (mut connection, _) = schema::initialize(
        &path.join("print-partner.db"),
        0,
        "2026-10-02T00:00:00.000Z",
    )
    .unwrap();
    connection
        .execute_batch(
            "CREATE TABLE admission_parent(id INTEGER PRIMARY KEY);
         CREATE TABLE admission_child(parent INTEGER REFERENCES admission_parent(id)
            DEFERRABLE INITIALLY DEFERRED);
         CREATE TRIGGER reject_body BEFORE INSERT ON app_settings WHEN NEW.key='reject-body'
         BEGIN INSERT INTO app_settings VALUES('test','partial','must rollback');
            SELECT RAISE(ABORT,'injected body failure'); END;
         CREATE TRIGGER reject_commit BEFORE INSERT ON app_settings WHEN NEW.key='reject-commit'
         BEGIN INSERT INTO admission_child VALUES(999); END;",
        )
        .unwrap();

    for (key, expected_commits) in [("reject-body", 0), ("reject-commit", 1)] {
        SETTING_ATTEMPTS.with(|counts| counts.set(SettingAttempts::default()));
        let setting = |key: &str| {
            Work::Setting(SettingCommand::Set(Setting {
                tenant: "test".into(),
                key: key.into(),
                value: "once".into(),
            }))
        };
        let error = execute(
            &mut connection,
            setting(key),
            writer_admission::Deadline::at_dequeue(),
        )
        .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<rusqlite::Error>()
                .unwrap()
                .sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation)
        );
        assert_eq!(
            SETTING_ATTEMPTS.with(|counts| counts.get()),
            SettingAttempts {
                executions: 1,
                inserts: 1,
                commits: expected_commits,
            }
        );
        assert!(connection.is_autocommit());
        assert_eq!(pragma_value(&connection, "busy_timeout"), 5000);
        for absent in [key, "partial"] {
            assert_eq!(
                snapshot(&connection, "test", absent).unwrap(),
                SettingSnapshot::Missing
            );
        }
        let children: i64 = connection
            .query_row("SELECT count(*) FROM admission_child", [], |r| r.get(0))
            .unwrap();
        assert_eq!(children, 0);

        assert!(
            execute(
                &mut connection,
                setting("next"),
                writer_admission::Deadline::at_dequeue()
            )
            .unwrap()
        );
        assert_eq!(
            snapshot(&connection, "test", "next").unwrap(),
            SettingSnapshot::Stored {
                value: "once".into()
            }
        );
        assert_eq!(
            SETTING_ATTEMPTS.with(|counts| counts.get()),
            SettingAttempts {
                executions: 2,
                inserts: 2,
                commits: expected_commits + 1,
            }
        );
        assert!(connection.is_autocommit());
        assert_eq!(pragma_value(&connection, "busy_timeout"), 5000);
    }
    drop(connection);
    std::fs::remove_dir_all(path).unwrap();
}

fn exited_true_pid() -> u32 {
    #[cfg(unix)]
    let mut prior = Command::new("/usr/bin/true").spawn().unwrap();
    #[cfg(windows)]
    let mut prior = Command::new("cmd").args(["/C", "exit 0"]).spawn().unwrap();
    let pid = prior.id();
    prior.wait().unwrap();
    pid
}

pub(super) fn directory(name: &str) -> PathBuf {
    let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "pp-storage-{name}-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    pp_os::create_private_dir_all(&path).unwrap();
    path
}
fn fixture(name: &str) -> (PathBuf, WriterOwner, SettingsClient) {
    let path = directory(name);
    let (owner, _) = WriterOwner::open_at(
        &path,
        Limits {
            queued_writes: 1,
            readers: 1,
        },
        "2026-10-02T00:00:00.000Z",
    )
    .unwrap();
    let client = owner.client();
    (path, owner, client)
}
fn set(client: &SettingsClient, key: &str, value: &str) -> Result<WriteReply> {
    client.submit(
        SettingCommand::Set(Setting {
            tenant: "test".into(),
            key: key.into(),
            value: value.into(),
        }),
        &AtomicBool::new(false),
        Duration::ZERO,
    )
}
#[test]
fn settings_are_atomic_and_tenant_scoped() {
    let (path, owner, client) = fixture("settings");
    set(&client, "empty", "").unwrap().recv().unwrap().unwrap();
    let read = client.reader(Duration::ZERO).unwrap();
    assert_eq!(
        read.snapshot("test", "empty").unwrap(),
        SettingSnapshot::Stored { value: "".into() }
    );
    assert_eq!(
        read.snapshot("other", "empty").unwrap(),
        SettingSnapshot::Missing
    );
    assert_eq!(
        read.get_setting("test", "empty", Some("fallback")).unwrap(),
        Some("fallback".into())
    );
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute_batch("CREATE TRIGGER reject_setting BEFORE INSERT ON app_settings WHEN NEW.key='reject' BEGIN INSERT INTO app_settings VALUES('test','partial','must rollback'); SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    assert!(
        set(&client, "reject", "no")
            .unwrap()
            .recv()
            .unwrap()
            .is_err()
    );
    assert_eq!(
        read.snapshot("test", "partial").unwrap(),
        SettingSnapshot::Missing
    );
    drop(raw);
    drop(read);
    owner.shutdown().unwrap();
}
#[test]
fn queue_cancellation_reply_loss_and_shutdown_drain() {
    let (path, owner, client) = fixture("queue");
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    // The writer upserts, so a replayed command takes the UPDATE path; count both.
    let count = "BEGIN INSERT INTO app_settings VALUES('test','execution-count','1') ON CONFLICT(tenant_id,key) DO UPDATE SET value=CAST(value AS INTEGER)+1; END;";
    raw.execute_batch(&format!("CREATE TRIGGER count_admitted_insert AFTER INSERT ON app_settings WHEN NEW.key='admitted' {count} CREATE TRIGGER count_admitted_update AFTER UPDATE ON app_settings WHEN NEW.key='admitted' {count} BEGIN IMMEDIATE;")).unwrap();
    let first = set(&client, "blocking", "yes").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.shared.queue.lock().unwrap().pending.is_empty() {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    drop(set(&client, "admitted", "once").unwrap());
    assert!(
        set(&client, "overflow", "no")
            .unwrap_err()
            .to_string()
            .contains("full")
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let waiting_client = client.clone();
    let waiting_cancel = cancel.clone();
    let waiting = thread::spawn(move || {
        waiting_client
            .submit(
                SettingCommand::Set(Setting {
                    tenant: "test".into(),
                    key: "cancelled".into(),
                    value: "no".into(),
                }),
                &waiting_cancel,
                Duration::from_secs(5),
            )
            .unwrap_err()
            .to_string()
    });
    cancel.store(true, Ordering::Release);
    assert!(waiting.join().unwrap().contains("Cancelled"));
    let read = client.reader(Duration::ZERO).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let close = thread::spawn(move || {
        let result = owner.shutdown();
        done_tx.send(result).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.shared.queue.lock().unwrap().closed {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(
        set(&client, "after-stop", "no")
            .unwrap_err()
            .to_string()
            .contains("stopped")
    );
    assert!(done_rx.try_recv().is_err());
    assert!(path.join(".desktop-lease").is_file());
    raw.execute_batch("COMMIT").unwrap();
    first.recv().unwrap().unwrap();
    drop(raw);
    drop(read);
    done_rx.recv().unwrap().unwrap();
    close.join().unwrap();
    let check = Connection::open(path.join("print-partner.db")).unwrap();
    assert_eq!(
        snapshot(&check, "test", "execution-count").unwrap(),
        SettingSnapshot::Stored { value: "1".into() }
    );
    for key in ["overflow", "cancelled", "after-stop"] {
        assert_eq!(
            snapshot(&check, "test", key).unwrap(),
            SettingSnapshot::Missing
        );
    }
    assert!(!path.join(".desktop-owner.json").exists());
}
#[test]
fn ticket_t_7_writer_disk_full_rolls_back_and_readmits() {
    let (path, owner, client) = fixture("disk-full");
    set(&client, "before", "kept")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    let pages = owner.database_pages().unwrap();
    assert_eq!(owner.admit_disk_quota(pages).unwrap(), pages);
    let filler = "x".repeat(2048);
    let mut refused = None;
    for index in 0..256 {
        let key = format!("fill-{index}");
        match set(&client, &key, &filler).unwrap().recv().unwrap() {
            Ok(written) => assert!(written),
            Err(error) => {
                refused = Some((key, error));
                break;
            }
        }
    }
    let (key, error) = refused.expect("disk quota refuses a settings write");
    assert!(
        matches!(
            error.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(code, _)) if code.code == rusqlite::ErrorCode::DiskFull
        ),
        "{error:#}"
    );
    assert!(owner.database_pages().unwrap() <= pages);
    let read = client.reader(Duration::ZERO).unwrap();
    assert_eq!(
        read.snapshot("test", &key).unwrap(),
        SettingSnapshot::Missing
    );
    assert_eq!(
        read.snapshot("test", "before").unwrap(),
        SettingSnapshot::Stored {
            value: "kept".into()
        }
    );
    drop(read);

    assert_eq!(owner.admit_disk_quota(pages + 64).unwrap(), pages + 64);
    assert!(
        set(&client, &key, &filler)
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
    );
    owner.shutdown().unwrap();
    let check = Connection::open(path.join("print-partner.db")).unwrap();
    assert_eq!(
        snapshot(&check, "test", &key).unwrap(),
        SettingSnapshot::Stored { value: filler }
    );
}
#[test]
fn reader_pool_resets_pragmas_rejects_writes_and_is_bounded() {
    let (_, owner, client) = fixture("readers");
    let read = client.reader(Duration::ZERO).unwrap();
    assert!(
        client
            .reader(Duration::ZERO)
            .err()
            .unwrap()
            .to_string()
            .contains("full")
    );
    let conn = read.connection.as_ref().unwrap();
    conn.execute_batch("PRAGMA query_only=OFF; PRAGMA foreign_keys=OFF; PRAGMA busy_timeout=0;")
        .unwrap();
    assert!(
        conn.execute("INSERT INTO app_settings VALUES('test','escape','no')", [])
            .is_err()
    );
    drop(read);
    let read = client.reader(Duration::ZERO).unwrap();
    for (name, value) in [
        ("query_only", 1),
        ("foreign_keys", 1),
        ("busy_timeout", 5000),
        ("synchronous", 2),
    ] {
        assert_eq!(
            read.connection
                .as_ref()
                .unwrap()
                .pragma_query_value(None, name, |r| r.get::<_, i64>(0))
                .unwrap(),
            value
        );
    }
    set(&client, "while-reader-held", "yes")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    drop(read);
    owner.shutdown().unwrap();
    assert!(
        client
            .reader(Duration::ZERO)
            .err()
            .unwrap()
            .to_string()
            .contains("stopped")
    );
}
#[test]
fn ahead_and_old_versions_reject_before_side_effects() {
    for version in [30, schema::CURRENT_VERSION + 1] {
        let path = directory("version");
        let database = path.join("print-partner.db");
        let conn = Connection::open(&database).unwrap();
        conn.execute_batch("CREATE TABLE app_settings(tenant_id TEXT,key TEXT,value TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO app_settings VALUES('default','schema_version',?1)",
            [version.to_string()],
        )
        .unwrap();
        drop(conn);
        let before = std::fs::read(&database).unwrap();
        assert!(
            WriterOwner::open_at(&path, Limits::default(), "2026-10-02T00:00:00.000Z").is_err()
        );
        assert_eq!(std::fs::read(&database).unwrap(), before);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 2);
    }
}

#[test]
fn reopen_preserves_the_pre_upgrade_backup() {
    let path = directory("upgrade-backup");
    let database = path.join("print-partner.db");
    std::fs::write(
        &database,
        include_bytes!("../tests/fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    let older_backup = path.join("backups/pre-schema34.db");
    std::fs::create_dir_all(older_backup.parent().unwrap()).unwrap();
    std::fs::write(&older_backup, b"retained older recovery copy").unwrap();
    let raw = Connection::open(&database).unwrap();
    raw.execute(
        "UPDATE app_settings SET value='33' WHERE tenant_id='default' AND key='schema_version'",
        [],
    )
    .unwrap();
    drop(raw);

    let (owner, ready) = WriterOwner::open(&path, Limits::default()).unwrap();
    let backup = ready.backup.unwrap();
    owner.shutdown().unwrap();
    let name = if cfg!(feature = "desktop_rebuild") {
        format!("pre-schema33-to-{}.db", crate::schema::CURRENT_VERSION)
    } else {
        "pre-schema35.db".into()
    };
    assert_eq!(backup, path.join("backups").join(name));
    let version = |path: &Path| {
        Connection::open(path)
            .unwrap()
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
    };
    assert_eq!(version(&backup), "33");

    let (owner, ready) = WriterOwner::open(&path, Limits::default()).unwrap();
    assert!(ready.backup.is_none());
    owner.shutdown().unwrap();
    assert_eq!(version(&backup), "33");
    assert_ne!(backup, older_backup);
    assert_eq!(
        std::fs::read(older_backup).unwrap(),
        b"retained older recovery copy"
    );
}
#[test]
fn backup_includes_uncheckpointed_wal() {
    let (path, owner, client) = fixture("backup");
    set(&client, "wal-only", "committed")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    assert!(path.join("print-partner.db-wal").metadata().unwrap().len() > 32);
    let target = path.with_extension("backup.db");
    owner.backup(&target).unwrap();
    let snapshot_db =
        Connection::open_with_flags(target, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        snapshot(&snapshot_db, "test", "wal-only").unwrap(),
        SettingSnapshot::Stored {
            value: "committed".into()
        }
    );
    drop(snapshot_db);
    owner.shutdown().unwrap();
}

#[test]
fn backup_cannot_replace_the_open_database() {
    let (path, owner, client) = fixture("backup-destination");
    set(&client, "retained", "yes")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    assert!(
        owner
            .backup(&path.join("print-partner.db"))
            .unwrap_err()
            .to_string()
            .contains("managed storage")
    );
    set(&client, "after-rejection", "works")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    let reader = client.reader(Duration::ZERO).unwrap();
    assert_eq!(
        reader.snapshot("test", "retained").unwrap(),
        SettingSnapshot::Stored {
            value: "yes".into()
        }
    );
    assert_eq!(
        reader.snapshot("test", "after-rejection").unwrap(),
        SettingSnapshot::Stored {
            value: "works".into()
        }
    );
    drop(reader);
    owner.shutdown().unwrap();
}

#[cfg(unix)]
#[test]
fn backup_rejects_managed_trees_aliases_and_existing_files() {
    use fs2::FileExt;
    use std::os::unix::fs::{MetadataExt, symlink};
    let (path, owner, client) = fixture("backup-authority");
    set(&client, "before", "committed")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    let marker_path = path.join(".desktop-lease");
    let marker = std::fs::read(&marker_path).unwrap();
    let runtime = owner.lease.as_ref().unwrap().runtime_dir().to_owned();
    let lock = path.join("printpartner.lock");
    let inode = std::fs::metadata(&lock).unwrap().ino();
    let mut denied = [
        "print-partner.db",
        "print-partner.db-wal",
        "print-partner.db-shm",
        "print-partner.db-journal",
        "printpartner.lock",
        ".desktop-owner.json",
        "new-file",
    ]
    .map(|name| path.join(name))
    .to_vec();
    denied.push(runtime.join("new-file"));
    let aliases = directory("aliases");
    symlink(&path, aliases.join("data")).unwrap();
    symlink(&runtime, aliases.join("runtime")).unwrap();
    symlink(path.join("print-partner.db-wal"), aliases.join("wal-alias")).unwrap();
    denied.extend([
        aliases.join("data/printpartner.lock"),
        aliases.join("runtime/new-file"),
        aliases.join("wal-alias"),
        path.join("repos/../printpartner.lock"),
    ]);
    let existing = aliases.join("existing");
    std::fs::write(&existing, b"preserve").unwrap();
    denied.push(existing.clone());
    for target in denied {
        assert!(owner.backup(&target).is_err(), "{}", target.display());
        assert_eq!(std::fs::read(&marker_path).unwrap(), marker);
        assert_eq!(std::fs::metadata(&lock).unwrap().ino(), inode);
        let probe = std::fs::File::open(&lock).unwrap();
        assert!(probe.try_lock_exclusive().is_err());
    }
    assert_eq!(std::fs::read(existing).unwrap(), b"preserve");
    set(&client, "after", "committed")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    let exported = aliases.join("offline.db");
    owner.backup(&exported).unwrap();
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let reader = owner.client().reader(Duration::ZERO).unwrap();
    let backup = Connection::open(&exported).unwrap();
    for key in ["before", "after"] {
        let expected = SettingSnapshot::Stored {
            value: "committed".into(),
        };
        assert_eq!(reader.snapshot("test", key).unwrap(), expected);
        assert_eq!(snapshot(&backup, "test", key).unwrap(), expected);
    }
    drop(reader);
    owner.shutdown().unwrap();
}

#[cfg(unix)]
#[test]
fn backup_survives_missing_runtime_without_relaxing_destination_checks() {
    use std::os::unix::fs::symlink;
    let (path, owner, client) = fixture("backup-missing-runtime");
    let external = directory("backup-missing-runtime-external");
    owner.backup(&external.join("before.db")).unwrap();
    set(&client, "wal-only", "committed")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    assert!(path.join("print-partner.db-wal").metadata().unwrap().len() > 32);
    let runtime = owner.lease.as_ref().unwrap().runtime_dir().to_owned();
    assert_eq!(std::fs::read_dir(&runtime).unwrap().count(), 0);
    std::fs::remove_dir(&runtime).unwrap();
    let existing = external.join("existing.db");
    std::fs::write(&existing, b"preserve").unwrap();
    symlink(&existing, external.join("existing-link.db")).unwrap();
    symlink(external.join("absent.db"), external.join("dangling.db")).unwrap();
    symlink(&path, external.join("data-alias")).unwrap();
    for destination in [
        path.join("backup.db"),
        runtime.join("backup.db"),
        external.join("data-alias/backup.db"),
        existing.clone(),
        external.join("existing-link.db"),
        external.join("dangling.db"),
    ] {
        assert!(owner.backup(&destination).is_err());
    }
    assert_eq!(std::fs::read(existing).unwrap(), b"preserve");
    let target = external.join("after.db");
    let result = owner.backup(&target);
    assert!(!runtime.exists());
    owner.shutdown().unwrap();
    result.unwrap();
    let backup = Connection::open_with_flags(&target, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        snapshot(&backup, "test", "wal-only").unwrap(),
        SettingSnapshot::Stored {
            value: "committed".into()
        }
    );
    assert_eq!(
        backup
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    drop(backup);
    std::fs::remove_dir_all(external).unwrap();
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn shutdown_with_held_reader_fails_bounded_and_retains_ownership() {
    let (path, owner, client) = fixture("held-reader");
    let marker = std::fs::read(path.join(".desktop-lease")).unwrap();
    let read = client.reader(Duration::ZERO).unwrap();
    let started = Instant::now();
    let error = owner.shutdown().unwrap_err().to_string();
    assert!(error.contains("Reader checkouts still active after 5s: 1"));
    assert!(started.elapsed() < READER_DRAIN + Duration::from_secs(5));
    assert!(
        set(&client, "after-stop", "no")
            .unwrap_err()
            .to_string()
            .contains("stopped")
    );
    assert_eq!(
        read.snapshot("test", "after-stop").unwrap(),
        SettingSnapshot::Missing
    );
    drop(read);
    assert_eq!(std::fs::read(path.join(".desktop-lease")).unwrap(), marker);
    assert!(lease::StorageLease::acquire(&path).is_err());
    assert!(lease::StorageLease::retry_failed_release(&path).is_err());
}

#[test]
fn failed_cleanup_retains_reservation_and_lock_until_explicit_retry() {
    use fs2::FileExt;
    let (path, owner, _) = fixture("retained-release");
    let marker = std::fs::read(path.join(".desktop-lease")).unwrap();
    let runtime = owner.lease.as_ref().unwrap().runtime_dir().to_owned();
    let blocker = runtime.join("block-release");
    std::fs::write(&blocker, b"retain").unwrap();
    assert!(owner.shutdown().is_err());
    assert_eq!(std::fs::read(path.join(".desktop-lease")).unwrap(), marker);
    assert!(lease::StorageLease::acquire(&path).is_err());
    assert!(
        std::fs::File::open(path.join("printpartner.lock"))
            .unwrap()
            .try_lock_exclusive()
            .is_err()
    );
    let again = lease::StorageLease::retry_failed_release(&path).unwrap();
    assert!(again.ownership_retained && !again.marker_removed && !again.lock_released);
    std::fs::remove_file(blocker).unwrap();
    let released = lease::StorageLease::retry_failed_release(&path).unwrap();
    assert!(
        released.marker_removed
            && released.runtime_removed
            && released.lock_released
            && !released.ownership_retained
    );
    let next = lease::StorageLease::acquire(&path).unwrap().release();
    assert!(next.lock_released);
}

#[test]
fn compatibility_cleanup_failure_retains_lock_until_retry() {
    use fs2::FileExt;
    let (path, owner, _) = fixture("retained-directory-lease");
    let lease_path = path.join(".desktop-lease");
    let hidden = path.join(".desktop-lease-hidden");
    std::fs::rename(&lease_path, &hidden).unwrap();
    std::fs::create_dir(&lease_path).unwrap();
    assert!(owner.shutdown().is_err());
    assert!(!path.join(".desktop-owner.json").exists());
    assert!(
        std::fs::File::open(path.join("printpartner.lock"))
            .unwrap()
            .try_lock_exclusive()
            .is_err()
    );
    std::fs::remove_dir(&lease_path).unwrap();
    std::fs::rename(hidden, &lease_path).unwrap();
    let released = lease::StorageLease::retry_failed_release(&path).unwrap();
    assert!(released.marker_removed && released.runtime_removed && released.lock_released);
    assert!(!released.ownership_retained);
    assert!(!lease_path.exists());
    let next = lease::StorageLease::acquire(&path).unwrap().release();
    assert!(next.lock_released);
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn wal_only_unsupported_versions_preserve_every_input_file() {
    use sha2::{Digest, Sha256};
    let tree = |path: &Path| {
        let mut entries: Vec<_> = std::fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name(),
                    Sha256::digest(std::fs::read(entry.path()).unwrap()).to_vec(),
                )
            })
            .collect();
        entries.sort();
        entries
    };
    for stale_marker in [false, true] {
        for version in [30, schema::CURRENT_VERSION + 1] {
            for with_shm in [false, true] {
                let source = directory("wal-source");
                let raw = Connection::open(source.join("print-partner.db")).unwrap();
                raw.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE app_settings(tenant_id TEXT,key TEXT,value TEXT); INSERT INTO app_settings VALUES('default','schema_version','34'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
                raw.execute("UPDATE app_settings SET value=?1", [version.to_string()])
                    .unwrap();
                let target = directory("wal-rejection");
                for suffix in if with_shm {
                    vec!["", "-wal", "-shm"]
                } else {
                    vec!["", "-wal"]
                } {
                    std::fs::copy(
                        source.join(format!("print-partner.db{suffix}")),
                        target.join(format!("print-partner.db{suffix}")),
                    )
                    .unwrap();
                }
                std::fs::write(target.join("retain-user-file"), b"untouched").unwrap();
                if stale_marker {
                    let pid = exited_true_pid();
                    std::fs::write(
                        target.join(".desktop-owner.json"),
                        serde_json::json!({"storage_protocol_version": 1, "pid": pid}).to_string(),
                    )
                    .unwrap();
                }
                std::fs::write(target.join("printpartner.lock"), b"").unwrap();
                let before = tree(&target);
                let error = WriterOwner::open(&target, Limits::default())
                    .err()
                    .unwrap()
                    .to_string();
                assert!(error.contains(&format!("version {version}")), "{error}");
                assert_eq!(tree(&target), before);
            }
        }
    }
}

#[test]
fn unproved_closure_cannot_be_released_by_cleanup_retry() {
    use fs2::FileExt;
    let path = directory("unproved-closure");
    let owner = lease::StorageLease::acquire(&path).unwrap();
    let marker = std::fs::read(path.join(".desktop-lease")).unwrap();
    owner.retain_until_process_exit();
    let error = lease::StorageLease::retry_failed_release(&path)
        .err()
        .unwrap();
    assert!(error.to_string().contains("Closure was not proved"));
    assert_eq!(std::fs::read(path.join(".desktop-lease")).unwrap(), marker);
    assert!(
        std::fs::File::open(path.join("printpartner.lock"))
            .unwrap()
            .try_lock_exclusive()
            .is_err()
    );
    assert!(lease::StorageLease::acquire(&path).is_err());
}

const SCHEMA_PARITY_NODE_VERSION: &str = "v22.23.3";
const SCHEMA_PARITY_NODE_ABI: &str = "127";

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SchemaParityNodeIdentity {
    exec_path: PathBuf,
    version: String,
    abi: String,
}

fn require_schema_parity_runtime(identity: &SchemaParityNodeIdentity) -> anyhow::Result<()> {
    anyhow::ensure!(
        identity.version == SCHEMA_PARITY_NODE_VERSION && identity.abi == SCHEMA_PARITY_NODE_ABI,
        "schema fixtures require Node {SCHEMA_PARITY_NODE_VERSION} ABI {SCHEMA_PARITY_NODE_ABI}, found {} ABI {}",
        identity.version,
        identity.abi
    );
    Ok(())
}

fn schema_parity_node_identity(node: &Path) -> anyhow::Result<SchemaParityNodeIdentity> {
    let output = Command::new(node)
        .args([
            "--eval",
            "process.stdout.write(JSON.stringify({execPath:process.execPath,version:process.version,abi:process.versions.modules}))",
        ])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Node identity probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let identity: SchemaParityNodeIdentity = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(
        std::fs::canonicalize(&identity.exec_path)? == std::fs::canonicalize(node)?,
        "fixture node must be its actual executable, not a dispatcher script"
    );
    require_schema_parity_runtime(&identity)?;
    Ok(identity)
}

#[test]
fn ticket_t_7_migrations_requires_pinned_runtime_identity() {
    let identity = schema_parity_node_identity(&schema_parity_node()).unwrap();
    assert_eq!(identity.version, "v22.23.3");
    assert_eq!(identity.abi, "127");
    let mut wrong_version = identity.clone();
    wrong_version.version = "v24.21.0".into();
    assert!(require_schema_parity_runtime(&wrong_version).is_err());
    let mut wrong_abi = identity;
    wrong_abi.abi = "137".into();
    assert!(require_schema_parity_runtime(&wrong_abi).is_err());
}

#[cfg(unix)]
#[test]
fn ticket_t_7_migrations_refuses_dispatcher_script() {
    use std::os::unix::fs::PermissionsExt;

    let root = directory("schema-node-dispatcher");
    let wrapper = root.join("node-wrapper");
    let node = schema_parity_node();
    let escaped = node.to_str().unwrap().replace('\'', "'\\''");
    std::fs::write(&wrapper, format!("#!/bin/sh\nexec '{escaped}' \"$@\"\n")).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = match schema_parity_node_identity(&wrapper) {
        Ok(_) => panic!("accepted a Node dispatcher"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("not a dispatcher script"), "{error}");
    std::fs::remove_dir_all(root).unwrap();
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SchemaGraph {
    version: String,
    objects: BTreeMap<(String, String), (String, Option<String>)>,
    columns: BTreeMap<String, Vec<String>>,
    foreign_keys: BTreeMap<String, Vec<String>>,
    rows: BTreeMap<String, Vec<String>>,
}

fn schema_parity_node() -> PathBuf {
    if let Ok(path) = std::env::var("PP_SCHEMA_PARITY_NODE") {
        return PathBuf::from(path);
    }
    let pinned = PathBuf::from(
        "/var/lib/agent-fleet-runtime-owner/runtime-root/releases/node-project-22.23.3-bb2b080fb03ea1f2/bin/node",
    );
    if pinned.is_file() {
        return pinned;
    }
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let candidate = PathBuf::from(dir).join("node");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("node binary for schema fixtures was not found");
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn value_token(value: rusqlite::types::Value) -> String {
    match value {
        rusqlite::types::Value::Null => "<null>".to_owned(),
        rusqlite::types::Value::Integer(value) => value.to_string(),
        rusqlite::types::Value::Real(value) => value.to_string(),
        rusqlite::types::Value::Text(value) => value,
        rusqlite::types::Value::Blob(value) => hex::encode(value),
    }
}

fn tree_hashes(root: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if !dir.exists() {
            continue;
        }
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let digest = hex::encode(Sha256::digest(std::fs::read(&path).unwrap()));
            (relative, digest)
        })
        .collect()
}

fn copy_db_files(from: &Path, to: &Path) {
    pp_os::create_private_dir_all(to).unwrap();
    if !from.is_dir() {
        return;
    }
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let label = name.to_string_lossy();
        if label.starts_with("print-partner.db") && entry.path().is_file() {
            std::fs::copy(entry.path(), to.join(name)).unwrap();
        }
    }
}

fn read_schema_graph(db: &Path) -> SchemaGraph {
    let conn = Connection::open(db).unwrap();
    let version: String = conn
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut objects = BTreeMap::new();
    let mut statement = conn
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY type, name")
        .unwrap();
    let listed = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .unwrap();
    for row in listed {
        let (kind, name, table, sql) = row.unwrap();
        assert!(
            objects.insert((kind, name), (table, sql)).is_none(),
            "duplicate sqlite_master name"
        );
    }
    drop(statement);
    let tables: Vec<String> = objects
        .iter()
        .filter(|((kind, _), _)| kind == "table")
        .map(|((_, name), _)| name.clone())
        .collect();
    let mut columns = BTreeMap::new();
    let mut foreign_keys = BTreeMap::new();
    let mut rows = BTreeMap::new();
    for table in tables {
        let ident = quote_ident(&table);
        let mut info = conn
            .prepare(&format!("PRAGMA table_info({ident})"))
            .unwrap();
        let mut names = Vec::new();
        let defs = info
            .query_map([], |row| {
                let name: String = row.get(1)?;
                let line = format!(
                    "{}\u{1f}{name}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
                    value_token(row.get(0)?),
                    value_token(row.get(2)?),
                    value_token(row.get(3)?),
                    value_token(row.get(4)?),
                    value_token(row.get(5)?),
                );
                Ok((name, line))
            })
            .unwrap()
            .map(|row| row.unwrap())
            .collect::<Vec<_>>();
        drop(info);
        let defs: Vec<String> = defs
            .into_iter()
            .map(|(name, line)| {
                names.push(name);
                line
            })
            .collect();
        let mut listed = conn
            .prepare(&format!("PRAGMA foreign_key_list({ident})"))
            .unwrap();
        let keys = listed
            .query_map([], |row| {
                let mut parts = Vec::new();
                for index in 0..8 {
                    parts.push(value_token(row.get(index)?));
                }
                Ok(parts.join("\u{1f}"))
            })
            .unwrap()
            .map(|row| row.unwrap())
            .collect::<Vec<_>>();
        drop(listed);
        let projected = names
            .iter()
            .map(|name| format!("quote({})", quote_ident(name)))
            .collect::<Vec<_>>()
            .join(", ");
        let mut data = conn
            .prepare(&format!(
                "SELECT {projected} FROM {ident} ORDER BY {projected}"
            ))
            .unwrap();
        let stored = data
            .query_map([], |row| {
                let mut cells = Vec::new();
                for index in 0..names.len() {
                    cells.push(row.get::<_, String>(index)?);
                }
                Ok(cells.join("\u{1e}"))
            })
            .unwrap()
            .map(|row| row.unwrap())
            .collect::<Vec<_>>();
        columns.insert(table.clone(), defs);
        foreign_keys.insert(table.clone(), keys);
        rows.insert(table, stored);
    }
    SchemaGraph {
        version,
        objects,
        columns,
        foreign_keys,
        rows,
    }
}

fn map_diff(
    label: &str,
    left_map: &BTreeMap<String, Vec<String>>,
    right_map: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut tables: Vec<_> = left_map.keys().chain(right_map.keys()).collect();
    tables.sort();
    tables.dedup();
    tables
        .into_iter()
        .filter(|table| left_map.get(*table) != right_map.get(*table))
        .map(|table| format!("{label} {table}"))
        .collect()
}

fn graph_diff(left: &SchemaGraph, right: &SchemaGraph) -> Vec<String> {
    let mut diffs = Vec::new();
    if left.version != right.version {
        diffs.push(format!("version {} != {}", left.version, right.version));
    }
    if left.objects != right.objects {
        let mut keys: Vec<_> = left.objects.keys().chain(right.objects.keys()).collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            if left.objects.get(key) != right.objects.get(key) {
                diffs.push(format!("object {} {}", key.0, key.1));
            }
        }
    }
    diffs.extend(map_diff("columns", &left.columns, &right.columns));
    diffs.extend(map_diff(
        "foreign_keys",
        &left.foreign_keys,
        &right.foreign_keys,
    ));
    diffs.extend(map_diff("rows", &left.rows, &right.rows));
    diffs.sort();
    diffs.dedup();
    diffs
}

fn rewrite_sql(graph: &mut SchemaGraph, needle: &str, replacement: &str) -> String {
    for ((kind, name), (_, sql)) in &mut graph.objects {
        let Some(sql) = sql.as_mut() else {
            continue;
        };
        let Some(index) = sql.find(needle) else {
            continue;
        };
        sql.replace_range(index..index + needle.len(), replacement);
        return format!("{kind} {name}");
    }
    panic!("fixture SQL has no {needle}");
}

fn drift_controls(source: &SchemaGraph) -> Vec<(&'static str, SchemaGraph)> {
    let mut check = source.clone();
    rewrite_sql(&mut check, "CHECK", "CHECK /*drift*/");
    let mut default = source.clone();
    rewrite_sql(&mut default, "DEFAULT", "DEFAULT /*drift*/");
    let mut foreign_key = source.clone();
    rewrite_sql(&mut foreign_key, "REFERENCES", "REFERENCES /*drift*/");
    let keys = foreign_key
        .foreign_keys
        .values_mut()
        .find(|keys| !keys.is_empty())
        .unwrap();
    keys[0].push_str("|drift");
    let mut index = source.clone();
    let index_key = index
        .objects
        .iter()
        .find(|((kind, _), (_, sql))| kind == "index" && sql.is_some())
        .map(|(key, _)| key.clone())
        .unwrap();
    index.objects.remove(&index_key);
    let mut trigger = source.clone();
    let trigger_sql = trigger
        .objects
        .iter_mut()
        .find(|((kind, _), _)| kind == "trigger")
        .map(|(_, (_, sql))| sql.as_mut().unwrap())
        .unwrap();
    trigger_sql.push_str(" /*drift*/");
    let mut tenant = source.clone();
    let row = tenant
        .rows
        .get_mut("app_settings")
        .unwrap()
        .iter_mut()
        .find(|row| row.contains("'tenant-a'"))
        .unwrap();
    *row = row.replacen("'tenant-a'", "'tenant-drift'", 1);
    let mut order = source.clone();
    let swapped = order.objects.iter_mut().find_map(|((kind, _), (_, sql))| {
        if kind != "table" {
            return None;
        }
        let sql = sql.as_mut()?;
        let left = "\n    id INTEGER";
        let right = "\n    tenant_id TEXT";
        if sql.contains(left) && sql.contains(right) {
            *sql = sql
                .replacen(left, "\n    \u{0}", 1)
                .replacen(right, left, 1)
                .replacen("\n    \u{0}", right, 1);
            Some(())
        } else {
            None
        }
    });
    assert!(swapped.is_some(), "no observed column pair to swap");
    vec![
        ("check", check),
        ("default", default),
        ("foreign-key", foreign_key),
        ("index", index),
        ("trigger", trigger),
        ("tenant", tenant),
        ("column-order", order),
    ]
}

fn open_parity(dir: &Path, clock: &str) -> (WriterOwner, SchemaReady) {
    WriterOwner::open_at(
        dir,
        Limits {
            queued_writes: 2,
            readers: 2,
        },
        clock,
    )
    .unwrap()
}

fn apply_parity_settings(owner: &WriterOwner) {
    let client = owner.client();
    let cancelled = AtomicBool::new(false);
    let wait = Duration::from_secs(5);
    for (tenant, key, value) in [
        ("tenant-a", "discord_notify_on_update", "1"),
        ("tenant-b", "discord_notify_on_update", "0"),
        ("tenant-a", "discord_notify_on_sync", ""),
    ] {
        client
            .submit(
                SettingCommand::Set(Setting {
                    tenant: tenant.into(),
                    key: key.into(),
                    value: value.into(),
                }),
                &cancelled,
                wait,
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap();
    }
    let read = client.reader(Duration::ZERO).unwrap();
    assert_eq!(
        read.snapshot("tenant-a", "discord_notify_on_sync").unwrap(),
        SettingSnapshot::Stored {
            value: String::new()
        }
    );
    drop(read);
    for (expected, value, outcome) in [
        (
            SettingSnapshot::Stored {
                value: String::new(),
            },
            "1",
            true,
        ),
        (SettingSnapshot::Missing, "wrong", false),
    ] {
        let applied = client
            .submit(
                SettingCommand::CompareAndSet {
                    setting: Setting {
                        tenant: "tenant-a".into(),
                        key: "discord_notify_on_sync".into(),
                        value: value.into(),
                    },
                    expected,
                },
                &cancelled,
                wait,
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap();
        assert_eq!(applied, outcome);
    }
}

fn schema_version_at(path: &Path) -> String {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn sqlite_schema_cookie(db: &Path) -> i64 {
    Connection::open(db)
        .unwrap()
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .unwrap()
}

fn index_and_trigger_sql(db: &Path) -> Vec<(String, String, String)> {
    let connection = Connection::open(db).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT type, name, sql FROM sqlite_master
              WHERE sql IS NOT NULL AND type IN ('index', 'trigger')
                AND name NOT LIKE 'sqlite_%'
              ORDER BY type, name",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn assert_second_open_keeps_catalog(dir: &Path, label: &str) {
    let db = dir.join("print-partner.db");
    assert_eq!(
        schema_version_at(&db),
        schema::CURRENT_VERSION.to_string(),
        "{label}"
    );
    let cookie = sqlite_schema_cookie(&db);
    let catalog = index_and_trigger_sql(&db);
    let (owner, ready) = open_parity(dir, "2026-10-02T00:00:00.000Z");
    assert_eq!(ready.version, schema::CURRENT_VERSION, "{label}");
    assert_eq!(ready.previous_version, schema::CURRENT_VERSION, "{label}");
    assert!(ready.backup.is_none(), "{label}");
    owner.shutdown().unwrap();
    assert_eq!(
        schema_version_at(&db),
        schema::CURRENT_VERSION.to_string(),
        "{label}"
    );
    assert_eq!(
        sqlite_schema_cookie(&db),
        cookie,
        "{label} second open changed the SQLite schema cookie"
    );
    assert_eq!(index_and_trigger_sql(&db), catalog, "{label}");
}

#[derive(Debug, PartialEq, Eq)]
enum FixtureRefusal {
    HistoricalClaim,
    RejectedKind,
    ShapeMismatch,
}

struct GeneratedFixture {
    name: String,
    initial: u64,
    shape_group: String,
    transformations: Vec<String>,
    source: PathBuf,
    expected_dir: PathBuf,
}

fn steps_are(steps: &[String], expected: &[&str]) -> bool {
    steps.len() == expected.len()
        && steps
            .iter()
            .zip(expected)
            .all(|(step, expected)| step == expected)
}

fn listed_shape(group: &str, initial: u64, steps: &[String]) -> bool {
    match group {
        "empty-directory" => {
            initial == 0 && steps_are(steps, &["node-connect-from-empty-directory"])
        }
        "generated-v34-minus-board-and-legacy-column" => {
            initial == 31
                && steps_are(
                    steps,
                    &[
                        "node-connect-current-schema",
                        "insert-tenant-claimed-project",
                        "drop-board-posts-and-board-comments",
                        "drop-projects-legacy-manifest-cutover",
                        "stamp-schema-version",
                    ],
                )
        }
        "generated-v34-minus-board-tables" => {
            (initial == 32 || initial == 33)
                && steps_are(
                    steps,
                    &[
                        "node-connect-current-schema",
                        "insert-tenant-claimed-project",
                        "drop-board-posts-and-board-comments",
                        "stamp-schema-version",
                    ],
                )
        }
        "generated-v34-current" => {
            initial == 34
                && steps_are(
                    steps,
                    &[
                        "node-connect-current-schema",
                        "insert-tenant-claimed-project",
                        "stamp-schema-version",
                    ],
                )
        }
        "generated-v34-current-autosave" => {
            initial == 34
                && steps_are(
                    steps,
                    &[
                        "node-connect-current-schema",
                        "replace-rows-from-autosave-golden",
                        "stamp-schema-version",
                    ],
                )
        }
        _ => false,
    }
}

fn accept_generated_fixture(case: &serde_json::Value) -> Result<GeneratedFixture, FixtureRefusal> {
    let kind = case["kind"].as_str().unwrap_or("");
    let historical = case["historicalSnapshot"].as_bool();
    if kind == "historical-snapshot" || historical == Some(true) {
        return Err(FixtureRefusal::HistoricalClaim);
    }
    if kind != "generated-current-node" || historical != Some(false) {
        return Err(FixtureRefusal::RejectedKind);
    }
    if case["sourceSchemaVersion"].as_u64() != Some(34) {
        return Err(FixtureRefusal::RejectedKind);
    }
    let name = case["name"]
        .as_str()
        .ok_or(FixtureRefusal::RejectedKind)?
        .to_owned();
    let initial = case["startingVersionStamp"]
        .as_u64()
        .ok_or(FixtureRefusal::RejectedKind)?;
    if case["version"].as_u64() != Some(initial) {
        return Err(FixtureRefusal::RejectedKind);
    }
    let shape_group = case["shapeGroup"]
        .as_str()
        .ok_or(FixtureRefusal::RejectedKind)?
        .to_owned();
    let transformations = case["transformations"]
        .as_array()
        .ok_or(FixtureRefusal::RejectedKind)?
        .iter()
        .map(|step| {
            step.as_str()
                .ok_or(FixtureRefusal::RejectedKind)
                .map(str::to_owned)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !listed_shape(&shape_group, initial, &transformations) {
        return Err(FixtureRefusal::ShapeMismatch);
    }
    Ok(GeneratedFixture {
        name,
        initial,
        shape_group,
        transformations,
        source: PathBuf::from(case["rust"].as_str().ok_or(FixtureRefusal::RejectedKind)?),
        expected_dir: PathBuf::from(
            case["expected"]
                .as_str()
                .ok_or(FixtureRefusal::RejectedKind)?,
        ),
    })
}

fn file_list(value: &serde_json::Value) -> Vec<(String, String)> {
    let mut files = value
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["path"].as_str().unwrap().to_owned(),
                row["sha256"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn cache_material(meta: &serde_json::Value, generator_sha: &str) -> String {
    let mut lines = vec![
        generator_sha.to_owned(),
        meta["node"].as_str().unwrap().to_owned(),
        meta["abi"].as_str().unwrap().to_owned(),
        meta["sourceSchemaVersion"].as_u64().unwrap().to_string(),
        meta["nodeSha256"].as_str().unwrap().to_owned(),
        meta["clock"].as_str().unwrap().to_owned(),
    ];
    for case in meta["inputs"].as_array().unwrap() {
        let steps = case["transformations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|step| step.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(",");
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            case["name"].as_str().unwrap(),
            case["kind"].as_str().unwrap(),
            case["historicalSnapshot"].as_bool().unwrap(),
            case["shapeGroup"].as_str().unwrap(),
            case["startingVersionStamp"].as_u64().unwrap(),
            steps,
        ));
    }
    format!("{}\n", lines.join("\n"))
}

fn table_present(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

fn column_present(conn: &Connection, table: &str, column: &str) -> bool {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({})", quote_ident(table)))
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(|name| name.unwrap())
        .any(|name| name == column)
}

fn require_observed_shape(fixture: &GeneratedFixture) {
    if fixture.shape_group == "empty-directory" {
        assert!(
            tree_hashes(&fixture.source).is_empty(),
            "{} input was not an empty directory",
            fixture.name
        );
        return;
    }
    let scratch = directory(&format!("generated-shape-{}", fixture.name));
    let database = scratch.join("print-partner.db");
    std::fs::copy(fixture.source.join("print-partner.db"), &database).unwrap();
    let conn = Connection::open(&database).unwrap();
    let board_posts = table_present(&conn, "board_posts");
    let board_comments = table_present(&conn, "board_comments");
    let legacy = column_present(&conn, "projects", "legacy_manifest_cutover");
    let stamp: String = conn
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stamp, fixture.initial.to_string(), "{}", fixture.name);
    let marks = (board_posts, board_comments, legacy);
    let expected = match fixture.shape_group.as_str() {
        "generated-v34-minus-board-and-legacy-column" => (false, false, false),
        "generated-v34-minus-board-tables" => (false, false, true),
        "generated-v34-current" | "generated-v34-current-autosave" => (true, true, true),
        other => panic!("{} has unexpected shape group {other}", fixture.name),
    };
    assert_eq!(
        marks, expected,
        "{} shape {} did not match the input database",
        fixture.name, fixture.shape_group
    );
}

fn sequence_identities(graph: &SchemaGraph) -> Vec<String> {
    graph
        .rows
        .get("sqlite_sequence")
        .map(|rows| {
            rows.iter()
                .map(|row| row.split('\u{1e}').next().unwrap_or("").to_owned())
                .collect()
        })
        .unwrap_or_default()
}

// Generated Node schema 34 fixtures with recorded manual edits.
// Passing this test does not prove a copied historical database or schema 32 to 33 migration order.
#[test]
fn ticket_t_7_migrations() {
    let node = schema_parity_node();
    let identity = schema_parity_node_identity(&node).unwrap();
    let bytes = std::fs::read(&node).unwrap();
    let node_hash = hex::encode(Sha256::digest(&bytes));
    let root = directory("schema-parity-fixtures");
    let web = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../web");
    let output = std::process::Command::new(&node)
        .current_dir(&web)
        .args([
            "--conditions=development",
            "--import",
            "tsx",
            "scripts/storage-fixtures/prepare.mts",
        ])
        .arg(pp_testkit::node_path::node_path(&root))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "node fixture prepare failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("fixtures.json")).unwrap()).unwrap();
    let clock = meta["clock"].as_str().unwrap().to_owned();
    let node_version = meta["node"].as_str().unwrap();
    let abi = meta["abi"].as_str().unwrap();
    assert_eq!(node_version, identity.version);
    assert_eq!(abi, identity.abi);
    assert_eq!(meta["nodeSha256"].as_str().unwrap(), node_hash);
    assert_eq!(
        meta["contract"].as_str().unwrap(),
        "generated-fixture-compatibility"
    );
    assert!(!meta["historicalSnapshot"].as_bool().unwrap());
    assert_eq!(meta["sourceSchemaVersion"].as_u64().unwrap(), 34);
    let generator = web.join("scripts/storage-fixtures/prepare.mts");
    let generator_sha = hex::encode(Sha256::digest(std::fs::read(&generator).unwrap()));
    assert_eq!(meta["generatorSha256"].as_str().unwrap(), generator_sha);
    let cache_key = hex::encode(Sha256::digest(
        cache_material(&meta, &generator_sha).as_bytes(),
    ));
    assert_eq!(meta["cacheKey"].as_str().unwrap(), cache_key);
    let mut forged = meta["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "schema-31")
        .unwrap()
        .clone();
    forged["kind"] = serde_json::json!("historical-snapshot");
    assert!(matches!(
        accept_generated_fixture(&forged),
        Err(FixtureRefusal::HistoricalClaim)
    ));
    let mut names = Vec::new();
    let mut fresh_graph = None;
    let mut opened = Vec::new();
    for case in meta["inputs"].as_array().unwrap() {
        let fixture = accept_generated_fixture(case).unwrap_or_else(|reason| {
            panic!(
                "{} generated fixture was refused: {reason:?}",
                case["name"].as_str().unwrap_or("unnamed")
            )
        });
        let name = fixture.name.clone();
        names.push(name.clone());
        let initial = fixture.initial;
        let source = fixture.source.clone();
        let expected = fixture.expected_dir.join("print-partner.db");
        let before_source = tree_hashes(&source);
        assert_eq!(
            before_source,
            file_list(&case["rustFiles"]),
            "{name} rust input hash"
        );
        assert_eq!(
            tree_hashes(&fixture.expected_dir),
            file_list(&case["expectedFiles"]),
            "{name} expected hash"
        );
        require_observed_shape(&fixture);
        assert_eq!(
            tree_hashes(&source),
            before_source,
            "{name} shape read changed the input"
        );
        opened.push((
            name.clone(),
            initial,
            fixture.shape_group.clone(),
            fixture.transformations.clone(),
            file_list(&case["expectedFiles"]),
        ));
        let work = directory(&format!("schema-parity-{name}"));
        copy_db_files(&source, &work);
        let before_work = tree_hashes(&work);
        if initial == 0 {
            assert!(before_work.is_empty(), "{name} input was not empty");
        } else {
            assert!(
                before_work
                    .iter()
                    .any(|(file, _)| file == "print-partner.db"),
                "{name} had no copied database"
            );
        }
        let (owner, ready) = open_parity(&work, &clock);
        assert_eq!(ready.version, schema::CURRENT_VERSION, "{name}");
        assert_eq!(ready.previous_version, initial, "{name}");
        let backup = ready.backup.clone();
        if (1..35).contains(&initial) {
            assert_eq!(
                schema_version_at(backup.as_ref().unwrap()),
                initial.to_string(),
                "{name}"
            );
        } else {
            assert!(backup.is_none(), "{name}");
        }
        apply_parity_settings(&owner);
        owner.shutdown().unwrap();
        assert_eq!(tree_hashes(&source), before_source, "{name} source changed");
        let observed = read_schema_graph(&work.join("print-partner.db"));
        let node_graph = read_schema_graph(&expected);
        let diff = node_owned_catalog(&node_graph, &observed);
        assert!(diff.is_empty(), "{name} node {node_version}: {diff:#?}");
        let backup_bytes = backup.as_ref().map(|path| std::fs::read(path).unwrap());
        let (reopened, again) = open_parity(&work, &clock);
        assert_eq!(again.version, schema::CURRENT_VERSION);
        assert_eq!(again.previous_version, schema::CURRENT_VERSION);
        assert!(again.backup.is_none());
        reopened.shutdown().unwrap();
        if let Some(bytes) = backup_bytes {
            assert_eq!(std::fs::read(backup.unwrap()).unwrap(), bytes);
        }
        let reopened_graph = read_schema_graph(&work.join("print-partner.db"));
        let reopened_diff = node_owned_catalog(&node_graph, &reopened_graph);
        assert!(
            reopened_diff.is_empty(),
            "{name} reopen: {reopened_diff:#?}"
        );
        if name == "fresh" {
            fresh_graph = Some(node_graph);
        }
    }
    assert_eq!(
        names,
        [
            "fresh",
            "schema-31",
            "schema-32",
            "schema-33",
            "schema-34",
            "immutable-corpus"
        ]
    );
    let find = |label: &str| {
        opened
            .iter()
            .find(|(name, ..)| name == label)
            .unwrap_or_else(|| panic!("missing {label}"))
    };
    let schema_32 = find("schema-32");
    let schema_33 = find("schema-33");
    assert_eq!(schema_32.1, 32);
    assert_eq!(schema_33.1, 33);
    assert_eq!(schema_32.2, "generated-v34-minus-board-tables");
    assert_eq!(schema_33.2, schema_32.2);
    assert_eq!(schema_33.3, schema_32.3);
    assert_eq!(
        schema_32.4, schema_33.4,
        "schema-32 and schema-33 share one generated shape, so their schema 34 endpoint trees match"
    );
    assert_eq!(
        meta["sameGeneratedShape"]["shapeGroup"].as_str().unwrap(),
        "generated-v34-minus-board-tables"
    );
    assert!(
        meta["sameGeneratedShape"]["expectedTreesEqual"]
            .as_bool()
            .unwrap()
    );
    let fresh = fresh_graph.unwrap();
    for (label, drifted) in drift_controls(&fresh) {
        let diff = graph_diff(&fresh, &drifted);
        assert!(!diff.is_empty(), "{label} drift was accepted: {diff:#?}");
    }
}

fn refused_wal_input(version: &str) -> PathBuf {
    let source = directory("refuse-source");
    let raw = Connection::open(source.join("print-partner.db")).unwrap();
    raw.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; CREATE TABLE app_settings(tenant_id TEXT, key TEXT, value TEXT); INSERT INTO app_settings VALUES('default','schema_version','34'); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
    raw.execute("UPDATE app_settings SET value=?1", [version])
        .unwrap();
    let target = directory("refuse-target");
    for suffix in ["", "-wal", "-shm"] {
        let from = source.join(format!("print-partner.db{suffix}"));
        if from.is_file() {
            std::fs::copy(&from, target.join(from.file_name().unwrap())).unwrap();
        }
    }
    drop(raw);
    std::fs::write(target.join("retain-user-file"), b"untouched").unwrap();
    assert!(target.join("print-partner.db-wal").is_file());
    assert!(target.join("print-partner.db-shm").is_file());
    target
}

fn assert_refused(path: &Path, message: &str) {
    std::fs::write(
        path.join("printpartner.lock"),
        b"keep existing lock contents",
    )
    .unwrap();
    let before = tree_hashes(path);
    assert!(
        before.iter().any(|(file, _)| file == "print-partner.db")
            && before.iter().any(|(file, _)| file == "retain-user-file")
    );
    let error = WriterOwner::open(path, Limits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains(message), "{error}");
    assert_eq!(tree_hashes(path), before);
}

#[test]
fn ticket_t_7_migrations_preserves_refused_inputs() {
    assert_refused(
        &refused_wal_input("30"),
        "Cannot upgrade database schema version 30",
    );
    let newer = if cfg!(feature = "desktop_rebuild") {
        "44"
    } else {
        "40"
    };
    assert_refused(
        &refused_wal_input(newer),
        &format!("newer than supported version {}", schema::CURRENT_VERSION),
    );
    assert_refused(&refused_wal_input("abc"), "Invalid database schema version");
    let corrupt = directory("refuse-corrupt");
    std::fs::write(corrupt.join("print-partner.db"), b"not a sqlite database").unwrap();
    std::fs::write(corrupt.join("print-partner.db-wal"), b"wal-bytes").unwrap();
    std::fs::write(corrupt.join("print-partner.db-shm"), b"shm-bytes").unwrap();
    std::fs::write(corrupt.join("retain-user-file"), b"untouched").unwrap();
    std::fs::write(
        corrupt.join("printpartner.lock"),
        b"keep existing lock contents",
    )
    .unwrap();
    let before = tree_hashes(&corrupt);
    let error = WriterOwner::open(&corrupt, Limits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("integrity")
            || error.contains("not a database")
            || error.contains("file is not a database"),
        "{error}"
    );
    assert_eq!(tree_hashes(&corrupt), before);
}

#[test]
fn ticket_t_7_writer_reads_wal_full_and_rejects_reader_writes() {
    let (path, owner, client) = fixture("t7-pragma");
    set(&client, "durable", "kept")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    assert!(path.join("print-partner.db-wal").metadata().unwrap().len() > 32);
    let read = client.reader(Duration::ZERO).unwrap();
    let conn = read.connection.as_ref().unwrap();
    assert_eq!(
        conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(pragma_value(conn, "foreign_keys"), 1);
    assert_eq!(pragma_value(conn, "busy_timeout"), 5000);
    assert_eq!(pragma_value(conn, "synchronous"), 2);
    assert_eq!(pragma_value(conn, "query_only"), 1);
    conn.execute_batch("PRAGMA query_only=OFF; PRAGMA foreign_keys=OFF; PRAGMA busy_timeout=0;")
        .unwrap();
    let write_error = conn
        .execute("INSERT INTO app_settings VALUES('test','escape','no')", [])
        .unwrap_err()
        .to_string();
    assert!(
        write_error.to_lowercase().contains("readonly"),
        "{write_error}"
    );
    drop(read);
    let read = client.reader(Duration::ZERO).unwrap();
    let conn = read.connection.as_ref().unwrap();
    assert_eq!(
        conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    assert_eq!(pragma_value(conn, "foreign_keys"), 1);
    assert_eq!(pragma_value(conn, "busy_timeout"), 5000);
    assert_eq!(pragma_value(conn, "synchronous"), 2);
    assert_eq!(pragma_value(conn, "query_only"), 1);
    assert_eq!(
        read.snapshot("test", "durable").unwrap(),
        SettingSnapshot::Stored {
            value: "kept".into()
        }
    );
    assert_eq!(
        read.snapshot("test", "escape").unwrap(),
        SettingSnapshot::Missing
    );
    drop(read);
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let read = owner.client().reader(Duration::ZERO).unwrap();
    assert_eq!(
        read.snapshot("test", "durable").unwrap(),
        SettingSnapshot::Stored {
            value: "kept".into()
        }
    );
    drop(read);
    owner.shutdown().unwrap();
}

fn sqlite_sequence_rows(connection: &Connection) -> Vec<(String, i64)> {
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'sqlite_sequence')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !present {
        return Vec::new();
    }
    let mut statement = connection
        .prepare("SELECT name, seq FROM sqlite_sequence ORDER BY name")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

#[test]
fn ticket_t_7_writer_reopen_keeps_starter_seed_sequences() {
    let path = directory("t7-seed-sequence");
    let clock = "2026-10-02T00:00:00.000Z";
    let (owner, _) = WriterOwner::open_at(&path, Limits::default(), clock).unwrap();
    let read = owner.client().reader(Duration::ZERO).unwrap();
    let conn = read.connection.as_ref().unwrap();
    assert_eq!(pragma_value(conn, "query_only"), 1);
    let write_error = conn
        .execute("DELETE FROM printer_profiles", [])
        .unwrap_err()
        .to_string();
    assert!(
        write_error.to_lowercase().contains("readonly"),
        "{write_error}"
    );
    let through_pool = sqlite_sequence_rows(conn);
    assert!(
        through_pool
            .iter()
            .any(|(name, _)| name == "printer_profiles")
            && through_pool
                .iter()
                .any(|(name, _)| name == "process_profiles")
            && through_pool
                .iter()
                .any(|(name, _)| name == "filament_profiles"),
        "starter seeds did not allocate sequences: {through_pool:?}"
    );
    drop(read);
    owner.shutdown().unwrap();

    let db = path.join("print-partner.db");
    let raw = Connection::open(&db).unwrap();
    assert_eq!(
        sqlite_sequence_rows(&raw),
        through_pool,
        "raw connection that skips startup moved sqlite_sequence"
    );
    drop(raw);

    let (owner, _) = WriterOwner::open_at(&path, Limits::default(), clock).unwrap();
    owner.shutdown().unwrap();
    let raw = Connection::open(&db).unwrap();
    assert_eq!(
        sqlite_sequence_rows(&raw),
        through_pool,
        "second WriterOwner open advanced sqlite_sequence"
    );

    let edited = raw
        .execute(
            "UPDATE filament_profiles SET resolved_flat_config = 'edited'
              WHERE tenant_id = 'default' AND name = 'Generic PLA'",
            [],
        )
        .unwrap();
    assert_eq!(edited, 1);
    let deleted = raw
        .execute(
            "DELETE FROM printer_profiles
              WHERE tenant_id = 'default' AND name = 'Generic FDM Machine'",
            [],
        )
        .unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(
        sqlite_sequence_rows(&raw),
        through_pool,
        "raw starter edit changed sqlite_sequence"
    );
    let previous_printer = through_pool
        .iter()
        .find(|(name, _)| name == "printer_profiles")
        .unwrap()
        .1;
    drop(raw);

    let (owner, _) = WriterOwner::open_at(&path, Limits::default(), clock).unwrap();
    owner.shutdown().unwrap();
    let raw = Connection::open(&db).unwrap();
    let pla: String = raw
        .query_row(
            "SELECT resolved_flat_config FROM filament_profiles
              WHERE tenant_id = 'default' AND name = 'Generic PLA'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pla, "edited");
    let restored_id: i64 = raw
        .query_row(
            "SELECT id FROM printer_profiles
              WHERE tenant_id = 'default' AND name = 'Generic FDM Machine'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(restored_id, previous_printer + 1);
    let mut expected = through_pool;
    expected
        .iter_mut()
        .find(|(name, _)| name == "printer_profiles")
        .unwrap()
        .1 = previous_printer + 1;
    assert_eq!(sqlite_sequence_rows(&raw), expected);
}

fn user_version(connection: &Connection) -> i64 {
    connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap()
}

fn starter_row_counts(connection: &Connection) -> Vec<(String, i64)> {
    ["filament_profiles", "printer_profiles", "process_profiles"]
        .into_iter()
        .map(|table| {
            let count: i64 = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (table.to_owned(), count)
        })
        .collect()
}

#[test]
fn ticket_t_7_writer_replays_an_existing_node_seed_only_once() {
    let path = directory("t7-node-seed-replay");
    let clock = "2026-10-02T00:00:00.000Z";
    let (owner, _) = WriterOwner::open_at(&path, Limits::default(), clock).unwrap();
    owner.shutdown().unwrap();
    let db = path.join("print-partner.db");
    let raw = Connection::open(&db).unwrap();
    assert_eq!(user_version(&raw), 1);
    // A generated schema-31 input is a Node database that already contains the
    // starters and has not recorded this Rust replay. Clearing the latch
    // reproduces that input.
    raw.execute_batch("PRAGMA user_version = 0").unwrap();
    let before = sqlite_sequence_rows(&raw);
    let counts = starter_row_counts(&raw);
    drop(raw);

    let (owner, _) = WriterOwner::open_at(&path, Limits::default(), clock).unwrap();
    owner.shutdown().unwrap();
    let raw = Connection::open(&db).unwrap();
    assert_eq!(user_version(&raw), 1);
    let replayed = sqlite_sequence_rows(&raw);
    for (name, count) in &counts {
        let old = before.iter().find(|(row, _)| row == name).unwrap().1;
        let new = replayed.iter().find(|(row, _)| row == name).unwrap().1;
        assert_eq!(new, old + count, "{name}");
    }
    for (name, seq) in &before {
        if counts.iter().any(|(starter, _)| starter == name) {
            continue;
        }
        assert_eq!(
            replayed
                .iter()
                .find(|(row, _)| row == name)
                .map(|(_, value)| *value),
            Some(*seq),
            "{name} moved during the one Node-equivalent replay"
        );
    }
    drop(raw);

    let (owner, _) = WriterOwner::open_at(&path, Limits::default(), clock).unwrap();
    owner.shutdown().unwrap();
    let raw = Connection::open(&db).unwrap();
    assert_eq!(sqlite_sequence_rows(&raw), replayed);
}

pub(super) fn pragma_value(connection: &Connection, name: &str) -> i64 {
    connection
        .pragma_query_value(None, name, |row| row.get(0))
        .unwrap()
}

#[test]
fn ticket_t_7_writer_rolls_back_and_refuses_a_second_runtime() {
    let (path, owner, client) = fixture("t7-rollback");
    let second = match WriterOwner::open(&path, Limits::default()) {
        Ok(_) => panic!("second runtime opened the same directory"),
        Err(error) => error.to_string(),
    };
    assert!(second.contains("Data directory already owned"), "{second}");
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute_batch("CREATE TRIGGER reject_partial BEFORE INSERT ON app_settings WHEN NEW.key='reject' BEGIN INSERT INTO app_settings VALUES('test','partial','leaked'); SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    let rejected = set(&client, "reject", "no")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(rejected.contains("injected failure"), "{rejected}");
    set(&client, "stable", "v1")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    let mismatched = client
        .submit(
            SettingCommand::CompareAndSet {
                setting: Setting {
                    tenant: "test".into(),
                    key: "stable".into(),
                    value: "v2".into(),
                },
                expected: SettingSnapshot::Missing,
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(!mismatched.recv().unwrap().unwrap());
    let matched = client
        .submit(
            SettingCommand::CompareAndSet {
                setting: Setting {
                    tenant: "test".into(),
                    key: "stable".into(),
                    value: "v2".into(),
                },
                expected: SettingSnapshot::Stored { value: "v1".into() },
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(matched.recv().unwrap().unwrap());
    drop(raw);
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let read = owner.client().reader(Duration::ZERO).unwrap();
    assert_eq!(
        read.snapshot("test", "partial").unwrap(),
        SettingSnapshot::Missing
    );
    assert_eq!(
        read.snapshot("test", "reject").unwrap(),
        SettingSnapshot::Missing
    );
    assert_eq!(
        read.snapshot("test", "stable").unwrap(),
        SettingSnapshot::Stored { value: "v2".into() }
    );
    drop(read);
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_7_writer_bounds_queue_contention_and_cancellation() {
    let (path, owner, client) = fixture("t7-queue");
    let raw = Connection::open(path.join("print-partner.db")).unwrap();
    raw.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let blocking = set(&client, "blocking", "yes").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.shared.queue.lock().unwrap().pending.is_empty() {
        assert!(Instant::now() < deadline, "writer never reached the lock");
        thread::yield_now();
    }
    let hidden = client.reader(Duration::ZERO).unwrap();
    assert_eq!(
        hidden.snapshot("test", "blocking").unwrap(),
        SettingSnapshot::Missing
    );
    drop(hidden);
    let admitted = set(&client, "admitted", "once").unwrap();
    let overflow = set(&client, "overflow", "no").unwrap_err().to_string();
    assert!(overflow.contains("Writer queue full"), "{overflow}");
    let cancel = Arc::new(AtomicBool::new(false));
    let waiting_client = client.clone();
    let waiting_cancel = cancel.clone();
    let waiting = thread::spawn(move || {
        waiting_client
            .submit(
                SettingCommand::Set(Setting {
                    tenant: "test".into(),
                    key: "cancelled".into(),
                    value: "no".into(),
                }),
                &waiting_cancel,
                Duration::from_secs(5),
            )
            .unwrap_err()
            .to_string()
    });
    cancel.store(true, Ordering::Release);
    let cancelled = waiting.join().unwrap();
    assert!(
        cancelled.contains("Cancelled before admission"),
        "{cancelled}"
    );
    raw.execute_batch("COMMIT").unwrap();
    assert!(blocking.recv().unwrap().unwrap());
    assert!(admitted.recv().unwrap().unwrap());
    drop(raw);
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let read = owner.client().reader(Duration::ZERO).unwrap();
    assert_eq!(
        read.snapshot("test", "blocking").unwrap(),
        SettingSnapshot::Stored {
            value: "yes".into()
        }
    );
    assert_eq!(
        read.snapshot("test", "admitted").unwrap(),
        SettingSnapshot::Stored {
            value: "once".into()
        }
    );
    for key in ["overflow", "cancelled"] {
        assert_eq!(
            read.snapshot("test", key).unwrap(),
            SettingSnapshot::Missing
        );
    }
    drop(read);
    owner.shutdown().unwrap();
}

#[cfg(unix)]
#[test]
fn ticket_t_7_writer_denies_managed_alias_backups_and_reopens() {
    use fs2::FileExt;
    use std::os::unix::fs::{MetadataExt, symlink};
    let (path, owner, client) = fixture("t7-backup");
    set(&client, "kept-a", "committed")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    set(&client, "kept-b", "committed")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    assert!(path.join("print-partner.db-wal").metadata().unwrap().len() > 32);
    let lock = path.join("printpartner.lock");
    let inode = std::fs::metadata(&lock).unwrap().ino();
    let marker = std::fs::read(path.join(".desktop-lease")).unwrap();
    let aliases = directory("t7-aliases");
    symlink(path.join("print-partner.db-wal"), aliases.join("wal-alias")).unwrap();
    symlink(&path, aliases.join("data")).unwrap();
    for target in [
        path.join("print-partner.db"),
        path.join("print-partner.db-wal"),
        path.join("printpartner.lock"),
        path.join(".desktop-lease"),
        aliases.join("wal-alias"),
        aliases.join("data/print-partner.db"),
    ] {
        let error = owner.backup(&target).unwrap_err().to_string();
        assert!(
            error.contains("managed storage") || error.contains("must be a new file"),
            "{target:?} {error}"
        );
        assert_eq!(std::fs::metadata(&lock).unwrap().ino(), inode);
        assert_eq!(std::fs::read(path.join(".desktop-lease")).unwrap(), marker);
        assert!(
            std::fs::File::open(&lock)
                .unwrap()
                .try_lock_exclusive()
                .is_err()
        );
    }
    let exported = aliases.join("offline.db");
    owner.backup(&exported).unwrap();
    owner.shutdown().unwrap();
    let backup = Connection::open_with_flags(&exported, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let read = owner.client().reader(Duration::ZERO).unwrap();
    for key in ["kept-a", "kept-b"] {
        let expected = SettingSnapshot::Stored {
            value: "committed".into(),
        };
        assert_eq!(read.snapshot("test", key).unwrap(), expected);
        assert_eq!(snapshot(&backup, "test", key).unwrap(), expected);
    }
    drop(read);
    drop(backup);
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_7_writer_retains_ownership_when_cleanup_fails() {
    use fs2::FileExt;
    let (path, owner, client) = fixture("t7-cleanup");
    set(&client, "kept", "yes")
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    let marker = std::fs::read(path.join(".desktop-lease")).unwrap();
    let runtime = owner.lease.as_ref().unwrap().runtime_dir().to_owned();
    std::fs::write(runtime.join("block-release"), b"retain").unwrap();
    let error = owner.shutdown().unwrap_err().to_string();
    assert!(
        error.contains(
            "Storage cleanup failed; ownership retained until explicit retry or process exit"
        ),
        "{error}"
    );
    assert_eq!(std::fs::read(path.join(".desktop-lease")).unwrap(), marker);
    let blocked = match WriterOwner::open(&path, Limits::default()) {
        Ok(_) => panic!("cleanup failure released ownership"),
        Err(error) => error.to_string(),
    };
    assert!(
        blocked.contains("already owned") || blocked.contains("may still be alive"),
        "{blocked}"
    );
    assert!(
        std::fs::File::open(path.join("printpartner.lock"))
            .unwrap()
            .try_lock_exclusive()
            .is_err()
    );
    let retained = lease::StorageLease::retry_failed_release(&path).unwrap();
    assert!(retained.ownership_retained);
    assert!(!retained.marker_removed);
    assert!(!retained.lock_released);
    std::fs::remove_file(runtime.join("block-release")).unwrap();
    let released = lease::StorageLease::retry_failed_release(&path).unwrap();
    assert!(released.marker_removed);
    assert!(released.runtime_removed);
    assert!(released.lock_released);
    assert!(!released.ownership_retained);
    let (owner, _) = WriterOwner::open(&path, Limits::default()).unwrap();
    let read = owner.client().reader(Duration::ZERO).unwrap();
    assert_eq!(
        read.snapshot("test", "kept").unwrap(),
        SettingSnapshot::Stored {
            value: "yes".into()
        }
    );
    drop(read);
    owner.shutdown().unwrap();
}

struct PinnedSchema {
    commit: &'static str,
    sha256: &'static str,
    released_version: u64,
    bytes: &'static [u8],
}

const HISTORICAL_SCHEMA_31: PinnedSchema = PinnedSchema {
    commit: "87b30cd13cfa496829edbef6e6d1c25be29e92d1",
    sha256: "6921e78e0b65be4d08c635e55fe1428b6a4bf071862982d0e46337a55cf4eb1e",
    released_version: 31,
    bytes: include_bytes!("../data/historical/schema-31.ts"),
};
const HISTORICAL_SCHEMA_33: PinnedSchema = PinnedSchema {
    commit: "ac0153b682d9565b8b2255b2b2747bee7f9b82d2",
    sha256: "3023074597eb6462866892aceaa1433dcb69b0cc68abfa8479cf920e2be0130f",
    released_version: 33,
    bytes: include_bytes!("../data/historical/schema-33.ts"),
};
const HISTORICAL_SCHEMA_34: PinnedSchema = PinnedSchema {
    commit: "71e25e7731feddd0eaec05a0d1070dc13de790a7",
    sha256: "f5755e7a045fcb8b638bbeb17321c57ff6737d0d6db6b317518c41a14780d8b1",
    released_version: 34,
    bytes: include_bytes!("../data/historical/schema-34.ts"),
};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn historical_source(pinned: &PinnedSchema) -> String {
    let digest = hex::encode(Sha256::digest(pinned.bytes));
    assert_eq!(digest, pinned.sha256, "{}", pinned.commit);
    let spec = format!("{}:web/apps/server/src/db/schema.ts", pinned.commit);
    let probe = Command::new("git")
        .args(["cat-file", "-e", &spec])
        .current_dir(repo_root())
        .output()
        .unwrap();
    if probe.status.success() {
        let object = Command::new("git")
            .args(["cat-file", "-p", &spec])
            .current_dir(repo_root())
            .output()
            .unwrap();
        assert!(
            object.status.success(),
            "{}",
            String::from_utf8_lossy(&object.stderr)
        );
        assert_eq!(
            object.stdout, pinned.bytes,
            "{spec} does not match the pinned historical source"
        );
    }
    std::str::from_utf8(pinned.bytes).unwrap().to_owned()
}

fn stamp_schema_version(connection: &Connection, version: u64) {
    node_schema::stamp_schema_version(connection, version).unwrap();
}

fn build_historical_database(
    name: &str,
    statements: &[HistoricalStatement],
    version: u64,
) -> PathBuf {
    let root = directory(name);
    let connection = Connection::open(root.join("print-partner.db")).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    execute_historical(&connection, statements).unwrap();
    stamp_schema_version(&connection, version);
    drop(connection);
    root
}

fn upload_columns() -> Vec<(&'static str, &'static str, &'static str)> {
    #[cfg(feature = "desktop_rebuild")]
    return crate::uploads::added_columns().unwrap();
    #[cfg(not(feature = "desktop_rebuild"))]
    Vec::new()
}

fn table_columns(db: &Path, table: &str) -> Vec<String> {
    let connection = Connection::open(db).unwrap();
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({})", quote_ident(table)))
        .unwrap();
    let added = upload_columns();
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(|name| name.unwrap())
        .filter(|name| {
            !added
                .iter()
                .any(|(owner, column, _)| *owner == table && column == name)
        })
        .collect()
}

fn historical_diff_names(diff: &[String]) -> BTreeSet<String> {
    diff.iter()
        .map(|line| {
            let mut parts = line.splitn(3, ' ');
            match parts.next().unwrap() {
                "object" => {
                    parts.next().unwrap();
                    parts.next().unwrap().to_owned()
                }
                "columns" | "foreign_keys" | "rows" => parts.next().unwrap().to_owned(),
                other => panic!("unexpected graph diff {other}: {line}"),
            }
        })
        .collect()
}

fn without_sequence_rows(diff: &mut Vec<String>, left: &SchemaGraph, right: &SchemaGraph) {
    if sequence_identities(left) == sequence_identities(right) {
        diff.retain(|line| line != "rows sqlite_sequence");
    }
}

// Replays pinned schema.ts migrations from the commits that released schema 31, 33, and 34.
// Schema 32 is that v32 statement applied to the schema 31 list. No release assigned
// currentSchemaVersion = 32. Passing this test does not claim AppStartupReady.
#[test]
fn ticket_t_7_migrations_historical_order() {
    let source_31 = historical_source(&HISTORICAL_SCHEMA_31);
    let source_33 = historical_source(&HISTORICAL_SCHEMA_33);
    let source_34 = historical_source(&HISTORICAL_SCHEMA_34);
    for source in [&source_31, &source_33, &source_34] {
        assert!(!source.contains("currentSchemaVersion = 32"));
    }
    assert!(source_31.contains("export const currentSchemaVersion = 31;"));
    assert!(source_33.contains("export const currentSchemaVersion = 33;"));
    assert!(source_34.contains("export const currentSchemaVersion = 34;"));
    assert_eq!(
        [
            HISTORICAL_SCHEMA_31.released_version,
            HISTORICAL_SCHEMA_33.released_version,
            HISTORICAL_SCHEMA_34.released_version
        ],
        [31, 33, 34]
    );
    let statements_31 = parse_schema_migrations(&source_31).unwrap();
    let statements_33 = parse_schema_migrations(&source_33).unwrap();
    let statements_34 = parse_schema_migrations(&source_34).unwrap();
    assert_eq!(statements_31.len(), 175);
    assert_eq!(statements_33.len(), 176);
    assert_eq!(statements_34.len(), 180);
    let v32_step: Vec<_> = statements_33
        .iter()
        .filter(|statement| {
            statement
                .comments
                .iter()
                .any(|comment| comment.contains("v32"))
        })
        .collect();
    assert_eq!(v32_step.len(), 1);
    assert_eq!(
        v32_step[0].comments,
        ["// v32 - durable cutover from mutable workspace manifests to Source revisions."]
    );
    assert!(statements_31.iter().all(|statement| {
        !statement
            .comments
            .iter()
            .any(|comment| comment.contains("v32"))
    }));
    assert_ne!(
        statements_31
            .iter()
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>(),
        statements_33[..statements_31.len()]
            .iter()
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(statements_33.last().unwrap().sql, v32_step[0].sql);
    assert_eq!(
        statements_34[..statements_33.len()]
            .iter()
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>(),
        statements_33
            .iter()
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>()
    );
    let mut statements_32 = statements_31.clone();
    statements_32.push(HistoricalStatement {
        comments: v32_step[0].comments.clone(),
        sql: v32_step[0].sql.clone(),
    });

    let schema_json: serde_json::Value =
        serde_json::from_str(include_str!("../data/schema.json")).unwrap();
    let rust_migrations = schema_json["migrations"].as_array().unwrap();
    // Main has 169 statements. Auth adds sessions.provider.
    assert_eq!(rust_migrations.len(), 170);
    assert_eq!(schema_json["repair"].as_array().unwrap().len(), 3);
    assert_eq!(schema_json["extras"].as_array().unwrap().len(), 2);
    assert_eq!(schema_json["seeds"].as_array().unwrap().len(), 3);
    assert!(
        schema_json["stranded"]
            .as_str()
            .unwrap()
            .contains("source_revisions")
    );
    let schema_text = include_str!("../data/schema.json");
    assert!(!schema_text.contains("print_outcomes"));
    assert!(!schema_text.contains("advisor"));
    let app = std::fs::read_to_string(repo_root().join("web/apps/server/src/app.ts")).unwrap();
    assert!(app.contains("migratePrintOutcomesBlob"));
    assert!(app.contains("backfillAdvisorNotesFromDomainPack"));
    let board: Vec<_> = rust_migrations
        .iter()
        .filter_map(|statement| statement.as_str())
        .filter(|statement| statement.contains("board_"))
        .collect();
    assert_eq!(board.len(), 4);
    let historical_board: Vec<_> = statements_34[statements_33.len()..]
        .iter()
        .map(|statement| statement.sql.as_str())
        .collect();
    assert_eq!(board, historical_board);
    assert_ne!(
        rust_migrations
            .iter()
            .filter_map(|statement| statement.as_str())
            .collect::<Vec<_>>(),
        statements_34
            .iter()
            .map(|statement| statement.sql.as_str())
            .collect::<Vec<_>>()
    );

    let built = [
        (
            "schema-31",
            31u64,
            build_historical_database("historical-31", &statements_31, 31),
        ),
        (
            "schema-32",
            32,
            build_historical_database("historical-32", &statements_32, 32),
        ),
        (
            "schema-33",
            33,
            build_historical_database("historical-33", &statements_33, 33),
        ),
        (
            "schema-34",
            34,
            build_historical_database("historical-34", &statements_34, 34),
        ),
    ];
    let db = |label: &str| {
        built
            .iter()
            .find(|(name, ..)| *name == label)
            .unwrap()
            .2
            .join("print-partner.db")
    };
    let columns = |label: &str| table_columns(&db(label), "projects");
    assert!(!columns("schema-31").contains(&"legacy_manifest_cutover".to_owned()));
    assert_eq!(
        columns("schema-32").last().unwrap(),
        "legacy_manifest_cutover"
    );
    let columns_33 = columns("schema-33");
    let metadata = columns_33
        .iter()
        .position(|name| name == "metadata_json")
        .unwrap();
    let legacy = columns_33
        .iter()
        .position(|name| name == "legacy_manifest_cutover")
        .unwrap();
    let pointer = columns_33
        .iter()
        .position(|name| name == "current_source_revision_id")
        .unwrap();
    assert!(metadata + 1 == legacy && legacy < pointer);
    assert_ne!(columns("schema-32"), columns_33);
    for label in ["schema-31", "schema-32", "schema-33"] {
        let connection = Connection::open(db(label)).unwrap();
        assert!(!table_present(&connection, "board_posts"));
        assert!(!table_present(&connection, "board_comments"));
    }
    let current = Connection::open(db("schema-34")).unwrap();
    assert!(table_present(&current, "board_posts"));
    assert!(table_present(&current, "board_comments"));
    drop(current);
    let graphs: Vec<_> = built
        .iter()
        .map(|(label, version, root)| {
            let graph = read_schema_graph(&root.join("print-partner.db"));
            assert_eq!(graph.version, version.to_string(), "{label}");
            graph
        })
        .collect();
    for pair in [(0, 1), (1, 2), (2, 3)] {
        let diff = graph_diff(&graphs[pair.0], &graphs[pair.1]);
        assert!(!diff.is_empty(), "{pair:?} historical graphs matched");
    }

    let stamped = directory("historical-stamp-only");
    std::fs::copy(db("schema-34"), stamped.join("print-partner.db")).unwrap();
    let stamped_db = Connection::open(stamped.join("print-partner.db")).unwrap();
    stamp_schema_version(&stamped_db, 32);
    drop(stamped_db);
    let stamped_graph = read_schema_graph(&stamped.join("print-partner.db"));
    assert_eq!(stamped_graph.version, "32");
    assert!(table_present(
        &Connection::open(stamped.join("print-partner.db")).unwrap(),
        "board_posts"
    ));
    assert!(!graph_diff(&graphs[1], &stamped_graph).is_empty());
    assert_ne!(
        table_columns(&stamped.join("print-partner.db"), "projects"),
        columns("schema-32")
    );

    let mut ordered_32 = statements_32.clone();
    let step = ordered_32.pop().unwrap();
    ordered_32.insert(0, step);
    let memory = Connection::open_in_memory().unwrap();
    memory.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let early_alter = execute_historical(&memory, &ordered_32);
    assert!(
        early_alter
            .as_ref()
            .unwrap_err()
            .to_lowercase()
            .contains("no such table"),
        "{early_alter:?}"
    );
    let board_index = statements_34
        .iter()
        .find(|statement| statement.sql.contains("idx_board_posts_created"))
        .unwrap();
    let index_first = Connection::open_in_memory().unwrap();
    let index_error = index_first
        .execute_batch(&board_index.sql)
        .unwrap_err()
        .to_string();
    assert!(
        index_error.to_lowercase().contains("no such table"),
        "{index_error}"
    );
    let mut drifted = graphs[0].clone();
    let trigger = drifted
        .objects
        .iter_mut()
        .find(|((kind, _), _)| kind == "trigger")
        .unwrap();
    trigger.1.1.as_mut().unwrap().push_str(" /*drift*/");
    assert!(!graph_diff(&graphs[0], &drifted).is_empty());

    let fresh_dir = directory("historical-fresh");
    let (fresh_owner, fresh_ready) = open_parity(&fresh_dir, "2026-10-02T00:00:00.000Z");
    assert_eq!(fresh_ready.version, schema::CURRENT_VERSION);
    assert_eq!(fresh_ready.previous_version, 0);
    assert!(fresh_ready.backup.is_none());
    fresh_owner.shutdown().unwrap();
    let fresh_graph = read_schema_graph(&fresh_dir.join("print-partner.db"));
    for (label, version, source) in &built {
        let before = tree_hashes(source);
        let work = directory(&format!("historical-upgrade-{label}"));
        copy_db_files(source, &work);
        let (owner, ready) = open_parity(&work, "2026-10-02T00:00:00.000Z");
        assert_eq!(ready.version, schema::CURRENT_VERSION, "{label}");
        assert_eq!(ready.previous_version, *version, "{label}");
        match ready.backup.as_ref() {
            Some(path) if *version < 35 => {
                assert_eq!(schema_version_at(path), version.to_string())
            }
            Some(_) => panic!("{label} backed up a current database"),
            None => assert_eq!(*version, 35, "{label}"),
        }
        owner.shutdown().unwrap();
        assert_eq!(tree_hashes(source), before, "{label} source changed");
        let observed = read_schema_graph(&work.join("print-partner.db"));
        let mut diff = graph_diff(&fresh_graph, &observed);
        without_sequence_rows(&mut diff, &fresh_graph, &observed);
        let names = historical_diff_names(&diff);
        // legacy_manifest_cutover keeps the ALTER position on schemas 31 and 32.
        // Schema 33+ creates that column in place. The sets match; only the order differs.
        // Historical snapshots create sessions without provider. ALTER appends it,
        // so sqlite_master text differs from the fresh CREATE while the columns match.
        let mut expected = BTreeSet::from(["sessions".to_owned()]);
        if *version < 33 {
            expected.insert("projects".to_owned());
        }
        assert_eq!(names, expected, "{label} upgrade vs fresh: {diff:#?}");
        let fresh_columns = table_columns(&fresh_dir.join("print-partner.db"), "projects");
        let upgraded_columns = table_columns(&work.join("print-partner.db"), "projects");
        let mut fresh_order = fresh_columns.clone();
        let mut upgraded_order = upgraded_columns.clone();
        fresh_order.sort();
        upgraded_order.sort();
        assert_eq!(fresh_order, upgraded_order, "{label}");
        assert_second_open_keeps_catalog(&work, label);
        if *version < 33 {
            assert_ne!(fresh_columns, upgraded_columns, "{label}");
            assert_eq!(
                upgraded_columns.last().map(String::as_str),
                Some("legacy_manifest_cutover"),
                "{label}"
            );
            let metadata = fresh_columns
                .iter()
                .position(|name| name == "metadata_json")
                .unwrap();
            assert_eq!(
                fresh_columns.get(metadata + 1).map(String::as_str),
                Some("legacy_manifest_cutover"),
                "{label}"
            );
        } else {
            assert_eq!(fresh_columns, upgraded_columns, "{label}");
        }
        let fresh_sessions = table_columns(&fresh_dir.join("print-partner.db"), "sessions");
        let upgraded_sessions = table_columns(&work.join("print-partner.db"), "sessions");
        assert_eq!(fresh_sessions, upgraded_sessions, "{label} sessions");
        assert_eq!(
            fresh_sessions.last().map(String::as_str),
            Some("provider"),
            "{label}"
        );
    }
    assert_second_open_keeps_catalog(&fresh_dir, "fresh");
}

#[cfg(unix)]
fn plan_input_column_defs(db: &Path) -> Vec<String> {
    let connection = Connection::open(db).unwrap();
    let mut statement = connection
        .prepare("PRAGMA table_info(plan_revision_inputs)")
        .unwrap();
    statement
        .query_map([], |row| {
            Ok(format!(
                "{}|{}|{}|{}|{}",
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                value_token(row.get(4)?),
                row.get::<_, i64>(5)?,
            ))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

#[cfg(unix)]
fn plan_input_index_defs(db: &Path) -> Vec<String> {
    let connection = Connection::open(db).unwrap();
    let mut listed = connection
        .prepare("PRAGMA index_list(plan_revision_inputs)")
        .unwrap();
    let indexes: Vec<(String, i64)> = listed
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .filter(|(name, _)| !name.starts_with("sqlite_"))
        .collect();
    drop(listed);
    let mut defs = Vec::new();
    for (name, unique) in indexes {
        let mut info = connection
            .prepare(&format!("PRAGMA index_info({})", quote_ident(&name)))
            .unwrap();
        let columns: Vec<String> = info
            .query_map([], |row| row.get::<_, String>(2))
            .unwrap()
            .map(|column| column.unwrap())
            .collect();
        drop(info);
        let sql: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = ?1",
                [&name],
                |row| row.get(0),
            )
            .unwrap();
        let identity = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let predicate = identity
            .split_once(" WHERE ")
            .map(|(_, predicate)| predicate.to_owned())
            .unwrap_or_default();
        defs.push(format!("{name}|{unique}|{}|{predicate}", columns.join(",")));
    }
    defs.sort();
    defs
}

#[cfg(unix)]
fn plan_input_rows(db: &Path) -> Vec<String> {
    let connection = Connection::open(db).unwrap();
    let mut statement = connection
        .prepare(
            "SELECT id, tenant_id, input_set_id, source_id, source_layer, layer_order,
                    tracking_kind, source_revision_id, manifest_digest, effective_naming_digest
               FROM plan_revision_inputs
              ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([], |row| {
            let mut cells = Vec::new();
            for index in 0..10 {
                cells.push(value_token(row.get(index)?));
            }
            Ok(cells.join("|"))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

#[cfg(unix)]
const HISTORICAL_NODE_CLIENT_SHA256: &str =
    "283c23182a1370c0ebfb9a56194db6f859ba763b178d10ec52dca7af780a72e5";

#[cfg(unix)]
fn materialize_historical_node() -> PathBuf {
    let root = directory("historical-node-src");
    let archive = Command::new("git")
        .args([
            "archive",
            "--format=tar",
            HISTORICAL_SCHEMA_34.commit,
            "web/apps/server/src/db",
            "web/apps/server/src/services",
        ])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(
        archive.status.success(),
        "historical Node client {} is unavailable: {}",
        HISTORICAL_SCHEMA_34.commit,
        String::from_utf8_lossy(&archive.stderr)
    );
    let archive_path = root.join("historical-node.tar");
    std::fs::write(&archive_path, &archive.stdout).unwrap();
    let extracted = Command::new("tar")
        .arg("-xf")
        .arg(&archive_path)
        .arg("-C")
        .arg(&root)
        .status()
        .unwrap();
    std::fs::remove_file(archive_path).unwrap();
    assert!(extracted.success(), "tar extract failed");
    let client = std::fs::read(root.join("web/apps/server/src/db/client.ts")).unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(&client)),
        HISTORICAL_NODE_CLIENT_SHA256
    );
    let rebuild = include_bytes!("../data/historical/plan-revision-inputs-rebuild.sql");
    assert_eq!(
        hex::encode(Sha256::digest(rebuild)),
        "a94c8cd765d5b963f50ed8741d9f05642237f0fcc9d4d28c119e64f484b31c67"
    );
    assert!(
        client
            .windows(rebuild.len())
            .any(|window| window == rebuild),
        "vendored rebuild SQL is not the historical client body"
    );
    std::os::unix::fs::symlink(
        repo_root().join("web/node_modules"),
        root.join("node_modules"),
    )
    .unwrap();
    std::fs::write(
        root.join("drive.mts"),
        "import { SqliteDatabase } from \"./web/apps/server/src/db/client.ts\";\nconst database = new SqliteDatabase(process.argv[2]);\ndatabase.connect();\ndatabase.close();\n",
    )
    .unwrap();
    root
}

#[cfg(unix)]
fn materialize_current_node_driver() -> PathBuf {
    let root = directory("current-node-driver");
    let client = serde_json::to_string(
        &repo_root()
            .join("web/apps/server/src/db/client.ts")
            .to_string_lossy()
            .as_ref(),
    )
    .unwrap();
    std::fs::write(
        root.join("drive.mts"),
        format!(
            "import {{ SqliteDatabase }} from {client};\nconst database = new SqliteDatabase(process.argv[2]);\ndatabase.connect();\ndatabase.close();\n"
        ),
    )
    .unwrap();
    root
}

#[cfg(unix)]
fn run_node_driver(driver_dir: &Path, data_dir: &Path) {
    let output = Command::new(schema_parity_node())
        .arg(repo_root().join("web/node_modules/tsx/dist/cli.mjs"))
        .arg(driver_dir.join("drive.mts"))
        .arg(data_dir)
        .env("NODE_PATH", repo_root().join("web/node_modules"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "node upgrade failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
fn apply_historical_node_upgrade(data_dir: &Path) {
    let client = materialize_historical_node();
    run_node_driver(&client, data_dir);
}

#[cfg(unix)]
fn blank_imported_at(graph: &mut SchemaGraph) {
    for table in ["printer_profiles", "process_profiles", "filament_profiles"] {
        let Some(columns) = graph.columns.get(table) else {
            continue;
        };
        let Some(index) = columns
            .iter()
            .position(|line| line.split('\u{1f}').nth(1) == Some("imported_at"))
        else {
            continue;
        };
        let Some(rows) = graph.rows.get_mut(table) else {
            continue;
        };
        for row in rows {
            let mut cells: Vec<String> = row.split('\u{1e}').map(str::to_owned).collect();
            if index < cells.len() {
                cells[index] = "'<clock>'".to_owned();
            }
            *row = cells.join("\u{1e}");
        }
    }
}

#[cfg(unix)]
fn drop_empty_plan_sequence(graph: &mut SchemaGraph) {
    let Some(rows) = graph.rows.get_mut("sqlite_sequence") else {
        return;
    };
    rows.retain(|row| {
        let mut cells = row.split('\u{1e}');
        let name = cells.next().unwrap_or("");
        let seq = cells.next().unwrap_or("");
        !(name == "'plan_revision_inputs'" && seq == "0")
    });
}

#[cfg(unix)]
fn with_sql_identity(graph: &SchemaGraph) -> SchemaGraph {
    let mut graph = graph.clone();
    for (_, sql) in graph.objects.values_mut() {
        if let Some(sql) = sql.as_mut() {
            *sql = schema::sql_identity(sql);
        }
    }
    graph
}

#[cfg(feature = "desktop_rebuild")]
fn omit_source_schema(graph: &mut SchemaGraph) {
    let mut owned = crate::uploads::owned_objects().unwrap();
    owned.extend(
        graph
            .objects
            .keys()
            .filter(|(_, name)| {
                name.starts_with("source_scan_execution")
                    || name.starts_with("sqlite_autoindex_source_scan_execution")
                    || name.starts_with("source_sync_")
                    || name.starts_with("sqlite_autoindex_source_sync_")
                    || name.starts_with("domain_events")
                    || name == "plan_draft_headers"
                    || name == "build_profile_details"
                    || name == "plan_draft_header_receipts"
                    || name == "sqlite_autoindex_plan_draft_header_receipts_1"
            })
            .map(|(_, name)| name.clone()),
    );
    graph.objects.retain(|(_, name), _| !owned.contains(name));
    for name in &owned {
        graph.columns.remove(name);
        graph.foreign_keys.remove(name);
        graph.rows.remove(name);
    }
    let added = upload_columns();
    for (table, column, definition) in &added {
        if let Some((_, Some(sql))) = graph.objects.get_mut(&("table".into(), (*table).into())) {
            *sql = sql.replacen(&format!(", {definition}"), "", 1);
        }
        let Some(keys) = graph.foreign_keys.get_mut(*table) else {
            continue;
        };
        let field = |key: &str, index: usize| key.split('\u{1f}').nth(index).unwrap().to_owned();
        let dropped: BTreeSet<String> = keys
            .iter()
            .filter(|key| field(key, 3) == *column)
            .map(|key| field(key, 0))
            .collect();
        keys.retain(|key| !dropped.contains(&field(key, 0)));
        for key in keys.iter_mut() {
            let id: i64 = field(key, 0).parse().unwrap();
            let (_, rest) = key.split_once('\u{1f}').unwrap();
            *key = format!("{}\u{1f}{rest}", id - dropped.len() as i64);
        }
    }
    for (table, defs) in graph.columns.iter_mut() {
        let before = defs.len();
        defs.retain(|line| {
            let name = line.split('\u{1f}').nth(1).unwrap();
            !added
                .iter()
                .any(|(owner, column, _)| owner == table && *column == name)
        });
        if defs.len() < before
            && let Some(rows) = graph.rows.get_mut(table)
        {
            for row in rows.iter_mut() {
                *row = row
                    .split('\u{1e}')
                    .take(defs.len())
                    .collect::<Vec<_>>()
                    .join("\u{1e}");
            }
        }
    }
}

fn omit_durable_jobs(graph: &mut SchemaGraph) {
    #[cfg(feature = "desktop_rebuild")]
    {
        graph
            .objects
            .retain(|(_, name), _| !name.contains("plan_apply_admissions"));
        graph.columns.remove("plan_apply_admissions");
        graph.foreign_keys.remove("plan_apply_admissions");
        graph.rows.remove("plan_apply_admissions");
        omit_source_schema(graph);
    }
    let job_object = |name: &str| {
        name.starts_with("durable_job") || name.starts_with("sqlite_autoindex_durable_job")
    };
    graph.objects.retain(|(_, name), _| !job_object(name));
    graph
        .columns
        .retain(|name, _| !name.starts_with("durable_job"));
    graph
        .foreign_keys
        .retain(|name, _| !name.starts_with("durable_job"));
    graph
        .rows
        .retain(|name, _| !name.starts_with("durable_job"));
    if let Some(rows) = graph.rows.get_mut("sqlite_sequence") {
        rows.retain(|row| !row.contains("durable_job"));
    }
}

fn blank_schema_version_stamp(graph: &mut SchemaGraph) {
    graph.version = "node-catalog".into();
    let Some(columns) = graph.columns.get("app_settings") else {
        return;
    };
    let Some(key_index) = columns
        .iter()
        .position(|line| line.split('\u{1f}').nth(1) == Some("key"))
    else {
        return;
    };
    let Some(value_index) = columns
        .iter()
        .position(|line| line.split('\u{1f}').nth(1) == Some("value"))
    else {
        return;
    };
    let Some(rows) = graph.rows.get_mut("app_settings") else {
        return;
    };
    for row in rows {
        let mut cells: Vec<String> = row.split('\u{1e}').map(str::to_owned).collect();
        if cells.get(key_index).map(String::as_str) == Some("'schema_version'")
            && value_index < cells.len()
        {
            cells[value_index] = "'<stamp>'".into();
            *row = cells.join("\u{1e}");
        }
    }
}

/// Compare Node-owned objects separately from the jobs and publication schemas.
fn node_owned_catalog(node: &SchemaGraph, rust: &SchemaGraph) -> Vec<String> {
    let mut node = node.clone();
    let mut rust = rust.clone();
    omit_durable_jobs(&mut node);
    omit_durable_jobs(&mut rust);
    blank_schema_version_stamp(&mut node);
    blank_schema_version_stamp(&mut rust);
    graph_diff(&node, &rust)
}

#[cfg(unix)]
fn equivalent_catalog(left_db: &Path, right_db: &Path) -> Vec<String> {
    let mut left = with_sql_identity(&read_schema_graph(left_db));
    let mut right = with_sql_identity(&read_schema_graph(right_db));
    blank_imported_at(&mut left);
    blank_imported_at(&mut right);
    drop_empty_plan_sequence(&mut left);
    drop_empty_plan_sequence(&mut right);
    omit_durable_jobs(&mut left);
    omit_durable_jobs(&mut right);
    blank_schema_version_stamp(&mut left);
    blank_schema_version_stamp(&mut right);
    graph_diff(&left, &right)
}

#[cfg(unix)]
fn insert_historical_plan_inputs(db: &Path) {
    let connection = Connection::open(db).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    connection
        .execute_batch(
            "INSERT INTO projects (name, url) VALUES ('Widget', 'https://example.test/widget');
             INSERT INTO source_revisions (
               project_id, upstream_revision_key, manifest_digest, snapshot_locator, synced_at
             ) VALUES (1, 'abc', 'manifest', 'snap://widget', '2020-01-01T00:00:00Z');
             INSERT INTO build_profiles (name) VALUES ('Profile');
             INSERT INTO plan_revision_input_sets (
               profile_id, input_set_digest, expected_input_count, recorded_at
             ) VALUES (1, 'set', 2, '2020-01-01T00:00:00Z');
             INSERT INTO plan_revision_inputs (
               input_set_id, source_revision_id, manifest_digest, source_id, source_layer,
               layer_order, tracking_kind, effective_naming_digest
             ) VALUES (1, 1, 'manifest', 1, 'primary', 0, 'revision', 'names');
             INSERT INTO plan_revision_inputs (input_set_id, source_revision_id, manifest_digest)
             VALUES (1, 1, 'manifest-filled');",
        )
        .unwrap();
}

#[cfg(unix)]
#[test]
fn ticket_t_7_migrations_preserves_plan_revision_inputs() {
    let source_31 = historical_source(&HISTORICAL_SCHEMA_31);
    let statements = parse_schema_migrations(&source_31).unwrap();
    let historical = build_historical_database("plan-input-history", &statements, 31);
    insert_historical_plan_inputs(&historical.join("print-partner.db"));
    let before = tree_hashes(&historical);
    let node_dir = directory("plan-input-node");
    let rust_dir = directory("plan-input-rust");
    copy_db_files(&historical, &node_dir);
    copy_db_files(&historical, &rust_dir);
    apply_historical_node_upgrade(&node_dir);
    let (owner, ready) = open_parity(&rust_dir, "2026-10-02T00:00:00.000Z");
    assert_eq!(ready.version, schema::CURRENT_VERSION);
    assert_eq!(ready.previous_version, 31);
    assert_eq!(schema_version_at(ready.backup.as_ref().unwrap()), "31");
    let backup_rows = plan_input_rows(ready.backup.as_ref().unwrap());
    assert_eq!(
        backup_rows,
        [
            "1|default|1|1|primary|0|revision|1|manifest|names",
            "2|default|1|<null>|<null>|0|revision|1|manifest-filled|<null>",
        ]
    );
    owner.shutdown().unwrap();
    assert_eq!(tree_hashes(&historical), before);
    assert_eq!(
        plan_input_column_defs(&node_dir.join("print-partner.db")),
        plan_input_column_defs(&rust_dir.join("print-partner.db"))
    );
    assert_eq!(
        plan_input_index_defs(&node_dir.join("print-partner.db")),
        plan_input_index_defs(&rust_dir.join("print-partner.db"))
    );
    assert_eq!(
        plan_input_rows(&rust_dir.join("print-partner.db")),
        [
            "1|default|1|1|primary|0|revision|1|manifest|names",
            "2|default|1|1|legacy:1|0|revision|1|manifest-filled|<null>",
        ]
    );
    assert_eq!(
        plan_input_rows(&node_dir.join("print-partner.db")),
        plan_input_rows(&rust_dir.join("print-partner.db"))
    );
    let node_sql: String = Connection::open(node_dir.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'plan_revision_inputs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let rust_sql: String = Connection::open(rust_dir.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'plan_revision_inputs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        schema::sql_identity(&node_sql),
        schema::sql_identity(&rust_sql)
    );
    let fresh_dir = directory("plan-input-fresh");
    let (fresh_owner, _) = open_parity(&fresh_dir, "2026-10-02T00:00:00.000Z");
    fresh_owner.shutdown().unwrap();
    let fresh_trigger: String = Connection::open(fresh_dir.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'trg_accepted_plate_revisions_immutable_update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let rust_trigger: String = Connection::open(rust_dir.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'trg_accepted_plate_revisions_immutable_update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(fresh_trigger, rust_trigger);
    let node_trigger: String = Connection::open(node_dir.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'trg_accepted_plate_revisions_immutable_update'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        schema::sql_identity(&node_trigger),
        schema::sql_identity(&rust_trigger)
    );
    assert!(node_trigger.contains("Accepted Plate revision is immutable"));
    assert_second_open_keeps_catalog(&rust_dir, "plan-input-rust");

    let orphan_dir = directory("plan-input-orphan");
    copy_db_files(&historical, &orphan_dir);
    let orphan = Connection::open(orphan_dir.join("print-partner.db")).unwrap();
    orphan.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    orphan
        .execute(
            "INSERT INTO plan_revision_inputs (input_set_id, source_revision_id, manifest_digest, source_id, source_layer)
             VALUES (1, 99, 'missing', 1, 'primary')",
            [],
        )
        .unwrap();
    drop(orphan);
    let error = WriterOwner::open_at(
        &orphan_dir,
        Limits {
            queued_writes: 2,
            readers: 2,
        },
        "2026-10-02T00:00:00.000Z",
    )
    .err()
    .unwrap()
    .to_string();
    assert!(
        error.contains("Cannot migrate Plan revision input 3: Source revision is missing"),
        "{error}"
    );
    let retained = Connection::open(orphan_dir.join("print-partner.db")).unwrap();
    let notnull: i64 = retained
        .query_row(
            "SELECT \"notnull\" FROM pragma_table_info('plan_revision_inputs') WHERE name = 'source_revision_id'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(notnull, 1);
    let version: String = retained
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "31");
}

#[cfg(unix)]
fn pinned_historical_copies() -> Vec<(&'static str, u64, PathBuf)> {
    let source_31 = historical_source(&HISTORICAL_SCHEMA_31);
    let source_33 = historical_source(&HISTORICAL_SCHEMA_33);
    let statements_31 = parse_schema_migrations(&source_31).unwrap();
    let statements_33 = parse_schema_migrations(&source_33).unwrap();
    let v32 = statements_33
        .iter()
        .find(|statement| {
            statement
                .comments
                .iter()
                .any(|comment| comment.contains("v32"))
        })
        .unwrap();
    let mut statements_32 = statements_31.clone();
    statements_32.push(v32.clone());
    let statements_34 = parse_schema_migrations(&historical_source(&HISTORICAL_SCHEMA_34)).unwrap();
    vec![
        (
            "schema-31",
            31,
            build_historical_database("node-proof-31", &statements_31, 31),
        ),
        (
            "schema-32",
            32,
            build_historical_database("node-proof-32", &statements_32, 32),
        ),
        (
            "schema-33",
            33,
            build_historical_database("node-proof-33", &statements_33, 33),
        ),
        (
            "schema-34",
            34,
            build_historical_database("node-proof-34", &statements_34, 34),
        ),
    ]
}

// Historical Node connect() is the upgrade that repaired plan_revision_inputs.
// Current Node initialization no longer does. Rust must match that repair and
// the current fresh catalog, including the two plan-input indexes and the
// accepted-plate immutability trigger.
#[cfg(unix)]
#[test]
fn ticket_t_7_migrations_matches_node_upgrade() {
    let current_client =
        std::fs::read_to_string(repo_root().join("web/apps/server/src/db/client.ts")).unwrap();
    assert!(
        !current_client.contains("plan_revision_inputs_v17"),
        "current Node still rebuilds plan_revision_inputs"
    );
    let app = std::fs::read_to_string(repo_root().join("web/apps/server/src/app.ts")).unwrap();
    assert!(app.contains("migratePrintOutcomesBlob"));
    assert!(app.contains("backfillAdvisorNotesFromDomainPack"));
    let historical = materialize_historical_node();
    let current = materialize_current_node_driver();

    let node_fresh = directory("current-node-fresh");
    run_node_driver(&current, &node_fresh);
    let rust_fresh = directory("current-rust-fresh");
    let (owner, ready) = open_parity(&rust_fresh, "2026-10-02T00:00:00.000Z");
    assert_eq!(ready.previous_version, 0);
    assert!(ready.backup.is_none());
    owner.shutdown().unwrap();
    let fresh_diff = equivalent_catalog(
        &node_fresh.join("print-partner.db"),
        &rust_fresh.join("print-partner.db"),
    );
    assert!(
        fresh_diff.is_empty(),
        "current Node fresh vs Rust fresh: {fresh_diff:#?}"
    );

    for (label, version, source) in pinned_historical_copies() {
        let before = tree_hashes(&source);
        let node_dir = directory(&format!("node-upgrade-{label}"));
        let rust_dir = directory(&format!("rust-upgrade-{label}"));
        copy_db_files(&source, &node_dir);
        copy_db_files(&source, &rust_dir);
        insert_historical_plan_inputs(&node_dir.join("print-partner.db"));
        insert_historical_plan_inputs(&rust_dir.join("print-partner.db"));
        run_node_driver(&historical, &node_dir);
        let (owner, ready) = open_parity(&rust_dir, "2026-10-02T00:00:00.000Z");
        assert_eq!(ready.version, schema::CURRENT_VERSION, "{label}");
        assert_eq!(ready.previous_version, version, "{label}");
        if version < 35 {
            assert_eq!(
                schema_version_at(ready.backup.as_ref().unwrap()),
                version.to_string(),
                "{label}"
            );
        } else {
            assert!(ready.backup.is_none(), "{label}");
        }
        owner.shutdown().unwrap();
        assert_eq!(tree_hashes(&source), before, "{label} source changed");
        let diff = equivalent_catalog(
            &node_dir.join("print-partner.db"),
            &rust_dir.join("print-partner.db"),
        );
        // The pinned Node client predates sessions.provider. Current Rust appends it.
        let names = historical_diff_names(&diff);
        assert_eq!(
            names,
            BTreeSet::from(["sessions".to_owned()]),
            "{label} Node upgrade vs Rust: {diff:#?}"
        );
        let node_sessions = table_columns(&node_dir.join("print-partner.db"), "sessions");
        let rust_sessions = table_columns(&rust_dir.join("print-partner.db"), "sessions");
        assert!(
            !node_sessions.iter().any(|column| column == "provider"),
            "{label}"
        );
        let mut without_provider = rust_sessions.clone();
        assert_eq!(
            without_provider.pop().as_deref(),
            Some("provider"),
            "{label}"
        );
        assert_eq!(node_sessions, without_provider, "{label}");
        assert_eq!(
            table_columns(&node_dir.join("print-partner.db"), "projects"),
            table_columns(&rust_dir.join("print-partner.db"), "projects"),
            "{label}"
        );
        assert_eq!(
            plan_input_rows(&node_dir.join("print-partner.db")),
            plan_input_rows(&rust_dir.join("print-partner.db")),
            "{label}"
        );
        assert_eq!(
            plan_input_rows(&rust_dir.join("print-partner.db")),
            [
                "1|default|1|1|primary|0|revision|1|manifest|names",
                "2|default|1|1|legacy:1|0|revision|1|manifest-filled|<null>",
            ],
            "{label}"
        );
        let fresh_columns = table_columns(&rust_fresh.join("print-partner.db"), "projects");
        let upgraded_columns = table_columns(&rust_dir.join("print-partner.db"), "projects");
        if version < 33 {
            assert_ne!(fresh_columns, upgraded_columns, "{label}");
            assert_eq!(
                upgraded_columns.last().map(String::as_str),
                Some("legacy_manifest_cutover"),
                "{label}"
            );
        } else {
            assert_eq!(fresh_columns, upgraded_columns, "{label}");
        }
    }
}
