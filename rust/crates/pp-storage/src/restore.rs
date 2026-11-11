//! Owner-bound whole-installation backup, staging and confirmed activation.
//!
//! Staging writes an uploaded app backup archive (Node's format 2, see
//! [`crate::archive`]) under `<data>/.pp-restore/`, validates and extracts it
//! there, and admits its schema as startup admits a live database. It never
//! touches live files. Confirmed activation runs on the writer thread, drains
//! readers and retains the prior database and roots under a replayable journal.
//!
//! Restore work belongs to one [`WriterOwner`]. Its [`Activity`] refuses new
//! work once the owner starts to close, and the owner keeps its lease while
//! work is still running, so the next owner's [`recover`] never removes an
//! upload or stage in use.

use crate::{Envelope, SettingsClient, WriterOwner, archive, ownership_marker, schema};

pub(crate) mod activation;
mod backup;
mod journal;
pub use activation::{Activated, CONFIRMATION};
use anyhow::{Context, Result, anyhow, bail, ensure};
pub use backup::BackupFile;
use std::{
    fmt,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc},
    time::Instant,
};

const WORK_DIR: &str = ".pp-restore";
const INCOMING_PREFIX: &str = "incoming-";
const INCOMING_SUFFIX: &str = ".tar.gz";
/// The most any upload may hold, whatever limit the caller asks for. It
/// matches the default decompressed limit in [`archive::Limits`].
pub const MAX_UPLOAD_BYTES: u64 = 20 * 1024 * 1024 * 1024;
const STAGED_PREFIX: &str = "staged-";

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
            Self::UnknownRestore => {
                f.write_str("No staged restore with that id; upload the backup again")
            }
            Self::Busy => f.write_str("Restore refused: storage work is still active"),
        }
    }
}

impl std::error::Error for Refusal {}

/// A validated archive extracted beside the live installation.
///
/// A stage lasts only as long as the [`WriterOwner`] that made it. The next
/// open removes every stage, complete or not, because schema admission holds
/// only for the build that ran it. After a restart the client uploads again.
#[derive(Debug, Clone)]
pub struct Staged {
    /// 32 lowercase hex digits; the stage lives in `.pp-restore/staged-<id>`.
    pub restore_id: String,
    pub metadata: archive::Metadata,
    /// Schema version of the archived database.
    pub schema_version: u64,
    /// Why an earlier stage could not be removed, if one could not. The new
    /// stage is complete either way; the next open removes the leftover.
    pub cleanup_error: Option<String>,
}

/// Restore work in flight for one [`WriterOwner`].
#[derive(Default)]
pub(crate) struct Activity {
    state: Mutex<ActivityState>,
    idle: Condvar,
    /// Serializes staging, so one stage never removes another's directory.
    staging: Mutex<()>,
}

#[derive(Default)]
struct ActivityState {
    closed: bool,
    paused: bool,
    active: usize,
}

/// One running restore operation. Dropping it lets a closing owner go on.
struct Active<'a>(&'a Activity);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        let mut state = self.0.lock();
        state.active -= 1;
        self.0.idle.notify_all();
    }
}

impl Activity {
    fn lock(&self) -> MutexGuard<'_, ActivityState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn enter(&self) -> Result<Active<'_>> {
        let mut state = self.lock();
        ensure!(!state.closed, "Storage owner is closed");
        ensure!(!state.paused, Refusal::Busy);
        state.active += 1;
        Ok(Active(self))
    }

    /// Refuses new restore work. Work already running goes on.
    pub(crate) fn refuse_new(&self) {
        self.lock().closed = true;
    }

    /// Waits until no restore work runs or `deadline` passes. Returns how
    /// many operations are still running.
    pub(crate) fn wait_until(&self, deadline: Instant) -> usize {
        let mut state = self.lock();
        while state.active > 0 && Instant::now() < deadline {
            state = self
                .idle
                .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state.active
    }
}

/// Restore staging for the data directory of one open [`WriterOwner`].
#[derive(Clone)]
pub struct InstallationClient {
    client: SettingsClient,
    data_dir: PathBuf,
    activity: Arc<Activity>,
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
            activity: self.restore.clone(),
        })
    }
}

/// An archive upload being written under `<data>/.pp-restore/`. Dropping it
/// removes the file.
pub struct Upload {
    file: File,
    path: PathBuf,
    written: u64,
    limit: u64,
    /// Set by the first failed write. It never clears, and
    /// [`InstallationClient::stage`] refuses a failed upload.
    failed: bool,
    activity: Arc<Activity>,
}

impl Upload {
    /// Appends `bytes`, refusing with [`Refusal::TooLarge`] once the total
    /// would pass the limit. Nothing past the limit is written. The first
    /// failed write, including one refused because the owner is closing,
    /// fails the upload for good.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            !self.failed,
            Refusal::InvalidArchive("the upload already failed".into())
        );
        let result = (|| {
            let _active = self.activity.enter()?;
            let written = self.written.saturating_add(bytes.len() as u64);
            ensure!(written <= self.limit, Refusal::TooLarge(self.limit));
            self.file.write_all(bytes)?;
            self.written = written;
            Ok(())
        })();
        self.failed |= result.is_err();
        result
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl InstallationClient {
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
        let _active = self.activity.enter()?;
        let _gate = self
            .activity
            .staging
            .lock()
            .map_err(|_| anyhow!("Restore staging lock poisoned"))?;
        let staged = self
            .data_dir
            .join(WORK_DIR)
            .join(format!("staged-{restore_id}"));
        ensure!(
            staged.symlink_metadata().is_ok_and(|m| m.is_dir()),
            Refusal::UnknownRestore
        );
        ensure!(
            journal::Journal::read(&self.data_dir)?.is_none(),
            "Restart to finish the previous restore"
        );
        require_marker(&self.data_dir)?;
        admit_schema(&staged)?;
        let (reply, receiver) = mpsc::channel();
        {
            let mut queue = self
                .client
                .shared
                .queue
                .lock()
                .map_err(|_| anyhow!("Writer admission poisoned"))?;
            ensure!(!queue.closed && !queue.restoring, "Storage stopped");
            queue
                .pending
                .push_back(Envelope::Control(crate::storage_control::Command::Restore(
                    activation::Command { staged, reply },
                )));
            self.client.shared.changed.notify_all();
        }
        receiver
            .recv()
            .map_err(|_| anyhow!("Writer stopped without restore result"))?
    }
    /// Starts an upload of at most `limit` bytes, capped at
    /// [`MAX_UPLOAD_BYTES`].
    pub fn begin_upload(&self, limit: u64) -> Result<Upload> {
        let _active = self.activity.enter()?;
        require_marker(&self.data_dir)?;
        let work = work_dir(&self.data_dir)?;
        #[cfg(all(test, unix))]
        tests::after_work_dir(&self.data_dir);
        let path = work.join(format!("{INCOMING_PREFIX}{}{INCOMING_SUFFIX}", random_id()));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Upload {
            file,
            path,
            written: 0,
            limit: limit.min(MAX_UPLOAD_BYTES),
            failed: false,
            activity: self.activity.clone(),
        })
    }

    /// Validates and extracts `upload` and admits its schema. Live files are
    /// not touched and the upload is removed either way. A refusal removes
    /// only its own stage. Earlier stages are removed only after the new one
    /// is complete; if that fails, the new stage is still returned, with
    /// [`Staged::cleanup_error`] set, and the next open removes the leftover.
    /// `.pp-restore` itself stays until the next open, so a concurrent
    /// [`Self::begin_upload`] never loses its directory.
    pub fn stage(&self, upload: Upload) -> Result<Staged> {
        ensure!(
            Arc::ptr_eq(&upload.activity, &self.activity),
            "Upload belongs to another storage owner"
        );
        let _active = self.activity.enter()?;
        let _staging =
            (self.activity.staging.lock()).map_err(|_| anyhow!("Restore staging lock poisoned"))?;
        require_marker(&self.data_dir)?;
        ensure!(
            !upload.failed,
            Refusal::InvalidArchive("the upload did not complete".into())
        );
        upload.file.sync_all()?;
        let work = work_dir(&self.data_dir)?;
        let restore_id = random_id();
        let staged = work.join(format!("{STAGED_PREFIX}{restore_id}"));
        pp_os::create_private_dir(&staged)?;
        let result = (|| {
            let metadata = archive::extract(&upload.path, archive::Limits::default(), &staged)
                .map_err(|error| Refusal::InvalidArchive(format!("{error:#}")))?;
            ensure!(
                metadata.scope == archive::MetadataScope::Full,
                Refusal::InvalidArchive("restore needs a full backup".into())
            );
            let schema_version = admit_schema(&staged).map_err(|error| {
                let detail = match error
                    .chain()
                    .find_map(|cause| cause.downcast_ref::<crate::versions::IncompatibleSchema>())
                {
                    Some(refusal) if !refusal.too_new() => format!(
                        "cannot upgrade database schema version {} (this release opens schema {} to {}). \
                         Restore this backup into Print Partner v3.3.0 and make a new backup, \
                         then restore the new backup into this release. The live database was not changed.",
                        refusal.found, refusal.oldest, refusal.newest
                    ),
                    Some(refusal) => refusal.to_string().trim_start_matches("Incompatible storage: ").to_owned(),
                    None => format!("{error:#}"),
                };
                Refusal::UnsupportedSchema(detail)
            })?;
            Ok((metadata, schema_version))
        })();
        drop(upload);
        let (metadata, schema_version) = result.inspect_err(|_| {
            let _ = std::fs::remove_dir_all(&staged);
        })?;
        Ok(Staged {
            cleanup_error: remove_earlier_stages(&work, &restore_id).err(),
            restore_id,
            metadata,
            schema_version,
        })
    }
}

/// Removes every `staged-<id>` directory except `keep`, and reports the
/// first failure. Entries of another name or type are left alone.
fn remove_earlier_stages(work: &Path, keep: &str) -> Result<(), String> {
    let mut first_error = None;
    let entries = std::fs::read_dir(work).map_err(|error| format!("List {WORK_DIR}: {error}"))?;
    for entry in entries {
        let removed = entry.map_err(anyhow::Error::from).and_then(|entry| {
            let name = entry.file_name();
            match leftover(&entry.path(), &name.to_string_lossy()) {
                Ok(Some(Leftover::Staged))
                    if name.to_str() != Some(&format!("{STAGED_PREFIX}{keep}")) =>
                {
                    remove_stage(&entry.path())
                        .with_context(|| format!("Remove {}", name.to_string_lossy()))
                }
                _ => Ok(()),
            }
        });
        if let Err(error) = removed {
            first_error.get_or_insert(format!("{error:#}"));
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Removes one stage directory. Tests can make it fail without relying on
/// file permissions, which root ignores.
fn remove_stage(path: &Path) -> std::io::Result<()> {
    #[cfg(all(test, unix))]
    tests::removal_fault(path)?;
    std::fs::remove_dir_all(path)
}

fn require_marker(data_dir: &Path) -> Result<()> {
    match ownership_marker::inspect(data_dir) {
        ownership_marker::MarkerState::Valid(_) => Ok(()),
        _ => Err(Refusal::Unmarked.into()),
    }
}

/// Creates `<data>/.pp-restore` if needed and refuses one that is not a
/// real directory, so no upload or extraction follows a link out of the
/// data directory.
fn work_dir(data_dir: &Path) -> Result<PathBuf> {
    let work = data_dir.join(WORK_DIR);
    if work.symlink_metadata().is_err() {
        #[cfg(all(test, unix))]
        tests::before_work_dir_create(data_dir);
        // An upload and a stage can both find it missing; the loser of the
        // create sees `AlreadyExists`, and the check below decides.
        match pp_os::create_private_dir(&work) {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            result => result?,
        }
    }
    let status = pp_os::status_nofollow(&work)?;
    ensure!(
        status.is_dir()
            && status
                .posix_access()
                .is_none_or(|access| access.owned_by_current_user()),
        "Restore work directory {} is not a directory owned by this user",
        work.display()
    );
    Ok(work)
}

fn random_id() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

fn is_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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

/// A `.pp-restore` entry this build creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leftover {
    /// An unpublished journal intent; no live rename can precede publication.
    JournalTemporary,
    /// `incoming-<32 hex>.tar.gz`, a regular file.
    Incoming,
    /// `staged-<32 hex>`, a real directory.
    Staged,
}

/// Which leftover `path` (named `name`) is, `None` for any other name. A
/// leftover name on a link or the wrong type is an error.
fn leftover(path: &Path, name: &str) -> Result<Option<Leftover>> {
    let kind = if name == "journal.json.tmp" {
        Leftover::JournalTemporary
    } else if name.strip_prefix(STAGED_PREFIX).is_some_and(is_id) {
        Leftover::Staged
    } else if name
        .strip_prefix(INCOMING_PREFIX)
        .and_then(|rest| rest.strip_suffix(INCOMING_SUFFIX))
        .is_some_and(is_id)
    {
        Leftover::Incoming
    } else {
        return Ok(None);
    };
    let file_type = path.symlink_metadata()?.file_type();
    match kind {
        Leftover::Staged if file_type.is_dir() => Ok(Some(kind)),
        Leftover::Incoming | Leftover::JournalTemporary if file_type.is_file() => Ok(Some(kind)),
        _ => bail!("{name} is a link or has the wrong type"),
    }
}

/// Clears an interrupted stage. Runs at open, under the directory lock and
/// after the ownership marker and both read-only schema preflights were admitted.
/// Every entry is checked before anything is removed: only a regular file
/// `incoming-<id>.tar.gz` and a real directory `staged-<id>` are removed. Any
/// other entry, a link, a wrong type, or a `.pp-restore` that is not a real
/// directory refuses the open without removing anything, because this build
/// cannot tell whether it holds the only copy of an installation.
pub(crate) fn recover(data_dir: &Path) -> Result<bool> {
    let work = data_dir.join(WORK_DIR);
    let Ok(metadata) = work.symlink_metadata() else {
        return Ok(false);
    };
    let refuse = |detail: String| {
        anyhow!(
            "Restore work directory {} {detail}; move it aside to open",
            work.display()
        )
    };
    if !metadata.file_type().is_dir() {
        return Err(refuse("is not a directory".into()));
    }
    journal::validate_work(data_dir)?;
    if journal::recover(data_dir)? {
        recover(data_dir)?;
        return Ok(true);
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&work)? {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        match leftover(&path, &name) {
            Ok(Some(kind)) => found.push((path.clone(), kind)),
            Ok(None) => return Err(refuse(format!("holds unexpected entry {name}"))),
            Err(error) => return Err(refuse(format!("holds {error:#}"))),
        }
    }
    found
        .iter()
        .try_for_each(|(path, kind)| match kind {
            Leftover::Staged => std::fs::remove_dir_all(path),
            Leftover::Incoming | Leftover::JournalTemporary => std::fs::remove_file(path),
        })
        .and_then(|()| std::fs::remove_dir(&work))
        .map_err(|error| refuse(format!("could not be cleared: {error}")))?;
    Ok(false)
}

/// Test hook: [`WriterOwner`]'s shutdown calls it once writer, reader and
/// restore admission are closed, with the one deadline both drains use.
#[cfg(all(test, unix))]
pub(crate) fn admission_closed(data_dir: &Path, deadline: Instant) {
    tests::admission_closed(data_dir, deadline);
}

#[cfg(all(test, unix))]
pub(super) mod tests;
