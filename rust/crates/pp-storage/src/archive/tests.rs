use super::*;
use crate::{Limits as WriterLimits, Setting, SettingCommand, WriterOwner};
use rusqlite::ErrorCode;
use std::{
    os::unix::fs::PermissionsExt,
    sync::{Mutex, atomic::AtomicBool, mpsc},
    thread,
    time::Duration,
};

type Hold = (PathBuf, mpsc::Sender<()>, mpsc::Receiver<()>);
static HOLDS: Mutex<Vec<Hold>> = Mutex::new(Vec::new());

/// Called by `write` after the last entry and before validation, while the
/// write lock is held. A test that registered `destination` sees `frozen`
/// and the archive waits until the test sends `release`.
pub(super) fn hold_freeze(destination: &Path) {
    let hold = {
        let mut holds = HOLDS.lock().unwrap();
        holds
            .iter()
            .position(|(path, _, _)| path == destination)
            .map(|index| holds.remove(index))
    };
    if let Some((_, frozen, release)) = hold {
        frozen.send(()).unwrap();
        release.recv().unwrap();
    }
}

fn hold(destination: &Path) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (frozen_tx, frozen_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    HOLDS
        .lock()
        .unwrap()
        .push((destination.to_owned(), frozen_tx, release_rx));
    (frozen_rx, release_tx)
}

fn scratch(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(std::env::temp_dir().canonicalize().unwrap())
        .unwrap()
}

fn stored(database: &Path, key: &str) -> Option<String> {
    Connection::open(database)
        .unwrap()
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='test' AND key=?",
            [key],
            |row| row.get(0),
        )
        .ok()
}

fn database_entry(archive: &Path, output: &Path) {
    let mut reader =
        tar::Archive::new(GzDecoder::new(BufReader::new(File::open(archive).unwrap())));
    for entry in reader.entries().unwrap() {
        let mut entry = entry.unwrap();
        if entry.path_bytes().as_ref() == DATABASE_FILE.as_bytes() {
            std::io::copy(&mut entry, &mut File::create(output).unwrap()).unwrap();
            return;
        }
    }
    panic!("archive has no database");
}

#[test]
fn ticket_t_8_archive_freezes_other_sqlite_writers_until_done() {
    let data = scratch("pp-t8-freeze-data");
    let out = scratch("pp-t8-freeze-out");
    let (owner, _) = WriterOwner::open(data.path(), WriterLimits::default()).unwrap();
    let target = out.path().join("frozen.tar.gz");
    let (frozen, release) = hold(&target);

    // A second process's writer that holds the lock first delays the archive,
    // and its commit is in the snapshot.
    let external = Connection::open(data.path().join("print-partner.db")).unwrap();
    external.execute_batch("BEGIN IMMEDIATE").unwrap();
    external
        .execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('test','external','first')",
            [],
        )
        .unwrap();
    let result = thread::scope(|scope| {
        let handle =
            scope.spawn(|| owner.create_archive(&target, Request::new("3.9.0", Scope::Full)));
        assert!(
            frozen.recv_timeout(Duration::from_millis(300)).is_err(),
            "archive ran while another writer held the lock"
        );
        external.execute_batch("COMMIT").unwrap();

        // The archive is now held after its last entry with the lock taken.
        frozen.recv_timeout(Duration::from_secs(20)).unwrap();
        external.busy_timeout(Duration::ZERO).unwrap();
        let blocked = external.execute_batch("BEGIN IMMEDIATE").unwrap_err();
        assert_eq!(blocked.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
        let blocked = external
            .execute(
                "INSERT INTO app_settings(tenant_id,key,value) VALUES('test','during','no')",
                [],
            )
            .unwrap_err();
        assert_eq!(blocked.sqlite_error_code(), Some(ErrorCode::DatabaseBusy));
        assert!(!target.exists());
        release.send(()).unwrap();
        handle.join().unwrap()
    });
    result.unwrap();
    external.busy_timeout(Duration::from_secs(5)).unwrap();
    external.execute_batch("BEGIN IMMEDIATE; COMMIT").unwrap();

    let restored = out.path().join("restored.db");
    database_entry(&target, &restored);
    assert_eq!(stored(&restored, "external").as_deref(), Some("first"));
    assert_eq!(stored(&restored, "during"), None);
    drop(external);

    // The writer still commits after the archive.
    owner
        .client()
        .submit(
            SettingCommand::Set(Setting {
                tenant: "test".into(),
                key: "after".into(),
                value: "works".into(),
            }),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_8_archive_publish_reserves_then_renames() {
    let out = scratch("pp-t8-publish");
    let temporary = out.path().join(".tmp");
    std::fs::write(&temporary, b"archive bytes").unwrap();
    let destination = out.path().join("app.tar.gz");
    publish(&temporary, &destination).unwrap();
    assert_eq!(std::fs::read(&destination).unwrap(), b"archive bytes");
    assert!(!temporary.exists());

    // An existing destination keeps its bytes, and the error names the cause.
    std::fs::write(&temporary, b"second").unwrap();
    let error = format!("{:#}", publish(&temporary, &destination).unwrap_err());
    assert!(error.contains("must be a new file"), "{error}");
    assert!(error.contains("exists"), "{error}");
    assert_eq!(std::fs::read(&destination).unwrap(), b"archive bytes");

    // A failed rename removes the reservation and reports the real error.
    let missing = out.path().join(".missing");
    let other = out.path().join("other.tar.gz");
    let error = format!("{:#}", publish(&missing, &other).unwrap_err());
    assert!(error.contains("Publish backup archive"), "{error}");
    assert!(!other.exists());
}
