//! The object names in a sliced print file, and the Checkoff units a send of
//! it prints (GRE-1896). This is Node's send-flow rule:
//! `apps/web/src/lib/parseSlicedObjects.ts` for the names and
//! `apps/web/src/lib/proposeCheckoffFromObjects.ts` for the units, over the
//! slicer-aware name match in [`crate::gcode_objects`].
//!
//! Text G-code is read one line at a time, and each line is scanned in time
//! linear in its length, so a malformed file cannot make the scan slow.
//! Binary G-code is walked block by block (libbgcode's `specifications.md`):
//! uncompressed and Deflate blocks are read, and a block this reader cannot
//! decode makes the file [`FileObjects::Unsupported`] rather than a file
//! with no objects.

use crate::gcode_objects::{interpret_sliced_object_name, match_object_name};
use std::collections::HashSet;
use std::io::Read;

/// What reading a file's object names found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileObjects {
    /// The file was read; its object names in Node's order, possibly none.
    Read(Vec<String>),
    /// A binary G-code block that may hold names uses a compression or an
    /// encoding this reader does not decode (Heatshrink, MeatPack).
    Unsupported,
    /// A binary G-code file whose structure or checksum is wrong.
    Unreadable,
    /// The file names more than [`MAX_NAMES`] distinct objects, so its list
    /// would be incomplete.
    TooMany,
}

/// Distinct object names a file may name, so a hostile file cannot grow the
/// list without bound. A file over it is [`FileObjects::TooMany`].
pub const MAX_NAMES: usize = 10_000;
/// An `objects_info` line longer than this is not parsed as JSON.
const MAX_INFO_LINE: usize = 4 * 1024 * 1024;

/// The object names in a sliced file's bytes.
pub fn object_names(bytes: &[u8]) -> FileObjects {
    if bytes.starts_with(BGCODE_MAGIC) {
        return bgcode(bytes);
    }
    let mut names = Names::default();
    names.text(bytes);
    names.finish()
}

/// Labels by kind, in the order Node collects them, each kind's names once.
#[derive(Default)]
struct Names {
    /// PrusaSlicer's `objects_info`: one entry per placed copy. It wins.
    info: Option<Vec<String>>,
    /// EXCLUDE_OBJECT_DEFINE JSON, EXCLUDE_OBJECT_DEFINE NAME=, quoted M486,
    /// bare M486, `; printing object` and Cura `;MESH:` labels.
    by_kind: [Vec<String>; 6],
    /// Each kind's names so far, in lower case. A label a slicer repeats on
    /// every layer is kept once per kind.
    seen: [HashSet<String>; 6],
    /// Every distinct name so far, in lower case, whatever its kind. Only a
    /// name new to the file counts against [`MAX_NAMES`], so one label two
    /// readers both match (quoted and bare M486) counts once.
    distinct: HashSet<String>,
    /// A distinct name past [`MAX_NAMES`] was found.
    overflow: bool,
    /// The unfinished last line of the previous binary G-code block.
    carry: Vec<u8>,
}

impl Names {
    /// Reads every line of `bytes` but the last unfinished one, which is kept.
    fn text(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while let Some(end) = rest.iter().position(|b| *b == b'\n') {
            let mut line = std::mem::take(&mut self.carry);
            line.extend_from_slice(&rest[..end]);
            self.line(&line);
            rest = &rest[end + 1..];
        }
        self.carry.extend_from_slice(rest);
    }

    /// A binary G-code metadata block: only its `objects_info` key is read.
    /// Toolpath labels count only in G-code.
    fn metadata(&mut self, bytes: &[u8]) {
        for line in bytes.split(|b| *b == b'\n') {
            if self.info.is_none() {
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                self.info = objects_info(&String::from_utf8_lossy(line));
            }
        }
    }

    fn line(&mut self, bytes: &[u8]) {
        let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
        if self.info.is_some() {
            // The inventory wins; labels after it do not matter.
            return;
        }
        let line = String::from_utf8_lossy(bytes);
        self.info = objects_info(&line);
        let found = [
            exclude_json(&line),
            exclude_name(&line),
            m486(&line, true),
            m486(&line, false),
            printing_object(&line),
            cura_mesh(&line),
        ];
        for (kind, name) in found.into_iter().enumerate() {
            let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) else {
                continue;
            };
            let key = name.to_lowercase();
            if self.seen[kind].contains(&key) {
                continue;
            }
            if !self.distinct.contains(&key) {
                if self.distinct.len() == MAX_NAMES {
                    self.overflow = true;
                    continue;
                }
                self.distinct.insert(key.clone());
            }
            // Each kind keeps its own order, so the file's order stays Node's.
            self.seen[kind].insert(key);
            self.by_kind[kind].push(name.to_owned());
        }
    }

    fn finish(mut self) -> FileObjects {
        let carry = std::mem::take(&mut self.carry);
        if !carry.is_empty() {
            self.line(&carry);
        }
        if let Some(info) = self.info {
            return match info.len() > MAX_NAMES {
                true => FileObjects::TooMany,
                false => FileObjects::Read(info),
            };
        }
        if self.overflow {
            return FileObjects::TooMany;
        }
        let mut seen = HashSet::new();
        FileObjects::Read(
            self.by_kind
                .into_iter()
                .flatten()
                .filter(|name| seen.insert(name.to_lowercase()))
                .collect(),
        )
    }
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Where `word` (ASCII, any case) ends in `text` at `at`, when it is there.
fn word_at(text: &str, at: usize, word: &str) -> Option<usize> {
    let end = at + word.len();
    text.get(at..end)
        .filter(|found| found.eq_ignore_ascii_case(word))
        .map(|_| end)
}

/// Each start of `word` in `text` with a word boundary after it, and before
/// it too when `left` is set (`\bword\b`).
fn words<'a>(text: &'a str, word: &'a str, left: bool) -> impl Iterator<Item = usize> + 'a {
    text.char_indices().filter_map(move |(at, _)| {
        let end = word_at(text, at, word)?;
        let before = text[..at].chars().next_back().is_none_or(|c| !is_word(c));
        let after = text[end..].chars().next().is_none_or(|c| !is_word(c));
        ((before || !left) && after).then_some(at)
    })
}

/// `s` without leading Unicode white space, as JavaScript's `\s*`.
fn skip_space(s: &str) -> &str {
    s.trim_start()
}

/// `"text"` or `'text'` at the start of `s`, with at least one character.
fn quoted(s: &str, quote: char) -> Option<&str> {
    let body = s.strip_prefix(quote)?;
    let end = body.find(quote)?;
    (end > 0).then(|| &body[..end])
}

/// `^\s*;?\s*objects_info\s*=\s*(\{[^\r\n]*\})\s*$`: the inventory's names,
/// when the line holds at least one.
fn objects_info(line: &str) -> Option<Vec<String>> {
    let rest = skip_space(line);
    let rest = skip_space(rest.strip_prefix(';').unwrap_or(rest));
    let rest = skip_space(rest.strip_prefix("objects_info")?);
    let json = skip_space(rest.strip_prefix('=')?).trim_end();
    if json.len() > MAX_INFO_LINE || !json.starts_with('{') || !json.ends_with('}') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let names: Vec<String> = value
        .get("objects")?
        .as_array()?
        .iter()
        .filter_map(|item| item.get("name")?.as_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .take(MAX_NAMES + 1)
        .map(str::to_owned)
        .collect();
    (!names.is_empty()).then_some(names)
}

/// `EXCLUDE_OBJECT_DEFINE\b[^\n]*?"name"\s*:\s*"([^"]+)"`, any case.
fn exclude_json(line: &str) -> Option<&str> {
    let start = words(line, "EXCLUDE_OBJECT_DEFINE", false).next()? + 21;
    let rest = &line[start..];
    rest.char_indices().find_map(|(at, _)| {
        let after = &rest[word_at(rest, at, "\"name\"")?..];
        quoted(skip_space(skip_space(after).strip_prefix(':')?), '"')
    })
}

/// `^\s*EXCLUDE_OBJECT_DEFINE\b` then `\bNAME\s*=\s*("…"|'…'|[^\s,]+)`, any case.
fn exclude_name(line: &str) -> Option<&str> {
    let rest = skip_space(line);
    let end = word_at(rest, 0, "EXCLUDE_OBJECT_DEFINE")?;
    if rest[end..].chars().next().is_some_and(is_word) {
        return None;
    }
    let rest = &rest[end..];
    words(rest, "NAME", true).find_map(|at| {
        let value = skip_space(skip_space(&rest[at + 4..]).strip_prefix('=')?);
        quoted(value, '"')
            .or_else(|| quoted(value, '\''))
            .or_else(|| {
                let end = value
                    .find(|c: char| c.is_whitespace() || c == ',')
                    .unwrap_or(value.len());
                (end > 0).then(|| &value[..end])
            })
    })
}

/// `\bM486\b[^\n]*?\bA\s*"([^"]+)"` (`quoted`) or `\bM486\b[^\n]*?\bA\s*([^\s;]+)`,
/// any case. A bare number is an object index, not a label.
fn m486(line: &str, quoted_only: bool) -> Option<&str> {
    let start = words(line, "M486", true).next()? + 4;
    let rest = &line[start..];
    // `rest` starts after a word boundary, so its first character is never a word character.
    let label = rest.char_indices().find_map(|(at, c)| {
        let boundary = rest[..at].chars().next_back().is_none_or(|p| !is_word(p));
        if !c.eq_ignore_ascii_case(&'a') || !boundary {
            return None;
        }
        let value = skip_space(&rest[at + 1..]);
        if quoted_only {
            return quoted(value, '"');
        }
        let end = value
            .find(|c: char| c.is_whitespace() || c == ';')
            .unwrap_or(value.len());
        (end > 0).then(|| &value[..end])
    })?;
    if quoted_only {
        return Some(label);
    }
    let label = label.trim_matches('"');
    (!label.chars().all(|c| c.is_ascii_digit())).then_some(label)
}

/// `;\s*printing object\s+(\S+?)(?:\s+id:\d+)?(?:\s+copy\s+\d+)?\s*$`, any case.
fn printing_object(line: &str) -> Option<&str> {
    line.match_indices(';').find_map(|(at, _)| {
        let rest = skip_space(&line[at + 1..]);
        let rest = &rest[word_at(rest, 0, "printing object")?..];
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        // At most `name id:N copy N`; a fifth token means no match, without reading further.
        let mut tokens = rest.split_whitespace();
        let name = tokens.next()?;
        let tail: Vec<&str> = tokens.take(4).collect();
        // `t` may hold any UTF-8, so it is compared as bytes, never sliced as text.
        let id = |t: &str| {
            let t = t.as_bytes();
            t.len() > 3
                && t[..3].eq_ignore_ascii_case(b"id:")
                && t[3..].iter().all(u8::is_ascii_digit)
        };
        let copy = |t: &[&str]| {
            t.len() == 2
                && t[0].eq_ignore_ascii_case("copy")
                && t[1].bytes().all(|b| b.is_ascii_digit())
        };
        let fits = match tail.as_slice() {
            [] => true,
            [first, more @ ..] if id(first) => more.is_empty() || copy(more),
            more => copy(more),
        };
        fits.then_some(name)
    })
}

/// `^;\s*MESH\s*:\s*(.+?)\s*$`, any case, without Cura's `NONMESH`.
fn cura_mesh(line: &str) -> Option<&str> {
    let rest = skip_space(line.strip_prefix(';')?);
    let rest = skip_space(&rest[word_at(rest, 0, "MESH")?..]);
    let name = skip_space(rest.strip_prefix(':')?).trim_end();
    (!name.is_empty() && !name.eq_ignore_ascii_case("NONMESH")).then_some(name)
}

// ---- Binary G-code ------------------------------------------------------------------------

const BGCODE_MAGIC: &[u8] = b"GCDE";
/// The only version libbgcode's specification defines.
const BGCODE_VERSION: u32 = 1;
const GCODE_BLOCK: u16 = 1;
const THUMBNAIL_BLOCK: u16 = 5;
/// A block's declared size is refused above this, before anything is read.
const MAX_BLOCK_BYTES: u32 = 64 * 1024 * 1024;
/// All decompressed text together, so a file of small bombs is refused too.
/// Tests use 1 MiB so they can reach it.
const MAX_TEXT_BYTES: usize = if cfg!(test) { 1 << 20 } else { 512 << 20 };

/// Reads the metadata and G-code blocks of a binary G-code file.
fn bgcode(bytes: &[u8]) -> FileObjects {
    let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let Some(checksum) = u16_at(8).filter(|kind| *kind <= 1) else {
        return FileObjects::Unreadable;
    };
    if u32_at(4) != Some(BGCODE_VERSION) || bytes.len() == 10 {
        return FileObjects::Unreadable;
    }
    let mut names = Names::default();
    let (mut at, mut text_bytes, mut unsupported) = (10, 0, false);
    while at < bytes.len() {
        let (Some(kind), Some(compression), Some(size)) =
            (u16_at(at), u16_at(at + 2), u32_at(at + 4))
        else {
            return FileObjects::Unreadable;
        };
        let (header, stored) = match compression {
            0 => (8, Some(size)),
            1..=3 => (12, u32_at(at + 8)),
            _ => return FileObjects::Unreadable,
        };
        let parameters = if kind == THUMBNAIL_BLOCK { 6 } else { 2 };
        let data = at + header + parameters;
        let (Some(stored), true) = (stored, kind <= THUMBNAIL_BLOCK && size <= MAX_BLOCK_BYTES)
        else {
            return FileObjects::Unreadable;
        };
        let end = data + stored as usize;
        let next = end + if checksum == 1 { 4 } else { 0 };
        let (Some(block), Some(encoding)) = (bytes.get(data..end), u16_at(at + header)) else {
            return FileObjects::Unreadable;
        };
        if checksum == 1 && !crc_matches(&bytes[at..end], u32_at(end)) {
            return FileObjects::Unreadable;
        }
        at = next;
        if kind == THUMBNAIL_BLOCK {
            continue;
        }
        if encoding != 0 || compression > 1 {
            unsupported = true;
            continue;
        }
        // The cap holds across blocks and is checked before anything is inflated.
        if size as usize > MAX_TEXT_BYTES - text_bytes {
            return FileObjects::Unreadable;
        }
        text_bytes += size as usize;
        let text = match compression {
            0 => Some(block.to_vec()),
            _ => inflate(block, size),
        };
        let Some(text) = text else {
            return FileObjects::Unreadable;
        };
        match kind {
            GCODE_BLOCK => names.text(&text),
            _ => names.metadata(&text),
        }
    }
    if unsupported && names.info.is_none() {
        return FileObjects::Unsupported;
    }
    names.finish()
}

fn crc_matches(covered: &[u8], stored: Option<u32>) -> bool {
    let mut crc = flate2::Crc::new();
    crc.update(covered);
    stored == Some(crc.sum())
}

/// A zlib (Deflate) block, which must inflate to exactly `size` bytes. It
/// reads at most `size + 1`, so a block that inflates further stops there.
fn inflate(block: &[u8], size: u32) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(size.min(1024 * 1024) as usize);
    let mut reader = flate2::read::ZlibDecoder::new(block).take(u64::from(size) + 1);
    reader.read_to_end(&mut out).ok()?;
    (out.len() == size as usize).then_some(out)
}

// ---- Checkoff units -----------------------------------------------------------------------

/// One included Part of the Build's Accepted Plan and its units still to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemainingPart {
    pub part_id: i64,
    pub filename: String,
    /// Unprinted units, lowest index first.
    pub units: Vec<RemainingUnit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemainingUnit {
    pub index: u32,
    /// The Required unit's object name, which Print Partner's 3MF export
    /// gives the object (`bracket__ppu_<token>`).
    pub object_name: String,
}

/// A unit the send links, as `checkoff_units` stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedUnit {
    pub part_id: i64,
    pub unit_index: u32,
    /// The file's object name, trimmed. Storage cuts it to 200 UTF-16 units.
    pub object_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Proposal {
    pub units: Vec<ProposedUnit>,
    /// Names that took no unit, in file order.
    pub unmatched: Vec<String>,
}

/// `interpretSlicedObjectName(raw).unwrappedName` with a trailing `_stl`
/// read as `.stl`, in lower case: how a name is compared with a Required
/// unit's object name, as Node's `resolveAcceptedPrinterAttribution` does.
fn required_name_key(raw: &str) -> String {
    let name = interpret_sliced_object_name(raw).unwrapped_name;
    let cut = name.len().saturating_sub(4);
    match name.get(cut..) {
        Some(tail) if name.len() >= 4 && tail.eq_ignore_ascii_case("_stl") => {
            format!("{}.stl", &name[..cut])
        }
        _ => name,
    }
    .to_lowercase()
}

/// The units each object name takes, one unit still to print per name. A
/// name that is a Required unit's object name (Print Partner's 3MF export)
/// takes that unit, as Node's accepted attribution does. The other names
/// follow `proposeCheckoffFromObjects`: first a unique export-style name
/// (`bracket_02` is unit 2 of bracket.stl), then the next free unit of the
/// one Part whose file name the name matches. A name that is ambiguous or
/// matches nothing takes no unit.
pub fn propose_units(names: &[String], parts: &[RemainingPart]) -> Proposal {
    let key = |name: &str| interpret_sliced_object_name(name).basename_key;
    let slots: Vec<(&RemainingPart, u32, String, String)> = parts
        .iter()
        .flat_map(|part| {
            let stem = key(&part.filename);
            part.units.iter().map(move |unit| {
                let export = key(&format!("{stem}_{:02}", unit.index + 1));
                (part, unit.index, export, unit.object_name.to_lowercase())
            })
        })
        .collect();
    let mut used = vec![false; slots.len()];
    let mut taken: Vec<(usize, &String)> = Vec::new();
    let mut proposal = Proposal::default();
    let mut pending = Vec::new();
    let mut rest = Vec::new();
    for name in names {
        let wanted = required_name_key(name);
        match (0..slots.len()).find(|&i| !used[i] && !slots[i].3.is_empty() && slots[i].3 == wanted)
        {
            Some(i) => {
                used[i] = true;
                taken.push((i, name));
            }
            None => rest.push(name),
        }
    }
    for name in rest {
        let name_key = key(name);
        let exact: Vec<usize> = (0..slots.len())
            .filter(|&i| !used[i] && slots[i].2 == name_key)
            .collect();
        match exact.as_slice() {
            _ if name_key.is_empty() => proposal.unmatched.push(name.clone()),
            [one] => {
                used[*one] = true;
                taken.push((*one, name));
            }
            [] => pending.push(name),
            _ => proposal.unmatched.push(name.clone()),
        }
    }
    for name in pending {
        let free: Vec<usize> = (0..slots.len()).filter(|&i| !used[i]).collect();
        let mut filenames: Vec<&str> = Vec::new();
        for &i in &free {
            if !filenames.contains(&slots[i].0.filename.as_str()) {
                filenames.push(&slots[i].0.filename);
            }
        }
        let matched = match match_object_name(name, &filenames).as_slice() {
            [one] => Some(one.clone()),
            _ => None,
        };
        let candidates: Vec<usize> = free
            .into_iter()
            .filter(|&i| matched.as_deref() == Some(slots[i].0.filename.as_str()))
            .collect();
        let parts: HashSet<i64> = candidates.iter().map(|&i| slots[i].0.part_id).collect();
        match (candidates.first(), parts.len()) {
            (Some(&first), 1) => {
                used[first] = true;
                taken.push((first, name));
            }
            _ => proposal.unmatched.push(name.clone()),
        }
    }
    proposal.units = taken
        .into_iter()
        .map(|(i, name)| ProposedUnit {
            part_id: slots[i].0.part_id,
            unit_index: slots[i].1,
            object_name: name.trim().to_owned(),
        })
        .collect();
    proposal
}

#[cfg(test)]
mod tests;
