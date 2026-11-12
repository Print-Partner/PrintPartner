//! One-time import of a Node installation into a Rust-owned data folder.
//!
//! Rust owns its data formats, and this module is the only Rust code that
//! reads Node's stored shapes on purpose. Today it holds the conversion
//! [`inventory`] and the dry run. [`dry_run`] reads a stopped Node data
//! folder or a full app backup archive, upgrades a private temporary copy of
//! the database, and counts what an import would convert. It opens nothing
//! in the source for writing, and SQLite never opens the source database, so
//! no `-shm` or `-wal` file appears there.

pub mod inventory;

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

/// Why an import did not run. Every variant but [`Self::Failed`] refuses the
/// source as it is. No variant leaves a write in the source.
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
    /// An I/O or SQLite failure.
    Failed(anyhow::Error),
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
            Self::Failed(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<anyhow::Error> for ImportError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

impl From<std::io::Error> for ImportError {
    fn from(error: std::io::Error) -> Self {
        Self::Failed(error.into())
    }
}

impl From<rusqlite::Error> for ImportError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Failed(error.into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Directory,
    Archive,
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
    pub tables: Vec<TableCount>,
    /// Every inventory row with the item count of each of its shapes.
    pub rows: Vec<(&'static Row, Vec<(Shape, u64)>)>,
    /// The named shapes the import copies byte for byte.
    pub copied: Vec<(Shape, u64)>,
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
        writeln!(
            f,
            "  scalar settings ({}): {}",
            scalars.len(),
            scalars.join(", ")
        )?;
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
pub fn dry_run(source: &Path) -> Result<DryRunReport, ImportError> {
    dry_run_in(source, &std::env::temp_dir())
}

fn dry_run_in(source: &Path, temporary: &Path) -> Result<DryRunReport, ImportError> {
    let metadata = source
        .metadata()
        .map_err(|error| anyhow::anyhow!("Read import source {}: {error}", source.display()))?;
    let scratch = Scratch(temporary.join(format!(
        "pp-import-dry-run-{}",
        hex::encode(rand::random::<[u8; 12]>())
    )));
    pp_os::create_private_dir(&scratch.0)?;
    let (kind, files) = if metadata.is_dir() {
        (SourceKind::Directory, stage_directory(source, &scratch.0)?)
    } else if metadata.is_file() {
        (SourceKind::Archive, stage_archive(source, &scratch.0)?)
    } else {
        return Err(anyhow::anyhow!("The import source is neither a folder nor a file").into());
    };
    // A failed check can hide the version: a Rust database stamped 35 or
    // later fails the job-table check first. Refuse by version when one was read.
    let admitted = crate::restore::admit_schema(&scratch.0);
    let found = admitted
        .as_ref()
        .ok()
        .copied()
        .or_else(versions::preflight_version);
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
    let (connection, ready) = schema::initialize(
        &scratch.0.join(archive::DATABASE_FILE),
        version,
        &crate::auth::catalog_timestamp(),
    )?;
    measure(&connection, kind, &ready, &files)
}

/// Copies the database files of a Node data folder into `scratch`.
fn stage_directory(source: &Path, scratch: &Path) -> Result<Files, ImportError> {
    if ownership_marker::inspect(source) != ownership_marker::MarkerState::Absent {
        return Err(ImportError::RustOwned);
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
    for name in DATABASE_FILES {
        let mut file = match pp_os::open_read_nofollow(&source.join(name)) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !file.metadata()?.is_file() {
            return Err(anyhow::anyhow!("{name} is not a regular file").into());
        }
        std::io::copy(
            &mut file,
            &mut std::fs::File::create_new(scratch.join(name))?,
        )?;
    }
    if stamps() != before {
        return Err(ImportError::Changed);
    }
    let mut repo_files = Vec::new();
    let mut pending = vec![source.join("repos")];
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            // `file_type` does not follow a link, so the walk stays in the folder.
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                repo_files.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    Ok(Files {
        documents: source.to_owned(),
        repo_files,
    })
}

/// Validates a backup archive and extracts its database and single-file
/// roots into `scratch`.
fn stage_archive(source: &Path, scratch: &Path) -> Result<Files, ImportError> {
    let (metadata, paths) = archive::extract_documents(source, archive::Limits::default(), scratch)
        .map_err(|error| ImportError::InvalidArchive(format!("{error:#}")))?;
    if metadata.scope != archive::MetadataScope::Full {
        return Err(ImportError::PartialArchive(metadata.scope));
    }
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

/// The elements of a stored JSON array, or one item for any other value.
fn items(stored: &[u8]) -> u64 {
    match serde_json::from_slice(stored) {
        Ok(Value::Array(elements)) => elements.len() as u64,
        _ => 1,
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
            .map(|(_, value)| items(value.as_bytes()))
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
        Shape::File(name) => match pp_os::open_read_nofollow(&files.documents.join(name)) {
            Ok(mut file) => {
                let mut stored = Vec::new();
                file.read_to_end(&mut stored)?;
                items(&stored)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        },
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
        printers: count(&Shape::Setting("printer.fleet"))?.1,
        unmatched_prints,
        rows,
        copied: inventory::COPIED
            .iter()
            .map(count)
            .collect::<Result<_, _>>()?,
        scalar_settings,
        unnamed,
        tables,
    })
}

#[cfg(test)]
mod tests;
