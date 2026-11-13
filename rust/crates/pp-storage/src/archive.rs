//! App backup archives (T-8-archive). The format is Node's tar.gz format 2
//! from `web/apps/server/src/services/backup-restore.ts`, so either runtime
//! validates the other's archives.

use anyhow::{Context, Result, anyhow, bail, ensure};
use flate2::{Compression, bufread::GzDecoder, write::GzEncoder};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{BufRead, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};
use tar::{EntryType, Header, HeaderMode};

pub const FORMAT_VERSION: u64 = 2;
pub const METADATA_FILE: &str = "backup-metadata.json";
pub const DATABASE_FILE: &str = "print-partner.db";
pub const WAL_FILE: &str = "print-partner.db-wal";
const CORE_FILES: [&str; 3] = [METADATA_FILE, DATABASE_FILE, WAL_FILE];
const MAX_METADATA_BYTES: u64 = 64 * 1024;
/// Zero padding allowed after the tar end-of-archive block, before the gzip
/// trailer. Tar writers pad to a record (10 KiB by default).
const MAX_TRAILING_BYTES: u64 = 1024 * 1024;
const SQLITE_HEADER: &[u8] = b"SQLite format 3\0";
const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootKind {
    Directory,
    File,
}

/// Node's `FULL_BACKUP_ROOTS`, in its order.
#[rustfmt::skip]
pub const FULL_ROOTS: [(&str, RootKind); 11] = [
    ("repos", RootKind::Directory),
    ("sources", RootKind::Directory),
    ("exports", RootKind::Directory),
    // ast-grep-ignore: no-writer-family-literal -- Names a backup root this module only reads; Rust does not write the family.
    ("thumbs", RootKind::Directory),
    // ast-grep-ignore: no-writer-family-literal -- Names a backup root this module only reads; Rust does not write the family.
    ("covers", RootKind::Directory),
    // ast-grep-ignore: no-writer-family-literal -- Names a backup root this module only reads; Rust does not write the family.
    ("assistant-domain", RootKind::Directory),
    // ast-grep-ignore: no-writer-family-literal -- Names a backup root this module only reads; Rust does not write the family.
    ("manifests", RootKind::Directory),
    // ast-grep-ignore: no-writer-family-literal -- Names a backup root this module only reads; Rust does not write the family.
    ("custom_filaments.json", RootKind::File),
    // ast-grep-ignore: no-writer-family-literal -- Names a backup root this module only reads; Rust does not write the family.
    ("kit-catalog.json", RootKind::File),
    // ast-grep-ignore: no-writer-family-literal -- Names a backup root this module only reads; Rust does not write the family.
    ("kit-catalog.yaml", RootKind::File),
    // ast-grep-ignore: no-writer-family-literal -- Names a backup root this module only reads; Rust does not write the family.
    ("path-hints.yaml", RootKind::File),
];

/// Node's `LEGACY_BACKUP_ROOTS`, the implicit scope of format 1.
#[rustfmt::skip]
pub const LEGACY_ROOTS: [&str; 5] = [
    "repos",
    "sources",
    "exports",
    // ast-grep-ignore: no-writer-family-literal -- Names a backup root this module only reads; Rust does not write the family.
    "thumbs",
    // ast-grep-ignore: no-writer-family-literal -- Names a backup root this module only reads; Rust does not write the family.
    "covers",
];

fn root_for(path: &str) -> Option<(&'static str, RootKind)> {
    FULL_ROOTS.into_iter().find(|(root, kind)| {
        path == *root
            || (*kind == RootKind::Directory
                && path
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with('/')))
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Full,
    DatabaseOnly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataScope {
    /// Format 1: no declared scope; Node restores [`LEGACY_ROOTS`].
    Legacy,
    Full,
    DatabaseOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub format_version: u64,
    pub created_at: String,
    pub app_version: String,
    pub scope: MetadataScope,
}

impl Metadata {
    pub fn new(app_version: &str, scope: Scope) -> Self {
        let now = time::OffsetDateTime::now_utc();
        Self {
            format_version: FORMAT_VERSION,
            created_at: format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
                now.year(),
                u8::from(now.month()),
                now.day(),
                now.hour(),
                now.minute(),
                now.second(),
                now.millisecond()
            ),
            app_version: app_version.to_owned(),
            scope: match scope {
                Scope::Full => MetadataScope::Full,
                Scope::DatabaseOnly => MetadataScope::DatabaseOnly,
            },
        }
    }

    pub fn included_roots(&self) -> Vec<&'static str> {
        match self.scope {
            MetadataScope::Legacy => LEGACY_ROOTS.to_vec(),
            MetadataScope::Full => FULL_ROOTS.iter().map(|(root, _)| *root).collect(),
            MetadataScope::DatabaseOnly => Vec::new(),
        }
    }

    fn to_json(&self) -> Result<Vec<u8>> {
        let kind = match self.scope {
            MetadataScope::Full => "full",
            MetadataScope::DatabaseOnly => "database-only",
            MetadataScope::Legacy => bail!("Rust writes only backup format {FORMAT_VERSION}"),
        };
        Ok(serde_json::to_vec_pretty(&json!({
            "version": FORMAT_VERSION.to_string(),
            "createdAt": self.created_at,
            "appVersion": self.app_version,
            "formatVersion": FORMAT_VERSION,
            "scope": { "kind": kind, "includedRoots": self.included_roots() },
        }))?)
    }

    /// Node's `parseBackupMetadata`.
    fn parse(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes).context("Invalid backup metadata JSON")?;
        let text = |key| value.get(key).and_then(Value::as_str);
        let (Some(version), Some(created_at), Some(app_version), Some(format_version)) = (
            text("version"),
            text("createdAt"),
            text("appVersion"),
            value.get("formatVersion").and_then(Value::as_u64),
        ) else {
            bail!("Invalid backup metadata: missing or invalid required fields");
        };
        ensure!(
            format_version >= 1
                && time::OffsetDateTime::parse(
                    created_at,
                    &time::format_description::well_known::Rfc3339
                )
                .is_ok(),
            "Invalid backup metadata: missing or invalid required fields"
        );
        ensure!(
            format_version <= FORMAT_VERSION,
            "Backup format version {format_version} is newer than supported {FORMAT_VERSION}"
        );
        ensure!(
            version == format_version.to_string(),
            "Invalid backup metadata: version fields do not match"
        );
        let metadata = |scope| Self {
            format_version,
            created_at: created_at.to_owned(),
            app_version: app_version.to_owned(),
            scope,
        };
        if format_version == 1 {
            return Ok(metadata(MetadataScope::Legacy));
        }
        let scope = value.get("scope");
        let roots = scope
            .and_then(|scope| scope.get("includedRoots"))
            .and_then(Value::as_array)
            .and_then(|roots| roots.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
            .filter(|roots| {
                roots
                    .iter()
                    .all(|root| root_for(root).is_some_and(|(r, _)| r == *root))
            })
            .ok_or_else(|| anyhow!("Invalid backup metadata: format v2 requires a valid scope"))?;
        let unique: BTreeSet<_> = roots.iter().copied().collect();
        match scope
            .and_then(|scope| scope.get("kind"))
            .and_then(Value::as_str)
        {
            Some("database-only") if roots.is_empty() => Ok(metadata(MetadataScope::DatabaseOnly)),
            Some("full") if unique.len() == roots.len() && roots.len() == FULL_ROOTS.len() => {
                Ok(metadata(MetadataScope::Full))
            }
            Some("full" | "database-only") => {
                bail!("Invalid backup metadata: scope roots do not match its kind")
            }
            _ => bail!("Invalid backup metadata: format v2 requires a valid scope"),
        }
    }
}

/// Free bytes a backup or a restore stage leaves on the data volume. The
/// reserve setting of GRE-1445 replaces this constant.
pub(crate) const RESERVE_BYTES: u64 = GIB;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_entries: u64,
    pub max_total_bytes: u64,
    pub max_entry_bytes: u64,
    pub max_decompression_ratio: u64,
}

impl Default for Limits {
    /// For the backups Rust writes and restores. Free disk space bounds their
    /// size, so no byte limit applies. The entry count guards against damaged
    /// metadata; it is not a product limit.
    fn default() -> Self {
        Self {
            max_entries: 5_000_000,
            max_total_bytes: u64::MAX,
            max_entry_bytes: u64::MAX,
            max_decompression_ratio: 200,
        }
    }
}

impl Limits {
    /// Node's validation limits. Node writes no archive over them, and the
    /// import reads Node's archives with them.
    #[cfg(feature = "desktop_rebuild")]
    pub(crate) fn node() -> Self {
        Self {
            max_entries: 100_000,
            max_total_bytes: 20 * GIB,
            max_entry_bytes: 8 * GIB,
            max_decompression_ratio: 200,
        }
    }

    /// Creation never accepts limits looser than the defaults restore enforces.
    fn tightened(self) -> Self {
        let defaults = Self::default();
        Self {
            max_entries: self.max_entries.min(defaults.max_entries),
            max_total_bytes: self.max_total_bytes.min(defaults.max_total_bytes),
            max_entry_bytes: self.max_entry_bytes.min(defaults.max_entry_bytes),
            max_decompression_ratio: self
                .max_decompression_ratio
                .min(defaults.max_decompression_ratio),
        }
    }
}

/// Not enough free disk space. Nothing was written.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Insufficient disk space: {needed} bytes needed, {available} bytes available")]
pub struct NoSpace {
    /// The bytes the operation writes, plus the reserve.
    pub needed: u64,
    pub available: u64,
}

/// An archive holds more bytes than [`Limits::max_total_bytes`], which is the
/// total its headers declared when staging extracts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Backup archive decompressed byte limit exceeded ({limit})")]
pub struct TotalExceeded {
    pub limit: u64,
}

/// Reads the free bytes on the filesystem that holds a path.
pub(crate) type FreeSpace = fn(&Path) -> std::io::Result<u64>;

pub(crate) fn free_space(path: &Path) -> std::io::Result<u64> {
    fs4::available_space(path)
}

/// The refusal for writing `bytes` at `path`, if that would leave less than
/// `reserve` free.
pub(crate) fn shortfall(
    read: FreeSpace,
    path: &Path,
    bytes: u64,
    reserve: u64,
) -> std::io::Result<Option<NoSpace>> {
    let needed = bytes.saturating_add(reserve);
    let available = read(path)?;
    Ok((available < needed).then_some(NoSpace { needed, available }))
}

#[derive(Clone, Debug)]
pub struct Request {
    pub app_version: String,
    pub scope: Scope,
    pub limits: Limits,
    /// Free bytes to leave at the destination after the uncompressed worst case.
    pub reserve_bytes: u64,
}

impl Request {
    pub fn new(app_version: &str, scope: Scope) -> Self {
        Self {
            app_version: app_version.to_owned(),
            scope,
            limits: Limits::default(),
            reserve_bytes: RESERVE_BYTES,
        }
    }
}

pub(crate) struct Job {
    pub destination: PathBuf,
    pub data_dir: PathBuf,
    pub metadata: Metadata,
    pub request: Request,
    pub free_space: FreeSpace,
}

pub(crate) enum Item {
    Directory(String),
    File(String, PathBuf, u64),
}

fn random_name(prefix: &str) -> String {
    format!("{prefix}{}", hex::encode(rand::random::<[u8; 12]>()))
}

/// The metadata and database entries every archive holds.
const CORE_ENTRIES: u64 = 2;

fn push(items: &mut Vec<Item>, item: Item, max_entries: u64) -> Result<()> {
    ensure!(
        items.len() as u64 + CORE_ENTRIES < max_entries,
        "Backup archive entry limit exceeded ({max_entries})"
    );
    items.push(item);
    Ok(())
}

/// Lists every root entry Node's `tar.c` filter keeps: regular files and
/// directories, never following a symlink. A root itself must not be a
/// symlink or the wrong type; a missing root is omitted. The walk stops as
/// soon as the archive would exceed `max_entries`.
pub(crate) fn plan(data_dir: &Path, scope: Scope, max_entries: u64) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    ensure!(
        CORE_ENTRIES <= max_entries,
        "Backup archive entry limit exceeded ({max_entries})"
    );
    if scope == Scope::DatabaseOnly {
        return Ok(items);
    }
    for (root, kind) in FULL_ROOTS {
        let path = data_dir.join(root);
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).with_context(|| format!("Backup root {root}")),
        };
        ensure!(
            !metadata.file_type().is_symlink(),
            "Backup root cannot be a symbolic link: {root}"
        );
        match kind {
            RootKind::File if metadata.is_file() => push(
                &mut items,
                Item::File(root.into(), path, metadata.len()),
                max_entries,
            )?,
            RootKind::Directory if metadata.is_dir() => {
                walk(&path, root.into(), &mut items, max_entries)?
            }
            _ => bail!("Backup root has the wrong file type: {root}"),
        }
    }
    Ok(items)
}

fn walk(directory: &Path, name: String, items: &mut Vec<Item>, max_entries: u64) -> Result<()> {
    push(items, Item::Directory(name.clone()), max_entries)?;
    let mut children = std::fs::read_dir(directory)
        .with_context(|| format!("Read backup directory {name}"))?
        .collect::<std::io::Result<Vec<_>>>()?;
    children.sort_by_key(|child| child.file_name());
    for child in children {
        let file_name = child.file_name();
        let Some(file_name) = file_name.to_str() else {
            bail!("Backup entry name is not UTF-8 under {name}");
        };
        let metadata = child.path().symlink_metadata()?;
        let child_name = format!("{name}/{file_name}");
        if metadata.is_dir() {
            walk(&child.path(), child_name, items, max_entries)?;
        } else if metadata.is_file() {
            push(
                items,
                Item::File(child_name, child.path(), metadata.len()),
                max_entries,
            )?;
        }
    }
    Ok(())
}

fn open_no_follow(path: &Path) -> Result<File> {
    let file = pp_os::open_read_nofollow(path)?;
    ensure!(
        file.metadata()?.is_file(),
        "Backup entry is no longer a regular file: {}",
        path.display()
    );
    Ok(file)
}

fn append<W: Write>(
    builder: &mut tar::Builder<W>,
    name: &str,
    metadata: &std::fs::Metadata,
    entry_type: EntryType,
    reader: impl Read,
) -> Result<()> {
    let mut header = Header::new_gnu();
    // Node writes `portable` entries: no owner, but real mtimes and modes.
    header.set_metadata_in_mode(metadata, HeaderMode::Deterministic);
    if let Ok(modified) = metadata.modified()
        && let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH)
    {
        header.set_mtime(since.as_secs());
    }
    header.set_entry_type(entry_type);
    if entry_type == EntryType::Directory {
        header.set_size(0);
    }
    builder.append_data(&mut header, name, reader)?;
    Ok(())
}

/// Writes the archive from the writer thread. The caller holds an IMMEDIATE
/// transaction, so no SQLite writer in any process commits between the
/// database snapshot and the last root entry.
pub(crate) fn write(connection: &Connection, job: &Job) -> Result<()> {
    let parent = job
        .destination
        .parent()
        .ok_or_else(|| anyhow!("Backup parent missing"))?;
    let name = job
        .destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("Backup file name missing"))?;
    let limits = job.request.limits.tightened();
    let items = plan(&job.data_dir, job.request.scope, limits.max_entries)?;
    let page_bytes: i64 = connection.query_row(
        "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
        [],
        |row| row.get(0),
    )?;
    let page_bytes = u64::try_from(page_bytes)?;
    let root_bytes: u64 = items
        .iter()
        .map(|item| match item {
            Item::File(_, _, len) => *len,
            Item::Directory(_) => 0,
        })
        .sum();
    ensure!(
        page_bytes.saturating_add(root_bytes) <= limits.max_total_bytes,
        TotalExceeded {
            limit: limits.max_total_bytes
        }
    );
    // The snapshot, an archive no smaller than its inputs and the database
    // copy validation extracts coexist at the destination until publication.
    let writes = page_bytes.saturating_mul(3).saturating_add(root_bytes);
    let reserve = job.request.reserve_bytes;
    if let Some(no_space) = shortfall(job.free_space, parent, writes, reserve)
        .context("Read free space at backup destination")?
    {
        return Err(no_space.into());
    }

    let work = parent.join(random_name(&format!(".{name}.work-")));
    let temporary = parent.join(random_name(&format!(".{name}.tmp-")));
    let result = (|| -> Result<()> {
        std::fs::create_dir(&work)?;
        let snapshot = work.join(DATABASE_FILE);
        // The writer's own connection cannot be a backup source inside its
        // transaction. A fresh reader sees exactly the committed state the
        // held lock now freezes.
        let source = connection
            .path()
            .filter(|path| !path.is_empty())
            .ok_or_else(|| anyhow!("Archive source database has no path"))?;
        Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?.backup(
            "main",
            &snapshot,
            rusqlite::backup::NO_PROGRESS,
        )?;
        let metadata = job.metadata.to_json()?;
        let output = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut builder = tar::Builder::new(GzEncoder::new(
            BufWriter::new(output),
            Compression::default(),
        ));
        let snapshot_file = File::open(&snapshot)?;
        append(
            &mut builder,
            DATABASE_FILE,
            &snapshot_file.metadata()?,
            EntryType::Regular,
            snapshot_file,
        )?;
        let mut header = Header::new_gnu();
        header.set_entry_type(EntryType::Regular);
        header.set_mode(0o644);
        header.set_size(metadata.len() as u64);
        header.set_mtime(time::OffsetDateTime::now_utc().unix_timestamp().max(0) as u64);
        builder.append_data(&mut header, METADATA_FILE, metadata.as_slice())?;
        for item in &items {
            match item {
                Item::Directory(name) => {
                    let path = job.data_dir.join(name);
                    let metadata = path.symlink_metadata()?;
                    ensure!(metadata.is_dir(), "Backup directory changed type: {name}");
                    append(
                        &mut builder,
                        name,
                        &metadata,
                        EntryType::Directory,
                        std::io::empty(),
                    )?;
                }
                Item::File(name, path, _) => {
                    let file = open_no_follow(path)?;
                    let metadata = file.metadata()?;
                    append(&mut builder, name, &metadata, EntryType::Regular, file)?;
                }
            }
        }
        let output = builder.into_inner()?.finish()?;
        output
            .into_inner()
            .map_err(|error| error.into_error())?
            .sync_all()?;
        #[cfg(all(test, unix))]
        tests::hold_freeze(&job.destination);
        validate_in(&temporary, limits, &work.join("validate"))?;
        publish(&temporary, &job.destination)?;
        pp_os::sync_directory(parent)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Publishes `temporary` as the new file `destination`. An exclusive create
/// reserves the name, then a rename replaces the empty reservation. Unlike a
/// hard link, this works on exFAT, FAT32 and SMB backup drives.
fn publish(temporary: &Path, destination: &Path) -> Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .with_context(|| {
            format!(
                "Backup destination must be a new file: {}",
                destination.display()
            )
        })?;
    pp_os::publish_rename(temporary, destination).map_err(|error| {
        let _ = std::fs::remove_file(destination);
        anyhow::Error::new(error).context(format!(
            "Publish backup archive to {}",
            destination.display()
        ))
    })
}

/// Node's archive entry guard: the path is relative, `/`-separated, has no
/// empty, `.` or `..` component, belongs to a core file or declared root
/// family, has the root's type and appears once.
fn checked_entry_path(
    raw: &[u8],
    entry_type: EntryType,
    seen: &mut BTreeSet<String>,
) -> Result<String> {
    let raw = std::str::from_utf8(raw).map_err(|_| anyhow!("Unsafe backup entry path"))?;
    let path = raw.trim_end_matches('/');
    let drive = path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic();
    ensure!(
        !path.is_empty()
            && !raw.contains('\\')
            && !raw.contains('\0')
            && !raw.starts_with('/')
            && !drive
            && path
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
        "Unsafe backup entry path: {raw}"
    );
    let file = entry_type == EntryType::Regular;
    if CORE_FILES.contains(&path) {
        ensure!(file, "Backup root entry must be a regular file: {raw}");
    } else {
        let (root, kind) =
            root_for(path).ok_or_else(|| anyhow!("Unexpected backup entry: {raw}"))?;
        match kind {
            RootKind::File => ensure!(file, "Backup root entry must be a regular file: {raw}"),
            RootKind::Directory => {
                ensure!(
                    file || entry_type == EntryType::Directory,
                    "Unsupported backup entry type for {raw}: {entry_type:?}"
                );
                ensure!(
                    path != root || entry_type == EntryType::Directory,
                    "Backup directory root has invalid type: {raw}"
                );
            }
        }
    }
    ensure!(
        seen.insert(path.to_owned()),
        "Duplicate backup entry: {raw}"
    );
    Ok(path.to_owned())
}

/// Validates entries, metadata and SQLite integrity without touching live
/// data. Only the metadata, database and WAL are extracted, to a private
/// temporary directory.
pub fn validate(archive: &Path, limits: Limits) -> Result<Metadata> {
    let scratch = std::env::temp_dir().join(random_name("pp-backup-validate-"));
    validate_in(archive, limits, &scratch)
}

/// [`validate`] with its extraction in `scratch`, a new directory this
/// creates and removes.
fn validate_in(archive: &Path, limits: Limits, scratch: &Path) -> Result<Metadata> {
    let compressed = archive
        .metadata()
        .with_context(|| format!("Open backup archive {}", archive.display()))?
        .len();
    pp_os::create_private_dir(scratch)?;
    let result = validate_into(archive, compressed, limits, scratch, |_| false);
    let _ = std::fs::remove_dir_all(scratch);
    Ok(result?.0)
}

/// The decompressed bytes an archive's tar headers declare, read without
/// extracting anything. Staging checks free space against it, then extracts
/// with it as [`Limits::max_total_bytes`].
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn declared_bytes(archive: &Path, limits: Limits) -> Result<u64> {
    let compressed = archive
        .metadata()
        .with_context(|| format!("Open backup archive {}", archive.display()))?
        .len();
    let mut reader = tar::Archive::new(GzDecoder::new(BufReader::new(File::open(archive)?)));
    let mut tally = Tally::default();
    for entry in reader.entries()? {
        let entry = entry?;
        if !extended_header(entry.header().entry_type()) {
            tally.add(entry.size(), compressed, limits)?;
        }
    }
    Ok(tally.total)
}

fn extended_header(entry_type: EntryType) -> bool {
    matches!(entry_type, EntryType::XGlobalHeader | EntryType::XHeader)
}

/// Entries and declared bytes counted so far, checked against [`Limits`].
#[derive(Default)]
struct Tally {
    entries: u64,
    total: u64,
}

impl Tally {
    fn add(&mut self, size: u64, compressed: u64, limits: Limits) -> Result<()> {
        self.entries += 1;
        ensure!(
            self.entries <= limits.max_entries,
            "Backup archive entry limit exceeded ({})",
            limits.max_entries
        );
        ensure!(
            size <= limits.max_entry_bytes,
            "Backup entry exceeds decompressed size limit"
        );
        self.total = self.total.saturating_add(size);
        ensure!(
            self.total <= limits.max_total_bytes,
            TotalExceeded {
                limit: limits.max_total_bytes
            }
        );
        ensure!(
            self.total
                <= compressed
                    .max(1)
                    .saturating_mul(limits.max_decompression_ratio),
            "Backup archive exceeds the decompression ratio limit ({})",
            limits.max_decompression_ratio
        );
        Ok(())
    }
}

/// Validates like [`validate`] and writes every entry under `destination`,
/// a new empty directory, for a whole-installation restore to stage. The
/// entry guard admits only regular files and directories under the core
/// files and declared roots, so nothing is written outside `destination`
/// and no link is created.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn extract(archive: &Path, limits: Limits, destination: &Path) -> Result<Metadata> {
    let compressed = archive
        .metadata()
        .with_context(|| format!("Open backup archive {}", archive.display()))?
        .len();
    Ok(validate_into(archive, compressed, limits, destination, |_| true)?.0)
}

/// Validates like [`validate`] and writes the database, its WAL and the
/// single-file roots under `destination`, a new empty directory. Returns the
/// path of every entry, so an import dry run can count the files under the
/// directory roots without writing them.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn extract_documents(
    archive: &Path,
    limits: Limits,
    destination: &Path,
) -> Result<(Metadata, BTreeSet<String>)> {
    let compressed = archive
        .metadata()
        .with_context(|| format!("Open backup archive {}", archive.display()))?
        .len();
    validate_into(archive, compressed, limits, destination, |path| {
        root_for(path).is_some_and(|(_, kind)| kind == RootKind::File)
    })
}

/// Checks what follows the tar end-of-archive block. Tar iteration stops at
/// that block, so this reads the gzip member to its end, which makes the
/// decoder check the CRC and length trailer. Only zero padding (at most
/// [`MAX_TRAILING_BYTES`]) may follow the tar end: Rust's `tar` crate and
/// Node's `tar` both end with zero blocks. Both exporters (Rust here, Node's
/// `tar.c({ gzip: true })`) write exactly one gzip member, so any byte in
/// the file after it, including a second member, refuses the archive: a
/// second member could carry entries that tar iteration never saw.
fn check_stream_end(mut decoder: GzDecoder<BufReader<File>>) -> Result<()> {
    let mut rest = (&mut decoder).take(MAX_TRAILING_BYTES + 1);
    let (mut trailing, mut block) = (0u64, [0u8; 8192]);
    loop {
        let read = rest
            .read(&mut block)
            .context("Backup archive gzip stream is corrupt")?;
        if read == 0 {
            break;
        }
        trailing += read as u64;
        ensure!(
            block[..read].iter().all(|byte| *byte == 0),
            "Backup archive has non-zero data after the tar end"
        );
    }
    ensure!(
        trailing <= MAX_TRAILING_BYTES,
        "Backup archive has more than {MAX_TRAILING_BYTES} bytes after the tar end"
    );
    ensure!(
        decoder.into_inner().fill_buf()?.is_empty(),
        "Backup archive has data after its gzip stream"
    );
    Ok(())
}

fn validate_into(
    archive: &Path,
    compressed: u64,
    limits: Limits,
    scratch: &Path,
    extract: fn(&str) -> bool,
) -> Result<(Metadata, BTreeSet<String>)> {
    // `bufread::GzDecoder` reads exactly one gzip member from the `BufReader`
    // and leaves anything after it there, so `check_stream_end` can see it.
    let mut reader = tar::Archive::new(GzDecoder::new(BufReader::new(File::open(archive)?)));
    let mut seen = BTreeSet::new();
    let mut tally = Tally::default();
    let mut metadata = None;
    for entry in reader.entries()? {
        let mut entry = entry?;
        let entry_type = entry.header().entry_type();
        if extended_header(entry_type) {
            continue;
        }
        let size = entry.size();
        tally.add(size, compressed, limits)?;
        let path = checked_entry_path(&entry.path_bytes(), entry_type, &mut seen)?;
        if path == METADATA_FILE {
            ensure!(
                size <= MAX_METADATA_BYTES,
                "Backup metadata exceeds {MAX_METADATA_BYTES} byte limit"
            );
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            metadata = Some(bytes);
        } else if entry_type == EntryType::Directory {
            if extract(&path) {
                std::fs::create_dir_all(scratch.join(&path))?;
            }
        } else if path == DATABASE_FILE || path == WAL_FILE || extract(&path) {
            let target = scratch.join(&path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(target)?;
            std::io::copy(&mut entry, &mut output)?;
        } else {
            std::io::copy(&mut entry, &mut std::io::sink())?;
        }
    }
    check_stream_end(reader.into_inner())?;
    let metadata = metadata.ok_or_else(|| anyhow!("Backup archive is missing {METADATA_FILE}"))?;
    ensure!(
        seen.contains(DATABASE_FILE),
        "Backup archive is missing {DATABASE_FILE}"
    );
    let metadata = Metadata::parse(&metadata)?;
    let included = metadata.included_roots();
    for path in &seen {
        if let Some((root, _)) = root_for(path) {
            ensure!(
                included.contains(&root),
                "Backup entry is outside the declared scope: {path}"
            );
        }
    }
    let database = scratch.join(DATABASE_FILE);
    let mut header = [0u8; 16];
    match File::open(&database)?.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Err(error) => return Err(error.into()),
    }
    ensure!(
        header == SQLITE_HEADER,
        "Backup contains an invalid SQLite database"
    );
    let integrity = (|| -> rusqlite::Result<String> {
        let check = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        check.pragma_update(None, "query_only", true)?;
        check.query_row("PRAGMA integrity_check", [], |row| row.get(0))
    })()
    .context("Backup SQLite integrity_check failed")?;
    ensure!(
        integrity == "ok",
        "Backup SQLite integrity_check failed: SQLite integrity_check returned: {integrity}"
    );
    Ok((metadata, seen))
}

#[cfg(all(test, unix))]
mod tests;
