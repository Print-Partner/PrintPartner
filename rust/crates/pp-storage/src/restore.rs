//! Whole-installation restore for the Rust runtime: staging.
//!
//! Staging writes an uploaded app backup archive (Node's format 2, see
//! [`crate::archive`]) under `<data>/.pp-restore/`, validates and extracts it
//! there, and admits its schema as startup admits a live database. It never
//! touches live files. Activation, which swaps the staged copy into place,
//! is not part of this module yet.

use crate::{WriterOwner, archive, ownership_marker, schema};
use anyhow::{Context, Result, anyhow, ensure};
use std::{
    fmt,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

const WORK_DIR: &str = ".pp-restore";
const INCOMING_PREFIX: &str = "incoming-";
const INCOMING_SUFFIX: &str = ".tar.gz";
/// The most any upload may hold, whatever limit the caller asks for. It
/// matches the default decompressed limit in [`archive::Limits`].
pub const MAX_UPLOAD_BYTES: u64 = 20 * 1024 * 1024 * 1024;
const STAGED_PREFIX: &str = "staged-";

/// Serializes staging in this process, so one stage never removes the
/// directory another is writing.
static GATE: Mutex<()> = Mutex::new(());

/// A restore refused before any live file changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The data directory has no valid `.pp-rust-owned.json`.
    Unmarked,
    /// The upload exceeded this many bytes.
    TooLarge(u64),
    InvalidArchive(String),
    UnsupportedSchema(String),
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
        }
    }
}

impl std::error::Error for Refusal {}

/// A validated archive extracted beside the live installation.
#[derive(Debug, Clone)]
pub struct Staged {
    /// 32 lowercase hex digits; the stage lives in `.pp-restore/staged-<id>`.
    pub restore_id: String,
    pub metadata: archive::Metadata,
    /// Schema version of the archived database.
    pub schema_version: u64,
}

/// Restore staging for the data directory of one open [`WriterOwner`].
#[derive(Clone)]
pub struct InstallationClient {
    data_dir: PathBuf,
}

impl WriterOwner {
    pub fn installation(&self) -> Result<InstallationClient> {
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow!("Storage owner is closed"))?;
        Ok(InstallationClient {
            data_dir: lease.data_dir().to_owned(),
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
    /// Set when a write failed; [`InstallationClient::stage`] refuses it.
    failed: bool,
}

impl Upload {
    /// Appends `bytes`, refusing with [`Refusal::TooLarge`] once the total
    /// would pass the limit. Nothing past the limit is written. After any
    /// failed write the upload can no longer be staged.
    pub fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let written = self.written.saturating_add(bytes.len() as u64);
        self.failed = true;
        ensure!(written <= self.limit, Refusal::TooLarge(self.limit));
        self.file.write_all(bytes)?;
        self.written = written;
        self.failed = false;
        Ok(())
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl InstallationClient {
    /// Starts an upload of at most `limit` bytes, capped at
    /// [`MAX_UPLOAD_BYTES`].
    pub fn begin_upload(&self, limit: u64) -> Result<Upload> {
        require_marker(&self.data_dir)?;
        let work = work_dir(&self.data_dir)?;
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
        })
    }

    /// Validates and extracts `upload` and admits its schema. On success it
    /// replaces any earlier staged restore; on a refusal the earlier one is
    /// kept. The upload is removed either way. Live files are not touched.
    pub fn stage(&self, upload: Upload) -> Result<Staged> {
        let _gate = gate()?;
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
            let schema_version = admit_schema(&staged)
                .map_err(|error| Refusal::UnsupportedSchema(format!("{error:#}")))?;
            Ok(Staged {
                restore_id,
                metadata,
                schema_version,
            })
        })();
        drop(upload);
        match &result {
            Ok(_) => remove_entries(&work, |name| {
                is_staged(name) && Some(name) != staged_name(&staged)
            })?,
            Err(_) => {
                let _ = std::fs::remove_dir_all(&staged);
                let _ = std::fs::remove_dir(&work);
            }
        }
        result
    }
}

fn gate() -> Result<MutexGuard<'static, ()>> {
    GATE.lock().map_err(|_| anyhow!("Restore gate poisoned"))
}

/// `staged-<32 hex>`, the only stage name this build creates or removes.
fn is_staged(name: &str) -> bool {
    name.strip_prefix(STAGED_PREFIX).is_some_and(is_id)
}

fn staged_name(staged: &Path) -> Option<&str> {
    staged.file_name().and_then(|name| name.to_str())
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
    if !exists(&work) {
        pp_os::create_private_dir(&work)?;
    }
    ensure!(
        work.symlink_metadata()?.file_type().is_dir(),
        "Restore work directory {} is not a directory",
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

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// Removes each entry of `directory` whose name passes `remove`, without
/// following links.
fn remove_entries(directory: &Path, remove: impl Fn(&str) -> bool) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_str().is_some_and(&remove) {
            continue;
        }
        if entry.file_type()?.is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

/// Clears an interrupted stage. Runs at open, under the directory lock and
/// after the ownership marker was admitted, before the database is read.
/// Only `incoming-<id>.tar.gz` and `staged-<id>` are removed. Any other entry
/// in `.pp-restore`, or a `.pp-restore` that is not a directory, refuses the
/// open without removing anything, because this build cannot tell whether it
/// holds the only copy of an installation.
pub(crate) fn recover(data_dir: &Path) -> Result<()> {
    let work = data_dir.join(WORK_DIR);
    let Ok(metadata) = work.symlink_metadata() else {
        return Ok(());
    };
    ensure!(
        metadata.file_type().is_dir(),
        "Restore work directory {} is not a directory; move it aside to open",
        work.display()
    );
    let leftover = |name: &str| {
        is_staged(name)
            || name
                .strip_prefix(INCOMING_PREFIX)
                .and_then(|rest| rest.strip_suffix(INCOMING_SUFFIX))
                .is_some_and(is_id)
    };
    for entry in std::fs::read_dir(&work)? {
        let name = entry?.file_name();
        ensure!(
            name.to_str().is_some_and(leftover),
            "Restore work directory {} holds unexpected entry {}; move it aside to open",
            work.display(),
            name.to_string_lossy()
        );
    }
    remove_entries(&work, leftover)
        .and_then(|()| Ok(std::fs::remove_dir(&work)?))
        .with_context(|| {
            format!(
                "Clear the interrupted restore in {}; move it aside to open",
                work.display()
            )
        })
}

#[cfg(all(test, unix))]
mod tests;
