use super::*;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use tempfile::NamedTempFile;

const BYTES: &[u8] = b"archive bytes";

/// Sorted names in `folder` that start with `prefix`; none when it is missing.
pub(crate) fn names(folder: &Path, prefix: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let names = entries.map(|entry| entry.unwrap().file_name().into_string().unwrap());
    let mut names: Vec<_> = names.filter(|name| name.starts_with(prefix)).collect();
    names.sort();
    names
}

/// Counts its calls and always refuses for lack of space.
struct Refusing(AtomicUsize);

impl Export for Refusing {
    fn export(&self) -> Result<Box<dyn Exported>, BackupFailure> {
        self.0.fetch_add(1, SeqCst);
        Err(BackupFailure::NoSpace)
    }
}

#[test]
fn ticket_t_29_tray_backup_saves_nothing_when_the_export_fails_or_shutdown_began() {
    let (refusing, gate) = (Arc::new(Refusing(AtomicUsize::new(0))), ShutdownGate::new());
    let backup = OfflineBackup::new(refusing.clone(), gate.clone());
    let folder = tempfile::tempdir().unwrap();

    assert_eq!(backup.create_in(folder.path()), Err(BackupFailure::NoSpace));
    assert_eq!(refusing.0.load(SeqCst), 1);
    gate.reject_new();
    assert_eq!(
        backup.create_in(folder.path()),
        Err(BackupFailure::Stopping)
    );
    assert_eq!(refusing.0.load(SeqCst), 1, "no export after shutdown began");
    assert!(names(folder.path(), "").is_empty());
}

/// An archive file and the digest the export reports for it.
struct FakeExported(NamedTempFile, String);

impl Exported for FakeExported {
    fn archive(&self) -> &Path {
        self.0.path()
    }
    fn sha256(&self) -> &str {
        &self.1
    }
}

#[test]
fn ticket_t_29_tray_backup_copy_checks_the_digest_and_leaves_nothing_on_failure() {
    let folder = tempfile::tempdir().unwrap();
    let exported = |sha256: String| {
        let archive = NamedTempFile::new().unwrap();
        std::fs::write(archive.path(), BYTES).unwrap();
        FakeExported(archive, sha256)
    };

    let wrong = exported("0".repeat(64));
    let refused = copy_then_publish(&wrong, folder.path());
    assert_eq!(refused, Err(BackupFailure::Failed));
    assert!(names(folder.path(), "").is_empty(), "no .partial left");

    let right = exported(hex::encode(sha2::Sha256::digest(BYTES)));
    let saved = copy_then_publish(&right, folder.path()).unwrap();
    assert_eq!(names(folder.path(), ""), ["print-partner-backup.tar.gz"]);
    assert_eq!(std::fs::read(&saved).unwrap(), BYTES);
    assert_eq!(
        std::fs::read(right.0.path()).unwrap(),
        BYTES,
        "a copy, not a move"
    );
}
