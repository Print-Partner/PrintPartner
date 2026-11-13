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
mod setup;
mod verify;

pub use copy::{ImportReport, REPORT_FILE, import};
pub use setup::{Counted, CredentialAddress, SetupReport, SizeChange};
pub use verify::SourcePaths;

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
    /// The folder has a valid `.pp-rust-owned.json`: it is a Rust installation.
    RustOwned,
    /// The folder has a `.pp-rust-owned.json` entry that is not a valid marker.
    SourceMarker(ownership_marker::MarkerRefusal),
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
    /// Rows are stored under an installation value other than `default`,
    /// which is the only one the Rust runtime reads.
    Installation {
        value: String,
        rows: u64,
    },
    /// More than one account is stored; the Rust runtime signs in with one.
    Accounts(u64),
    /// The Source with this id has a current revision with no files under `repos/`.
    MissingRevision(i64),
    /// The Source with this id stores a path the import cannot place under `repos/`.
    SourcePath(i64),
    /// Rows of these tables differ from the source after the upgrade.
    RowsChanged(Vec<String>),
    /// These two Sources would lead to the same folder in the copy.
    SourcesShare(i64, i64),
    /// The Source's rewritten path does not stay under the copy's `repos/`.
    SourceEscapes(i64),
    /// The Source's path in the copy is not what its stored path maps to.
    SourceRebase(i64),
    /// Stored printers or integrations hold values Rust's documents cannot
    /// hold: each kind and field with the number of rows, as the dry run
    /// lists them.
    SetupValues(Vec<Counted>),
    /// The converted `printer.fleet` or `integrations` setting failed its
    /// check. With a fault, Rust's reader refuses the document. Without one,
    /// the document does not hold every entry and unread key of the source.
    Setup {
        setting: &'static str,
        fault: Option<crate::printer_setup::DocumentFault>,
    },
    /// Backup roots hold entries that are neither regular files nor folders.
    /// The import skips nothing, so it copies none of the installation.
    NotRegular(Vec<Unsupported>),
    /// A copied file read back differently from its source.
    CopyMismatch(PathBuf),
    /// The flush of the staged files to disk failed. No marker is written.
    Flush {
        path: PathBuf,
        source: std::io::Error,
    },
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
    /// The temporary directory is the source folder or the destination, or
    /// lies inside one. The scratch copy would be written there.
    TemporaryInside {
        temporary: PathBuf,
        folder: PathBuf,
    },
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RustOwned => write!(
                f,
                "The folder has a {} entry: it is a Rust installation, not a Node one",
                ownership_marker::MARKER_FILE
            ),
            Self::SourceMarker(refusal) => write!(
                f,
                "The folder has a {} entry that is no valid marker ({refusal}). A Node installation has none",
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
                (tenants.iter().map(|tenant| masked(tenant)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Destination(_)
            | Self::Unnamed(_)
            | Self::Installation { .. }
            | Self::Accounts(_)
            | Self::NotRegular(_)
            | Self::MissingRevision(_)
            | Self::SourcePath(_)
            | Self::RowsChanged(_)
            | Self::SourcesShare(..)
            | Self::SourceEscapes(_)
            | Self::SourceRebase(_)
            | Self::SetupValues(_)
            | Self::Setup { .. }
            | Self::CopyMismatch(_)
            | Self::Flush { .. } => self.fmt_write_step(f),
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
            Self::TemporaryInside { temporary, folder } => write!(
                f,
                "The temporary directory {} is inside {}, where nothing may be written. Set TMPDIR to a directory outside the source and the destination",
                temporary.display(),
                folder.display()
            ),
        }
    }
}

impl ImportError {
    /// The messages of the variants only the import itself returns.
    fn fmt_write_step(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Destination(found) => {
                // Only a folder that merely holds something may be emptied.
                // Never say so of the source or of a Rust installation.
                let advice = match found {
                    Destination::NotEmpty(_) => "Empty it or choose another folder",
                    _ => "Choose another folder",
                };
                write!(f, "The destination {found}. {advice}")
            }
            Self::Unnamed(shapes) => write!(
                f,
                "The inventory does not name this stored JSON: {}. Report it; the import runs once the inventory names it",
                shapes.join(", ")
            ),
            Self::Installation { value, rows } => write!(
                f,
                "Rows stored under the installation value {:?}: {rows}. The import handles only data stored under \"default\" today. Send the dry-run report back so this case can be added",
                masked(value)
            ),
            Self::Accounts(accounts) => write!(
                f,
                "{accounts} accounts are stored, and the Rust runtime signs in with one. Remove the extra accounts in Node first, or send the dry-run report back so a choice can be added"
            ),
            Self::NotRegular(found) => write!(
                f,
                "The folder holds entries that are neither files nor folders: {}. The import copies files and folders and skips nothing. Remove or replace them, then retry",
                listed(found)
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
            Self::SourcesShare(first, second) => write!(
                f,
                "Sources {first} and {second} would lead to the same folder in the copy, so the import stops. Send the dry-run report back"
            ),
            Self::SourceEscapes(source) => write!(
                f,
                "Source {source}'s folder in the copy does not stay under repos/, so the import stops"
            ),
            Self::SourceRebase(source) => write!(
                f,
                "Source {source}'s path in the copy is not what its stored path maps to, so the import stops"
            ),
            Self::SetupValues(refused) => write!(
                f,
                "Stored printers or integrations hold values Rust's documents cannot hold (kind, field, rows): {}. Nothing is guessed or dropped, so the import stops. Send the dry-run report back so each field can be decided",
                (refused.iter())
                    .map(|one| format!("{} {} = {}", one.kind, one.name, one.count))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Setup { setting, fault } => write!(
                f,
                "The converted setting {setting} failed its check ({}), so the import stops. Send the dry-run report back",
                match fault {
                    Some(fault) => format!("{fault:?}"),
                    None => "it does not hold every stored entry and key".to_owned(),
                }
            ),
            Self::CopyMismatch(copy) => write!(
                f,
                "The copy {} does not match its source. Check the disk, empty the destination and retry",
                copy.display()
            ),
            Self::Flush { path, source } => write!(
                f,
                "The imported files could not be flushed to disk in {}: {source}. Empty the destination and retry",
                path.display()
            ),
            _ => Ok(()),
        }
    }
}

impl std::error::Error for ImportError {}

fn listed(found: &[Unsupported]) -> String {
    let found: Vec<String> = found.iter().map(Unsupported::to_string).collect();
    found.join(", ")
}

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

/// An entry in a backup root that is neither a regular file nor a folder.
/// The import copies files and folders and skips nothing, so one stops it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    Link,
    Fifo,
    Socket,
    Device,
    Other,
}

impl fmt::Display for EntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Link => "symbolic links",
            Self::Fifo => "FIFOs",
            Self::Socket => "sockets",
            Self::Device => "devices",
            Self::Other => "special files",
        })
    }
}

impl EntryKind {
    fn of(kind: std::fs::FileType) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            if kind.is_fifo() {
                return Self::Fifo;
            } else if kind.is_socket() {
                return Self::Socket;
            } else if kind.is_block_device() || kind.is_char_device() {
                return Self::Device;
            }
        }
        if kind.is_symlink() {
            Self::Link
        } else {
            Self::Other
        }
    }
}

/// How many entries of one kind a backup root holds that the import cannot
/// copy. It names the root and the kind, never a file name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsupported {
    pub root: &'static str,
    pub kind: EntryKind,
    pub entries: u64,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} under {}", self.entries, self.kind, self.root)
    }
}

/// How much one backup root of a source holds. A backup is bounded by its
/// number of entries, so the dry run reports this for every root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootCount {
    pub root: &'static str,
    /// Its regular files and folders, the root itself included, as a backup
    /// counts them.
    pub entries: u64,
    /// The bytes of its files. `None` for an archive, which is only listed.
    pub bytes: Option<u64>,
}

/// What one walk of the backup roots found.
struct Scan {
    roots: Vec<RootCount>,
    /// Per root and kind, the entries that are neither files nor folders.
    unsupported: Vec<Unsupported>,
    /// The name of every regular file under `repos/`. An entry of any other
    /// kind is not copied, so it is not listed here.
    repo_files: Vec<String>,
}

/// Walks every backup root of a Node data folder once. It counts each
/// root's files, folders and bytes, lists the files under `repos/`, and
/// counts the entries that are neither regular files nor folders. No link
/// is followed, so the walk stays in the folder.
fn scan_roots(source: &Path) -> Result<Scan, ImportError> {
    let mut found: BTreeMap<(&'static str, EntryKind), u64> = BTreeMap::new();
    let (mut roots, mut repo_files) = (Vec::new(), Vec::new());
    for (root, _) in archive::FULL_ROOTS {
        let path = source.join(root);
        let Ok(metadata) = path.symlink_metadata() else {
            continue;
        };
        if !metadata.is_dir() && !metadata.is_file() {
            *found
                .entry((root, EntryKind::of(metadata.file_type())))
                .or_insert(0) += 1;
            continue;
        }
        let (mut entries, mut bytes) = (1, 0);
        let mut pending = Vec::from_iter(metadata.is_dir().then_some(path));
        if metadata.is_file() {
            bytes = metadata.len();
        }
        while let Some(directory) = pending.pop() {
            let failed = |error| io_error(&directory, error);
            for entry in std::fs::read_dir(&directory).map_err(failed)? {
                let entry = entry.map_err(failed)?;
                let kind = entry.file_type().map_err(failed)?;
                if kind.is_dir() {
                    entries += 1;
                    pending.push(entry.path());
                } else if kind.is_file() {
                    entries += 1;
                    bytes += entry.metadata().map_err(failed)?.len();
                    if root == "repos" {
                        repo_files.push(entry.file_name().to_string_lossy().into_owned());
                    }
                } else {
                    *found.entry((root, EntryKind::of(kind))).or_insert(0) += 1;
                }
            }
        }
        roots.push(RootCount {
            root,
            entries,
            bytes: Some(bytes),
        });
    }
    let counted = |((root, kind), entries)| Unsupported {
        root,
        kind,
        entries,
    };
    Ok(Scan {
        roots,
        unsupported: found.into_iter().map(counted).collect(),
        repo_files,
    })
}

/// The database files in `stage`, with their bytes.
fn staged_sizes(stage: &Path) -> Vec<(&'static str, u64)> {
    let sized = |name| Some((name, stage.join(name).metadata().ok()?.len()));
    DATABASE_FILES.into_iter().filter_map(sized).collect()
}

/// The entries an archive lists under each backup root.
fn listed_roots(paths: &BTreeSet<String>) -> Vec<RootCount> {
    let under = |root: &str| {
        let below = |path: &&String| {
            path.as_str() == root
                || path
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with('/'))
        };
        paths.iter().filter(below).count() as u64
    };
    let roots = archive::FULL_ROOTS.iter().map(|(root, _)| RootCount {
        root,
        entries: under(root),
        bytes: None,
    });
    roots.filter(|count| count.entries > 0).collect()
}

/// What the import would find at its destination, read without writing.
#[derive(Debug, PartialEq, Eq)]
pub enum Destination {
    /// Absent, or empty by the marker rule and without a lease entry. Its
    /// owner and permissions are not checked here.
    Free,
    /// It holds this entry and no marker: the import would refuse it.
    NotEmpty(String),
    /// It has a valid marker: a Rust installation is there already.
    Marked,
    /// It has a marker entry that is not a valid marker.
    BadMarker(ownership_marker::MarkerRefusal),
    /// It, or its marker, could not be read. A destination that is a file
    /// is one case.
    Unreadable(std::io::ErrorKind),
    /// It shares files with the source.
    Overlaps(Overlap),
    /// Its path has a `.` or `..` component. Such a path can name one folder
    /// before the import creates it and another after.
    Indirect,
    /// The path no longer leads to the folder the import wrote into.
    Replaced,
}

/// How a destination and the source share files, once links and `..` are
/// resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlap {
    /// They are the same folder.
    Same,
    /// The destination lies inside the source folder.
    DestinationInside,
    /// The source lies inside the destination.
    SourceInside,
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Free => f.write_str("is absent or empty, as the import needs"),
            Self::Indirect => f.write_str(
                "is given with a \".\" or \"..\" component, so the import refuses it. Give the folder's own path",
            ),
            Self::Replaced => f.write_str(
                "no longer leads to the folder the import wrote into, so no marker is written",
            ),
            Self::Overlaps(Overlap::Same) => {
                f.write_str("is the source folder, so the import refuses it")
            }
            Self::Overlaps(Overlap::DestinationInside) => {
                f.write_str("lies inside the source folder, so the import refuses it")
            }
            Self::Overlaps(Overlap::SourceInside) => {
                f.write_str("holds the source, so the import refuses it")
            }
            Self::NotEmpty(entry) => write!(f, "holds {entry:?}, so the import refuses it"),
            Self::Marked => f.write_str("is a Rust installation, so the import refuses it"),
            Self::BadMarker(refusal) => write!(
                f,
                "has a {} entry that is no valid marker ({refusal}), so the import refuses it",
                ownership_marker::MARKER_FILE
            ),
            Self::Unreadable(kind) => {
                write!(f, "cannot be read ({kind}), so the import refuses it")
            }
        }
    }
}

/// `path` with its links and `..` resolved. A path that does not exist yet
/// has its nearest existing ancestor resolved and the rest appended: a name
/// that does not exist cannot be a link.
fn resolved(path: &Path) -> std::io::Result<PathBuf> {
    let path = std::path::absolute(path)?;
    let mut existing = path.as_path();
    let mut rest = Vec::new();
    let mut base = loop {
        match existing.canonicalize() {
            Ok(base) => break base,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return Err(ErrorKind::NotFound.into());
        };
        rest.push(name);
        existing = parent;
    };
    base.extend(rest.into_iter().rev());
    Ok(base)
}

/// Inspects the folder an import of `source` would write to. Nothing is
/// created. A destination that is the source, lies inside it or holds it is
/// refused before anything else is read.
pub fn place_destination(source: &Path, destination: &Path) -> Destination {
    // A `..` after a name that does not exist yet resolves only once the
    // import has created that name, and then to its parent.
    let text = destination.to_string_lossy();
    if (text.split(std::path::is_separator)).any(|part| part == "." || part == "..") {
        return Destination::Indirect;
    }
    let (from, to) = match (resolved(source), resolved(destination)) {
        (Ok(from), Ok(to)) => (from, to),
        (Err(error), _) | (_, Err(error)) => return Destination::Unreadable(error.kind()),
    };
    if from == to {
        return Destination::Overlaps(Overlap::Same);
    }
    if to.starts_with(&from) {
        return Destination::Overlaps(Overlap::DestinationInside);
    }
    if from.starts_with(&to) {
        return Destination::Overlaps(Overlap::SourceInside);
    }
    inspect_destination(destination)
}

/// Inspects the folder an import would write to. Nothing is created.
pub fn inspect_destination(destination: &Path) -> Destination {
    use ownership_marker::{MarkerRefusal, MarkerState};
    match ownership_marker::inspect(destination) {
        MarkerState::Absent => {}
        // The folder cannot be opened, for example because it is a file, or
        // its marker cannot be read.
        MarkerState::Refused(MarkerRefusal::Unreadable(kind)) => {
            return Destination::Unreadable(kind);
        }
        MarkerState::Refused(refusal) => return Destination::BadMarker(refusal),
        MarkerState::Valid(_) => return Destination::Marked,
    }
    // The writer refuses a Node lease directory that the marker rule alone
    // counts as empty, so any lease entry makes the folder unusable.
    if destination.join(LEASE).symlink_metadata().is_ok() {
        return Destination::NotEmpty(LEASE.into());
    }
    match ownership_marker::first_non_empty_entry(destination) {
        Ok(None) => Destination::Free,
        Ok(Some(entry)) => Destination::NotEmpty(entry),
        Err(error) => Destination::Unreadable(error.kind()),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct TableCount {
    pub table: String,
    pub rows: u64,
    /// Rows per `tenant_id` value, as stored. Empty for a table without that
    /// column. A value can hold a sign-in name; printing goes through
    /// [`masked`].
    pub by_tenant: BTreeMap<String, u64>,
}

/// A stored JSON document the inventory does not name.
#[derive(Debug, PartialEq, Eq)]
pub struct Unnamed {
    /// `table.column`, or `setting <key>` with each run of digits as `#`.
    pub shape: String,
    pub items: u64,
}

/// What a dry run found. It holds keys, names and counts, and no stored
/// value but the `tenant_id` values, which its text masks, and the bed sizes
/// the import would round.
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
    /// Every `tenant_id` value that any table holds rows under, as stored;
    /// printing goes through [`masked`]. The Rust-only runtime reads only
    /// `default`.
    pub installations: BTreeSet<String>,
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
    /// Entries of the backup roots that are neither files nor folders. The
    /// import refuses a folder that holds any.
    pub unsupported: Vec<Unsupported>,
    /// What each backup root holds, in backup order. A root the source
    /// lacks is left out.
    pub roots: Vec<RootCount>,
    /// The bytes of `print-partner.db`, its `-wal` and its `-journal`, for
    /// each the source holds. The startup check copies them under `TMPDIR`
    /// and reads them into memory.
    pub database_files: Vec<(&'static str, u64)>,
    /// What the conversion of the stored printers and integrations finds.
    pub setup: SetupReport,
    /// How the Sources' stored paths would come out. A Source whose path is
    /// outside the installation, or whose folder is missing, keeps its
    /// record but its files are not in the copy.
    pub source_paths: SourcePaths,
}

impl DryRunReport {
    /// The bytes of the database files, then the entries, and for a folder
    /// the bytes, under each backup root.
    fn fmt_roots(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "\nDatabase files (bytes):")?;
        for (name, bytes) in &self.database_files {
            writeln!(f, "  {name} {bytes}")?;
        }
        let bytes: u64 = self.database_files.iter().map(|(_, bytes)| bytes).sum();
        writeln!(f, "  all database files: {bytes} bytes")?;
        writeln!(
            f,
            "\nBackup roots (files and folders, then bytes of the files):"
        )?;
        for count in &self.roots {
            write!(f, "  {} {}", count.root, count.entries)?;
            if let Some(bytes) = count.bytes {
                write!(f, " {bytes}")?;
            }
            writeln!(f)?;
        }
        let entries: u64 = self.roots.iter().map(|count| count.entries).sum();
        writeln!(f, "  all roots: {entries} entries")?;
        writeln!(
            f,
            "\nSource paths outside the installation: {}",
            self.source_paths.outside
        )?;
        writeln!(
            f,
            "Source paths whose folder is missing in the copy: {}",
            self.source_paths.missing
        )?;
        let refused = match self.source_paths.missing_revisions {
            0 => "",
            _ => " The import would refuse this installation.",
        };
        writeln!(
            f,
            "Sources whose current revision cannot be placed in the copy: {}.{refused}",
            self.source_paths.missing_revisions
        )?;
        write!(f, "\n{}", self.setup)
    }
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
        let values: Vec<String> = (self.installations.iter())
            .map(|value| format!("{:?}", masked(value)))
            .collect();
        let label = match values.len() {
            1 => "Installation value",
            _ => "Installation values",
        };
        let values = match values.len() {
            0 => "none".to_owned(),
            _ => values.join(", "),
        };
        writeln!(f, "{label} {values}; accounts stored: {}.", self.users)?;
        let refused = "so the import would refuse this installation";
        if self.installations.iter().any(|value| value != "default") {
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
        if !self.unsupported.is_empty() {
            writeln!(
                f,
                "Entries that are neither files nor folders: {}. The import skips nothing, {refused}.",
                listed(&self.unsupported)
            )?;
        }
        if let Some(destination) = &self.destination {
            writeln!(f, "The destination {destination}.")?;
        }
        self.fmt_roots(f)?;
        writeln!(f, "\nTables with rows (rows, then rows per tenant):")?;
        for table in self.tables.iter().filter(|table| table.rows > 0) {
            write!(f, "  {} {}", table.table, table.rows)?;
            for (tenant, rows) in &table.by_tenant {
                write!(f, " {}={rows}", masked(tenant))?;
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
    /// The name of every regular file under `repos/`.
    repo_files: Vec<String>,
    /// Entries of the backup roots that are neither files nor folders.
    unsupported: Vec<Unsupported>,
    /// What each backup root holds.
    roots: Vec<RootCount>,
    /// The database files the source holds, with their bytes.
    database: Vec<(&'static str, u64)>,
    /// The Node data folder itself, for a folder source. Stored Source
    /// paths are compared with it. An archive has none.
    root: Option<PathBuf>,
    /// Where to look for a Source's folder below `repos/`.
    held: Held,
}

/// What says whether the copy will hold a folder below `repos/`.
enum Held {
    /// The folder that holds `repos/`: the source folder, or the stage of
    /// an extracted archive.
    Folder(PathBuf),
    /// The entries of an archive a dry run listed and did not extract.
    Listed(BTreeSet<String>),
}

impl Held {
    fn holds(&self, inside: &str) -> bool {
        match self {
            Self::Folder(folder) => folder.join("repos").join(inside).is_dir(),
            // The listing does not say whether an entry is a folder, so a
            // folder is one that has something in it. A snapshot always
            // holds its manifest.
            Self::Listed(paths) => {
                let below = format!("repos/{inside}/");
                paths.iter().any(|path| path.starts_with(&below))
            }
        }
    }
}

/// Reports what importing `source` would convert. `source` is a stopped Node
/// data folder or a format 2 `full` backup archive. The database is copied
/// into a private directory under the system temporary directory, upgraded
/// there, and removed before this returns.
///
/// With a `destination`, the report also says whether the import would
/// accept that folder. The folder is read, never created.
pub fn dry_run(source: &Path, destination: Option<&Path>) -> Result<DryRunReport, ImportError> {
    dry_run_in(source, destination, &std::env::temp_dir())
}

/// Refuses when `temporary` is `folder` or lies inside it, with links and
/// `..` resolved in both. The dry run stages there, and the startup check
/// that the dry run and the import share copies the database there. A path
/// that cannot be resolved is an error, never a pass.
fn ensure_outside(temporary: &Path, folder: &Path) -> Result<(), ImportError> {
    let inside = temporary
        .canonicalize()
        .map_err(|error| io_error(temporary, error))?;
    let around = resolved(folder).map_err(|error| io_error(folder, error))?;
    if !inside.starts_with(&around) {
        return Ok(());
    }
    Err(ImportError::TemporaryInside {
        temporary: temporary.to_owned(),
        folder: folder.to_owned(),
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

/// [`dry_run`] with its scratch copy under `temporary`, which is also where
/// the startup check copies the database when it is the system's directory.
fn dry_run_in(
    source: &Path,
    destination: Option<&Path>,
    temporary: &Path,
) -> Result<DryRunReport, ImportError> {
    let metadata = source.metadata().map_err(|error| io_error(source, error))?;
    // Before any scratch exists: a dry run writes nothing into the source or
    // the destination.
    ensure_outside(temporary, source)?;
    let placed = destination.map(|destination| place_destination(source, destination));
    // A path with `..`, or one that cannot be resolved, is reported as
    // refused. It names no folder to keep the temporary directory out of.
    let unresolved =
        |placed: &Destination| matches!(placed, Destination::Indirect | Destination::Unreadable(_));
    if let Some(destination) = destination
        && !placed.as_ref().is_some_and(unresolved)
    {
        ensure_outside(temporary, destination)?;
    }
    let destination = placed;
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
    let report = measure(&connection, kind, &ready, &files)?;
    Ok(DryRunReport {
        destination,
        ..report
    })
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
    let database = stage.join(archive::DATABASE_FILE);
    let version = admitted
        .map_err(|error| upgrade_failure(&database, format!("{error:#}"), error.chain()))?;
    let read = before(&database)?;
    let now = crate::auth::catalog_timestamp();
    let (connection, ready) = schema::initialize_with(&database, version, &now, false)
        .map_err(|error| upgrade_failure(&database, format!("{error:#}"), error.chain()))?;
    Ok((connection, ready, read))
}

/// Sorts a failed check or upgrade of the staged database by its causes. A
/// disk, filesystem or lock that refused the work fails this run and says
/// nothing about the source, so it is an [`ImportError::Io`]. Anything else
/// is the database failing its checks.
fn upgrade_failure<'a>(
    database: &Path,
    detail: String,
    causes: impl Iterator<Item = &'a (dyn std::error::Error + 'static)>,
) -> ImportError {
    use rusqlite::ErrorCode;
    for cause in causes {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return io_error(database, std::io::Error::new(io.kind(), detail));
        }
        let storage = matches!(
            (cause.downcast_ref::<rusqlite::Error>()).and_then(rusqlite::Error::sqlite_error_code),
            Some(
                ErrorCode::DiskFull
                    | ErrorCode::SystemIoFailure
                    | ErrorCode::ReadOnly
                    | ErrorCode::CannotOpen
                    | ErrorCode::PermissionDenied
                    | ErrorCode::OutOfMemory
                    | ErrorCode::DatabaseBusy
                    | ErrorCode::DatabaseLocked
            )
        );
        if storage {
            return io_error(database, std::io::Error::other(detail));
        }
    }
    ImportError::Database(detail)
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
        MarkerState::Refused(refusal) => return Err(ImportError::SourceMarker(refusal)),
        MarkerState::Valid(_) => return Err(ImportError::RustOwned),
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
    // Each is read back and compared before anything opens the copy: a
    // short write-ahead log still opens, and silently lacks its last rows.
    for name in DATABASE_FILES {
        let path = source.join(name);
        if path.symlink_metadata().is_ok() {
            copy::copy_verified(&path, &scratch.join(name))?;
        }
    }
    if stamps() != before {
        return Err(ImportError::Changed);
    }
    let scan = scan_roots(source)?;
    let sized = |(name, stamp): (&'static str, &Option<(u64, _)>)| Some((name, stamp.as_ref()?.0));
    Ok(Files {
        documents: source.to_owned(),
        repo_files: scan.repo_files,
        unsupported: scan.unsupported,
        roots: scan.roots,
        database: DATABASE_FILES
            .into_iter()
            .zip(&before)
            .filter_map(sized)
            .collect(),
        root: Some(source.to_owned()),
        held: Held::Folder(source.to_owned()),
    })
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
        // Validation refuses an archive entry that is no file or folder.
        unsupported: Vec::new(),
        roots: listed_roots(&paths),
        database: staged_sizes(scratch),
        root: None,
        held: Held::Listed(paths),
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

/// A `tenant_id` value as a report or a message may show it. Values are
/// counted and compared as stored and masked only here, when printed, so two
/// account ids stay two values. `default` and
/// `anonymous` are Node's own words. Any other value is an account id or
/// `basic-<login>`, so only the `basic-` prefix is kept.
fn masked(value: &str) -> String {
    match value {
        "default" | "anonymous" => value.to_owned(),
        _ if value.starts_with("basic-") => "basic-***".to_owned(),
        _ => "***".to_owned(),
    }
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
    // What the import would do with each Source's path, judged against the
    // `repos` the import copies or extracts.
    let held = |inside: &str| files.held.holds(inside);
    let rows = verify::source_rows(connection)?;
    let source_paths = SourcePaths::of(&verify::decide(&rows, files.root.as_deref(), held));
    let setup = setup::plan(connection)?.report;
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
    // Every table with the column counts: an account without Builds or
    // Sources still stores its settings and events under its own value.
    let installations = (tables.iter())
        .flat_map(|table| table.by_tenant.keys().cloned())
        .collect();

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
        // The printers the converted fleet holds.
        printers: setup.printers,
        unmatched_prints,
        installations,
        users: table("users").map_or(0, |table| table.rows),
        destination: None,
        rows,
        copied: (inventory::COPIED.iter().map(count)).collect::<Result<_, _>>()?,
        unused: (inventory::UNUSED.iter().map(count)).collect::<Result<_, _>>()?,
        scalar_settings,
        unnamed,
        unsupported: files.unsupported.clone(),
        roots: files.roots.clone(),
        database_files: files.database.clone(),
        source_paths,
        setup,
        tables,
    })
}

#[cfg(test)]
mod tests;
