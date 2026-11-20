//! Owner-bound whole-installation backup export, restore staging and
//! confirmed activation for the Rust runtime.
//!
//! Backup export ([`InstallationClient::export_backup`]) writes a full
//! archive under `<data>/backups/`. Free disk space bounds a backup and a
//! stage; no fixed size limit applies.
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

use crate::{
    Envelope, SettingsClient, WriterFailure, WriterOwner, archive, ownership_marker, schema,
};

pub(crate) mod activation;
mod journal;
pub use activation::{Activated, CONFIRMATION};
use anyhow::{Context, Result, anyhow, bail, ensure};
pub use backup::{BackupFile, BackupWarning};
use std::{
    fmt,
    fs::File,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc},
    time::Instant,
};

mod backup;

const WORK_DIR: &str = ".pp-restore";
const INCOMING_PREFIX: &str = "incoming-";
const INCOMING_SUFFIX: &str = ".tar.gz";
const STAGED_PREFIX: &str = "staged-";

/// A restore refused before any live file changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The data directory has no valid `.pp-rust-owned.json`.
    Unmarked,
    /// The upload exceeded this many bytes.
    TooLarge(u64),
    /// The upload or the extracted stage would leave less than the reserve
    /// free. Nothing was written or extracted.
    NoSpace(archive::NoSpace),
    /// Extraction stopped before writing more than this many bytes, the
    /// total the archive's headers declared. The stage was removed.
    LargerThanDeclared(u64),
    InvalidArchive(String),
    UnsupportedSchema(String),
    /// Too little free disk on the data volume for the schema check's
    /// working copy of the staged database. The stage was removed.
    InsufficientSpace(schema::InsufficientSpace),
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
            Self::NoSpace(no_space) => no_space.fmt(f),
            Self::LargerThanDeclared(declared) => write!(
                f,
                "Backup archive holds more than the {declared} bytes its headers declare"
            ),
            Self::InvalidArchive(detail) => write!(f, "Invalid backup archive: {detail}"),
            Self::UnsupportedSchema(detail) => write!(f, "Unsupported backup schema: {detail}"),
            Self::InsufficientSpace(space) => space.fmt(f),
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
/// Only the [`WriterOwner`] that made a stage can activate it: the next open
/// removes every stage, complete or not, because schema admission holds only
/// for the build that ran it. Before that, a later completed stage replaces
/// it, a successful activation uses it up, and an activation that fails after
/// publishing its journal removes it; one that fails earlier leaves it. Once
/// it is gone the client uploads again.
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

/// Why a typed operation of this family did not complete. Staging predates
/// it and still returns `anyhow` errors that carry a [`Refusal`].
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// The request was refused. Nothing changed. Too little free disk is
    /// [`Refusal::NoSpace`].
    #[error(transparent)]
    Refused(#[from] Refusal),
    /// Activation failed after it began and rolled back: the previous
    /// installation is kept and serving again. The cause is logged.
    #[error("restore failed; the previous installation is kept")]
    RolledBack,
    /// An activation did not finish and could not roll back, or an earlier
    /// one left its journal. A restart replays the journal. In the first
    /// case storage refuses all work until then; in the second only
    /// activation refuses. The cause is logged.
    #[error("restore did not finish; restart to recover")]
    RestartRequired,
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

/// The sizes and disk readings backup and staging decide by. Tests replace
/// them, so no test needs real gigabytes.
#[derive(Clone, Copy)]
pub(crate) struct Sizing {
    pub reserve: u64,
    pub warn_over: u64,
    pub free_space: archive::FreeSpace,
    pub declared_bytes: fn(&File, archive::Limits) -> Result<u64>,
}

impl Sizing {
    const MACHINE: Self = Self {
        reserve: archive::RESERVE_BYTES,
        // A backup archive over 20 GiB carries a warning. It is not refused.
        warn_over: 20 * 1024 * 1024 * 1024,
        free_space: archive::free_space,
        declared_bytes: archive::declared_bytes,
    };

    /// Refuses with [`Refusal::NoSpace`] when writing `bytes` under `path`
    /// would leave less than the reserve free.
    fn require(&self, path: &Path, bytes: u64) -> Result<()> {
        match archive::shortfall(self.free_space, path, bytes, self.reserve)? {
            Some(no_space) => Err(Refusal::NoSpace(no_space).into()),
            None => Ok(()),
        }
    }
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

    /// Starts one restore operation, unless the owner is closing or an
    /// activation has paused restore work.
    fn admit(&self) -> Result<Active<'_>, RestoreError> {
        let mut state = self.lock();
        if state.closed {
            return Err(WriterFailure::Stopped.into());
        }
        if state.paused {
            return Err(Refusal::Busy.into());
        }
        state.active += 1;
        Ok(Active(self))
    }

    /// [`Self::admit`] for staging, which returns `anyhow` errors.
    fn enter(&self) -> Result<Active<'_>> {
        self.admit().map_err(|error| match error {
            RestoreError::Refused(refusal) => refusal.into(),
            _ => anyhow!("Storage owner is closed"),
        })
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

/// Backup export and restore staging for the data directory of one open
/// [`WriterOwner`].
#[derive(Clone)]
pub struct InstallationClient {
    client: SettingsClient,
    data_dir: PathBuf,
    activity: Arc<Activity>,
    sizing: Sizing,
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
            sizing: Sizing::MACHINE,
        })
    }
}

/// An archive upload being written under `<data>/.pp-restore/`. Dropping it
/// removes the file through the handle that created it.
pub struct Upload {
    file: File,
    /// `.pp-restore` as [`InstallationClient::begin_upload`] opened it.
    work: pp_os::Dir,
    name: String,
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
        let _ = self.work.remove_file(&self.name);
    }
}

impl InstallationClient {
    /// Activates the staged restore `restore_id` once `confirmation` equals
    /// [`CONFIRMATION`]. Blocks until the writer has reopened.
    pub fn activate(
        &self,
        restore_id: &str,
        confirmation: &str,
    ) -> Result<Activated, RestoreError> {
        if confirmation != CONFIRMATION {
            return Err(Refusal::Confirmation.into());
        }
        let _active = self.activity.admit()?;
        let _gate = (self.activity.staging.lock()).map_err(|_| WriterFailure::Stopped)?;
        if !is_id(restore_id) {
            return Err(Refusal::UnknownRestore.into());
        }
        let staged = format!("{STAGED_PREFIX}{restore_id}");
        // A missing or linked `.pp-restore` holds no stage this owner made.
        let Some(work) = Work::open(&self.data_dir).ok().flatten() else {
            return Err(Refusal::UnknownRestore.into());
        };
        let Ok(stage) = work.dir.open_dir_nofollow(&staged) else {
            return Err(Refusal::UnknownRestore.into());
        };
        if (journal::Journal::read(&work).map_err(WriterFailure::from)?).is_some() {
            return Err(RestoreError::RestartRequired);
        }
        if !marked(&self.data_dir) {
            return Err(Refusal::Unmarked.into());
        }
        admit_staged(&stage, &work.path.join(&staged))
            .map_err(|error| RestoreError::Refused(schema_refusal(error)))?;
        drop(stage);
        let (reply, receiver) = mpsc::channel();
        {
            let shared = &self.client.shared;
            let mut queue = shared.queue.lock().map_err(|_| WriterFailure::Stopped)?;
            if queue.closed || queue.restoring {
                return Err(WriterFailure::Stopped.into());
            }
            let command = activation::Command {
                work,
                staged,
                reply,
            };
            (queue.pending).push_back(Envelope::Control(crate::storage_control::Command::Restore(
                command,
            )));
            shared.changed.notify_all();
        }
        receiver.recv().map_err(|_| WriterFailure::Stopped)?
    }

    /// Starts an upload of at most `limit` bytes, the size the caller
    /// declares. Refuses with [`Refusal::NoSpace`] when that many bytes would
    /// leave less than the reserve free.
    pub fn begin_upload(&self, limit: u64) -> Result<Upload> {
        let _active = self.activity.enter()?;
        require_marker(&self.data_dir)?;
        let work = Work::create(&self.data_dir)?;
        #[cfg(all(test, unix))]
        tests::after_work_dir(&self.data_dir);
        // `.pp-restore` is inside the data directory, on its volume.
        self.sizing.require(&self.data_dir, limit)?;
        let name = format!("{INCOMING_PREFIX}{}{INCOMING_SUFFIX}", random_id());
        let file = work.dir.create_new_nofollow(&name)?;
        Ok(Upload {
            file,
            work: work.dir,
            name,
            written: 0,
            limit,
            failed: false,
            activity: self.activity.clone(),
        })
    }

    /// Validates and extracts `upload` and admits its schema. A first pass
    /// totals the sizes the archive's tar headers declare and refuses with
    /// [`Refusal::NoSpace`] before extracting anything. Extraction then stops
    /// with [`Refusal::LargerThanDeclared`] before it writes more than that
    /// total. Live files are not touched and the upload is removed either
    /// way. A refusal removes only its own stage. Earlier stages are removed
    /// only after the new one is complete; if that fails, the new stage is
    /// still returned, with [`Staged::cleanup_error`] set, and the next open
    /// removes the leftover. `.pp-restore` itself stays until the next open,
    /// so a concurrent [`Self::begin_upload`] never loses its directory.
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
        let work = Work::create(&self.data_dir)?;
        let restore_id = random_id();
        let staged = format!("{STAGED_PREFIX}{restore_id}");
        work.dir.create_dir(&staged)?;
        let result =
            (|| {
                let stage = work.dir.open_dir_nofollow(&staged)?;
                let stage_path = work.path.join(&staged);
                let limits = archive::Limits::backup();
                let declared = (self.sizing.declared_bytes)(&upload.file, limits)
                    .map_err(|error| Refusal::InvalidArchive(format!("{error:#}")))?;
                self.sizing.require(&self.data_dir, declared)?;
                let limits = archive::Limits {
                    max_total_bytes: declared,
                    ..limits
                };
                let source = upload.file.try_clone()?;
                let metadata = archive::extract_into(source, limits, (&stage, &stage_path))
                    .map_err(|error| match error.downcast::<archive::TotalExceeded>() {
                        Ok(exceeded) => Refusal::LargerThanDeclared(exceeded.limit),
                        Err(error) => Refusal::InvalidArchive(format!("{error:#}")),
                    })?;
                ensure!(
                    metadata.scope == archive::MetadataScope::Full,
                    Refusal::InvalidArchive("restore needs a full backup".into())
                );
                let schema_version = admit_staged(&stage, &stage_path).map_err(schema_refusal)?;
                Ok((metadata, schema_version))
            })();
        drop(upload);
        let (metadata, schema_version) = result.inspect_err(|_| {
            let _ = work.dir.remove_all(&staged);
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
fn remove_earlier_stages(work: &Work, keep: &str) -> Result<(), String> {
    let mut first_error = None;
    let entries = (work.dir.entries()).map_err(|error| format!("List {WORK_DIR}: {error}"))?;
    for entry in entries {
        let removed = entry.map_err(anyhow::Error::from).and_then(|name| {
            let Some(name) = name.to_str() else {
                return Ok(());
            };
            match leftover(&work.dir, name) {
                Ok(Some(Leftover::Staged)) if name != format!("{STAGED_PREFIX}{keep}") => {
                    remove_stage(work, name).with_context(|| format!("Remove {name}"))
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
fn remove_stage(work: &Work, name: &str) -> std::io::Result<()> {
    #[cfg(all(test, unix))]
    tests::removal_fault(&work.path.join(name))?;
    work.dir.remove_all(name)
}

fn marked(data_dir: &Path) -> bool {
    matches!(
        ownership_marker::inspect(data_dir),
        ownership_marker::MarkerState::Valid(_)
    )
}

fn require_marker(data_dir: &Path) -> Result<()> {
    ensure!(marked(data_dir), Refusal::Unmarked);
    Ok(())
}

/// `<data>/.pp-restore`, opened once relative to a handle on the data
/// directory without following a link: `openat` with `O_NOFOLLOW |
/// O_DIRECTORY` on Unix. That open is the check. Staging, activation and
/// [`recover`] then create, open, list, rename and remove its entries only
/// through `dir`, and move live entries only through `data`, so swapping
/// `.pp-restore` for a link afterwards redirects nothing. `path` names it in
/// messages and for SQLite, which needs a pathname and gets one only after
/// [`archive::same_directory`] confirms it still names the handle.
pub(crate) struct Work {
    data: pp_os::Dir,
    data_dir: PathBuf,
    dir: pp_os::Dir,
    path: PathBuf,
}

impl Work {
    /// Opens `.pp-restore`, or returns `None` when it is missing. A link or
    /// any other non-directory is an error.
    pub(crate) fn open(data_dir: &Path) -> Result<Option<Self>> {
        let data = pp_os::Dir::open(data_dir)
            .with_context(|| format!("Open data directory {}", data_dir.display()))?;
        Self::open_in(data, data_dir)
    }

    fn open_in(data: pp_os::Dir, data_dir: &Path) -> Result<Option<Self>> {
        let path = data_dir.join(WORK_DIR);
        let dir = match data.open_dir_nofollow(WORK_DIR) {
            Ok(dir) => dir,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                ensure!(
                    !(data.status_nofollow(WORK_DIR)).is_ok_and(|status| !status.is_dir()),
                    "Restore work directory {} is not a directory",
                    path.display()
                );
                return Err(anyhow::Error::from(error).context(format!("Open {}", path.display())));
            }
        };
        #[cfg(all(test, unix))]
        tests::after_work_check(data_dir);
        Ok(Some(Self {
            data,
            data_dir: data_dir.to_owned(),
            dir,
            path,
        }))
    }

    /// Creates `.pp-restore` if needed and opens it as [`Self::open`] does,
    /// refusing one this user does not own.
    fn create(data_dir: &Path) -> Result<Self> {
        let data = pp_os::Dir::open(data_dir)
            .with_context(|| format!("Open data directory {}", data_dir.display()))?;
        if data.status_nofollow(WORK_DIR).is_err() {
            #[cfg(all(test, unix))]
            tests::before_work_dir_create(data_dir);
            // An upload and a stage can both find it missing; the loser of the
            // create sees `AlreadyExists`, and the open below decides.
            match data.create_dir(WORK_DIR) {
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                result => result?,
            }
        }
        let path = data_dir.join(WORK_DIR);
        let work = Self::open_in(data, data_dir)?
            .ok_or_else(|| anyhow!("Restore work directory {} disappeared", path.display()))?;
        ensure!(
            (work.dir.posix_access()?).is_none_or(|access| access.owned_by_current_user()),
            "Restore work directory {} is not a directory owned by this user",
            path.display()
        );
        Ok(work)
    }

    /// Refuses unless `<data>/.pp-restore` still names `dir`, so that the
    /// next open finds the journal written there. Activation runs this before
    /// it publishes the journal and before each live rename, after the test
    /// hook for that step.
    pub(super) fn forward_step(&self) -> Result<()> {
        activation::hooks::step(&self.data_dir)?;
        let held = self.dir.status()?.identity();
        let named = self.data.status_nofollow(WORK_DIR).ok();
        ensure!(
            named.is_some_and(|named| named.is_dir() && named.identity() == held),
            "Restore work directory {} no longer names the directory holding the journal",
            self.path.display()
        );
        Ok(())
    }
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

/// Turns a schema preflight failure into a restore refusal. The working
/// copy is on the data volume, so too little free disk is
/// [`Refusal::InsufficientSpace`], not an unsupported schema.
fn schema_refusal(error: anyhow::Error) -> Refusal {
    if let Some(space) = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<schema::InsufficientSpace>())
    {
        return Refusal::InsufficientSpace(*space);
    }
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
        Some(refusal) => refusal
            .to_string()
            .trim_start_matches("Incompatible storage: ")
            .to_owned(),
        None => format!("{error:#}"),
    };
    Refusal::UnsupportedSchema(detail)
}

/// The schema version of the database in `directory`, refused as startup
/// refuses a live one: newer than supported, too old to upgrade, missing, or
/// failing integrity or job-table checks. The preflight's working copy goes
/// in a folder inside `directory`, on the data filesystem.
pub(crate) fn admit_schema(directory: &Path) -> Result<u64> {
    let stage = pp_os::Dir::open(directory)
        .with_context(|| format!("Open stage {}", directory.display()))?;
    preflight(
        &stage,
        directory,
        |name| File::open(directory.join(name)),
        || Ok(()),
    )
}

/// [`admit_schema`] for `stage`, a directory under `.pp-restore` named
/// `path`. Its files are read through the handle, and every step of the
/// preflight, including the one before SQLite opens the working copy by
/// pathname, first checks that `path` still names `stage`.
fn admit_staged(stage: &pp_os::Dir, path: &Path) -> Result<u64> {
    preflight(
        stage,
        path,
        |name| stage.open_read_nofollow(name),
        || archive::same_directory(stage, path),
    )
}

fn preflight(
    stage: &pp_os::Dir,
    directory: &Path,
    open_file: impl Fn(&str) -> std::io::Result<File>,
    validate_directory: impl Fn() -> Result<()>,
) -> Result<u64> {
    let version = schema::preflight(
        #[cfg(test)]
        &directory.join(archive::DATABASE_FILE),
        #[cfg(test)]
        false,
        (stage, directory),
        open_file,
        validate_directory,
    )?;
    ensure!(version > 0, "database has no schema version");
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

/// Which leftover the entry `name` of `work` is, `None` for any other name.
/// A leftover name on a link or the wrong type is an error.
fn leftover(work: &pp_os::Dir, name: &str) -> Result<Option<Leftover>> {
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
    let status = work.status_nofollow(name)?;
    match kind {
        Leftover::Staged if status.is_dir() => Ok(Some(kind)),
        Leftover::Incoming | Leftover::JournalTemporary if status.is_file() => Ok(Some(kind)),
        _ => bail!("{name} is a link or has the wrong type"),
    }
}

/// Clears an interrupted backup export, every stage, and an interrupted
/// activation. Runs at open, under the directory lock and after the ownership
/// marker and both read-only schema preflights were admitted, before any
/// client exists. Every entry is checked before anything is removed. A
/// journal is rolled back or finished first, through the copies it names.
/// Then only a regular file `incoming-<id>.tar.gz` or `journal.json.tmp` and
/// a real directory `staged-<id>` are removed, and `.pp-restore` itself. Any
/// other entry, a link, a wrong type, or a `.pp-restore` that is not a real
/// directory refuses the open without removing anything, because this build
/// cannot tell whether it holds the only copy of an installation.
pub(crate) fn recover(data_dir: &Path) -> Result<bool> {
    let replayed = recover_work(data_dir)?;
    // Only an open that goes ahead removes anything.
    backup::sweep(data_dir);
    Ok(replayed)
}

fn recover_work(data_dir: &Path) -> Result<bool> {
    let work = match Work::open(data_dir) {
        Ok(Some(work)) => work,
        Ok(None) => return Ok(false),
        Err(error) => return Err(anyhow!("{error:#}; move it aside to open")),
    };
    let refuse = |detail: String| {
        anyhow!(
            "Restore work directory {} {detail}; move it aside to open",
            work.path.display()
        )
    };
    journal::validate_work(&work)?;
    let replayed = journal::recover(&work)?;
    let mut found = Vec::new();
    for name in work.dir.entries()? {
        let name = name?;
        let Some(name) = name.to_str() else {
            let name = name.to_string_lossy();
            return Err(refuse(format!("holds unexpected entry {name}")));
        };
        match leftover(&work.dir, name) {
            Ok(Some(kind)) => found.push((name.to_owned(), kind)),
            Ok(None) => return Err(refuse(format!("holds unexpected entry {name}"))),
            Err(error) => return Err(refuse(format!("holds {error:#}"))),
        }
    }
    found
        .iter()
        .try_for_each(|(name, kind)| match kind {
            Leftover::Staged => work.dir.remove_all(name),
            Leftover::Incoming | Leftover::JournalTemporary => work.dir.remove_file(name),
        })
        // Removing `.pp-restore` itself never follows a link: `unlinkat`
        // with `AT_REMOVEDIR` refuses one.
        .and_then(|()| work.data.remove_dir(WORK_DIR))
        .map_err(|error| refuse(format!("could not be cleared: {error}")))?;
    Ok(replayed)
}

/// Test hook: [`WriterOwner`]'s shutdown calls it once writer, reader and
/// restore admission are closed, with the one deadline both drains use.
#[cfg(all(test, unix))]
pub(crate) fn admission_closed(data_dir: &Path, deadline: Instant) {
    tests::admission_closed(data_dir, deadline);
}

#[cfg(all(test, unix))]
pub(super) mod tests;
