#![cfg(unix)]
//! T-8-archive (GRE-403): keep three verified pre-upgrade backups and never
//! prune the last usable rollback.

use pp_storage::{Limits, SchemaReady, WriterOwner};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

#[path = "common/scratch.rs"]
mod scratch;
use scratch::Scratch;

const SCHEMA34: &[u8] = include_bytes!("fixtures/accepted-plan-node.db");

/// A valid WAL-mode SQLite copy, like one the backup API writes from a WAL
/// database, with its mtime set `age` before now.
fn verified_copy(path: &Path, age: Duration) {
    let connection = Connection::open(path).unwrap();
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    connection
        .execute_batch("CREATE TABLE marker(value); INSERT INTO marker VALUES('old copy');")
        .unwrap();
    drop(connection);
    assert!(!sidecar(path, "-wal").exists() && !sidecar(path, "-shm").exists());
    age_file(path, age);
}

fn age_file(path: &Path, age: Duration) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn hour(n: u64) -> Duration {
    Duration::from_secs(3600 * n)
}

fn contents(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let bytes = std::fs::read(entry.path()).unwrap_or_default();
            (entry.file_name().into_string().unwrap(), bytes)
        })
        .collect()
}

fn upgrade(data: &Path) -> SchemaReady {
    std::fs::write(data.join("print-partner.db"), SCHEMA34).unwrap();
    let (owner, ready) = WriterOwner::open(data, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    ready
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<_> = paths
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn ticket_t_8_archive_retention_keeps_rollback_this_upgrade_and_three_older() {
    let data = Scratch::new("pp-t8-retention");
    let backups = data.join("backups");
    std::fs::create_dir(&backups).unwrap();
    // Six older verified copies, 20 the oldest and 25 the newest.
    for (index, version) in (20..=25).enumerate() {
        verified_copy(
            &backups.join(format!("pre-schema{version}.db")),
            hour(10 - index as u64),
        );
    }
    // An immutable open would ignore a real WAL, so a copy with one is kept.
    std::fs::write(sidecar(&backups.join("pre-schema20.db"), "-wal"), b"stale").unwrap();
    // A shared-memory sidecar of a copy retention removes goes with it.
    std::fs::write(sidecar(&backups.join("pre-schema21.db"), "-shm"), b"stale").unwrap();
    // The oldest verified copy is a pre-37 legacy copy. After an upgrade
    // interrupted between steps it can be the only one the previous release
    // opens, so it is never pruned.
    verified_copy(&backups.join("pre-schema31-to-41.db"), hour(40));
    // The oldest file of all fails verification and is kept.
    std::fs::write(backups.join("pre-schema19.db"), b"not a database").unwrap();
    age_file(&backups.join("pre-schema19.db"), hour(20));
    // Files retention never touches.
    let untouched = [
        (
            "print-partner-pre-update-to-3.8.0-schema-31-to-34.tar.gz",
            b"node pre-update".as_slice(),
        ),
        (
            "print-partner-backup-2026-01-01T00-00-00-000Z.tar.gz",
            b"manual backup",
        ),
        (
            ".backup-00112233445566778899aabbccddeeff.db",
            b"crashed temp copy",
        ),
        ("pre-schema21.db.bak", b"user copy"),
    ];
    for (name, bytes) in untouched {
        std::fs::write(backups.join(name), bytes).unwrap();
        age_file(&backups.join(name), hour(30));
    }
    let before = contents(&backups);

    let ready = upgrade(&data);

    let rollback = ready
        .backup
        .clone()
        .expect("an upgrade from 34 takes a rollback copy");
    assert!(rollback.exists());
    assert_eq!(
        names(&ready.pruned_backups),
        ["pre-schema21.db", "pre-schema22.db"]
    );
    let after = contents(&backups);
    for removed in ["pre-schema21.db", "pre-schema21.db-shm", "pre-schema22.db"] {
        assert!(!after.contains_key(removed), "{removed} survived");
    }
    for kept in [
        "pre-schema19.db",
        "pre-schema20.db",
        "pre-schema20.db-wal",
        "pre-schema31-to-41.db",
        "pre-schema23.db",
        "pre-schema24.db",
        "pre-schema25.db",
    ] {
        assert_eq!(after.get(kept), before.get(kept), "{kept} changed");
    }
    for (name, bytes) in untouched {
        assert_eq!(after.get(name).map(Vec::as_slice), Some(bytes), "{name}");
    }
    // Every copy this upgrade wrote survives, the rollback among them.
    let written: Vec<_> = after
        .keys()
        .filter(|name| !before.contains_key(*name))
        .cloned()
        .collect();
    assert!(written.contains(&rollback.file_name().unwrap().to_string_lossy().into_owned()));
    assert!(
        written
            .iter()
            .all(|name| name.starts_with("pre-schema") && name.ends_with(".db")),
        "{written:?}"
    );
    // Verification opened every kept copy without leaving sidecars.
    let sidecars: Vec<_> = after
        .keys()
        .filter(|name| name.ends_with("-wal") || name.ends_with("-shm"))
        .collect();
    assert_eq!(sidecars, ["pre-schema20.db-wal"]);
    assert_eq!(
        ready.backup_warnings.len(),
        2,
        "{:?}",
        ready.backup_warnings
    );
    for name in ["pre-schema19.db", "pre-schema20.db"] {
        assert!(
            ready
                .backup_warnings
                .iter()
                .any(|warning| warning.contains(name)),
            "{name}: {:?}",
            ready.backup_warnings
        );
    }
}

#[test]
fn ticket_t_8_archive_retention_breaks_mtime_ties_by_step() {
    let data = Scratch::new("pp-t8-retention-ties");
    let backups = data.join("backups");
    std::fs::create_dir(&backups).unwrap();
    for version in 20..=25 {
        verified_copy(&backups.join(format!("pre-schema{version}.db")), hour(5));
    }
    let tied = SystemTime::now() - hour(5);
    for version in 20..=25 {
        std::fs::File::options()
            .write(true)
            .open(backups.join(format!("pre-schema{version}.db")))
            .unwrap()
            .set_modified(tied)
            .unwrap();
    }
    let ready = upgrade(&data);
    assert_eq!(
        names(&ready.pruned_backups),
        ["pre-schema20.db", "pre-schema21.db", "pre-schema22.db"]
    );
}

#[test]
fn ticket_t_8_archive_retention_never_deletes_below_three_verified_or_without_an_upgrade() {
    let data = Scratch::new("pp-t8-retention-floor");
    let backups = data.join("backups");
    std::fs::create_dir(&backups).unwrap();
    verified_copy(&backups.join("pre-schema20.db"), hour(3));
    verified_copy(&backups.join("pre-schema21.db"), hour(2));
    for version in 22..=23 {
        let path = backups.join(format!("pre-schema{version}.db"));
        std::fs::write(&path, b"corrupt copy").unwrap();
        age_file(&path, hour(1));
    }
    // A valid copy nobody can read is kept too. Root reads it anyway, and
    // then it counts as the third verified copy, which is also kept.
    let unreadable = backups.join("pre-schema24.db");
    verified_copy(&unreadable, hour(1));
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    let root = unsafe { libc::geteuid() } == 0;
    let ready = upgrade(&data);
    assert!(
        ready.pruned_backups.is_empty(),
        "{:?}",
        ready.pruned_backups
    );
    assert_eq!(
        ready.backup_warnings.len(),
        if root { 2 } else { 3 },
        "{:?}",
        ready.backup_warnings
    );
    std::fs::set_permissions(
        backups.join("pre-schema24.db"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    for version in 20..=24 {
        assert!(backups.join(format!("pre-schema{version}.db")).exists());
    }

    // Four more verified older copies, then a reopen at the current schema:
    // no upgrade ran, so nothing is pruned.
    for version in 10..=13 {
        verified_copy(&backups.join(format!("pre-schema{version}.db")), hour(50));
    }
    let (owner, reopened) = WriterOwner::open(&data, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!(reopened.backup, None);
    assert!(reopened.pruned_backups.is_empty());
    assert!(reopened.backup_warnings.is_empty());
    for version in (10..=13).chain(20..=24) {
        assert!(backups.join(format!("pre-schema{version}.db")).exists());
    }
}

/// A 37 to 42 upgrade interrupted after step 41 commits restarts from 41 with
/// copies 38 to 41 on disk. The restart's opening `pre-schema42.db` is a new
/// name, so retention must not run, or it would prune `pre-schema38.db`, the
/// untouched schema-37 input.
#[cfg(feature = "desktop_rebuild")]
#[test]
fn ticket_t_8_archive_retention_restart_from_41_keeps_the_interrupted_upgrade_input() {
    let (data, backups) = interrupted("pp-t8-retention-restart-41", 41);
    let (owner, ready) = WriterOwner::open(&data, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!(ready.previous_version, 41);
    assert_eq!(ready.backup, Some(backups.join("pre-schema42.db")));
    assert!(
        ready.pruned_backups.is_empty(),
        "{:?}",
        ready.pruned_backups
    );
    for step in 38..=42 {
        assert!(
            backups.join(format!("pre-schema{step}.db")).exists(),
            "{step}"
        );
    }
}

/// Preflight checks the event objects against the input version, so a
/// schema-41 database that already has them is refused before the opening
/// copy or any write.
#[cfg(feature = "desktop_rebuild")]
#[test]
fn domain_events_objects_below_42_are_refused_by_preflight_before_any_write() {
    let data = Scratch::new("pp-domain-events-preflight");
    upgrade(&data);
    std::fs::remove_dir_all(data.join("backups")).unwrap();
    Connection::open(data.join("print-partner.db"))
        .unwrap()
        .execute_batch(
            "UPDATE app_settings SET value='41' WHERE tenant_id='default' AND key='schema_version'",
        )
        .unwrap();
    let before = contents(&data);
    let error = WriterOwner::open(&data, Limits::default()).err().unwrap();
    assert!(
        format!("{error:#}").contains("Unexpected domain event schema"),
        "{error:#}"
    );
    assert_eq!(contents(&data), before);
}

/// Sets up a 37 to 45 upgrade interrupted after step `at` committed: the
/// live database is at `at` and copies 38 to `at` are on disk, oldest first.
#[cfg(feature = "desktop_rebuild")]
fn interrupted(name: &str, at: u64) -> (Scratch, PathBuf) {
    let data = Scratch::new(name);
    upgrade(&data);
    let backups = data.join("backups");
    std::fs::remove_dir_all(&backups).unwrap();
    std::fs::create_dir(&backups).unwrap();
    let events = if at < 42 {
        "DROP TABLE domain_events;"
    } else {
        ""
    };
    Connection::open(data.join("print-partner.db"))
        .unwrap()
        .execute_batch(&format!(
            "DROP TABLE plan_snapshot_headers; DROP TABLE plan_draft_header_receipts; DROP TABLE plan_draft_headers; DROP TABLE build_profile_details; DROP TABLE printer_send_queue; {events} UPDATE app_settings SET value='{at}' WHERE tenant_id='default' AND key='schema_version'"
        ))
        .unwrap();
    for (index, step) in (38..=at).enumerate() {
        verified_copy(
            &backups.join(format!("pre-schema{step}.db")),
            hour(at - 37 - index as u64),
        );
    }
    (data, backups)
}

/// The restart of an interrupted upgrade finds the run marker naming that
/// upgrade's input copy, so retention keeps it, and the marker goes once the
/// upgrade commits.
#[cfg(feature = "desktop_rebuild")]
#[test]
fn ticket_t_8_archive_retention_restart_from_42_keeps_the_marked_upgrade_input() {
    let (data, backups) = interrupted("pp-t8-retention-marker-42", 42);
    std::fs::write(backups.join("pre-schema-run"), "pre-schema38.db\n").unwrap();
    let (owner, ready) = WriterOwner::open(&data, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!(ready.previous_version, 42);
    assert_eq!(ready.backup, Some(backups.join("pre-schema43.db")));
    assert!(backups.join("pre-schema38.db").exists());
    assert!(backups.join("pre-schema43.db").exists());
    assert!(
        !ready
            .pruned_backups
            .contains(&backups.join("pre-schema38.db")),
        "{:?}",
        ready.pruned_backups
    );
    assert!(!backups.join("pre-schema-run").exists());
}

/// Without a marker the same restart is an ordinary upgrade from 42, so
/// retention may prune the oldest copy. This is the case the marker exists
/// to tell apart.
#[cfg(feature = "desktop_rebuild")]
#[test]
fn ticket_t_8_archive_retention_restart_from_42_without_a_marker_prunes_as_usual() {
    let (data, backups) = interrupted("pp-t8-retention-unmarked-42", 42);
    let (owner, ready) = WriterOwner::open(&data, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!(ready.previous_version, 42);
    assert!(
        ready
            .pruned_backups
            .contains(&backups.join("pre-schema38.db")),
        "{:?}",
        ready.pruned_backups
    );
    assert!(backups.join("pre-schema43.db").exists());
    assert!(!backups.join("pre-schema-run").exists());
}

/// A marker that names no pre-upgrade copy is refused before any write.
#[cfg(feature = "desktop_rebuild")]
#[test]
fn ticket_t_8_archive_retention_refuses_a_marker_naming_no_copy() {
    let (data, backups) = interrupted("pp-t8-retention-bad-marker", 42);
    std::fs::write(backups.join("pre-schema-run"), "print-partner.db\n").unwrap();
    let database = std::fs::read(data.join("print-partner.db")).unwrap();
    let error = WriterOwner::open(&data, Limits::default()).err().unwrap();
    assert!(
        format!("{error:#}").contains("names no pre-upgrade copy"),
        "{error:#}"
    );
    assert!(std::fs::read(data.join("print-partner.db")).unwrap() == database);
}

/// A new release's first attempt from an unmarked 41 fails before step 42
/// commits. Its marker names the older run's copies 38 to 41 too, so the
/// retry, which now finds a marker and runs retention, still keeps the
/// untouched input in `pre-schema38.db`.
#[cfg(feature = "desktop_rebuild")]
#[test]
fn ticket_t_8_archive_retention_retry_from_unmarked_41_keeps_the_older_run_input() {
    let (data, backups) = interrupted("pp-t8-retention-retry-41", 41);
    let database = data.join("print-partner.db");
    Connection::open(&database)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER reject_schema42 BEFORE UPDATE OF value ON app_settings
             WHEN NEW.tenant_id = 'default' AND NEW.key = 'schema_version' AND NEW.value = '42'
             BEGIN SELECT RAISE(ABORT, 'schema42 stamp rejected'); END;",
        )
        .unwrap();
    assert!(WriterOwner::open(&data, Limits::default()).is_err());
    assert!(backups.join("pre-schema-run").exists());
    Connection::open(&database)
        .unwrap()
        .execute_batch("DROP TRIGGER reject_schema42")
        .unwrap();
    let (owner, ready) = WriterOwner::open(&data, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    assert_eq!(ready.previous_version, 41);
    for step in 38..=45 {
        assert!(
            backups.join(format!("pre-schema{step}.db")).exists(),
            "{step} {:?}",
            ready.pruned_backups
        );
    }
    assert!(!backups.join("pre-schema-run").exists());
}
