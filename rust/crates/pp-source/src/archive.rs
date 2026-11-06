use crate::{
    Directory, LocalFiles, PathCollisions, SourceLockGuard, SourcePath, SourceRoot, compare_paths,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fmt,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
#[cfg(feature = "desktop_rebuild")]
use unicode_normalization::UnicodeNormalization;

const CANDIDATE: &str = ".candidate";
pub const MAX_COMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_INFLATED_BYTES: u64 = 1024 * 1024 * 1024;
pub const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
/// Each file entry may inflate to at most this multiple of its compressed size.
pub const MAX_COMPRESSION_RATIO: u64 = 100;
/// Entries up to this many inflated bytes are exempt from the ratio cap, so
/// small highly compressible files (padding, empty meshes) stay valid.
pub const COMPRESSION_RATIO_FLOOR_BYTES: u64 = 1024 * 1024;

#[derive(Debug)]
pub enum ArchiveError {
    Source(crate::Error),
    Zip(zip::result::ZipError),
    InvalidArchive,
    /// An entry's extra-field records are truncated, overrun their declared
    /// length, or carry a ZIP64 field too short for the sizes it must replace.
    MalformedExtraField,
    /// The opened ZIP reader indexed different metadata than preflight checked.
    ReaderMismatch(ReaderMismatch),
    UnsafeEntry,
    DuplicateEntry,
    Unsupported,
    Limit,
    CompressionRatio,
    Cancelled,
    Busy,
    Cleanup {
        operation: Box<ArchiveError>,
        cleanup: Box<ArchiveError>,
    },
}
impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(e) => write!(f, "{e}"),
            Self::Zip(e) => write!(f, "invalid-archive: {e}"),
            Self::InvalidArchive => write!(f, "invalid-archive"),
            Self::MalformedExtraField => write!(f, "invalid-archive: malformed extra field"),
            Self::ReaderMismatch(field) => {
                write!(f, "invalid-archive: reader {field} differs from preflight")
            }
            Self::UnsafeEntry => write!(f, "unsafe-entry"),
            Self::DuplicateEntry => write!(f, "duplicate-entry"),
            Self::Unsupported => write!(f, "unsupported-archive"),
            Self::Limit => write!(f, "archive-limit"),
            Self::CompressionRatio => write!(f, "compression-ratio"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Busy => write!(f, "archive-busy"),
            Self::Cleanup { operation, cleanup } => write!(f, "{operation}; cleanup: {cleanup}"),
        }
    }
}
impl std::error::Error for ArchiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::Zip(error) => Some(error),
            Self::Cleanup { operation, .. } => Some(operation),
            _ => None,
        }
    }
}
impl From<crate::Error> for ArchiveError {
    fn from(e: crate::Error) -> Self {
        Self::Source(e)
    }
}
impl From<io::Error> for ArchiveError {
    fn from(e: io::Error) -> Self {
        Self::Source(e.into())
    }
}
impl From<zip::result::ZipError> for ArchiveError {
    fn from(e: zip::result::ZipError) -> Self {
        Self::Zip(e)
    }
}
type Result<T> = std::result::Result<T, ArchiveError>;

/// How ZIP entry labels become destination paths. Raw archive names stay unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveLabelPolicy {
    /// Requires labels to satisfy `SourcePath` without normalization.
    Strict,
    /// Normalizes destination labels to NFC before validation.
    #[cfg(feature = "desktop_rebuild")]
    NfcAtAcquisition,
}

/// Which reader-resolved value disagreed with the preflight record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderMismatch {
    ArchiveOffset,
    CentralDirectoryStart,
    Comment,
    EntryCount,
    CentralHeaderOffset,
    LocalHeaderOffset,
    EntryMetadata,
}
impl fmt::Display for ReaderMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ArchiveOffset => "archive offset",
            Self::CentralDirectoryStart => "central directory start",
            Self::Comment => "archive comment",
            Self::EntryCount => "entry count",
            Self::CentralHeaderOffset => "central header offset",
            Self::LocalHeaderOffset => "local header offset",
            Self::EntryMetadata => "entry metadata",
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArchiveLimits {
    pub max_compressed_bytes: u64,
    pub max_inflated_bytes: u64,
    pub max_entries: usize,
}
impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_compressed_bytes: MAX_COMPRESSED_BYTES,
            max_inflated_bytes: MAX_INFLATED_BYTES,
            max_entries: 10_000,
        }
    }
}
pub struct ZipInput<R = File>(R);
impl ZipInput {
    pub fn open(files: &LocalFiles, path: &SourcePath) -> Result<Self> {
        Ok(Self(files.root.file(path.as_str(), false)?))
    }
    /// Wraps the pinned input file without changing which file it reads, so a
    /// caller can observe or pace the copy.
    pub fn map_reader<R: Read + Seek>(self, wrap: impl FnOnce(File) -> R) -> ZipInput<R> {
        ZipInput(wrap(self.0))
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedFile {
    pub path: SourcePath,
    pub size_bytes: u64,
    pub sha256: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveReceipt {
    pub tenant_id: String,
    pub source_id: u64,
    pub compressed_bytes: u64,
    pub archive_sha256: String,
    pub inflated_bytes: u64,
    pub entry_count: usize,
    pub stl_count: usize,
    pub files: Vec<ExtractedFile>,
    pub directories: Vec<SourcePath>,
}

struct Stage {
    parent: Directory,
    root: Directory,
    cleaned: bool,
    // The per-Source revisions lock shared with snapshot materialization, so
    // a live archive stage and a peer's snapshot candidate never coexist.
    // Dropped after `Drop::drop` removes the stage, so the lock outlives it.
    _lock: SourceLockGuard,
}
impl Stage {
    fn cleanup(&mut self) -> Result<()> {
        self.parent.remove_tree(CANDIDATE)?;
        self.parent.sync()?;
        self.cleaned = true;
        Ok(())
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.cleanup();
        }
    }
}
/// Owned extraction readiness. It holds the Source's revisions lock until
/// consumed: the originating `SourceRoot` may materialize from it, while other
/// handles block in `materialize` or get `archive-busy` from `extract_zip`.
/// Prefer `discard()`, which reports cleanup errors; dropping also cleans up
/// but cannot report failure.
pub struct ExtractedArchive {
    stage: Stage,
    files: LocalFiles,
    receipt: ArchiveReceipt,
}
impl ExtractedArchive {
    pub fn files(&self) -> &LocalFiles {
        &self.files
    }
    pub fn receipt(&self) -> &ArchiveReceipt {
        &self.receipt
    }
    pub fn discard(mut self) -> Result<()> {
        self.stage.cleanup()
    }
}

impl SourceRoot {
    /// Extracts using strict `SourcePath` labels without normalization.
    pub fn extract_zip(
        &mut self,
        input: ZipInput<impl Read + Seek>,
        limits: ArchiveLimits,
        cancelled: &AtomicBool,
    ) -> Result<ExtractedArchive> {
        self.extract_zip_impl(input, limits, ArchiveLabelPolicy::Strict, cancelled)
    }

    /// Applies the label policy before path validation and collision checks.
    /// Payload bytes and the original input's receipt hash remain unchanged.
    #[cfg(feature = "desktop_rebuild")]
    pub fn extract_zip_with_label_policy(
        &mut self,
        input: ZipInput<impl Read + Seek>,
        limits: ArchiveLimits,
        label_policy: ArchiveLabelPolicy,
        cancelled: &AtomicBool,
    ) -> Result<ExtractedArchive> {
        self.extract_zip_impl(input, limits, label_policy, cancelled)
    }

    fn extract_zip_impl(
        &mut self,
        mut input: ZipInput<impl Read + Seek>,
        limits: ArchiveLimits,
        label_policy: ArchiveLabelPolicy,
        cancelled: &AtomicBool,
    ) -> Result<ExtractedArchive> {
        if limits.max_compressed_bytes > MAX_COMPRESSED_BYTES
            || limits.max_inflated_bytes > MAX_INFLATED_BYTES
            || limits.max_entries > 10_000
        {
            return Err(ArchiveError::Limit);
        }
        check_cancel(cancelled)?;
        // This handle's own materialize joins the hold instead of waiting.
        let lock = self.lock.try_acquire_alone()?.ok_or(ArchiveError::Busy)?;
        let parent = self.revisions.child(".pp-source-archives", true)?;
        parent.remove_tree(CANDIDATE)?;
        let root = match parent.child(CANDIDATE, true) {
            Ok(root) => root,
            Err(error) => {
                // Creation can fail after mkdir (e.g. parent fsync); never
                // leave the candidate behind.
                let _ = parent.remove_tree(CANDIDATE);
                return Err(error.into());
            }
        };
        let mut stage = Stage {
            parent,
            root,
            cleaned: false,
            _lock: lock,
        };
        let outcome = self.extract_to(&mut input, limits, label_policy, cancelled, &stage);
        match outcome {
            Ok((files, receipt)) => Ok(ExtractedArchive {
                stage,
                files,
                receipt,
            }),
            Err(operation) => match stage.cleanup() {
                Ok(()) => Err(operation),
                Err(cleanup) => Err(ArchiveError::Cleanup {
                    operation: Box::new(operation),
                    cleanup: Box::new(cleanup),
                }),
            },
        }
    }
    fn extract_to(
        &self,
        input: &mut ZipInput<impl Read + Seek>,
        limits: ArchiveLimits,
        label_policy: ArchiveLabelPolicy,
        cancelled: &AtomicBool,
        stage: &Stage,
    ) -> Result<(LocalFiles, ArchiveReceipt)> {
        input.0.seek(SeekFrom::Start(0))?;
        let mut compressed = stage.root.file("archive.zip", true)?;
        let (compressed_bytes, archive_sha256) = transfer(
            &mut input.0,
            &mut compressed,
            limits.max_compressed_bytes,
            cancelled,
        )?;
        compressed.sync_all()?;
        let mut archive_file = stage.root.file_read_write("archive.zip")?;
        let preflight = inspect_index_with_label_policy(
            &mut archive_file,
            compressed_bytes,
            limits,
            label_policy,
            cancelled,
        )?;
        bind_zip_reader_to_validated_eocd(&mut archive_file, preflight.eocd_offset)?;
        let mut archive = zip::ZipArchive::with_config(
            zip::read::Config {
                archive_offset: zip::read::ArchiveOffset::Known(0),
            },
            archive_file,
        )?;
        verify_reader_archive(&archive, &preflight)?;
        let entries = &preflight.entries;
        let files = LocalFiles {
            root: stage.root.child("files", true)?,
        };
        let mut receipt = ArchiveReceipt {
            tenant_id: self.tenant_id.clone(),
            source_id: self.source_id,
            compressed_bytes,
            archive_sha256,
            inflated_bytes: 0,
            entry_count: entries.len(),
            stl_count: 0,
            files: vec![],
            directories: vec![],
        };
        let mut directories = BTreeSet::new();
        for (index, entry) in entries.iter().enumerate() {
            check_cancel(cancelled)?;
            let mut member = archive.by_index(index)?;
            verify_reader_entry(&member, entry)?;
            let path = entry.path.as_str();
            let mut parent = path;
            while let Some((prefix, _)) = parent.rsplit_once('/') {
                directories.insert(prefix.to_owned());
                parent = prefix;
            }
            if entry.directory {
                if member.size() != 0 {
                    return Err(ArchiveError::InvalidArchive);
                }
                let (parent, name) = files.root.parent(path, true)?;
                parent.child(&name, true)?;
                directories.insert(path.to_owned());
                let (size, _) = transfer(&mut member, io::sink(), 0, cancelled)?;
                if size != 0 {
                    return Err(ArchiveError::InvalidArchive);
                }
            } else {
                let mut destination = files.root.file(path, true)?;
                let remaining = limits.max_inflated_bytes - receipt.inflated_bytes;
                let ratio_limit = ratio_limit(entry.compressed_bytes);
                let (size_bytes, sha256) = match transfer(
                    &mut member,
                    &mut destination,
                    remaining.min(ratio_limit),
                    cancelled,
                ) {
                    Err(ArchiveError::Limit) if ratio_limit < remaining => {
                        return Err(ArchiveError::CompressionRatio);
                    }
                    result => result?,
                };
                if size_bytes != member.size() {
                    return Err(ArchiveError::InvalidArchive);
                }
                receipt.inflated_bytes += size_bytes;
                if path.to_ascii_lowercase().ends_with(".stl") {
                    receipt.stl_count += 1;
                }
                receipt.files.push(ExtractedFile {
                    path: entry.path.clone(),
                    size_bytes,
                    sha256,
                });
            }
        }
        receipt
            .files
            .sort_by(|a, b| compare_paths(a.path.as_str(), b.path.as_str()));
        receipt.directories = directories
            .into_iter()
            .map(SourcePath::try_from)
            .collect::<std::result::Result<_, _>>()?;
        receipt
            .directories
            .sort_by(|a, b| compare_paths(a.as_str(), b.as_str()));
        check_cancel(cancelled)?;
        Ok((files, receipt))
    }
}

pub(crate) fn check_cancel(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(ArchiveError::Cancelled)
    } else {
        Ok(())
    }
}
pub(crate) fn transfer(
    mut input: impl Read,
    mut output: impl Write,
    limit: u64,
    cancelled: &AtomicBool,
) -> Result<(u64, String)> {
    let mut buffer = [0u8; 64 * 1024];
    let mut bytes = 0u64;
    let mut hash = Sha256::new();
    loop {
        check_cancel(cancelled)?;
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes = bytes.checked_add(n as u64).ok_or(ArchiveError::Limit)?;
        if bytes > limit {
            return Err(ArchiveError::Limit);
        }
        output.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
    }
    Ok((bytes, hex::encode(hash.finalize())))
}
pub(crate) struct Entry {
    pub(crate) raw_name: String,
    path: SourcePath,
    directory: bool,
    compressed_bytes: u64,
    pub(crate) inflated_bytes: u64,
    central_header_offset: u64,
    pub(crate) local_header_offset: u64,
}
pub(crate) struct Preflight {
    pub(crate) entries: Vec<Entry>,
    central_offset: u64,
    eocd_offset: u64,
}
struct ResolvedZip64 {
    uncompressed_bytes: u64,
    compressed_bytes: u64,
    local_header_offset: u64,
    disk: u64,
}
struct Zip64Directory {
    total_entries: u64,
    central_size: u64,
    central_offset: u64,
    record_offset: u64,
}

/// The ZIP crate falls back to earlier end records when the validated central
/// directory fails its own parse, so the opened reader must be proven to index
/// exactly the directory preflight walked.
fn verify_reader_archive<R: Read + Seek>(
    archive: &zip::ZipArchive<R>,
    preflight: &Preflight,
) -> Result<()> {
    let mismatch = if archive.offset() != 0 {
        ReaderMismatch::ArchiveOffset
    } else if archive.central_directory_start() != preflight.central_offset {
        ReaderMismatch::CentralDirectoryStart
    } else if !archive.comment().is_empty() {
        ReaderMismatch::Comment
    } else if archive.len() != preflight.entries.len() {
        ReaderMismatch::EntryCount
    } else {
        return Ok(());
    };
    Err(ArchiveError::ReaderMismatch(mismatch))
}

/// ZIP64 extra fields may replace sizes and the local header offset; the ratio
/// cap, limits and local-header validation only hold for preflight's values.
fn verify_reader_entry<R: Read + ?Sized>(
    member: &zip::read::ZipFile<'_, R>,
    entry: &Entry,
) -> Result<()> {
    let mismatch = if member.central_header_start() != entry.central_header_offset {
        ReaderMismatch::CentralHeaderOffset
    } else if member.header_start() != entry.local_header_offset {
        ReaderMismatch::LocalHeaderOffset
    } else if member.name_raw() != entry.raw_name.as_bytes()
        || member.compressed_size() != entry.compressed_bytes
        || member.size() != entry.inflated_bytes
    {
        ReaderMismatch::EntryMetadata
    } else {
        return Ok(());
    };
    Err(ArchiveError::ReaderMismatch(mismatch))
}

/// Walks central extra-field records the way zip 9 parses them and rejects
/// anything it would fail on, so the reader never abandons the validated
/// directory for an earlier end record.
fn validate_extra_field(extra: &[u8], central: &[u8; 46]) -> Result<ResolvedZip64> {
    let original = [
        u64::from(u32_at(central, 24)?),
        u64::from(u32_at(central, 20)?),
        u64::from(u32_at(central, 42)?),
    ];
    let ([uncompressed_bytes, compressed_bytes, local_header_offset], disk) =
        absorb_zip64(extra, original, u64::from(u16_at(central, 34)?))?;
    Ok(ResolvedZip64 {
        uncompressed_bytes,
        compressed_bytes,
        local_header_offset,
        disk,
    })
}

/// Walks extra-field records the way zip 9 parses them. A ZIP64 record's
/// length decides which values it must carry, but a value replaces the
/// header's only where the header field holds the sentinel, so a full record
/// cannot rewrite the sizes and offset the ratio cap and local-header checks
/// trusted. Each record is read against the header's own fields, and the last
/// one wins.
fn absorb_zip64(extra: &[u8], original: [u64; 3], original_disk: u64) -> Result<([u64; 3], u64)> {
    let mut values = original;
    let mut disk = original_disk;
    let mut rest = extra;
    while !rest.is_empty() {
        if rest.len() < 4 {
            return Err(ArchiveError::MalformedExtraField);
        }
        let id = u16_at(rest, 0)?;
        let len = u16_at(rest, 2)? as usize;
        let Some(data) = rest.get(4..4 + len) else {
            return Err(ArchiveError::MalformedExtraField);
        };
        match id {
            0x0001 => {
                let mut cursor = 0;
                for (value, original) in values.iter_mut().zip(original) {
                    let saturated = original == u64::from(u32::MAX);
                    if len >= 24 || saturated {
                        let Some(bytes) = data.get(cursor..cursor + 8) else {
                            return Err(ArchiveError::MalformedExtraField);
                        };
                        if saturated {
                            let mut wide = [0u8; 8];
                            wide.copy_from_slice(bytes);
                            *value = u64::from_le_bytes(wide);
                        }
                        cursor += 8;
                    }
                }
                if original_disk == u64::from(u16::MAX) {
                    let Some(bytes) = data.get(cursor..cursor + 4) else {
                        return Err(ArchiveError::MalformedExtraField);
                    };
                    let mut narrow = [0u8; 4];
                    narrow.copy_from_slice(bytes);
                    disk = u64::from(u32::from_le_bytes(narrow));
                }
            }
            0x000a => {
                if len != 32 || u16_at(data, 4)? != 1 || u16_at(data, 6)? != 24 {
                    return Err(ArchiveError::MalformedExtraField);
                }
            }
            0x5455 => {
                if len == 0 || (len != 5 && len != 1 + 4 * data[0].count_ones() as usize) {
                    return Err(ArchiveError::MalformedExtraField);
                }
            }
            // Unicode path/comment fields rename entries and AES is
            // encryption; neither is supported.
            0x6375 | 0x7075 | 0x9901 => return Err(ArchiveError::Unsupported),
            _ => {}
        }
        rest = &rest[4 + len..];
    }
    Ok((values, disk))
}
fn ratio_limit(compressed_bytes: u64) -> u64 {
    compressed_bytes
        .saturating_mul(MAX_COMPRESSION_RATIO)
        .max(COMPRESSION_RATIO_FLOOR_BYTES)
}
fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    bytes
        .get(offset..offset + 2)
        .and_then(|field| field.try_into().ok())
        .map(u16::from_le_bytes)
        .ok_or(ArchiveError::InvalidArchive)
}
fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    bytes
        .get(offset..offset + 4)
        .and_then(|field| field.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or(ArchiveError::InvalidArchive)
}
const ZIP32_EOCD_SIGNATURE: &[u8] = b"PK\x05\x06";
const ZIP64_EOCD_SIGNATURE: &[u8] = b"PK\x06\x06";
const ZIP64_EOCD_LOCATOR_SIGNATURE: &[u8] = b"PK\x06\x07";

fn comment_contains_zip_end_record(comment: &[u8]) -> bool {
    for signature in [
        ZIP32_EOCD_SIGNATURE,
        ZIP64_EOCD_SIGNATURE,
        ZIP64_EOCD_LOCATOR_SIGNATURE,
    ] {
        if comment
            .windows(signature.len())
            .any(|window| window == signature)
        {
            return true;
        }
    }
    false
}

/// Truncate the staged copy at the validated EOCD and clear its comment length so
/// the ZIP crate cannot index a decoy end record embedded in the archive comment.
fn bind_zip_reader_to_validated_eocd(file: &mut File, eocd_offset: u64) -> Result<()> {
    file.seek(SeekFrom::Start(eocd_offset + 20))?;
    file.write_all(&[0u8; 2])?;
    file.set_len(eocd_offset + 22)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}

static BOUND_COPIES: AtomicU64 = AtomicU64::new(0);

/// Private file that receives the validated archive prefix. Binding rewrites the
/// EOCD, so this must never be the caller's 3MF or a published original.
struct BoundCopy {
    path: PathBuf,
    file: Option<File>,
}
impl BoundCopy {
    fn create() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "pp-source-bind-{}-{}",
            std::process::id(),
            BOUND_COPIES.fetch_add(1, Ordering::Relaxed)
        ));
        let file = pp_os::create_new_private_file(&path)?;
        Ok(Self {
            path,
            file: Some(file),
        })
    }
}
impl Drop for BoundCopy {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

fn local_member_span(
    source: &mut (impl Read + Seek),
    entry: &Entry,
    central_offset: u64,
) -> Result<(u64, u64)> {
    source.seek(SeekFrom::Start(entry.local_header_offset))?;
    let mut local = [0u8; 30];
    source.read_exact(&mut local)?;
    if local[..4] != *b"PK\x03\x04" {
        return Err(ArchiveError::InvalidArchive);
    }
    let name_len = u64::from(u16_at(&local, 26)?);
    let extra_len = u64::from(u16_at(&local, 28)?);
    let payload_end = entry
        .local_header_offset
        .checked_add(30)
        .and_then(|n| n.checked_add(name_len))
        .and_then(|n| n.checked_add(extra_len))
        .and_then(|n| n.checked_add(entry.compressed_bytes))
        .ok_or(ArchiveError::InvalidArchive)?;
    if payload_end > central_offset {
        return Err(ArchiveError::InvalidArchive);
    }
    let descriptor = if u16_at(&local, 6)? & 8 != 0 {
        (central_offset - payload_end).min(24)
    } else {
        0
    };
    Ok((
        entry.local_header_offset,
        payload_end - entry.local_header_offset + descriptor,
    ))
}

fn copy_span(
    source: &mut (impl Read + Seek),
    dest: &mut (impl Write + Seek),
    offset: u64,
    len: u64,
    cancelled: &AtomicBool,
) -> Result<()> {
    source.seek(SeekFrom::Start(offset))?;
    dest.seek(SeekFrom::Start(offset))?;
    let mut remaining = len;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        check_cancel(cancelled)?;
        let want = remaining.min(buffer.len() as u64) as usize;
        source.read_exact(&mut buffer[..want])?;
        dest.write_all(&buffer[..want])?;
        remaining -= want as u64;
    }
    Ok(())
}

/// Copy the central directory and the selected member into a private file, bind
/// that EOCD, and return the member bytes only after the opened reader matches
/// the preflight record. A ZIP64 extra that redirects the local header is
/// rejected before those bytes are returned. Sibling members stay unread.
pub(crate) fn read_bound_entry(
    source: &mut (impl Read + Seek),
    preflight: &Preflight,
    index: usize,
    limit: u64,
    cancelled: &AtomicBool,
) -> Result<Vec<u8>> {
    let entry = preflight
        .entries
        .get(index)
        .ok_or(ArchiveError::InvalidArchive)?;
    let prefix = preflight
        .eocd_offset
        .checked_add(22)
        .ok_or(ArchiveError::InvalidArchive)?;
    if preflight.central_offset > prefix {
        return Err(ArchiveError::InvalidArchive);
    }
    let mut bound = BoundCopy::create()?;
    let mut file = bound.file.take().ok_or(ArchiveError::InvalidArchive)?;
    file.set_len(prefix)?;
    copy_span(
        source,
        &mut file,
        preflight.central_offset,
        prefix - preflight.central_offset,
        cancelled,
    )?;
    let (local_offset, local_len) = local_member_span(source, entry, preflight.central_offset)?;
    copy_span(source, &mut file, local_offset, local_len, cancelled)?;
    bind_zip_reader_to_validated_eocd(&mut file, preflight.eocd_offset)?;
    let mut archive = zip::ZipArchive::with_config(
        zip::read::Config {
            archive_offset: zip::read::ArchiveOffset::Known(0),
        },
        file,
    )?;
    verify_reader_archive(&archive, preflight)?;
    let mut member = archive.by_index(index)?;
    verify_reader_entry(&member, entry)?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_cancel(cancelled)?;
        let n = member.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let next = (bytes.len() as u64)
            .checked_add(n as u64)
            .ok_or(ArchiveError::Limit)?;
        if next > limit {
            return Err(ArchiveError::Limit);
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    Ok(bytes)
}

pub(crate) fn inspect_index(
    file: &mut (impl Read + Seek),
    length: u64,
    limits: ArchiveLimits,
    cancelled: &AtomicBool,
) -> Result<Preflight> {
    inspect_index_with_label_policy(file, length, limits, ArchiveLabelPolicy::Strict, cancelled)
}

fn inspect_index_with_label_policy(
    file: &mut (impl Read + Seek),
    length: u64,
    limits: ArchiveLimits,
    label_policy: ArchiveLabelPolicy,
    cancelled: &AtomicBool,
) -> Result<Preflight> {
    let tail_size = length.min(65_557) as usize;
    file.seek(SeekFrom::End(-(tail_size as i64)))?;
    let mut tail = vec![0u8; tail_size];
    file.read_exact(&mut tail)?;
    let tail_start = length - tail_size as u64;
    let mut chosen = None;
    for i in (0..tail_size.saturating_sub(21)).rev() {
        if tail[i..i + 4] != *ZIP32_EOCD_SIGNATURE {
            continue;
        }
        let comment_len = u16_at(&tail, i + 20)? as usize;
        if i + 22 + comment_len != tail_size {
            continue;
        }
        let comment = &tail[i + 22..i + 22 + comment_len];
        if comment_contains_zip_end_record(comment) {
            return Err(ArchiveError::InvalidArchive);
        }
        if chosen.is_some() {
            return Err(ArchiveError::InvalidArchive);
        }
        let mut footer = [0u8; 22];
        footer.copy_from_slice(&tail[i..i + 22]);
        chosen = Some((footer, tail_start + i as u64));
    }
    let Some((footer, eocd_offset)) = chosen else {
        return Err(ArchiveError::InvalidArchive);
    };
    let mut count = u64::from(u16_at(&footer, 10)?);
    let mut central_size = u64::from(u32_at(&footer, 12)?);
    let mut central_offset = u64::from(u32_at(&footer, 16)?);
    let disk = u16_at(&footer, 4)?;
    let start_disk = u16_at(&footer, 6)?;
    let entries_on_disk = u64::from(u16_at(&footer, 8)?);
    if (disk != 0 && disk != u16::MAX)
        || (start_disk != 0 && start_disk != u16::MAX)
        || entries_on_disk != count
    {
        return Err(ArchiveError::Unsupported);
    }
    if let Some(zip64) = read_zip64_directory(file, eocd_offset)? {
        for (old, sentinel, new) in [
            (count, u64::from(u16::MAX), zip64.total_entries),
            (central_size, u64::from(u32::MAX), zip64.central_size),
            (central_offset, u64::from(u32::MAX), zip64.central_offset),
        ] {
            if old != sentinel && old != new {
                return Err(ArchiveError::InvalidArchive);
            }
        }
        count = zip64.total_entries;
        central_size = zip64.central_size;
        central_offset = zip64.central_offset;
        if central_offset.checked_add(central_size) != Some(zip64.record_offset) {
            return Err(ArchiveError::InvalidArchive);
        }
    } else if disk == u16::MAX
        || start_disk == u16::MAX
        || count == u64::from(u16::MAX)
        || central_size == u64::from(u32::MAX)
        || central_offset == u64::from(u32::MAX)
    {
        return Err(ArchiveError::Unsupported);
    } else if central_offset.checked_add(central_size) != Some(eocd_offset) {
        return Err(ArchiveError::InvalidArchive);
    }
    let count = usize::try_from(count).map_err(|_| ArchiveError::InvalidArchive)?;
    if count > limits.max_entries || central_size > MAX_METADATA_BYTES {
        return Err(ArchiveError::Limit);
    }
    file.seek(SeekFrom::Start(central_offset))?;
    let mut entries = Vec::with_capacity(count);
    let mut paths = PathCollisions::default();
    for _ in 0..count {
        check_cancel(cancelled)?;
        let central_header_offset = file.stream_position()?;
        let mut header = [0u8; 46];
        file.read_exact(&mut header)?;
        if header[..4] != *b"PK\x01\x02" {
            return Err(ArchiveError::InvalidArchive);
        }
        let flags = u16_at(&header, 8)?;
        let compression = u16_at(&header, 10)?;
        let mode = u32_at(&header, 38)? >> 16;
        let disk_start = u16_at(&header, 34)?;
        if flags & (1 | 0x40 | 0x2000) != 0
            || ![0, 8].contains(&compression)
            || (disk_start != 0 && disk_start != u16::MAX)
            || (mode & 0o170000 != 0 && ![0o100000, 0o040000].contains(&(mode & 0o170000)))
        {
            return Err(ArchiveError::Unsupported);
        }
        let name_length = u16_at(&header, 28)? as usize;
        if name_length == 0 || (label_policy == ArchiveLabelPolicy::Strict && name_length > 4097) {
            return Err(ArchiveError::UnsafeEntry);
        }
        let mut name = vec![0; name_length];
        file.read_exact(&mut name)?;
        let raw_name = String::from_utf8(name).map_err(|_| ArchiveError::Unsupported)?;
        if raw_name.chars().any(char::is_control) {
            return Err(ArchiveError::UnsafeEntry);
        }
        let directory = raw_name.ends_with('/');
        let label = if directory {
            &raw_name[..raw_name.len() - 1]
        } else {
            &raw_name
        };
        let path = archive_path(label, label_policy)?;
        paths
            .insert(&path, directory)
            .map_err(|error| match error {
                crate::Error::DuplicatePath => ArchiveError::DuplicateEntry,
                crate::Error::Limit => ArchiveError::Limit,
                other => ArchiveError::Source(other),
            })?;
        let extra_len = u16_at(&header, 30)? as u64;
        let extra_end = file
            .stream_position()?
            .checked_add(extra_len)
            .ok_or(ArchiveError::InvalidArchive)?;
        if extra_end > central_offset + central_size {
            return Err(ArchiveError::InvalidArchive);
        }
        let mut extra = vec![0; extra_len as usize];
        file.read_exact(&mut extra)?;
        // Sizes in the central header are untrusted and may be ZIP64 sentinels.
        // Resolve them before the ratio cap so a saturated marker is not treated
        // as a multi-gigabyte payload.
        let resolved = validate_extra_field(&extra, &header)?;
        if resolved.disk != 0 {
            return Err(ArchiveError::Unsupported);
        }
        if resolved.compressed_bytes > limits.max_compressed_bytes
            || resolved.uncompressed_bytes > limits.max_inflated_bytes
        {
            return Err(ArchiveError::Limit);
        }
        if resolved.uncompressed_bytes > ratio_limit(resolved.compressed_bytes) {
            return Err(ArchiveError::CompressionRatio);
        }
        validate_local_header(file, &header, &raw_name, central_offset, &resolved)?;
        file.seek(SeekFrom::Current(u16_at(&header, 32)? as i64))?;
        if file.stream_position()? > central_offset + central_size {
            return Err(ArchiveError::InvalidArchive);
        }
        entries.push(Entry {
            raw_name,
            path,
            directory,
            compressed_bytes: resolved.compressed_bytes,
            inflated_bytes: resolved.uncompressed_bytes,
            central_header_offset,
            local_header_offset: resolved.local_header_offset,
        });
    }
    if file.stream_position()? != central_offset + central_size {
        return Err(ArchiveError::InvalidArchive);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(Preflight {
        entries,
        central_offset,
        eocd_offset,
    })
}

fn archive_path(raw_label: &str, label_policy: ArchiveLabelPolicy) -> Result<SourcePath> {
    let label = match label_policy {
        ArchiveLabelPolicy::Strict => raw_label.to_owned(),
        #[cfg(feature = "desktop_rebuild")]
        ArchiveLabelPolicy::NfcAtAcquisition => raw_label.nfc().collect(),
    };
    SourcePath::try_from(label)
        .and_then(|path| path.require_portable().map(|()| path))
        .map_err(|_| ArchiveError::UnsafeEntry)
}

fn read_zip64_directory(
    file: &mut (impl Read + Seek),
    eocd_offset: u64,
) -> Result<Option<Zip64Directory>> {
    if eocd_offset < 20 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(eocd_offset - 20))?;
    let mut locator = [0u8; 20];
    file.read_exact(&mut locator)?;
    if locator[..4] != *ZIP64_EOCD_LOCATOR_SIGNATURE {
        return Ok(None);
    }
    if u32_at(&locator, 4)? != 0 || u32_at(&locator, 16)? != 1 {
        return Err(ArchiveError::Unsupported);
    }
    let record_offset = u64_at(&locator, 8)?;
    match record_offset.checked_add(56) {
        Some(end) if end <= eocd_offset - 20 => {}
        _ => return Err(ArchiveError::InvalidArchive),
    }
    file.seek(SeekFrom::Start(record_offset))?;
    let mut record = [0u8; 56];
    file.read_exact(&mut record)?;
    let size = u64_at(&record, 4)?;
    if record[..4] != *ZIP64_EOCD_SIGNATURE
        || !(44..=MAX_METADATA_BYTES).contains(&size)
        || record_offset
            .checked_add(12)
            .and_then(|start| start.checked_add(size))
            != Some(eocd_offset - 20)
    {
        return Err(ArchiveError::InvalidArchive);
    }
    let entries_on_disk = u64_at(&record, 24)?;
    let total_entries = u64_at(&record, 32)?;
    if u32_at(&record, 16)? != 0 || u32_at(&record, 20)? != 0 || entries_on_disk != total_entries {
        return Err(ArchiveError::Unsupported);
    }
    Ok(Some(Zip64Directory {
        total_entries,
        central_size: u64_at(&record, 40)?,
        central_offset: u64_at(&record, 48)?,
        record_offset,
    }))
}

fn validate_local_header(
    file: &mut (impl Read + Seek),
    central: &[u8; 46],
    name: &str,
    central_offset: u64,
    resolved: &ResolvedZip64,
) -> Result<()> {
    let next = file.stream_position()?;
    let local_offset = resolved.local_header_offset;
    if local_offset >= central_offset {
        return Err(ArchiveError::InvalidArchive);
    }
    file.seek(SeekFrom::Start(local_offset))?;
    let mut local = [0u8; 30];
    file.read_exact(&mut local)?;
    if local[..4] != *b"PK\x03\x04"
        || u16_at(&local, 6)? != u16_at(central, 8)?
        || u16_at(&local, 8)? != u16_at(central, 10)?
        || u16_at(&local, 26)? as usize != name.len()
    {
        return Err(ArchiveError::InvalidArchive);
    }
    let descriptor = u16_at(&local, 6)? & 8 != 0;
    if !descriptor && u32_at(&local, 14)? != u32_at(central, 16)? {
        return Err(ArchiveError::InvalidArchive);
    }
    let extra_len = u64::from(u16_at(&local, 28)?);
    let payload_end = local_offset
        .checked_add(30)
        .and_then(|n| n.checked_add(name.len() as u64))
        .and_then(|n| n.checked_add(extra_len))
        .and_then(|n| n.checked_add(resolved.compressed_bytes))
        .ok_or(ArchiveError::InvalidArchive)?;
    if payload_end > central_offset {
        return Err(ArchiveError::InvalidArchive);
    }
    let mut local_name = vec![0; name.len()];
    file.read_exact(&mut local_name)?;
    if local_name != name.as_bytes() {
        return Err(ArchiveError::InvalidArchive);
    }
    let mut extra = vec![0; extra_len as usize];
    file.read_exact(&mut extra)?;
    if !descriptor {
        let local_uncompressed = u64::from(u32_at(&local, 22)?);
        let local_compressed = u64::from(u32_at(&local, 18)?);
        // Opaque local extra bytes are not a second central directory. Only a
        // saturated size is resolved from the local ZIP64 field.
        let (uncompressed, compressed) = if local_uncompressed == u64::from(u32::MAX)
            || local_compressed == u64::from(u32::MAX)
        {
            resolved_local_sizes(&extra, &local)?
        } else {
            (local_uncompressed, local_compressed)
        };
        if (uncompressed, compressed) != (resolved.uncompressed_bytes, resolved.compressed_bytes) {
            return Err(ArchiveError::InvalidArchive);
        }
    }
    if descriptor
        && !data_descriptor_matches(
            file,
            payload_end,
            central_offset,
            &local,
            u32_at(central, 16)?,
            resolved,
        )?
    {
        return Err(ArchiveError::InvalidArchive);
    }
    file.seek(SeekFrom::Start(next))?;
    Ok(())
}

fn resolved_local_sizes(extra: &[u8], local: &[u8; 30]) -> Result<(u64, u64)> {
    let original = [
        u64::from(u32_at(local, 22)?),
        u64::from(u32_at(local, 18)?),
        0,
    ];
    let ([uncompressed, compressed, _], _) = absorb_zip64(extra, original, 0)?;
    Ok((uncompressed, compressed))
}

fn data_descriptor_matches(
    file: &mut (impl Read + Seek),
    payload_end: u64,
    central_offset: u64,
    local: &[u8; 30],
    central_crc: u32,
    resolved: &ResolvedZip64,
) -> Result<bool> {
    file.seek(SeekFrom::Start(payload_end))?;
    let wide = u32_at(local, 18)? == u32::MAX || u32_at(local, 22)? == u32::MAX;
    let width = if wide { 16 } else { 8 };
    let available = (central_offset - payload_end).min(8 + width) as usize;
    let mut bytes = [0u8; 24];
    file.read_exact(&mut bytes[..available])?;
    Ok([4usize, 0].into_iter().any(|prefix| {
        let length = prefix + 4 + width as usize;
        if length > available || (prefix == 4 && bytes[..4] != *b"PK\x07\x08") {
            return false;
        }
        let Ok(crc) = u32_at(&bytes, prefix) else {
            return false;
        };
        let sizes = &bytes[prefix + 4..];
        let parsed = if wide {
            let (Ok(compressed), Ok(uncompressed)) = (u64_at(sizes, 0), u64_at(sizes, 8)) else {
                return false;
            };
            (compressed, uncompressed)
        } else {
            let (Ok(compressed), Ok(uncompressed)) = (u32_at(sizes, 0), u32_at(sizes, 4)) else {
                return false;
            };
            (u64::from(compressed), u64::from(uncompressed))
        };
        crc == central_crc
            && parsed.0 == resolved.compressed_bytes
            && parsed.1 == resolved.uncompressed_bytes
    }))
}

fn u64_at(bytes: &[u8], offset: usize) -> Result<u64> {
    bytes
        .get(offset..offset + 8)
        .and_then(|field| field.try_into().ok())
        .map(u64::from_le_bytes)
        .ok_or(ArchiveError::InvalidArchive)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn central(size: u32, extra: &[u8]) -> Vec<u8> {
        let mut header = Vec::from(*b"PK\x01\x02");
        header.extend_from_slice(&[45, 0, 45, 0]);
        header.extend_from_slice(&[0; 12]);
        header.extend_from_slice(&size.to_le_bytes());
        header.extend_from_slice(&size.to_le_bytes());
        header.extend_from_slice(&8u16.to_le_bytes());
        header.extend_from_slice(&(extra.len() as u16).to_le_bytes());
        header.extend_from_slice(&[0; 14]);
        header.extend_from_slice(b"part.stl");
        header.extend_from_slice(extra);
        header
    }
    fn end_record(offset: usize, size: usize) -> Vec<u8> {
        let mut record = Vec::from(*b"PK\x05\x06");
        record.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
        record.extend_from_slice(&(size as u32).to_le_bytes());
        record.extend_from_slice(&(offset as u32).to_le_bytes());
        record.extend_from_slice(&[0, 0]);
        record
    }

    #[test]
    fn reader_on_a_fallback_directory_is_rejected_even_when_old_checks_pass() {
        // Exercise the post-open binding on its own: preflight recorded the
        // directory at `central_offset`, but the reader fell back to a decoy
        // directory with the same count, name and sizes.
        let size = 256u32;
        let mut data = Vec::from(*b"PK\x03\x04");
        data.extend_from_slice(&[20, 0]);
        data.extend_from_slice(&[0; 12]);
        data.extend_from_slice(&size.to_le_bytes());
        data.extend_from_slice(&size.to_le_bytes());
        data.extend_from_slice(&[8, 0, 0, 0]);
        data.extend_from_slice(b"part.stl");
        let decoy_offset = data.len() + 16;
        let decoy = central(size, &[]);
        let mut payload = vec![b'p'; 16];
        payload.extend_from_slice(&decoy);
        payload.extend(end_record(decoy_offset, decoy.len()));
        payload.resize(size as usize, b'p');
        data.extend_from_slice(&payload);
        let central_offset = data.len();
        let real = central(size, &[1, 0]);
        data.extend_from_slice(&real);
        data.extend(end_record(central_offset, real.len()));

        let archive = zip::ZipArchive::with_config(
            zip::read::Config {
                archive_offset: zip::read::ArchiveOffset::Known(0),
            },
            io::Cursor::new(data),
        )
        .unwrap();
        let preflight = Preflight {
            entries: vec![Entry {
                raw_name: "part.stl".into(),
                path: SourcePath::try_from("part.stl".to_owned()).unwrap(),
                directory: false,
                compressed_bytes: size as u64,
                inflated_bytes: size as u64,
                central_header_offset: central_offset as u64,
                local_header_offset: 0,
            }],
            central_offset: central_offset as u64,
            eocd_offset: (central_offset + real.len()) as u64,
        };
        // The pre-fix check compared only the entry count after opening.
        assert_eq!(archive.len(), preflight.entries.len());
        assert_eq!(archive.central_directory_start(), decoy_offset as u64);
        assert!(matches!(
            verify_reader_archive(&archive, &preflight),
            Err(ArchiveError::ReaderMismatch(
                ReaderMismatch::CentralDirectoryStart
            ))
        ));
        assert!(matches!(
            validate_extra_field(&[1, 0], &[0; 46]),
            Err(ArchiveError::MalformedExtraField)
        ));
    }

    #[test]
    fn every_zip64_record_is_read_against_the_header_sentinels() {
        let zip64 = |values: &[u64]| {
            let mut record = Vec::from([1, 0]);
            record.extend_from_slice(&(values.len() as u16 * 8).to_le_bytes());
            values
                .iter()
                .for_each(|v| record.extend_from_slice(&v.to_le_bytes()));
            record
        };
        let header = |size: u32| <[u8; 46]>::try_from(&central(size, &[])[..46]).unwrap();
        let resolved = |extra: &[u8], size: u32| {
            validate_extra_field(extra, &header(size)).map(|r| {
                (
                    r.uncompressed_bytes,
                    r.compressed_bytes,
                    r.local_header_offset,
                )
            })
        };
        // Saturated sizes oblige every record to carry them, and the last wins.
        let saturated = [zip64(&[7, 7]), zip64(&[])].concat();
        assert!(matches!(
            resolved(&saturated, u32::MAX),
            Err(ArchiveError::MalformedExtraField)
        ));
        let repeated = [zip64(&[7, 7]), zip64(&[8, 9])].concat();
        assert_eq!(resolved(&repeated, u32::MAX).unwrap(), (8, 9, 0));
        // A full record over an unsaturated header is read but changes nothing,
        // so it obliges no later record.
        let full = [zip64(&[u32::MAX.into(); 3]), zip64(&[])].concat();
        assert_eq!(resolved(&full, 10).unwrap(), (10, 10, 0));
    }
}
