//! Classify a print file from its bytes: Node's
//! `web/apps/server/src/lib/print-file-classification.ts` (GRE-1210).
//!
//! Nothing here trusts a filename, a content type or an operator checkbox.
//! Every answer comes from the file signature and, for 3MF, from the ZIP
//! central directory. The 3MF path never inflates an entry and never parses
//! XML. Entry names alone tell a slicer project from bare geometry from an
//! embedded toolpath. `fixtures/parity/print-file-classification` holds
//! goldens the Node classifier wrote, and `tests/print_file_parity.rs` replays
//! them.
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Largest file PrintPartner reads into memory to classify and hash.
pub const MAX_CLASSIFIABLE_BYTES: usize = 256 * 1024 * 1024;

const MAX_ENTRIES: u64 = 2_000;
const MAX_ENTRY_NAME_BYTES: usize = 1_024;
const MAX_ENTRY_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_NESTING_DEPTH: usize = 16;

const ZIP_LOCAL_HEADER: u32 = 0x0403_4b50;
const ZIP_CENTRAL_HEADER: u32 = 0x0201_4b50;
const ZIP_EOCD: u32 = 0x0605_4b50;
const ZIP64_EOCD: u32 = 0x0606_4b50;
const ZIP64_EOCD_LOCATOR: u32 = 0x0706_4b50;
const ZIP_EOCD_FIXED_BYTES: usize = 22;
const ZIP_CENTRAL_HEADER_FIXED_BYTES: usize = 46;
const ZIP_EOCD_SEARCH_BYTES: usize = ZIP_EOCD_FIXED_BYTES + 0xffff;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// libbgcode file header magic, ASCII "GCDE".
const BGCODE_MAGIC: u32 = 0x4544_4347;
const BGCODE_MAX_VERSION: u32 = 1;
const BGCODE_MAX_CHECKSUM_TYPE: u16 = 1;
const BGCODE_HEADER_BYTES: usize = 10;
const BGCODE_CRC32_BYTES: u64 = 4;
const BGCODE_BLOCK_HEADER_BYTES: u64 = 8;
const BGCODE_COMPRESSED_SIZE_BYTES: u64 = 4;

const TOOLPATH_ENTRY_SUFFIXES: [&str; 4] = [".gcode", ".gco", ".g", ".bgcode"];
const SLICER_STATE_PREFIX: &str = "metadata/";
const SLICER_STATE_SUFFIXES: [&str; 2] = [".config", ".ini"];

/// What a 3MF container holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreeMfKind {
    ToolpathPackage,
    SlicerProject,
    ModelPackage,
    Unsupported,
}

/// The contract's `PrintFileClassification`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "format")]
pub enum Classification {
    #[serde(rename = "gcode")]
    Gcode,
    #[serde(rename = "bgcode")]
    Bgcode,
    #[serde(rename = "3mf")]
    ThreeMf { kind: ThreeMfKind },
}

impl Classification {
    /// The contract's `isPrintReady`: a printer can run the file as it is.
    pub fn is_print_ready(self) -> bool {
        self.next_action().is_empty()
    }

    /// Node's `printFileNextAction`: what has to happen before a printer can
    /// run the file. Empty when nothing does.
    pub fn next_action(self) -> &'static str {
        match self {
            Self::Gcode | Self::Bgcode => "",
            Self::ThreeMf { kind } => match kind {
                ThreeMfKind::ToolpathPackage => "",
                ThreeMfKind::SlicerProject => {
                    "That 3MF is a slicer project, so it needs slicing before a printer can run it"
                }
                ThreeMfKind::ModelPackage => {
                    "That 3MF holds only geometry, so it needs preparation and slicing"
                }
                ThreeMfKind::Unsupported => {
                    "PrintPartner cannot interpret that 3MF, so it can be downloaded but not printed"
                }
            },
        }
    }
}

/// Why a file was refused before any classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Rejection {
    EmptyFile,
    FileTooLarge,
    UnrecognizedSignature,
    BgcodeHeaderInvalid,
    ArchiveTruncated,
    ArchiveEntryLimit,
    ArchiveEntryTooLarge,
    ArchiveTotalTooLarge,
    ArchiveNestingLimit,
    ArchiveUnsafeEntry,
}

impl Rejection {
    /// Node's `printFileRejectionMessage`.
    pub fn message(self) -> &'static str {
        match self {
            Self::EmptyFile => "That print file is empty",
            Self::FileTooLarge => "That print file is too large for PrintPartner to inspect",
            Self::UnrecognizedSignature => {
                "Those bytes are not G-code, binary G-code, or a 3MF package"
            }
            Self::BgcodeHeaderInvalid => "That binary G-code file has a damaged header",
            Self::ArchiveTruncated => {
                "That 3MF package is truncated or is not a valid ZIP container"
            }
            Self::ArchiveEntryLimit => "That 3MF package holds too many entries to inspect safely",
            Self::ArchiveEntryTooLarge => {
                "That 3MF package holds an entry too large to inspect safely"
            }
            Self::ArchiveTotalTooLarge => {
                "That 3MF package unpacks to more data than PrintPartner will inspect"
            }
            Self::ArchiveNestingLimit => {
                "That 3MF package nests its entries too deeply to inspect safely"
            }
            Self::ArchiveUnsafeEntry => "That 3MF package holds an unsafe entry name",
        }
    }
}

/// A classified file with the SHA-256 and size of exactly the bytes read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Classified {
    pub classification: Classification,
    pub sha256: String,
    pub size_bytes: u64,
}

/// Node's `classifyPrintFileBytes`: classify and hash in one call. The
/// result is the only thing that decides whether a file is print-ready.
pub fn classify(bytes: &[u8]) -> Result<Classified, Rejection> {
    if bytes.is_empty() {
        return Err(Rejection::EmptyFile);
    }
    if bytes.len() > MAX_CLASSIFIABLE_BYTES {
        return Err(Rejection::FileTooLarge);
    }
    let classified = |classification| Classified {
        classification,
        sha256: Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
        size_bytes: bytes.len() as u64,
    };
    match u32_at(bytes, 0) {
        Some(ZIP_LOCAL_HEADER) => {
            return scan_three_mf(bytes).map(|kind| classified(Classification::ThreeMf { kind }));
        }
        Some(BGCODE_MAGIC) if !bgcode_is_valid(bytes) => {
            return Err(Rejection::BgcodeHeaderInvalid);
        }
        Some(BGCODE_MAGIC) => return Ok(classified(Classification::Bgcode)),
        _ => {}
    }
    if gcode_is_valid(bytes) {
        return Ok(classified(Classification::Gcode));
    }
    Err(Rejection::UnrecognizedSignature)
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(at..at.checked_add(8)?)?.try_into().ok()?,
    ))
}

/// Entry names an OPC package has no business carrying. Backslashes are
/// refused rather than translated, so one archive cannot name two paths.
fn entry_name_is_unsafe(name: &str) -> bool {
    let drive = name.as_bytes();
    name.is_empty()
        || name.contains('\0')
        || name.contains('\\')
        || name.starts_with('/')
        || (drive.len() >= 2 && drive[0].is_ascii_alphabetic() && drive[1] == b':')
        || name
            .split('/')
            .any(|segment| segment == ".." || segment == ".")
}

/// Finds the central directory through the end-of-central-directory record,
/// or its ZIP64 record when the classic fields saturate. Returns the entry
/// count and the directory's offset, both already checked against the caps.
fn central_directory(bytes: &[u8]) -> Result<(u64, usize), Rejection> {
    use Rejection::{ArchiveEntryLimit, ArchiveTruncated};
    let len = bytes.len();
    let search_start = len.saturating_sub(ZIP_EOCD_SEARCH_BYTES);
    let eocd = (search_start
        ..=len
            .checked_sub(ZIP_EOCD_FIXED_BYTES)
            .ok_or(ArchiveTruncated)?)
        .rev()
        .find(|&offset| u32_at(bytes, offset) == Some(ZIP_EOCD))
        .ok_or(ArchiveTruncated)?;
    let mut entries = u64::from(u16_at(bytes, eocd + 10).ok_or(ArchiveTruncated)?);
    let mut central = u64::from(u32_at(bytes, eocd + 16).ok_or(ArchiveTruncated)?);

    let locator = eocd.checked_sub(20);
    if let Some(locator) = locator
        && u32_at(bytes, locator) == Some(ZIP64_EOCD_LOCATOR)
        && (entries == 0xffff || central == 0xffff_ffff)
    {
        let zip64 = u64_at(bytes, locator + 8).ok_or(ArchiveTruncated)?;
        if zip64 > MAX_SAFE_INTEGER || zip64 + 56 > len as u64 {
            return Err(ArchiveTruncated);
        }
        let zip64 = zip64 as usize;
        if u32_at(bytes, zip64) != Some(ZIP64_EOCD) {
            return Err(ArchiveTruncated);
        }
        let zip64_entries = u64_at(bytes, zip64 + 32).ok_or(ArchiveTruncated)?;
        let zip64_central = u64_at(bytes, zip64 + 48).ok_or(ArchiveTruncated)?;
        if zip64_entries > MAX_SAFE_INTEGER || zip64_central > MAX_SAFE_INTEGER {
            return Err(ArchiveTruncated);
        }
        if zip64_entries > MAX_ENTRIES {
            return Err(ArchiveEntryLimit);
        }
        entries = zip64_entries;
        central = zip64_central;
    }
    if entries > MAX_ENTRIES {
        return Err(ArchiveEntryLimit);
    }
    if central >= len as u64 {
        return Err(ArchiveTruncated);
    }
    Ok((entries, central as usize))
}

/// Walks the ZIP central directory and classifies by entry name. Every cap is
/// checked from the central directory before anything is inflated, so a zip
/// bomb is a refusal and never an allocation.
fn scan_three_mf(bytes: &[u8]) -> Result<ThreeMfKind, Rejection> {
    use Rejection::{
        ArchiveEntryTooLarge, ArchiveNestingLimit, ArchiveTotalTooLarge, ArchiveTruncated,
        ArchiveUnsafeEntry,
    };
    let len = bytes.len();
    let (entries, mut cursor) = central_directory(bytes)?;
    let mut total: u64 = 0;
    let (mut model, mut slicer_state, mut toolpath) = (false, false, false);
    for _ in 0..entries {
        if cursor + ZIP_CENTRAL_HEADER_FIXED_BYTES > len
            || u32_at(bytes, cursor) != Some(ZIP_CENTRAL_HEADER)
        {
            return Err(ArchiveTruncated);
        }
        let field16 = |at| {
            u16_at(bytes, cursor + at)
                .map(usize::from)
                .ok_or(ArchiveTruncated)
        };
        let field32 = |at| u32_at(bytes, cursor + at).ok_or(ArchiveTruncated);
        let uncompressed = u64::from(field32(24)?);
        let (name_len, extra_len, comment_len) = (field16(28)?, field16(30)?, field16(32)?);
        let external = field32(38)?;
        let name_start = cursor + ZIP_CENTRAL_HEADER_FIXED_BYTES;
        let name_end = name_start + name_len;
        if name_end > len {
            return Err(ArchiveTruncated);
        }
        if name_len > MAX_ENTRY_NAME_BYTES {
            return Err(ArchiveUnsafeEntry);
        }
        if uncompressed > MAX_ENTRY_UNCOMPRESSED_BYTES {
            return Err(ArchiveEntryTooLarge);
        }
        total += uncompressed;
        if total > MAX_TOTAL_UNCOMPRESSED_BYTES {
            return Err(ArchiveTotalTooLarge);
        }
        if (external >> 16) & 0xf000 == 0xa000 {
            return Err(ArchiveUnsafeEntry);
        }
        let raw = String::from_utf8_lossy(&bytes[name_start..name_end]);
        let directory = raw.ends_with('/');
        let name = raw.strip_suffix('/').unwrap_or(&raw).to_lowercase();
        if entry_name_is_unsafe(&name) {
            return Err(ArchiveUnsafeEntry);
        }
        if name.split('/').count() > MAX_NESTING_DEPTH {
            return Err(ArchiveNestingLimit);
        }
        if !directory {
            toolpath |= TOOLPATH_ENTRY_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix));
            model |= name.starts_with("3d/") && name.ends_with(".model");
            slicer_state |= name.starts_with(SLICER_STATE_PREFIX)
                && SLICER_STATE_SUFFIXES
                    .iter()
                    .any(|suffix| name.ends_with(suffix));
        }
        cursor = name_end + extra_len + comment_len;
    }
    Ok(match (toolpath, model, slicer_state) {
        (true, _, _) => ThreeMfKind::ToolpathPackage,
        (false, true, true) => ThreeMfKind::SlicerProject,
        (false, true, false) => ThreeMfKind::ModelPackage,
        (false, false, _) => ThreeMfKind::Unsupported,
    })
}

/// Structural check of a libbgcode container: magic, version, checksum type
/// and a block chain whose declared sizes stay inside the file. Block payloads
/// are never decoded (libbgcode is AGPL-3.0 and has no license review).
fn bgcode_is_valid(bytes: &[u8]) -> bool {
    let (Some(version), Some(checksum)) = (u32_at(bytes, 4), u16_at(bytes, 8)) else {
        return false;
    };
    if bytes.len() < BGCODE_HEADER_BYTES
        || version == 0
        || version > BGCODE_MAX_VERSION
        || checksum > BGCODE_MAX_CHECKSUM_TYPE
    {
        return false;
    }
    let len = bytes.len() as u64;
    let checksum_bytes = if checksum == 0 { 0 } else { BGCODE_CRC32_BYTES };
    let mut cursor = BGCODE_HEADER_BYTES as u64;
    let mut blocks = 0;
    while cursor < len {
        if cursor + BGCODE_BLOCK_HEADER_BYTES > len {
            return false;
        }
        let at = cursor as usize;
        let (Some(block_type), Some(compression), Some(uncompressed)) = (
            u16_at(bytes, at),
            u16_at(bytes, at + 2),
            u32_at(bytes, at + 4),
        ) else {
            return false;
        };
        if block_type > 5 {
            return false;
        }
        let parameters = if block_type == 5 { 6 } else { 2 };
        let mut payload_start = cursor + BGCODE_BLOCK_HEADER_BYTES;
        let mut payload = u64::from(uncompressed);
        if compression != 0 {
            if payload_start + BGCODE_COMPRESSED_SIZE_BYTES > len {
                return false;
            }
            let Some(compressed) = u32_at(bytes, payload_start as usize) else {
                return false;
            };
            payload = u64::from(compressed);
            payload_start += BGCODE_COMPRESSED_SIZE_BYTES;
        }
        let next = payload_start + parameters + payload + checksum_bytes;
        if next <= cursor || next > len {
            return false;
        }
        cursor = next;
        blocks += 1;
    }
    blocks > 0
}

/// Structural check of ASCII G-code: text with at least one line that opens
/// with G, M or T and a digit. The whole buffer is scanned, because slicer
/// headers can run to hundreds of kilobytes of comments and thumbnails.
fn gcode_is_valid(bytes: &[u8]) -> bool {
    let mut saw_command = false;
    let mut at_line_start = true;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == 0 {
            return false;
        }
        if byte == b'\n' || byte == b'\r' {
            at_line_start = true;
            continue;
        }
        if byte < 0x09 || (byte > 0x0d && byte < 0x20) {
            return false;
        }
        if at_line_start {
            at_line_start = byte == b' ' || byte == b'\t';
            if !saw_command
                && matches!(byte, b'G' | b'M' | b'T' | b'g' | b'm' | b't')
                && bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
            {
                saw_command = true;
            }
        }
    }
    saw_command
}
