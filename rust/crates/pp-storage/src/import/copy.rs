//! Writes the import into a new Rust-owned data folder.
//!
//! Everything is staged in `<destination>/.pp-import/`: the database copy,
//! its upgrade and the roots. Only a finished stage is moved into place, and
//! the ownership marker is written last. An import that stops early leaves a
//! folder without a marker, which no Rust runtime opens.

use super::{
    Destination, DryRunReport, Files, ImportError, STAGE_RESERVE, Scratch, SourceKind,
    ensure_space, inspect_destination, inventory, measure, open_regular, repo_files,
    stage_directory, unreadable, upgrade, verify,
};
use crate::{archive, ownership_marker};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    fs::File,
    io::{Read, Write},
    path::Path,
};

const STAGE: &str = ".pp-import";
/// The saved report, in the destination.
pub const REPORT_FILE: &str = "import-report.json";

/// The item count of one stored shape.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ShapeReport {
    pub shape: String,
    pub items: u64,
}

/// One inventory row as the import left it.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct RowReport {
    pub step: u8,
    pub status: String,
    pub shapes: Vec<ShapeReport>,
}

/// What an import wrote. It holds keys, names and counts, never a stored value.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// False while any inventory row is still `pending`. Such a copy is
    /// disposable: import again once every converter exists.
    pub complete: bool,
    pub source_schema: u64,
    pub schema: u64,
    pub builds: u64,
    pub sources: u64,
    pub printers: u64,
    pub unmatched_prints: u64,
    /// Sources whose path now leads into the destination's `repos/`.
    pub rebased_sources: u64,
    /// Files copied or extracted under the roots, and their bytes.
    pub files: u64,
    pub bytes: u64,
    pub rows: Vec<RowReport>,
    /// Rows per table in the imported database.
    pub tables: BTreeMap<String, u64>,
}

impl ImportReport {
    fn new(counts: &DryRunReport, rebased_sources: u64, (files, bytes): (u64, u64)) -> Self {
        let rows = counts.rows.iter().map(|(row, shapes)| RowReport {
            step: row.step,
            status: row.status.to_string(),
            shapes: (shapes.iter())
                .map(|(shape, items)| ShapeReport {
                    shape: shape.to_string(),
                    items: *items,
                })
                .collect(),
        });
        Self {
            complete: (inventory::ROWS.iter()).all(|row| row.status != inventory::Status::Pending),
            source_schema: counts.schema_version,
            schema: counts.upgraded_schema,
            builds: counts.builds,
            sources: counts.sources,
            printers: counts.printers,
            unmatched_prints: counts.unmatched_prints,
            rebased_sources,
            files,
            bytes,
            rows: rows.collect(),
            tables: (counts.tables.iter())
                .map(|table| (table.table.clone(), table.rows))
                .collect(),
        }
    }
}

impl fmt::Display for ImportReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "Imported schema {} as schema {}: {} Builds, {} Sources, {} printers, {} unmatched prints, {} files.",
            self.source_schema,
            self.schema,
            self.builds,
            self.sources,
            self.printers,
            self.unmatched_prints,
            self.files
        )?;
        let pending = (self.rows.iter())
            .filter(|row| row.status == "pending")
            .count();
        writeln!(
            f,
            "complete: {}. {pending} inventory rows are not converted yet. {} Source paths now lead into the destination.",
            self.complete, self.rebased_sources
        )?;
        writeln!(
            f,
            "The report is saved as {REPORT_FILE} in the destination."
        )
    }
}

/// Imports `source`, a stopped Node data folder or a format 2 `full` backup
/// archive, into `destination`, which must be absent or empty by the
/// ownership-marker rule. The source is never opened for writing.
pub fn import(source: &Path, destination: &Path) -> Result<ImportReport, ImportError> {
    let metadata = source
        .metadata()
        .map_err(|error| anyhow::anyhow!("Read import source {}: {error}", source.display()))?;
    match inspect_destination(destination)? {
        Destination::Free => {}
        found => return Err(ImportError::Destination(found)),
    }
    pp_os::create_private_dir_all(destination)?;
    let stage = Scratch(destination.join(STAGE));
    pp_os::create_private_dir(&stage.0)?;
    let (kind, files, copied) = if metadata.is_dir() {
        let files = stage_directory(source, &stage.0)?;
        let copied = copy_roots(source, &stage.0)?;
        (SourceKind::Directory, files, copied)
    } else {
        let (files, copied) = extract_archive(source, metadata.len(), &stage.0)?;
        (SourceKind::Archive, files, copied)
    };
    let (connection, ready, stored) = upgrade(&stage.0, |database| {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
        let source = rusqlite::Connection::open_with_flags(database, flags)?;
        Ok(verify::digests(&source, None)?)
    })?;
    let counts = measure(&connection, kind, &ready, &files)?;
    if !counts.unnamed.is_empty() {
        let shapes = counts.unnamed.iter().map(|unnamed| unnamed.shape.clone());
        return Err(ImportError::Unnamed(shapes.collect()));
    }
    if let Some(value) = counts
        .installation
        .as_deref()
        .filter(|value| *value != "default")
    {
        return Err(ImportError::Installation(value.to_owned()));
    }
    let repos = destination.join("repos");
    let rebased = verify::rebase_sources(&connection, &stage.0.join("repos"), &repos)?;
    let changed = verify::changed(&stored, &verify::digests(&connection, Some(&stored))?);
    if !changed.is_empty() {
        return Err(ImportError::RowsChanged(changed));
    }
    // Sign-in state is not carried: every Node session ends with the import.
    connection.execute_batch(
        "DELETE FROM sessions; DELETE FROM password_reset_tokens; PRAGMA wal_checkpoint(TRUNCATE);",
    )?;
    connection.close().map_err(|(_, error)| error)?;
    let report = ImportReport::new(&counts, rebased, copied);
    publish(&stage.0, destination, &report)?;
    drop(stage);
    ownership_marker::mark_imported(destination)?;
    Ok(report)
}

/// Copies every root of a Node data folder into `stage` and returns the
/// number of files and their bytes. Like a backup, it takes regular files
/// and directories and follows no link.
fn copy_roots(source: &Path, stage: &Path) -> Result<(u64, u64), ImportError> {
    let limit = archive::Limits::default().max_entries;
    let items = archive::plan(source, archive::Scope::Full, limit)?;
    let sizes = items.iter().map(|item| match item {
        archive::Item::File(_, _, bytes) => *bytes,
        archive::Item::Directory(_) => 0,
    });
    ensure_space(stage, sizes.sum::<u64>() + STAGE_RESERVE)?;
    let (mut files, mut bytes) = (0, 0);
    for item in &items {
        match item {
            archive::Item::Directory(name) => pp_os::create_private_dir_all(&stage.join(name))?,
            archive::Item::File(name, path, _) => {
                bytes += copy_verified(path, &stage.join(name))?;
                files += 1;
            }
        }
    }
    Ok((files, bytes))
}

/// Copies one file, keeps its modification time, and reads the copy back:
/// its SHA-256 must equal that of the bytes read from the source.
pub(super) fn copy_verified(from: &Path, to: &Path) -> Result<u64, ImportError> {
    let missing = || unreadable(from, std::io::ErrorKind::NotFound.into());
    let mut source = open_regular(from)?.ok_or_else(missing)?;
    let mut copy = File::create_new(to)?;
    let (mut read, mut bytes, mut buffer) = (Sha256::new(), 0, vec![0; 64 * 1024]);
    loop {
        let filled = source
            .read(&mut buffer)
            .map_err(|error| unreadable(from, error))?;
        if filled == 0 {
            break;
        }
        read.update(&buffer[..filled]);
        copy.write_all(&buffer[..filled])?;
        bytes += filled as u64;
    }
    if let Ok(modified) = source.metadata().and_then(|metadata| metadata.modified()) {
        copy.set_modified(modified)?;
    }
    copy.sync_all()?;
    confirm_copy(to, read.finalize().as_slice())?;
    Ok(bytes)
}

/// Reads the copy at `to` back from disk: its SHA-256 must be `expected`,
/// the digest of the bytes read from the source.
pub(super) fn confirm_copy(to: &Path, expected: &[u8]) -> Result<(), ImportError> {
    let mut written = Sha256::new();
    std::io::copy(&mut File::open(to)?, &mut written)?;
    if written.finalize().as_slice() != expected {
        return Err(ImportError::CopyMismatch(to.to_owned()));
    }
    Ok(())
}

/// Validates a backup archive and extracts all of it into `stage`.
fn extract_archive(
    source: &Path,
    compressed: u64,
    stage: &Path,
) -> Result<(Files, (u64, u64)), ImportError> {
    // The extracted size is unknown until the archive is read; it is at
    // least the archive's own size.
    ensure_space(stage, compressed + STAGE_RESERVE)?;
    let metadata =
        archive::extract(source, archive::Limits::default(), stage).map_err(|error| {
            match error.downcast_ref::<std::io::Error>() {
                Some(io) if io.kind() == std::io::ErrorKind::StorageFull => {
                    ImportError::Failed(error)
                }
                _ => ImportError::InvalidArchive(format!("{error:#}")),
            }
        })?;
    if metadata.scope != archive::MetadataScope::Full {
        return Err(ImportError::PartialArchive(metadata.scope));
    }
    let limit = archive::Limits::default().max_entries;
    let extracted = archive::plan(stage, archive::Scope::Full, limit)?;
    let sizes = extracted.iter().filter_map(|item| match item {
        archive::Item::File(_, _, bytes) => Some(*bytes),
        archive::Item::Directory(_) => None,
    });
    let (files, bytes) = sizes.fold((0, 0), |(files, total), bytes| (files + 1, total + bytes));
    let files_read = Files {
        documents: stage.to_owned(),
        repo_files: repo_files(stage)?,
    };
    Ok((files_read, (files, bytes)))
}

/// Moves the staged database and roots into `destination` and saves the
/// report beside them.
fn publish(stage: &Path, destination: &Path, report: &ImportReport) -> Result<(), ImportError> {
    let roots = archive::FULL_ROOTS.iter().map(|(root, _)| *root);
    for name in [archive::DATABASE_FILE].into_iter().chain(roots) {
        if stage.join(name).symlink_metadata().is_ok() {
            std::fs::rename(stage.join(name), destination.join(name))?;
        }
    }
    let mut saved = File::create_new(destination.join(REPORT_FILE))?;
    saved.write_all(&serde_json::to_vec_pretty(report).map_err(anyhow::Error::from)?)?;
    saved.sync_all()?;
    pp_os::sync_file(&destination.join(archive::DATABASE_FILE))?;
    Ok(pp_os::sync_directory(destination)?)
}
