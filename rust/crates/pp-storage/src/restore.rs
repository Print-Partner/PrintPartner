//! Whole-installation backup and restore for the Rust runtime.
//!
//! Staging validates an uploaded app backup archive (Node's format 2, see
//! [`crate::archive`]), extracts it under `<data>/.pp-restore/` and admits its
//! schema without touching live files. Activation runs on the writer thread:
//! it stops writer admission, drains reader connections, closes the writer's
//! connection, swaps the database files and backup roots under a journal,
//! and reopens. A failure before the journal commits moves the prior files
//! back; [`recover`] does the same at the next open after a crash.

use crate::{Envelope, READER_DRAIN, ReaderPool, SettingsClient, Shared, Work, WriterOwner};
use crate::{archive, jobs, ownership_marker, schema};
use anyhow::{Context, Result, anyhow, ensure};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    fmt,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{MutexGuard, atomic::AtomicBool, mpsc},
    time::{Duration, Instant},
};

/// The text a restore request repeats to confirm it.
pub const CONFIRMATION: &str = "Restore replaces this installation's database and files with the backup. It cannot reverse prints already sent to printers or filament usage already recorded in Spoolman.";

const WORK_DIR: &str = ".pp-restore";
const JOURNAL: &str = "journal.json";
const MAX_JOURNAL_BYTES: u64 = 64 * 1024;
const DATABASE_FILES: [&str; 4] = [
    archive::DATABASE_FILE,
    archive::WAL_FILE,
    "print-partner.db-shm",
    "print-partner.db-journal",
];

/// A restore refused before any live file changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The data directory has no valid `.pp-rust-owned.json`.
    Unmarked,
    /// The upload exceeded this many bytes.
    TooLarge(u64),
    InvalidArchive(String),
    UnsupportedSchema(String),
    Confirmation,
    UnknownRestore,
    /// Reader connections did not close in time.
    Busy,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unmarked => {
                f.write_str("Restore needs a Rust-owned installation (.pp-rust-owned.json)")
            }
            Self::TooLarge(limit) => write!(f, "Backup archive exceeds {limit} bytes"),
            Self::InvalidArchive(detail) => write!(f, "Invalid backup archive: {detail}"),
            Self::UnsupportedSchema(detail) => write!(f, "Unsupported backup schema: {detail}"),
            Self::Confirmation => f.write_str("Restore confirmation does not match"),
            Self::UnknownRestore => f.write_str("No staged restore with that id"),
            Self::Busy => f.write_str("Restore refused: reader connections are still open"),
        }
    }
}

impl std::error::Error for Refusal {}

/// A validated archive extracted beside the live installation.
#[derive(Debug, Clone)]
pub struct Staged {
    pub restore_id: String,
    pub metadata: archive::Metadata,
    /// Schema version of the archived database.
    pub schema_version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Activated {
    /// Schema version of the database as restored.
    pub previous_schema_version: u64,
    /// Schema version after the restored database was opened and migrated.
    pub schema_version: u64,
}

/// Backup and restore for one data directory. Clones share the writer.
#[derive(Clone)]
pub struct InstallationClient {
    client: SettingsClient,
    data_dir: PathBuf,
}

impl WriterOwner {
    pub fn installation(&self) -> Result<InstallationClient> {
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow!("Storage owner is closed"))?;
        Ok(InstallationClient {
            client: self.client.clone(),
            data_dir: lease.data_dir().to_owned(),
        })
    }
}

/// A full backup archive in a private temporary directory outside managed
/// storage. Dropping it removes the directory.
pub struct BackupFile {
    directory: PathBuf,
    pub path: PathBuf,
    pub metadata: archive::Metadata,
    pub size_bytes: u64,
    pub sha256: String,
}

impl Drop for BackupFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// An archive upload being written under `<data>/.pp-restore/`. Dropping it
/// removes the file.
pub struct Upload {
    file: File,
    path: PathBuf,
    written: u64,
    limit: u64,
}

impl Upload {
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.written = self.written.saturating_add(bytes.len() as u64);
        ensure!(self.written <= self.limit, Refusal::TooLarge(self.limit));
        self.file.write_all(bytes)?;
        Ok(())
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl InstallationClient {
    /// Writes a full backup to a new private temporary directory, outside
    /// managed storage, through the writer like `WriterOwner::create_archive`.
    pub fn backup_to_temporary(&self, app_version: &str) -> Result<BackupFile> {
        let directory = std::env::temp_dir().join(format!("pp-backup-{}", random_id()));
        ensure!(
            !directory.starts_with(&self.data_dir),
            "Backup inside managed storage"
        );
        pp_os::create_private_dir(&directory)?;
        let mut backup = BackupFile {
            path: directory.join(format!("print-partner-backup-{}.tar.gz", random_id())),
            directory,
            metadata: archive::Metadata::new(app_version, archive::Scope::Full),
            size_bytes: 0,
            sha256: String::new(),
        };
        let job = archive::Job {
            destination: backup.path.clone(),
            data_dir: self.data_dir.clone(),
            metadata: backup.metadata.clone(),
            request: archive::Request::new(app_version, archive::Scope::Full),
        };
        let reply = self.client.enqueue(
            Work::Archive(Box::new(job)),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?;
        reply
            .recv()
            .map_err(|_| anyhow!("Writer stopped without backup result"))??;
        let mut file = File::open(&backup.path)?;
        let mut digest = sha2::Sha256::new();
        backup.size_bytes = std::io::copy(&mut file, &mut digest)?;
        backup.sha256 = hex::encode(digest.finalize());
        Ok(backup)
    }

    /// Starts an upload of at most `limit` bytes.
    pub fn begin_upload(&self, limit: u64) -> Result<Upload> {
        require_marker(&self.data_dir)?;
        let work = self.data_dir.join(WORK_DIR);
        pp_os::create_private_dir_all(&work)?;
        let path = work.join(format!("incoming-{}.tar.gz", random_id()));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Upload {
            file,
            path,
            written: 0,
            limit,
        })
    }

    /// Validates and extracts `upload` and admits its schema. Replaces any
    /// earlier staged restore. Live files are not touched.
    pub fn stage(&self, upload: Upload) -> Result<Staged> {
        let _gate = self.gate()?;
        require_marker(&self.data_dir)?;
        upload.file.sync_all()?;
        let work = self.data_dir.join(WORK_DIR);
        for entry in std::fs::read_dir(&work)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with("staged-") {
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        let restore_id = random_id();
        let staged = work.join(format!("staged-{restore_id}"));
        pp_os::create_private_dir(&staged)?;
        let result = (|| {
            let metadata = archive::extract(&upload.path, archive::Limits::default(), &staged)
                .map_err(|error| Refusal::InvalidArchive(format!("{error:#}")))?;
            ensure!(
                metadata.scope == archive::MetadataScope::Full,
                Refusal::InvalidArchive("restore needs a full backup".into())
            );
            let schema_version = admit_schema(&staged)
                .map_err(|error| Refusal::UnsupportedSchema(format!("{error:#}")))?;
            Ok(Staged {
                restore_id,
                metadata,
                schema_version,
            })
        })();
        drop(upload);
        if result.is_err() {
            let _ = std::fs::remove_dir_all(&staged);
            let _ = std::fs::remove_dir(&work);
        }
        result
    }

    /// Activates the staged restore `restore_id` once `confirmation` equals
    /// [`CONFIRMATION`]. Blocks until the writer has reopened.
    pub fn activate(&self, restore_id: &str, confirmation: &str) -> Result<Activated> {
        ensure!(confirmation == CONFIRMATION, Refusal::Confirmation);
        ensure!(
            restore_id.len() == 32
                && restore_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            Refusal::UnknownRestore
        );
        let _gate = self.gate()?;
        let staged = self
            .data_dir
            .join(WORK_DIR)
            .join(format!("staged-{restore_id}"));
        ensure!(staged.is_dir(), Refusal::UnknownRestore);
        require_marker(&self.data_dir)?;
        let (reply, receiver) = mpsc::channel();
        {
            let mut queue = self
                .client
                .shared
                .queue
                .lock()
                .map_err(|_| anyhow!("Writer admission poisoned"))?;
            ensure!(!queue.closed, "Storage stopped");
            queue.pending.push_back(Envelope::Restore { staged, reply });
            self.client.shared.changed.notify_all();
        }
        receiver
            .recv()
            .map_err(|_| anyhow!("Writer stopped without restore result"))?
    }
}

impl InstallationClient {
    fn gate(&self) -> Result<MutexGuard<'_, ()>> {
        (self.client.shared.restore_gate.lock()).map_err(|_| anyhow!("Restore gate poisoned"))
    }
}

fn require_marker(data_dir: &Path) -> Result<()> {
    match ownership_marker::inspect(data_dir) {
        ownership_marker::MarkerState::Valid(_) => Ok(()),
        _ => Err(Refusal::Unmarked.into()),
    }
}

fn random_id() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

/// The schema version of the database in `directory`, refused as startup
/// refuses a live one: newer than supported, too old to upgrade, missing, or
/// failing integrity or job-table checks.
fn admit_schema(directory: &Path) -> Result<u64> {
    let version = schema::preflight(
        #[cfg(test)]
        &directory.join(archive::DATABASE_FILE),
        #[cfg(test)]
        false,
        |name| File::open(directory.join(name)),
        || Ok(()),
    )?;
    ensure!(version > 0, "backup database has no schema version");
    Ok(version)
}

/// Runs on the writer thread for [`Envelope::Restore`].
pub(crate) fn activate(
    shared: &Shared,
    readers: &ReaderPool,
    data_dir: &Path,
    seed: &str,
    connection: &mut Connection,
    staged: &Path,
) -> Result<Activated> {
    stop_admission(shared);
    let database = data_dir.join(archive::DATABASE_FILE);
    let reader_count = match drain_readers(readers) {
        Ok(count) => count,
        Err(count) => {
            open_readers(readers, &database, count)?;
            resume_admission(shared);
            return Err(Refusal::Busy.into());
        }
    };
    #[cfg(test)]
    tests::during_activation(data_dir);
    let forward = (|| -> Result<(Connection, schema::SchemaReady)> {
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        std::mem::replace(connection, Connection::open_in_memory()?)
            .close()
            .map_err(|(_, error)| error)?;
        let mut journal = Journal::begin(data_dir, staged)?;
        journal.swap(data_dir)?;
        let opened = open_database(data_dir, seed)?;
        #[cfg(test)]
        tests::step(data_dir)?;
        journal.commit(data_dir)?;
        #[cfg(test)]
        tests::step(data_dir)?;
        journal.finish(data_dir);
        Ok(opened)
    })();
    let activated = match forward {
        Ok((restored, ready)) => {
            *connection = restored;
            Activated {
                previous_schema_version: ready.previous_version,
                schema_version: ready.version,
            }
        }
        Err(error) => {
            #[cfg(test)]
            if error.is::<tests::Crash>() {
                return Err(error);
            }
            if let Some(mut journal) = Journal::read(data_dir)? {
                journal.rollback(data_dir)?;
            }
            *connection = open_database(data_dir, seed)?.0;
            open_readers(readers, &database, reader_count)?;
            resume_admission(shared);
            return Err(error.context("Restore failed; the previous installation is kept"));
        }
    };
    if shared.job_subscriptions.revalidate(connection).is_err() {
        shared.job_subscriptions.stop();
    }
    open_readers(readers, &database, reader_count)?;
    resume_admission(shared);
    Ok(activated)
}

/// Closes writer admission and fails work queued behind the restore.
fn stop_admission(shared: &Shared) {
    let mut queue = shared
        .queue
        .lock()
        .unwrap_or_else(crate::stop_poisoned_queue);
    queue.closed = true;
    queue.pending.clear();
    for work in queue.auth_ready.drain(..) {
        crate::auth::rebuild::finish_stopped(work);
    }
    shared.changed.notify_all();
}

fn resume_admission(shared: &Shared) {
    let mut queue = shared
        .queue
        .lock()
        .unwrap_or_else(crate::stop_poisoned_queue);
    queue.closed = false;
    shared.changed.notify_all();
}

/// Closes idle readers and waits for checkouts to return. `Err` carries the
/// pool size when a checkout is still out after [`READER_DRAIN`].
fn drain_readers(readers: &ReaderPool) -> Result<usize, usize> {
    let mut state = readers
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let count = state.idle.len() + state.active;
    state.closed = true;
    state.idle.clear();
    readers.changed.notify_all();
    let deadline = Instant::now() + READER_DRAIN;
    while state.active > 0 && Instant::now() < deadline {
        state = readers
            .changed
            .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
    }
    if state.active > 0 {
        Err(count)
    } else {
        Ok(count)
    }
}

fn open_readers(readers: &ReaderPool, database: &Path, count: usize) -> Result<()> {
    let mut idle = Vec::with_capacity(count);
    for _ in 0..count {
        let reader = Connection::open_with_flags(
            database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        schema::configure(&reader, true)?;
        idle.push(reader);
    }
    let mut state = readers
        .state
        .lock()
        .map_err(|_| anyhow!("Reader pool poisoned"))?;
    state.idle = idle;
    state.closed = false;
    readers.changed.notify_all();
    Ok(())
}

/// Opens the live database as `WriterOwner::open` does: schema admission,
/// migrations and job recovery.
fn open_database(data_dir: &Path, seed: &str) -> Result<(Connection, schema::SchemaReady)> {
    let version = admit_schema(data_dir)?;
    let (mut connection, ready) =
        schema::initialize(&data_dir.join(archive::DATABASE_FILE), version, seed)?;
    jobs::recover(&mut connection)?;
    Ok((connection, ready))
}

/// Finishes or undoes an interrupted activation. Runs at open, under the
/// directory lock and after the ownership marker was admitted, before the
/// database is read.
pub(crate) fn recover(data_dir: &Path) -> Result<()> {
    let work = data_dir.join(WORK_DIR);
    if !exists(&work) {
        return Ok(());
    }
    if let Some(mut journal) = Journal::read(data_dir)? {
        match journal.state {
            JournalState::Swapping => journal.rollback(data_dir)?,
            JournalState::Committed => journal.finish(data_dir),
        }
    }
    std::fs::remove_dir_all(&work).context("Remove the restore work directory")
}

/// Every top-level name a restore replaces. Everything else in the data
/// directory, including the ownership marker and lease files, stays.
fn managed() -> impl Iterator<Item = &'static str> {
    DATABASE_FILES
        .into_iter()
        .chain(archive::FULL_ROOTS.iter().map(|(name, _)| *name))
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn remove_entry(path: &Path) -> std::io::Result<()> {
    if path.symlink_metadata()?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalState {
    Swapping,
    Committed,
}

/// `<data>/.pp-restore/journal.json`. `live` lists the managed names present
/// before the swap; each moves to `prior`. `incoming` lists the names that
/// move in from `staged`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    state: JournalState,
    staged: String,
    prior: String,
    live: Vec<String>,
    incoming: Vec<String>,
}

impl Journal {
    fn begin(data_dir: &Path, staged: &Path) -> Result<Self> {
        let work = data_dir.join(WORK_DIR);
        let staged_name = staged
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("Staged restore has no name"))?;
        let journal = Self {
            version: 1,
            state: JournalState::Swapping,
            staged: staged_name.to_owned(),
            prior: format!("prior-{}", random_id()),
            live: managed()
                .filter(|name| exists(&data_dir.join(name)))
                .map(str::to_owned)
                .collect(),
            incoming: managed()
                .filter(|name| exists(&staged.join(name)))
                .map(str::to_owned)
                .collect(),
        };
        ensure!(
            journal
                .incoming
                .iter()
                .any(|name| name == archive::DATABASE_FILE),
            "Staged restore has no database"
        );
        pp_os::create_private_dir(&work.join(&journal.prior))?;
        journal.write(data_dir)?;
        Ok(journal)
    }

    fn read(data_dir: &Path) -> Result<Option<Self>> {
        let path = data_dir.join(WORK_DIR).join(JOURNAL);
        let file = match pp_os::open_read_nofollow(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
        let journal: Self = serde_json::from_slice(&bytes)
            .ok()
            .filter(|_| bytes.len() as u64 <= MAX_JOURNAL_BYTES)
            .ok_or_else(|| anyhow!("Restore journal {} is unreadable", path.display()))?;
        let named = |name: &str, prefix: &str| {
            name.strip_prefix(prefix)
                .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        };
        ensure!(
            journal.version == 1
                && named(&journal.staged, "staged-")
                && named(&journal.prior, "prior-")
                && journal
                    .live
                    .iter()
                    .chain(&journal.incoming)
                    .all(|name| managed().any(|known| known == name)),
            "Restore journal {} is invalid",
            path.display()
        );
        Ok(Some(journal))
    }

    fn write(&self, data_dir: &Path) -> Result<()> {
        let work = data_dir.join(WORK_DIR);
        let temporary = work.join(format!("{JOURNAL}.tmp"));
        std::fs::write(&temporary, serde_json::to_vec(self)?)?;
        pp_os::sync_file(&temporary)?;
        pp_os::publish_rename(&temporary, &work.join(JOURNAL))?;
        pp_os::sync_directory(&work)?;
        Ok(())
    }

    fn swap(&self, data_dir: &Path) -> Result<()> {
        let work = data_dir.join(WORK_DIR);
        let (prior, staged) = (work.join(&self.prior), work.join(&self.staged));
        for name in &self.live {
            #[cfg(test)]
            tests::step(data_dir)?;
            std::fs::rename(data_dir.join(name), prior.join(name))
                .with_context(|| format!("Move {name} aside"))?;
        }
        for name in &self.incoming {
            #[cfg(test)]
            tests::step(data_dir)?;
            std::fs::rename(staged.join(name), data_dir.join(name))
                .with_context(|| format!("Move restored {name} into place"))?;
        }
        pp_os::sync_directory(data_dir)?;
        pp_os::sync_directory(&prior)?;
        Ok(())
    }

    fn commit(&mut self, data_dir: &Path) -> Result<()> {
        self.state = JournalState::Committed;
        self.write(data_dir)
    }

    /// Moves every prior entry back. An entry is new when it was not live
    /// before the swap or its prior copy is still aside; new entries are
    /// removed first. Safe to repeat after a crash.
    fn rollback(&mut self, data_dir: &Path) -> Result<()> {
        if self.state == JournalState::Committed {
            self.state = JournalState::Swapping;
            self.write(data_dir)?;
        }
        let prior = data_dir.join(WORK_DIR).join(&self.prior);
        for name in managed() {
            let (live, kept) = (data_dir.join(name), prior.join(name));
            let was_live = self.live.iter().any(|known| known == name);
            if exists(&live) && (!was_live || exists(&kept)) {
                remove_entry(&live).with_context(|| format!("Remove restored {name}"))?;
            }
            if exists(&kept) {
                std::fs::rename(&kept, &live).with_context(|| format!("Move {name} back"))?;
            }
        }
        pp_os::sync_directory(data_dir)?;
        self.finish(data_dir);
        Ok(())
    }

    /// Removes the journal, then the prior and staged copies. A leftover
    /// work directory is removed at the next open.
    fn finish(&self, data_dir: &Path) {
        let work = data_dir.join(WORK_DIR);
        if std::fs::remove_file(work.join(JOURNAL)).is_ok() {
            let _ = pp_os::sync_directory(&work);
            let _ = std::fs::remove_dir_all(work.join(&self.prior));
            let _ = std::fs::remove_dir_all(work.join(&self.staged));
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
#[cfg(all(test, not(unix)))]
mod tests {
    pub(super) struct Crash;
    pub(super) fn during_activation(_: &std::path::Path) {}
    pub(super) fn step(_: &std::path::Path) -> anyhow::Result<()> {
        Ok(())
    }
}
