use super::*;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use tempfile::TempDir;

const BYTES: &[u8] = b"archive bytes";

struct FakeExported {
    _folder: TempDir,
    archive: PathBuf,
    sha256: String,
}

impl Exported for FakeExported {
    fn archive(&self) -> &Path {
        &self.archive
    }
    fn sha256(&self) -> &str {
        &self.sha256
    }
}

fn exported_in(parent: &Path, sha256: String) -> FakeExported {
    let folder = tempfile::tempdir_in(parent).unwrap();
    let archive = folder.path().join("print-partner-backup.tar.gz");
    std::fs::write(&archive, BYTES).unwrap();
    FakeExported {
        _folder: folder,
        archive,
        sha256,
    }
}

fn digest() -> String {
    hex::encode(sha2::Sha256::digest(BYTES))
}

struct FakeExport {
    exports: TempDir,
    calls: AtomicUsize,
    fails: Option<BackupFailure>,
}

impl Export for FakeExport {
    fn export(&self) -> Result<Box<dyn Exported>, BackupFailure> {
        self.calls.fetch_add(1, SeqCst);
        match self.fails {
            Some(failure) => Err(failure),
            None => Ok(Box::new(exported_in(self.exports.path(), digest()))),
        }
    }
}

fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The fake, the backup over it, its shutdown gate and an empty folder.
fn fake_backup(
    fails: Option<BackupFailure>,
) -> (Arc<FakeExport>, OfflineBackup, ShutdownGate, TempDir) {
    let fake = Arc::new(FakeExport {
        exports: tempfile::tempdir().unwrap(),
        calls: AtomicUsize::new(0),
        fails,
    });
    let gate = ShutdownGate::new();
    let backup = OfflineBackup::new(fake.clone(), gate.clone());
    (fake, backup, gate, tempfile::tempdir().unwrap())
}

#[test]
fn ticket_t_29_tray_backup_runs_the_export_once_and_never_replaces_a_file() {
    let (fake, backup, _gate, folder) = fake_backup(None);
    let first = folder.path().join("print-partner-backup.tar.gz");
    std::fs::write(&first, b"keep").unwrap();

    let saved = backup.create_in(folder.path()).unwrap();

    assert_eq!(saved, folder.path().join("print-partner-backup (1).tar.gz"));
    assert_eq!(std::fs::read(&saved).unwrap(), BYTES);
    assert_eq!(std::fs::read(&first).unwrap(), b"keep");
    assert_eq!(fake.calls.load(SeqCst), 1);
    assert!(
        names(fake.exports.path()).is_empty(),
        "export folder removed"
    );
}

#[test]
fn ticket_t_29_tray_backup_saves_nothing_when_the_export_fails_or_shutdown_began() {
    let (fake, backup, _gate, folder) = fake_backup(Some(BackupFailure::NoSpace));
    assert_eq!(backup.create_in(folder.path()), Err(BackupFailure::NoSpace));
    assert!(names(folder.path()).is_empty());
    assert_eq!(fake.calls.load(SeqCst), 1);

    let (fake, backup, gate, folder) = fake_backup(None);
    gate.reject_new();
    assert_eq!(
        backup.create_in(folder.path()),
        Err(BackupFailure::Stopping)
    );
    assert!(names(folder.path()).is_empty());
    assert_eq!(fake.calls.load(SeqCst), 0);
}

#[test]
fn ticket_t_29_tray_backup_copy_checks_the_digest_and_leaves_nothing_on_failure() {
    let (exports, folder) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let running = CancellationToken::new();

    let wrong = exported_in(exports.path(), "0".repeat(64));
    assert_eq!(
        copy_then_publish(&wrong, folder.path(), &running),
        Err(BackupFailure::Failed)
    );
    assert!(names(folder.path()).is_empty(), "no .partial left");

    let right = exported_in(exports.path(), digest());
    let stopped = CancellationToken::new();
    stopped.cancel();
    assert_eq!(
        copy_then_publish(&right, folder.path(), &stopped),
        Err(BackupFailure::Stopping)
    );
    assert!(names(folder.path()).is_empty(), "no .partial left");

    let saved = copy_then_publish(&right, folder.path(), &running).unwrap();
    assert_eq!(names(folder.path()), ["print-partner-backup.tar.gz"]);
    assert_eq!(std::fs::read(&saved).unwrap(), BYTES);
    assert_eq!(
        std::fs::read(right.archive()).unwrap(),
        BYTES,
        "a copy, not a move"
    );
}
