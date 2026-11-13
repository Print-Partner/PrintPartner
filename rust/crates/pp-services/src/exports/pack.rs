//! The STL pack: one ZIP entry per Required unit, named by role and Source
//! folder, written as stored entries so each is byte-identical to its STL.
//! The caller opens and verifies the files; this module only lays out names
//! and writes the archive to any `Write`.

use std::{
    collections::HashSet,
    io::{self, Read, Write},
};

/// One included Part of the accepted Plan.
pub struct PackPart {
    pub role: String,
    pub relative_path: String,
    pub filename: String,
    /// The verified length of the Part's STL.
    pub size: u64,
    pub units: Vec<PackUnit>,
}

/// One Required unit of a Part.
pub struct PackUnit {
    /// The unit's number within its Part, from 1.
    pub number: u32,
    pub printed: bool,
}

/// Which units the pack holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    /// Only units not yet checked off as printed.
    Remaining,
}

/// The largest pack a plain ZIP holds: its entry count and archive size fit
/// the 16- and 32-bit fields without ZIP64.
#[derive(Clone, Copy)]
pub struct Limits {
    pub entries: usize,
    pub bytes: u64,
}

pub const LIMITS: Limits = Limits {
    entries: 65_535,
    bytes: u32::MAX as u64,
};

/// One archive entry: its name and the index of its Part.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub part: usize,
}

/// Why no pack can be built.
#[derive(Debug, PartialEq, Eq)]
pub enum PackRefusal {
    /// The scope leaves no unit to pack.
    NothingToPack,
    TooManyEntries {
        entries: usize,
        limit: usize,
    },
    TooLarge {
        bytes: u64,
        limit: u64,
    },
}

/// The entries of the pack, in Part order then unit order. Names are
/// `<role>/<folder>/<stem>_<NN>.stl`, with `_root` for a file at the top of
/// its Source; a name already used, ignoring ASCII case so that no entry
/// overwrites another on a case-insensitive file system, gains `_2`, `_3`
/// and so on.
/// Each of the three segments is cleaned with [`safe_segment`], so no entry
/// name can leave the folder it is extracted into.
pub fn layout(parts: &[PackPart], scope: Scope, limits: Limits) -> Result<Vec<Entry>, PackRefusal> {
    let mut used: HashSet<String> = HashSet::new();
    let mut entries = Vec::new();
    let mut bytes = END_RECORD;
    for (index, part) in parts.iter().enumerate() {
        let folder = format!(
            "{}/{}",
            safe_segment(&part.role, "_root"),
            safe_segment(source_folder(&part.relative_path), "_root")
        );
        let stem = safe_segment(without_stl(&part.filename), "_");
        for unit in &part.units {
            if scope == Scope::Remaining && unit.printed {
                continue;
            }
            let base = format!("{stem}_{:02}", unit.number);
            let mut name = format!("{folder}/{base}.stl");
            let mut suffix = 2;
            while !used.insert(name.to_ascii_lowercase()) {
                name = format!("{folder}/{base}_{suffix}.stl");
                suffix += 1;
            }
            bytes = bytes.saturating_add(entry_bytes(&name, part.size));
            entries.push(Entry { name, part: index });
        }
    }
    if entries.is_empty() {
        return Err(PackRefusal::NothingToPack);
    }
    if entries.len() > limits.entries {
        return Err(PackRefusal::TooManyEntries {
            entries: entries.len(),
            limit: limits.entries,
        });
    }
    if bytes > limits.bytes {
        return Err(PackRefusal::TooLarge {
            bytes,
            limit: limits.bytes,
        });
    }
    Ok(entries)
}

/// The download's file name: `<build-slug>-v<plan version>-stl.zip`.
pub fn filename(build_name: &str, plan_version: i64) -> String {
    format!("{}-v{plan_version}-stl.zip", super::slug(build_name))
}

/// `filename` without a final `.stl` in any case.
fn without_stl(filename: &str) -> &str {
    match filename.len().checked_sub(4) {
        Some(cut)
            if filename
                .get(cut..)
                .is_some_and(|end| end.eq_ignore_ascii_case(".stl")) =>
        {
            &filename[..cut]
        }
        _ => filename,
    }
}

/// The folder part of a Source-relative path.
fn source_folder(relative_path: &str) -> &str {
    relative_path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// One archive path segment: ASCII letters and digits, `_`, `-` and `.`
/// kept, any other run of characters (`/` and `\\` among them) one `_`, and
/// `empty` when nothing usable is left or the result is `.` or `..`.
fn safe_segment(name: &str, empty: &str) -> String {
    let mut safe = String::new();
    for c in name.chars() {
        let keep = c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.');
        if keep {
            safe.push(c);
        } else if !safe.ends_with('_') {
            safe.push('_');
        }
    }
    if safe.is_empty() || safe == "." || safe == ".." {
        empty.into()
    } else {
        safe
    }
}

const LOCAL_HEADER: u64 = 30;
const DESCRIPTOR: u64 = 16;
const CENTRAL_HEADER: u64 = 46;
const END_RECORD: u64 = 22;

/// The archive bytes one entry takes: local header, data, descriptor and
/// central record.
fn entry_bytes(name: &str, size: u64) -> u64 {
    let name = name.len() as u64;
    LOCAL_HEADER + name + size + DESCRIPTOR + CENTRAL_HEADER + name
}

/// 1980-01-01 00:00, the earliest DOS time, so the archive does not depend on
/// the clock.
const DOS_DATE: u16 = 0x0021;
/// Stored entries with a data descriptor (bit 3) and UTF-8 names (bit 11).
const FLAGS: u16 = 0x0808;

/// Writes a stored-entry ZIP to `out` one entry at a time. Each entry's CRC
/// and length follow its data, so nothing is buffered or written twice.
pub struct ZipStream<W: Write> {
    out: W,
    offset: u64,
    central: Vec<u8>,
    entries: u16,
}

impl<W: Write> ZipStream<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            offset: 0,
            central: Vec::new(),
            entries: 0,
        }
    }

    /// Copies `data` to the end as the entry `name`. A read error, including
    /// one the reader raises on a hash mismatch, stops the archive there.
    pub fn entry(&mut self, name: &str, data: &mut impl Read) -> io::Result<()> {
        let start = fits(self.offset)?;
        let name_len = u16::try_from(name.len()).map_err(|_| too_large())?;
        let mut header = Vec::with_capacity(30 + name.len());
        header.extend(0x0403_4b50u32.to_le_bytes());
        header.extend(20u16.to_le_bytes());
        header.extend(FLAGS.to_le_bytes());
        header.extend([0u8; 4]); // method stored, time 00:00
        header.extend(DOS_DATE.to_le_bytes());
        header.extend([0u8; 12]); // CRC and sizes follow in the descriptor
        header.extend(name_len.to_le_bytes());
        header.extend(0u16.to_le_bytes());
        header.extend(name.as_bytes());
        self.write(&header)?;
        let mut crc = crc32fast::Hasher::new();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut length = 0u64;
        loop {
            let read = data.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            crc.update(&buffer[..read]);
            length += read as u64;
            self.write(&buffer[..read])?;
        }
        let (crc, length) = (crc.finalize(), fits(length)?);
        let mut descriptor = Vec::with_capacity(16);
        descriptor.extend(0x0807_4b50u32.to_le_bytes());
        for value in [crc, length, length] {
            descriptor.extend(value.to_le_bytes());
        }
        self.write(&descriptor)?;
        self.central.extend(0x0201_4b50u32.to_le_bytes());
        self.central.extend(20u16.to_le_bytes()); // made by
        self.central.extend(20u16.to_le_bytes()); // needed
        self.central.extend(FLAGS.to_le_bytes());
        self.central.extend([0u8; 4]);
        self.central.extend(DOS_DATE.to_le_bytes());
        for value in [crc, length, length] {
            self.central.extend(value.to_le_bytes());
        }
        self.central.extend(name_len.to_le_bytes());
        self.central.extend([0u8; 12]); // extra, comment, disk, attributes
        self.central.extend(start.to_le_bytes());
        self.central.extend(name.as_bytes());
        self.entries = self.entries.checked_add(1).ok_or_else(too_large)?;
        Ok(())
    }

    /// Writes the central directory and returns the writer.
    pub fn finish(mut self) -> io::Result<W> {
        let start = fits(self.offset)?;
        let size = fits(self.central.len() as u64)?;
        let central = std::mem::take(&mut self.central);
        self.write(&central)?;
        let mut end = Vec::with_capacity(22);
        end.extend(0x0605_4b50u32.to_le_bytes());
        end.extend([0u8; 4]); // disk numbers
        end.extend(self.entries.to_le_bytes());
        end.extend(self.entries.to_le_bytes());
        end.extend(size.to_le_bytes());
        end.extend(start.to_le_bytes());
        end.extend(0u16.to_le_bytes());
        self.write(&end)?;
        fits(self.offset)?;
        self.out.flush()?;
        Ok(self.out)
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.offset += bytes.len() as u64;
        Ok(())
    }
}

fn too_large() -> io::Error {
    io::Error::other("pack exceeds the limits of a plain ZIP")
}

fn fits(value: u64) -> io::Result<u32> {
    u32::try_from(value).map_err(|_| too_large())
}

#[cfg(test)]
mod tests;
