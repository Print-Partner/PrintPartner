//! Writes the import into a new Rust-owned data folder.
//!
//! Everything is staged in `<destination>/.pp-import/`: the database copy,
//! its upgrade and the roots. Only a finished stage is moved into place, and
//! the ownership marker is written last. An import that stops early leaves a
//! folder without a marker, which no Rust runtime opens.

use super::{
    DATABASE_FILES, Destination, DryRunReport, Files, ImportError, STAGE_RESERVE, Scratch,
    SourceKind, ensure_outside, inventory, io_error, measure, open_regular, place_destination,
    repo_files, scan_roots, stage_directory, upgrade, verify,
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
    /// Source tables, and their rows, that arrived as the source stored them.
    pub verified_tables: u64,
    pub verified_rows: u64,
    /// Files copied or extracted under the roots, and their bytes.
    pub files: u64,
    pub bytes: u64,
    pub rows: Vec<RowReport>,
    /// Rows per table in the imported database.
    pub tables: BTreeMap<String, u64>,
}

impl ImportReport {
    fn new(
        counts: &DryRunReport,
        rebased_sources: u64,
        stored: &verify::Digests,
        (files, bytes): (u64, u64),
    ) -> Self {
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
            verified_tables: stored.len() as u64,
            verified_rows: stored.values().map(|(_, rows)| rows.len() as u64).sum(),
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
            "{} rows in {} tables arrived as the source stored them.",
            self.verified_rows, self.verified_tables
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
    let limits = archive::Limits::default();
    let temporary = std::env::temp_dir();
    import_with(source, destination, &temporary, limits, flush_destination)
}

/// The second guard: the folder that now exists, with its links resolved,
/// must still be empty and apart from the source. When it is not, a folder
/// this run `created` is removed again if it is still empty, so a refusal
/// leaves nothing behind.
pub(super) fn recheck(source: &Path, root: &Path, created: bool) -> Result<(), ImportError> {
    match place_destination(source, root) {
        Destination::Free => Ok(()),
        found => {
            if created {
                // `remove_dir` refuses a folder that holds anything.
                let _ = std::fs::remove_dir(root);
            }
            Err(ImportError::Destination(found))
        }
    }
}

/// Makes everything the import staged in `destination` durable. Linux
/// flushes the destination's filesystem in one call. Other platforms have no
/// such call, so every staged file and directory is synced.
fn flush_destination(destination: &Path) -> std::io::Result<()> {
    if pp_os::SYNCS_FILESYSTEM {
        return pp_os::sync_filesystem(destination);
    }
    sync_tree(&destination.join(STAGE))
}

/// Syncs every file under `root`, then each directory after its entries.
pub(super) fn sync_tree(root: &Path) -> std::io::Result<()> {
    let (mut pending, mut directories) = (vec![root.to_owned()], Vec::new());
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                pp_os::sync_file(&entry.path())?;
            }
        }
        directories.push(directory);
    }
    directories
        .iter()
        .rev()
        .try_for_each(|directory| pp_os::sync_directory(directory))
}

/// [`import`] with the caps an archive must stay under, and with the call
/// that flushes the destination's filesystem. A folder has no caps: the only
/// size refusal for one is free space at the destination.
///
/// Durability rests on this order, and nothing may reorder it: every file is
/// copied and read back; the database is checkpointed and closed; one flush
/// of the destination's filesystem; the rename into place and a sync of the
/// destination directory; the ownership marker last, synced with its
/// directory. A destination with a marker is complete and durable. One
/// without is discarded.
///
/// Nothing is created until the destination is known to be apart from the
/// source, and from then on every write goes through `root`, the created
/// folder with its links resolved, never through the path as given.
/// `temporary` is where the startup check copies the database.
pub(super) fn import_with(
    source: &Path,
    destination: &Path,
    temporary: &Path,
    limits: archive::Limits,
    flush: fn(&Path) -> std::io::Result<()>,
) -> Result<ImportReport, ImportError> {
    let metadata = source.metadata().map_err(|error| io_error(source, error))?;
    match place_destination(source, destination) {
        Destination::Free => {}
        found => return Err(ImportError::Destination(found)),
    }
    // Known before anything is created: the copy takes files and folders
    // only, and it must not leave anything out.
    if metadata.is_dir() {
        let (_, found) = scan_roots(source)?;
        if !found.is_empty() {
            return Err(ImportError::NotRegular(found));
        }
    }
    for folder in [source, destination] {
        ensure_outside(temporary, folder)?;
    }
    let created = destination.symlink_metadata().is_err();
    pp_os::create_private_dir_all(destination).map_err(|error| io_error(destination, error))?;
    let root = destination
        .canonicalize()
        .map_err(|error| io_error(destination, error))?;
    recheck(source, &root, created)?;
    let stage = Scratch(root.join(STAGE));
    pp_os::create_private_dir(&stage.0).map_err(|error| io_error(&stage.0, error))?;
    let (kind, files, copied) = if metadata.is_dir() {
        let files = stage_directory(source, &stage.0)?;
        let copied = copy_roots(source, &stage.0)?;
        (SourceKind::Directory, files, copied)
    } else {
        let (files, copied) = extract_archive(source, metadata.len(), &stage.0, limits)?;
        (SourceKind::Archive, files, copied)
    };
    let (connection, ready, stored) = upgrade(&stage.0, |database| {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
        let source = rusqlite::Connection::open_with_flags(database, flags)?;
        Ok(verify::digests(&source, None)?)
    })?;
    // Sign-in state is not carried: every Node session ends with the import.
    // It goes before the count, so the report describes what was imported.
    connection.execute_batch("DELETE FROM sessions; DELETE FROM password_reset_tokens;")?;
    let counts = measure(&connection, kind, &ready, &files)?;
    if !counts.unnamed.is_empty() {
        let shapes = counts.unnamed.iter().map(|unnamed| unnamed.shape.clone());
        return Err(ImportError::Unnamed(shapes.collect()));
    }
    if let Some(value) = (counts.installations.iter()).find(|value| *value != "default") {
        let stored = |table: &super::TableCount| table.by_tenant.get(value).copied();
        return Err(ImportError::Installation {
            value: value.clone(),
            rows: counts.tables.iter().filter_map(stored).sum(),
        });
    }
    if counts.users > 1 {
        return Err(ImportError::Accounts(counts.users));
    }
    let repos = root.join("repos");
    let rebased = verify::rebase_sources(&connection, &stage.0.join("repos"), &repos)?;
    let changed = verify::changed(&stored, &verify::digests(&connection, Some(&stored))?);
    if !changed.is_empty() {
        return Err(ImportError::RowsChanged(changed));
    }
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    connection.close().map_err(|(_, error)| error)?;
    // The closed database is one file. A write-ahead log left beside it
    // would hold rows the published copy lacks.
    for suffix in ["-wal", "-shm"] {
        let left = stage.0.join(format!("{}{suffix}", archive::DATABASE_FILE));
        if left.symlink_metadata().is_ok() {
            // A failure of this run's own write step, not a fault of the
            // source database.
            let detail = "is still there after the staged database was closed";
            return Err(io_error(&left, std::io::Error::other(detail)));
        }
    }
    // Everything staged reaches the disk before the folder can become an
    // installation. The stage is a directory the import made inside the
    // destination, so both are one filesystem and one flush covers them.
    flush(&root).map_err(|source| ImportError::Flush {
        path: destination.to_owned(),
        source,
    })?;
    let report = ImportReport::new(&counts, rebased, &stored, copied);
    publish(&stage.0, &root, &report)?;
    drop(stage);
    // The marker goes into the folder this run filled, and only while the
    // given path still leads there.
    if destination.canonicalize().ok().as_deref() != Some(root.as_path()) {
        return Err(ImportError::Destination(Destination::Replaced));
    }
    ownership_marker::mark_imported(&root).map_err(|error| io_error(destination, error))?;
    Ok(report)
}

/// The free bytes the destination needs once the database is staged: the
/// files still to be copied, working space for the schema upgrade, whose
/// write-ahead log can grow as large as the database, and the reserve. No
/// size cap applies: an import is bounded by free disk space alone.
pub(super) fn room_needed(files: u64, database: u64) -> u64 {
    files.saturating_add(database).saturating_add(STAGE_RESERVE)
}

/// The bytes of the database files staged in `stage`.
fn staged_database(stage: &Path) -> u64 {
    let staged = |name| stage.join(name).metadata().map_or(0, |file| file.len());
    DATABASE_FILES.map(staged).iter().sum()
}

/// Copies every root of a Node data folder into `stage` and returns the
/// number of files and their bytes. Like a backup, it takes regular files
/// and directories and follows no link. Unlike a backup, it has no cap on
/// the number of entries or on their size.
fn copy_roots(source: &Path, stage: &Path) -> Result<(u64, u64), ImportError> {
    let items = archive::plan(source, archive::Scope::Full, u64::MAX)
        .map_err(|error| ImportError::Roots(format!("{error:#}")))?;
    let sizes = items.iter().map(|item| match item {
        archive::Item::File(_, _, bytes) => *bytes,
        archive::Item::Directory(_) => 0,
    });
    let roots = sizes.fold(0, u64::saturating_add);
    ensure_room(stage, room_needed(roots, staged_database(stage)))?;
    let (mut files, mut bytes) = (0, 0);
    for item in &items {
        match item {
            archive::Item::Directory(name) => {
                let directory = stage.join(name);
                pp_os::create_private_dir_all(&directory)
                    .map_err(|error| io_error(&directory, error))?;
            }
            archive::Item::File(name, path, _) => {
                bytes += copy_verified(path, &stage.join(name))?;
                files += 1;
            }
        }
    }
    Ok((files, bytes))
}

/// Copies one file, keeps its modification time, and reads the copy back:
/// its SHA-256 must equal that of the bytes read from the source. The read
/// comes from the page cache, so it catches a wrong copy, not a bad disk.
/// The copy is not synced here: the import flushes everything once, later.
pub(super) fn copy_verified(from: &Path, to: &Path) -> Result<u64, ImportError> {
    let missing = || io_error(from, std::io::ErrorKind::NotFound.into());
    let mut source = open_regular(from)?.ok_or_else(missing)?;
    let written = |error| io_error(to, error);
    let mut copy = File::create_new(to).map_err(written)?;
    let (mut read, mut bytes, mut buffer) = (Sha256::new(), 0, vec![0; 64 * 1024]);
    loop {
        let filled = source
            .read(&mut buffer)
            .map_err(|error| io_error(from, error))?;
        if filled == 0 {
            break;
        }
        read.update(&buffer[..filled]);
        copy.write_all(&buffer[..filled]).map_err(written)?;
        bytes += filled as u64;
    }
    if let Ok(modified) = source.metadata().and_then(|metadata| metadata.modified()) {
        copy.set_modified(modified).map_err(written)?;
    }
    drop(copy);
    #[cfg(test)]
    damage::apply(to);
    confirm_copy(to, read.finalize().as_slice())?;
    Ok(bytes)
}

/// Reads the copy at `to` back from disk: its SHA-256 must be `expected`,
/// the digest of the bytes read from the source.
pub(super) fn confirm_copy(to: &Path, expected: &[u8]) -> Result<(), ImportError> {
    let mut written = Sha256::new();
    let mut copy = File::open(to).map_err(|error| io_error(to, error))?;
    std::io::copy(&mut copy, &mut written).map_err(|error| io_error(to, error))?;
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
    limits: archive::Limits,
) -> Result<(Files, (u64, u64)), ImportError> {
    // The extracted size is unknown until the archive is read; it is at
    // least the archive's own size.
    ensure_room(stage, compressed.saturating_add(STAGE_RESERVE))?;
    let extracted = archive::extract(source, limits, stage);
    let metadata = extracted.map_err(|error| match error.downcast_ref::<std::io::Error>() {
        Some(io) if io.kind() == std::io::ErrorKind::StorageFull => {
            io_error(stage, io.kind().into())
        }
        _ => ImportError::InvalidArchive(format!("{error:#}")),
    })?;
    if metadata.scope != archive::MetadataScope::Full {
        return Err(ImportError::PartialArchive(metadata.scope));
    }
    // Everything is extracted; the upgrade still needs its working space.
    ensure_room(stage, room_needed(0, staged_database(stage)))?;
    // The archive passed its caps; listing what it wrote needs none.
    let extracted = archive::plan(stage, archive::Scope::Full, u64::MAX)
        .map_err(|error| ImportError::Roots(format!("{error:#}")))?;
    let sizes = extracted.iter().filter_map(|item| match item {
        archive::Item::File(_, _, bytes) => Some(*bytes),
        archive::Item::Directory(_) => None,
    });
    let (files, bytes) = sizes.fold((0, 0), |(files, total), bytes| (files + 1, total + bytes));
    let files_read = Files {
        documents: stage.to_owned(),
        repo_files: repo_files(stage)?,
        unsupported: Vec::new(),
        roots: Vec::new(),
        database: Vec::new(),
    };
    Ok((files_read, (files, bytes)))
}

/// Refuses when the destination's disk has fewer than `needed` bytes free.
pub(super) fn ensure_room(stage: &Path, needed: u64) -> Result<(), ImportError> {
    let available = fs4::available_space(stage).map_err(|error| io_error(stage, error))?;
    if available >= needed {
        return Ok(());
    }
    Err(ImportError::DestinationSpace { needed, available })
}

/// Moves the staged database and roots into `destination`, saves the report
/// beside them and syncs the directory. The staged files are durable before
/// this runs. It never replaces anything: the folder was empty when the
/// import began, and if it has since gained any name this would write, or a
/// marker, nothing is moved.
pub(super) fn publish(
    stage: &Path,
    destination: &Path,
    report: &ImportReport,
) -> Result<(), ImportError> {
    let roots = archive::FULL_ROOTS.iter().map(|(root, _)| *root);
    let moved = [archive::DATABASE_FILE].into_iter().chain(roots);
    let taken = |name: &str| ImportError::Destination(Destination::NotEmpty(name.to_owned()));
    for name in (moved.clone()).chain([REPORT_FILE, ownership_marker::MARKER_FILE]) {
        if destination.join(name).symlink_metadata().is_ok() {
            return Err(taken(name));
        }
    }
    for name in moved {
        let (from, to) = (stage.join(name), destination.join(name));
        if from.symlink_metadata().is_ok() {
            pp_os::rename_new(&from, &to).map_err(|error| match error.kind() {
                std::io::ErrorKind::AlreadyExists => taken(name),
                _ => io_error(&to, error),
            })?;
        }
    }
    let path = destination.join(REPORT_FILE);
    let saved = |error| io_error(&path, error);
    let text = serde_json::to_vec_pretty(report).map_err(|error| saved(error.into()))?;
    let mut file = File::create_new(&path).map_err(saved)?;
    file.write_all(&text).map_err(saved)?;
    file.sync_all().map_err(saved)?;
    pp_os::sync_directory(destination).map_err(|error| io_error(destination, error))
}

/// Lets a test change one byte of a named copy before it is read back.
#[cfg(test)]
pub(super) mod damage {
    use std::{cell::Cell, path::Path};

    thread_local! {
        pub(in crate::import) static FILE: Cell<Option<&'static str>> = const { Cell::new(None) };
    }

    pub(super) fn apply(copy: &Path) {
        let named = FILE.get().is_some_and(|name| copy.ends_with(name));
        if named && let Ok(mut bytes) = std::fs::read(copy) {
            bytes[0] ^= 1;
            let _ = std::fs::write(copy, bytes);
        }
    }
}
