//! One-time import of a Node installation into a Rust-owned data folder.
//!
//! Rust owns its data formats, and this module is the only Rust code that
//! reads Node's stored shapes on purpose. Today it holds the conversion
//! [`inventory`] and the dry run. [`dry_run`] reads a stopped Node data
//! folder or a full app backup archive, upgrades a private temporary copy of
//! the database, and counts what an import would convert. It opens nothing
//! in the source for writing, and SQLite never opens the source database, so
//! no `-shm` or `-wal` file appears there.

mod copy;
pub mod inventory;
mod verify;

pub use copy::{ImportReport, REPORT_FILE, import};

use crate::{archive, ownership_marker, schema, versions};
use inventory::{Row, Shape};
use rusqlite::Connection;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

/// The database files copied from a data folder. SQLite rebuilds `-shm`.
const DATABASE_FILES: [&str; 3] = [
    archive::DATABASE_FILE,
    archive::WAL_FILE,
    "print-partner.db-journal",
];
const LEASE: &str = ".desktop-lease";
const UNMATCHED_PRINTS: &str = "printer.unattributed_prints";

/// Why an import did not run. [`Self::Io`], [`Self::Sqlite`] and
/// [`Self::CopyMismatch`] are failures; every other variant refuses the
/// source or the destination as it is. No variant leaves a write in the source.
#[derive(Debug)]
pub enum ImportError {
    /// The folder has a `.pp-rust-owned.json` entry.
    RustOwned,
    /// The folder holds `.desktop-lease`: something runs or crashed there.
    Leased,
    /// The folder has no `print-partner.db`.
    NoDatabase,
    /// The database files changed while they were read.
    Changed,
    InvalidArchive(String),
    /// The archive is not a format 2 `full` backup.
    PartialArchive(archive::MetadataScope),
    /// The stored schema is outside 31 to 34, the range the import upgrades.
    Schema(versions::IncompatibleSchema),
    /// The database failed its integrity or schema checks.
    Database(String),
    /// Builds or Sources belong to more than one tenant.
    SplitTenants(Vec<String>),
    /// The destination holds data or is a Rust installation already.
    Destination(Destination),
    /// Stored JSON that the inventory does not name, as the dry run lists it.
    Unnamed(Vec<String>),
    /// The Builds and Sources are under an installation value other than
    /// `default`, which is the only one the Rust runtime reads.
    Installation {
        value: String,
        builds: u64,
        sources: u64,
    },
    /// More than one account is stored; the Rust runtime signs in with one.
    Accounts(u64),
    /// The Source with this id has a current revision with no files under `repos/`.
    MissingRevision(i64),
    /// The Source with this id stores a path the import cannot place under `repos/`.
    SourcePath(i64),
    /// Rows of these tables differ from the source after the upgrade.
    RowsChanged(Vec<String>),
    /// A copied file read back differently from its source.
    CopyMismatch(PathBuf),
    /// The staging directory has too little free space.
    NoSpace {
        directory: PathBuf,
        needed: u64,
        free: u64,
    },
    /// The destination's disk cannot hold the files the import writes.
    DestinationSpace {
        needed: u64,
        available: u64,
    },
    /// A backup root is a link, has the wrong type or holds too many entries.
    Roots(String),
    /// A read or write of `path` failed.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Sqlite(rusqlite::Error),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RustOwned => write!(
                f,
                "The folder has a {} entry: it is a Rust installation, not a Node one",
                ownership_marker::MARKER_FILE
            ),
            Self::Leased => write!(
                f,
                "The folder holds {LEASE}: Print Partner runs or crashed there. Stop it cleanly, then retry"
            ),
            Self::NoDatabase => write!(f, "The folder has no {}", archive::DATABASE_FILE),
            Self::Changed => write!(
                f,
                "{} changed while it was read. Stop the Node server, then retry",
                archive::DATABASE_FILE
            ),
            Self::InvalidArchive(detail) => write!(f, "Invalid backup archive: {detail}"),
            Self::PartialArchive(scope) => write!(
                f,
                "The import needs a format 2 full backup; this archive is {}",
                match scope {
                    archive::MetadataScope::Legacy => "format 1",
                    _ => "database-only",
                }
            ),
            Self::Schema(refusal) if refusal.too_new() => write!(
                f,
                "Database schema {} is newer than Node's schema {}: this is not a Node installation",
                refusal.found, refusal.newest
            ),
            Self::Schema(refusal) => write!(
                f,
                "Database schema {} is older than schema {}. Start Print Partner v3.3.0 on the Node installation once so it upgrades, then retry",
                refusal.found, refusal.oldest
            ),
            Self::Database(detail) => write!(f, "The database failed its checks: {detail}"),
            Self::SplitTenants(tenants) => write!(
                f,
                "Builds or Sources belong to more than one tenant ({}); the import does not merge tenants",
                tenants.join(", ")
            ),
            Self::Destination(found) => {
                write!(
                    f,
                    "The destination {found}. Empty it or choose another folder"
                )
            }
            Self::Unnamed(shapes) => write!(
                f,
                "The inventory does not name this stored JSON: {}. Report it; the import runs once the inventory names it",
                shapes.join(", ")
            ),
            Self::Installation {
                value,
                builds,
                sources,
            } => write!(
                f,
                "{builds} Builds and {sources} Sources are stored under the installation value {value:?}. The import handles only data stored under \"default\" today. Send the dry-run report back so this case can be added"
            ),
            Self::Accounts(accounts) => write!(
                f,
                "{accounts} accounts are stored, and the Rust runtime signs in with one. Remove the extra accounts in Node first, or send the dry-run report back so a choice can be added"
            ),
            Self::MissingRevision(source) => write!(
                f,
                "Source {source} has a current revision whose files are not under repos/"
            ),
            Self::SourcePath(source) => write!(
                f,
                "Source {source} stores a path that does not lead below a repos folder"
            ),
            Self::RowsChanged(tables) => write!(
                f,
                "Rows changed during the import in {}; they must arrive as the source stored them",
                tables.join(", ")
            ),
            Self::CopyMismatch(copy) => write!(
                f,
                "The copy {} does not match its source. Check the disk, empty the destination and retry",
                copy.display()
            ),
            Self::NoSpace {
                directory,
                needed,
                free,
            } => write!(
                f,
                "Staging needs {needed} bytes free in {} and {free} are free. Free space there, or for a dry run set TMPDIR to a directory with more room",
                directory.display()
            ),
            Self::DestinationSpace { needed, available } => write!(
                f,
                "The destination needs {needed} bytes free for the files the import writes and {available} are available"
            ),
            Self::Roots(detail) => write!(f, "The folder's files cannot be copied: {detail}"),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Sqlite(error) => write!(f, "SQLite: {error}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<rusqlite::Error> for ImportError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Directory,
    Archive,
}

/// What the import would find at its destination, read without writing.
#[derive(Debug, PartialEq, Eq)]
pub enum Destination {
    /// Absent, or empty by the marker rule and without a lease entry. Its
    /// owner and permissions are not checked here.
    Free,
    /// It holds this entry and no marker: the import would refuse it.
    NotEmpty(String),
    /// It has a marker entry: a Rust installation is there already.
    Marked,
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Free => f.write_str("is absent or empty, as the import needs"),
            Self::NotEmpty(entry) => write!(f, "holds {entry:?}, so the import refuses it"),
            Self::Marked => f.write_str("is a Rust installation, so the import refuses it"),
        }
    }
}

/// Inspects the folder an import would write to. Nothing is created.
pub fn inspect_destination(destination: &Path) -> Result<Destination, ImportError> {
    use ownership_marker::{MarkerRefusal, MarkerState};
    match ownership_marker::inspect(destination) {
        MarkerState::Absent => {}
        MarkerState::Refused(MarkerRefusal::Unreadable(kind)) => {
            return Err(io_error(destination, kind.into()));
        }
        _ => return Ok(Destination::Marked),
    }
    // The writer refuses a Node lease directory that the marker rule alone
    // counts as empty, so any lease entry makes the folder unusable.
    if destination.join(LEASE).symlink_metadata().is_ok() {
        return Ok(Destination::NotEmpty(LEASE.into()));
    }
    match ownership_marker::first_non_empty_entry(destination) {
        Ok(None) => Ok(Destination::Free),
        Ok(Some(entry)) => Ok(Destination::NotEmpty(entry)),
        Err(error) => Err(io_error(destination, error)),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct TableCount {
    pub table: String,
    pub rows: u64,
    /// Rows per `tenant_id`. Empty for a table without that column.
    pub by_tenant: BTreeMap<String, u64>,
}

/// A stored JSON document the inventory does not name.
#[derive(Debug, PartialEq, Eq)]
pub struct Unnamed {
    /// `table.column`, or `setting <key>` with each run of digits as `#`.
    pub shape: String,
    pub items: u64,
}

/// What a dry run found. It holds keys, names and counts, never a stored value.
#[derive(Debug, PartialEq, Eq)]
pub struct DryRunReport {
    pub source: SourceKind,
    /// The schema the source stores.
    pub schema_version: u64,
    /// The schema the temporary copy was upgraded to before counting.
    pub upgraded_schema: u64,
    pub builds: u64,
    pub sources: u64,
    pub printers: u64,
    /// Unmatched prints that are neither claimed nor dismissed.
    pub unmatched_prints: u64,
    /// The `tenant_id` value that holds the Builds and Sources, when there
    /// are any. The Rust-only runtime reads only `default`.
    pub installation: Option<String>,
    /// Rows in `users`. The Rust-only runtime signs in only with one or none.
    pub users: u64,
    /// The destination, when the dry run was given one.
    pub destination: Option<Destination>,
    pub tables: Vec<TableCount>,
    /// Every inventory row with the item count of each of its shapes.
    pub rows: Vec<(&'static Row, Vec<(Shape, u64)>)>,
    /// The named shapes the import copies byte for byte.
    pub copied: Vec<(Shape, u64)>,
    /// The named shapes no Rust code reads. The import keeps them too.
    pub unused: Vec<(Shape, u64)>,
    /// Keys of settings that hold no JSON document. They are copied as stored.
    pub scalar_settings: BTreeSet<String>,
    pub unnamed: Vec<Unnamed>,
}

impl fmt::Display for DryRunReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = match self.source {
            SourceKind::Directory => "Node data folder",
            SourceKind::Archive => "Node backup archive",
        };
        writeln!(f, "Dry run of a {source}. Nothing was written.")?;
        writeln!(
            f,
            "Schema {}, upgraded to {} on a temporary copy.",
            self.schema_version, self.upgraded_schema
        )?;
        writeln!(
            f,
            "{} Builds, {} Sources, {} printers, {} unmatched prints.",
            self.builds, self.sources, self.printers, self.unmatched_prints
        )?;
        let value = self.installation.as_deref().unwrap_or("default");
        writeln!(f, "Installation value {value:?}, {} accounts.", self.users)?;
        let refused = "so the import would refuse this installation";
        if value != "default" {
            writeln!(
                f,
                "The Rust runtime reads only data under \"default\", {refused}."
            )?;
        }
        if self.users > 1 {
            writeln!(
                f,
                "The Rust runtime signs in only when one account is stored, {refused}."
            )?;
        }
        if let Some(destination) = &self.destination {
            writeln!(f, "The destination {destination}.")?;
        }
        writeln!(f, "\nTables with rows (rows, then rows per tenant):")?;
        for table in self.tables.iter().filter(|table| table.rows > 0) {
            write!(f, "  {} {}", table.table, table.rows)?;
            for (tenant, rows) in &table.by_tenant {
                write!(f, " {tenant}={rows}")?;
            }
            writeln!(f)?;
        }
        writeln!(f, "\nConversion inventory (step, status, items per shape):")?;
        for (row, shapes) in &self.rows {
            write!(f, "  step {} {}:", row.step, row.status)?;
            for (index, (shape, items)) in shapes.iter().enumerate() {
                let separator = if index == 0 { "" } else { "," };
                write!(f, "{separator} {shape} = {items}")?;
            }
            writeln!(f)?;
        }
        writeln!(f, "\nCopied unchanged:")?;
        for (shape, items) in self.copied.iter().filter(|(_, items)| *items > 0) {
            writeln!(f, "  {shape} = {items}")?;
        }
        let scalars: Vec<&str> = self.scalar_settings.iter().map(String::as_str).collect();
        let keys = if scalars.is_empty() {
            "none".into()
        } else {
            scalars.join(", ")
        };
        writeln!(f, "  scalar settings ({}): {keys}", scalars.len())?;
        writeln!(f, "\nKept, not used by Rust:")?;
        let mut unused = self
            .unused
            .iter()
            .filter(|(_, items)| *items > 0)
            .peekable();
        if unused.peek().is_none() {
            writeln!(f, "  none")?;
        }
        for (shape, items) in unused {
            writeln!(f, "  {shape} = {items}")?;
        }
        writeln!(f, "\nStored JSON the inventory does not name:")?;
        for unnamed in &self.unnamed {
            writeln!(f, "  {} = {}", unnamed.shape, unnamed.items)?;
        }
        if self.unnamed.is_empty() {
            writeln!(f, "  none")?;
        }
        Ok(())
    }
}

/// The private temporary directory of one dry run, removed on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The files a dry run reads beside the staged database.
struct Files {
    /// The directory that holds the single-file roots.
    documents: PathBuf,
    /// The name of every file under `repos/`.
    repo_files: Vec<String>,
}

/// Reports what importing `source` would convert. `source` is a stopped Node
/// data folder or a format 2 `full` backup archive. The database is copied
/// into a private directory under the system temporary directory, upgraded
/// there, and removed before this returns.
///
/// With a `destination`, the report also says whether the import would
/// accept that folder. The folder is read, never created.
pub fn dry_run(source: &Path, destination: Option<&Path>) -> Result<DryRunReport, ImportError> {
    let destination = destination.map(inspect_destination).transpose()?;
    let report = dry_run_in(source, &std::env::temp_dir())?;
    Ok(DryRunReport {
        destination,
        ..report
    })
}

/// Free space kept beyond the copies, for the tables the upgrade adds.
const STAGE_RESERVE: u64 = 64 * 1024 * 1024;

/// The free bytes staging needs for a database of `database` bytes, of which
/// `staged` copies are still to be written. Beside the staged copy, the
/// startup check makes one short-lived copy; it is gone before the upgrade
/// writes its write-ahead log, which can grow as large again.
fn stage_bytes(database: u64, staged: u64) -> u64 {
    (database.saturating_mul(staged.saturating_add(1))).saturating_add(STAGE_RESERVE)
}

/// Refuses when the directory that holds `stage` has fewer than `needed`
/// bytes free.
fn ensure_space(stage: &Path, needed: u64) -> Result<(), ImportError> {
    let directory = stage.parent().unwrap_or(stage);
    let free = fs4::available_space(directory).map_err(|error| io_error(directory, error))?;
    if free >= needed {
        return Ok(());
    }
    Err(ImportError::NoSpace {
        directory: directory.to_owned(),
        needed,
        free,
    })
}

fn dry_run_in(source: &Path, temporary: &Path) -> Result<DryRunReport, ImportError> {
    let metadata = source.metadata().map_err(|error| io_error(source, error))?;
    let scratch = Scratch(temporary.join(format!(
        "pp-import-dry-run-{}",
        hex::encode(rand::random::<[u8; 12]>())
    )));
    pp_os::create_private_dir(&scratch.0).map_err(|error| io_error(&scratch.0, error))?;
    let (kind, files) = if metadata.is_dir() {
        (SourceKind::Directory, stage_directory(source, &scratch.0)?)
    } else if metadata.is_file() {
        (SourceKind::Archive, stage_archive(source, &scratch.0)?)
    } else {
        let kind = std::io::Error::new(ErrorKind::InvalidInput, "neither a folder nor a file");
        return Err(io_error(source, kind));
    };
    let (connection, ready, ()) = upgrade(&scratch.0, |_| Ok(()))?;
    measure(&connection, kind, &ready, &files)
}

/// Checks the staged database as startup checks a live one and upgrades it
/// in place to the current Rust schema, without rollback copies. `before`
/// reads the checked database while it is still as the source stored it.
fn upgrade<T>(
    stage: &Path,
    before: impl FnOnce(&Path) -> Result<T, ImportError>,
) -> Result<(Connection, schema::SchemaReady, T), ImportError> {
    // A failed check can hide the version: a Rust database stamped 35 or
    // later fails the job-table check first. Refuse by version when one was read.
    let admitted = crate::restore::admit_schema(stage);
    let found = (admitted.as_ref().ok().copied()).or_else(versions::preflight_version);
    if let Some(found) = found
        && versions::classify(found, versions::NODE_SCHEMA).is_err()
    {
        return Err(ImportError::Schema(versions::IncompatibleSchema {
            found,
            oldest: versions::OLDEST_UPGRADABLE,
            newest: versions::NODE_SCHEMA,
        }));
    }
    let version = admitted.map_err(|error| ImportError::Database(format!("{error:#}")))?;
    let database = stage.join(archive::DATABASE_FILE);
    let read = before(&database)?;
    let now = crate::auth::catalog_timestamp();
    let (connection, ready) = schema::initialize_with(&database, version, &now, false)
        .map_err(|error| ImportError::Database(format!("{error:#}")))?;
    Ok((connection, ready, read))
}

/// An I/O failure that names the entry it happened on.
fn io_error(path: &Path, source: std::io::Error) -> ImportError {
    ImportError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Opens the regular file at `path` for reading; `None` when nothing is
/// there. A link, a directory or a device is an error, checked before the
/// open so a FIFO cannot block it.
fn open_regular(path: &Path) -> Result<Option<std::fs::File>, ImportError> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() => pp_os::open_read_nofollow(path)
            .map(Some)
            .map_err(|error| io_error(path, error)),
        Ok(_) => Err(io_error(
            path,
            std::io::Error::new(
                ErrorKind::InvalidInput,
                "not a regular file; the import follows no link",
            ),
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(path, error)),
    }
}

/// Copies the database files of a Node data folder into `scratch`.
fn stage_directory(source: &Path, scratch: &Path) -> Result<Files, ImportError> {
    use ownership_marker::{MarkerRefusal, MarkerState};
    match ownership_marker::inspect(source) {
        MarkerState::Absent => {}
        MarkerState::Refused(MarkerRefusal::Unreadable(kind)) => {
            return Err(io_error(source, kind.into()));
        }
        _ => return Err(ImportError::RustOwned),
    }
    if source.join(LEASE).symlink_metadata().is_ok() {
        return Err(ImportError::Leased);
    }
    let stamps = || {
        DATABASE_FILES.map(|name| {
            let metadata = source.join(name).symlink_metadata().ok()?;
            Some((metadata.len(), metadata.modified().ok()))
        })
    };
    let before = stamps();
    if before[0].is_none() {
        return Err(ImportError::NoDatabase);
    }
    let bytes: u64 = before.iter().flatten().map(|(bytes, _)| bytes).sum();
    ensure_space(scratch, stage_bytes(bytes, 1))?;
    for name in DATABASE_FILES {
        let path = source.join(name);
        if let Some(mut file) = open_regular(&path)? {
            let staged = scratch.join(name);
            let mut copy =
                std::fs::File::create_new(&staged).map_err(|error| io_error(&staged, error))?;
            std::io::copy(&mut file, &mut copy).map_err(|error| io_error(&path, error))?;
        }
    }
    if stamps() != before {
        return Err(ImportError::Changed);
    }
    Ok(Files {
        documents: source.to_owned(),
        repo_files: repo_files(source)?,
    })
}

/// The name of every file under `root/repos`.
fn repo_files(root: &Path) -> Result<Vec<String>, ImportError> {
    let mut repo_files = Vec::new();
    let repos = root.join("repos");
    // Neither check follows a link, so the walk stays in the folder.
    let mut pending = Vec::from_iter(
        repos
            .symlink_metadata()
            .is_ok_and(|m| m.is_dir())
            .then_some(repos),
    );
    while let Some(directory) = pending.pop() {
        let failed = |error| io_error(&directory, error);
        for entry in std::fs::read_dir(&directory).map_err(failed)? {
            let entry = entry.map_err(failed)?;
            if entry.file_type().map_err(failed)?.is_dir() {
                pending.push(entry.path());
            } else {
                repo_files.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    Ok(repo_files)
}

/// Validates a backup archive and extracts its database and single-file
/// roots into `scratch`.
fn stage_archive(source: &Path, scratch: &Path) -> Result<Files, ImportError> {
    let (metadata, paths) = archive::extract_documents(source, archive::Limits::default(), scratch)
        .map_err(|error| match error.downcast_ref::<std::io::Error>() {
            // The temporary directory filled up; the archive is not at fault.
            Some(io) if io.kind() == ErrorKind::StorageFull => io_error(scratch, io.kind().into()),
            _ => ImportError::InvalidArchive(format!("{error:#}")),
        })?;
    if metadata.scope != archive::MetadataScope::Full {
        return Err(ImportError::PartialArchive(metadata.scope));
    }
    // The archive's size says nothing here: it also holds `repos/`, which a
    // dry run only lists. The database size is known once it is extracted.
    let extracted = |name| scratch.join(name).metadata().map_or(0, |file| file.len());
    let bytes: u64 = DATABASE_FILES.map(extracted).iter().sum();
    ensure_space(scratch, stage_bytes(bytes, 0))?;
    Ok(Files {
        documents: scratch.to_owned(),
        repo_files: paths
            .iter()
            .filter(|path| path.starts_with("repos/"))
            .filter_map(|path| path.rsplit('/').next())
            .map(str::to_owned)
            .collect(),
    })
}

/// The elements of a stored JSON array, or `other` for any other value.
fn items(stored: &[u8], other: u64) -> u64 {
    match serde_json::from_slice(stored) {
        Ok(Value::Array(elements)) => elements.len() as u64,
        _ => other,
    }
}

/// `key` with each run of digits replaced by `#`, so per-Build keys group.
fn key_pattern(key: &str) -> String {
    let mut pattern = String::new();
    for character in key.chars() {
        if !character.is_ascii_digit() {
            pattern.push(character);
        } else if !pattern.ends_with('#') {
            pattern.push('#');
        }
    }
    pattern
}

fn quoted(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn strings(
    connection: &Connection,
    sql: &str,
    table: Option<&str>,
) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(table), |row| row.get(0))?;
    rows.collect()
}

/// Counts every table's rows, and lists each column that holds JSON objects
/// or arrays and that no shape in `named` covers.
fn count_tables(
    connection: &Connection,
    named: &[&Shape],
) -> rusqlite::Result<(Vec<TableCount>, Vec<Unnamed>)> {
    let (mut tables, mut unnamed) = (Vec::new(), Vec::new());
    let names = "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name";
    for table in strings(connection, names, None)? {
        let mut columns = strings(
            connection,
            "SELECT name FROM pragma_table_info(?1)",
            Some(&table),
        )?;
        let tenanted = columns.iter().any(|column| column == "tenant_id");
        let tenant = if tenanted { "tenant_id" } else { "NULL" };
        // Settings are classified by key, not as one JSON column.
        if table == "app_settings" {
            columns.clear();
        }
        // CASE evaluates lazily, so `json_type` only sees valid JSON text.
        let documents: String = (columns.iter().map(|column| quoted(column)))
            .map(|column| {
                format!(
                    ", SUM(CASE WHEN typeof({column}) = 'text' THEN CASE WHEN json_valid({column}) \
                     THEN json_type({column}) IN ('object', 'array') ELSE 0 END ELSE 0 END)"
                )
            })
            .collect();
        let sql = format!(
            "SELECT {tenant}, COUNT(*){documents} FROM {} GROUP BY 1",
            quoted(&table)
        );
        let (mut rows, mut by_tenant) = (0, BTreeMap::new());
        let mut found = vec![0u64; columns.len()];
        let mut statement = connection.prepare(&sql)?;
        let mut groups = statement.query([])?;
        while let Some(group) = groups.next()? {
            let count = group.get::<_, i64>(1)?.unsigned_abs();
            rows += count;
            if let Some(tenant) = group.get::<_, Option<String>>(0)? {
                by_tenant.insert(tenant, count);
            }
            for (index, total) in found.iter_mut().enumerate() {
                *total += group.get::<_, i64>(index + 2)?.unsigned_abs();
            }
        }
        for (column, items) in columns.iter().zip(found) {
            if items > 0 && !named.iter().any(|shape| shape.names_column(&table, column)) {
                let shape = format!("{table}.{column}");
                unnamed.push(Unnamed { shape, items });
            }
        }
        tables.push(TableCount {
            table,
            rows,
            by_tenant,
        });
    }
    Ok((tables, unnamed))
}

/// The item count of one inventory shape.
fn count_shape(
    connection: &Connection,
    settings: &[(String, String)],
    files: &Files,
    shape: &Shape,
) -> Result<(Shape, u64), ImportError> {
    let rows = |sql: String| connection.query_row(&sql, [], |row| row.get::<_, i64>(0));
    let items = match shape {
        Shape::Setting(name) => (settings.iter())
            .filter(|(key, _)| key == name)
            .map(|(_, value)| items(value.as_bytes(), 1))
            .sum(),
        Shape::SettingPrefix(prefix) => settings
            .iter()
            .filter(|(key, _)| key.starts_with(prefix))
            .count() as u64,
        Shape::Column(table, column) => rows(format!(
            "SELECT COUNT(*) FROM {} WHERE {1} IS NOT NULL AND {1} <> ''",
            quoted(table),
            quoted(column)
        ))?
        .unsigned_abs(),
        Shape::Table(table) => {
            rows(format!("SELECT COUNT(*) FROM {}", quoted(table)))?.unsigned_abs()
        }
        Shape::File(name) => {
            let path = files.documents.join(name);
            let mut stored = Vec::new();
            if let Some(mut file) = open_regular(&path)? {
                (file.read_to_end(&mut stored)).map_err(|error| io_error(&path, error))?;
            }
            // A missing file reads as empty, which is no JSON and counts as none.
            items(&stored, u64::from(!stored.is_empty()))
        }
        Shape::SourceFile(name) => {
            files.repo_files.iter().filter(|file| file == name).count() as u64
        }
    };
    Ok((*shape, items))
}

fn measure(
    connection: &Connection,
    source: SourceKind,
    ready: &schema::SchemaReady,
    files: &Files,
) -> Result<DryRunReport, ImportError> {
    let named: Vec<&Shape> = inventory::named().collect();
    let (tables, mut unnamed) = count_tables(connection, &named)?;
    let table = |name: &str| tables.iter().find(|table| table.table == name);
    let tenants: BTreeSet<&String> = (["build_profiles", "projects"].into_iter())
        .filter_map(table)
        .flat_map(|table| table.by_tenant.keys())
        .collect();
    if tenants.len() > 1 {
        return Err(ImportError::SplitTenants(
            tenants.into_iter().cloned().collect(),
        ));
    }
    let installation = tenants.into_iter().next().cloned();

    let settings: Vec<(String, String)> = {
        let mut statement = connection.prepare("SELECT key, value FROM app_settings")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut scalar_settings = BTreeSet::new();
    let mut unnamed_settings = BTreeMap::<String, u64>::new();
    for (key, value) in &settings {
        if key == "schema_version" || named.iter().any(|shape| shape.names_setting(key)) {
            continue;
        }
        match serde_json::from_str(value) {
            Ok(Value::Array(_) | Value::Object(_)) => {
                *unnamed_settings.entry(key_pattern(key)).or_default() += 1;
            }
            _ => drop(scalar_settings.insert(key_pattern(key))),
        }
    }
    unnamed.extend(unnamed_settings.into_iter().map(|(key, items)| Unnamed {
        shape: format!("setting {key}"),
        items,
    }));

    let count = |shape: &Shape| count_shape(connection, &settings, files, shape);
    let rows = (inventory::ROWS.iter())
        .map(|row| Ok((row, row.shapes.iter().map(count).collect::<Result<_, _>>()?)))
        .collect::<Result<_, ImportError>>()?;
    // A print is unmatched until Node stores a claim time or a dismissal on it.
    let open = |print: &&Value| {
        print
            .get("claimed_at")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            && print.get("dismissed").and_then(Value::as_bool) != Some(true)
    };
    let unmatched_prints = (settings.iter())
        .filter(|(key, _)| key == UNMATCHED_PRINTS)
        .filter_map(|(_, value)| serde_json::from_str::<Vec<Value>>(value).ok())
        .map(|prints| prints.iter().filter(open).count() as u64)
        .sum();
    Ok(DryRunReport {
        source,
        schema_version: ready.previous_version,
        upgraded_schema: ready.version,
        builds: table("build_profiles").map_or(0, |table| table.rows),
        sources: table("projects").map_or(0, |table| table.rows),
        // Node reads a fleet that is not an array as no printers.
        printers: (settings.iter())
            .filter(|(key, _)| key == "printer.fleet")
            .map(|(_, value)| items(value.as_bytes(), 0))
            .sum(),
        unmatched_prints,
        installation,
        users: table("users").map_or(0, |table| table.rows),
        destination: None,
        rows,
        copied: (inventory::COPIED.iter().map(count)).collect::<Result<_, _>>()?,
        unused: (inventory::UNUSED.iter().map(count)).collect::<Result<_, _>>()?,
        scalar_settings,
        unnamed,
        tables,
    })
}

#[cfg(test)]
mod tests;
