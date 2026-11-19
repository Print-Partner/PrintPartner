//! The assembly tree of a STEP file (ISO 10303-21), read from its product
//! structure instead of its geometry (GRE-1007).
//!
//! occt-import-js returns at most two levels of a STEP part tree (its bug
//! #60). This reader rebuilds the whole tree from the entities that define it:
//! each `PRODUCT_DEFINITION` points through a `PRODUCT_DEFINITION_FORMATION`
//! to its `PRODUCT`, and each `NEXT_ASSEMBLY_USAGE_OCCURRENCE` places one
//! product definition inside another. A definition that no occurrence places
//! is a root. A sub-assembly used twice appears twice, once per use.
//!
//! The file is read once, front to back. Only those four entity kinds are
//! buffered; every other record (the geometry, nearly all of a real file) is
//! skipped as it streams past. [`Limits`] bounds what is kept while reading
//! and what the finished tree holds, so memory follows the caps, not the file.
//!
//! A tree is returned only for a whole exchange: every statement ends with
//! `;`, a `DATA` section is closed by `ENDSEC;`, and `END-ISO-10303-21;`
//! follows it. A file cut anywhere before that fails as
//! [`StepError::Incomplete`], so a partial tree is never returned.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

/// The most nodes one tree may hold, counting every use of a shared part.
pub const MAX_PARTS: usize = 100_000;
/// The most levels a tree may have; a lone root is 1 level. A cached tree
/// nests two JSON values per level plus two (`roots` and its object), so 60
/// levels stay under serde_json's 128-value nesting limit and every accepted
/// tree reads back from the cache.
pub const MAX_LEVELS: usize = 60;
/// How many bytes are read between checks of the cancel flag.
const READ_CHUNK: usize = 64 * 1024;

/// Bounds on one read. Every cap that is passed fails the read as
/// [`StepError::TooLarge`] at the record or node that passes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Nodes in the tree, counting every use of a shared part.
    pub max_parts: usize,
    /// Levels in the tree; a lone root is 1.
    pub max_levels: usize,
    /// Product-structure records kept while reading.
    pub max_records: usize,
    /// Bytes in one product-structure record. Geometry records are skipped
    /// unbuffered whatever their size.
    pub max_record_bytes: usize,
    /// Bytes of ids and names kept while reading.
    pub max_kept_text: usize,
    /// Bytes of ids and names in the finished tree, counting every use.
    pub max_tree_text: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_parts: MAX_PARTS,
            max_levels: MAX_LEVELS,
            max_records: 4 * MAX_PARTS,
            max_record_bytes: 64 * 1024,
            max_kept_text: 16 * 1024 * 1024,
            max_tree_text: 32 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartTree {
    /// In entity order. Usually one top assembly.
    pub roots: Vec<PartNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartNode {
    /// The product's name, or its id when the name is empty.
    pub name: String,
    pub product_id: String,
    /// The occurrence's name for this use (its id when unnamed); `None` for
    /// a root and for an occurrence with neither.
    pub instance: Option<String>,
    /// In entity order of their occurrences.
    pub children: Vec<PartNode>,
}

#[derive(Debug)]
pub enum StepError {
    Io(io::Error),
    /// The file does not start with `ISO-10303-21;`.
    NotStep,
    /// The file ends before a closed `DATA` section and `END-ISO-10303-21;`,
    /// or inside a statement.
    Incomplete,
    /// No product definition resolves to a product.
    NoProducts,
    /// Over one of the [`Limits`].
    TooLarge,
    /// An assembly contains itself.
    Cycle,
    Cancelled,
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::NotStep => f.write_str("not a STEP file"),
            Self::Incomplete => f.write_str("the STEP file is incomplete"),
            Self::NoProducts => f.write_str("no product structure"),
            Self::TooLarge => f.write_str("part tree over the size limit"),
            Self::Cycle => f.write_str("an assembly contains itself"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for StepError {}

impl From<io::Error> for StepError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// [`part_tree_within`] the default [`Limits`].
pub fn part_tree(input: impl Read, cancelled: &AtomicBool) -> Result<PartTree, StepError> {
    part_tree_within(input, Limits::default(), cancelled)
}

/// Reads the part tree of the STEP file `input`. `cancelled` is checked
/// before every 64 KiB read and for every node built; setting it ends the
/// read with [`StepError::Cancelled`].
pub fn part_tree_within(
    input: impl Read,
    limits: Limits,
    cancelled: &AtomicBool,
) -> Result<PartTree, StepError> {
    let mut scanner = Scanner::new(input, cancelled, limits.max_record_bytes);
    if !scanner.starts_with_magic()? {
        return Err(StepError::NotStep);
    }
    let mut entities = Entities::new(limits);
    let mut section = Section::Outside;
    let mut data_closed = false;
    loop {
        match scanner.next_statement()?.ok_or(StepError::Incomplete)? {
            Statement::Keyword(word) => match word.as_slice() {
                b"HEADER" => section = Section::Header,
                b"DATA" => section = Section::Data,
                b"ENDSEC" => {
                    data_closed |= section == Section::Data;
                    section = Section::Outside;
                }
                b"END-ISO-10303-21" if section == Section::Outside && data_closed => break,
                b"END-ISO-10303-21" => return Err(StepError::Incomplete),
                _ => {}
            },
            Statement::Entity(id, kind, body) if section == Section::Data => {
                entities.add(id, kind, &params(&body))?;
            }
            Statement::Entity(..) | Statement::Other => {}
        }
    }
    entities.tree(cancelled)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Outside,
    Header,
    Data,
}

/// The four kinds of record the tree is built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Product,
    Formation,
    Definition,
    Usage,
}

impl Kind {
    fn of(keyword: &[u8]) -> Option<Self> {
        match keyword {
            b"PRODUCT" => Some(Self::Product),
            b"PRODUCT_DEFINITION_FORMATION"
            | b"PRODUCT_DEFINITION_FORMATION_WITH_SPECIFIED_SOURCE" => Some(Self::Formation),
            b"PRODUCT_DEFINITION" | b"PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS" => {
                Some(Self::Definition)
            }
            b"NEXT_ASSEMBLY_USAGE_OCCURRENCE" => Some(Self::Usage),
            _ => None,
        }
    }
}

/// One statement of the exchange.
enum Statement {
    /// A section keyword such as `DATA` or `ENDSEC`, or a header entity.
    Keyword(Vec<u8>),
    /// A product-structure record: its entity id, kind and parameter text.
    Entity(u64, Kind, Vec<u8>),
    /// Any other entity record, skipped unbuffered.
    Other,
}

/// One top-level parameter of a record.
#[derive(Debug, PartialEq)]
enum Param {
    Text(String),
    Ref(u64),
    Other,
}

struct Usage {
    instance: Option<String>,
    parent: u64,
    child: u64,
}

struct Entities {
    limits: Limits,
    /// Records and text bytes kept so far.
    records: usize,
    text: usize,
    /// Entity id to (product id, name).
    products: HashMap<u64, (String, String)>,
    /// Formation to its product.
    formations: HashMap<u64, u64>,
    /// Definition to its formation, in entity order.
    definitions: BTreeMap<u64, u64>,
    /// Keyed by the occurrence's entity id, so children keep file order.
    usages: BTreeMap<u64, Usage>,
}

impl Entities {
    fn new(limits: Limits) -> Self {
        Self {
            limits,
            records: 0,
            text: 0,
            products: HashMap::new(),
            formations: HashMap::new(),
            definitions: BTreeMap::new(),
            usages: BTreeMap::new(),
        }
    }

    /// Keeps one record, counting it and its text against the limits first.
    fn add(&mut self, id: u64, kind: Kind, params: &[Param]) -> Result<(), StepError> {
        let text = |index: usize| match params.get(index) {
            Some(Param::Text(value)) => Some(value.clone()),
            _ => None,
        };
        let reference = |index: usize| match params.get(index) {
            Some(Param::Ref(value)) => Some(*value),
            _ => None,
        };
        let mut keep = |bytes: usize| {
            self.records += 1;
            self.text += bytes;
            if self.records > self.limits.max_records || self.text > self.limits.max_kept_text {
                return Err(StepError::TooLarge);
            }
            Ok(())
        };
        match kind {
            Kind::Product => {
                if let (Some(product_id), Some(name)) = (text(0), text(1)) {
                    keep(product_id.len() + name.len())?;
                    self.products.insert(id, (product_id, name));
                }
            }
            Kind::Formation => {
                if let Some(product) = reference(2) {
                    keep(0)?;
                    self.formations.insert(id, product);
                }
            }
            Kind::Definition => {
                if let Some(formation) = reference(2) {
                    keep(0)?;
                    self.definitions.insert(id, formation);
                }
            }
            Kind::Usage => {
                if let (Some(parent), Some(child)) = (reference(3), reference(4)) {
                    let instance = [text(1), text(0)]
                        .into_iter()
                        .flatten()
                        .find(|value| !value.is_empty());
                    keep(instance.as_ref().map_or(0, String::len))?;
                    let usage = Usage {
                        instance,
                        parent,
                        child,
                    };
                    self.usages.insert(id, usage);
                }
            }
        }
        Ok(())
    }

    /// The product a definition names, through its formation.
    fn product(&self, definition: u64) -> Option<&(String, String)> {
        let formation = self.definitions.get(&definition)?;
        self.products.get(self.formations.get(formation)?)
    }

    fn tree(&self, cancelled: &AtomicBool) -> Result<PartTree, StepError> {
        let mut children: HashMap<u64, Vec<&Usage>> = HashMap::new();
        let mut placed = BTreeSet::new();
        for usage in self.usages.values() {
            if self.product(usage.parent).is_some() && self.product(usage.child).is_some() {
                children.entry(usage.parent).or_default().push(usage);
                placed.insert(usage.child);
            }
        }
        let mut build = Build {
            entities: self,
            children: &children,
            cancelled,
            path: Vec::new(),
            count: 0,
            text: 0,
        };
        let mut roots = Vec::new();
        for &definition in self.definitions.keys() {
            if self.product(definition).is_some() && !placed.contains(&definition) {
                roots.push(build.node(definition, None)?);
            }
        }
        if roots.is_empty() {
            // Every definition is placed inside another: a cycle, or nothing.
            return Err(if placed.is_empty() {
                StepError::NoProducts
            } else {
                StepError::Cycle
            });
        }
        Ok(PartTree { roots })
    }
}

struct Build<'a> {
    entities: &'a Entities,
    children: &'a HashMap<u64, Vec<&'a Usage>>,
    cancelled: &'a AtomicBool,
    /// The definitions from the root down to the node being built.
    path: Vec<u64>,
    /// Nodes and text bytes built so far.
    count: usize,
    text: usize,
}

impl Build<'_> {
    fn node(&mut self, definition: u64, instance: Option<String>) -> Result<PartNode, StepError> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(StepError::Cancelled);
        }
        if self.path.contains(&definition) {
            return Err(StepError::Cycle);
        }
        let (product_id, name) = self
            .entities
            .product(definition)
            .ok_or(StepError::NoProducts)?;
        let name = if name.is_empty() { product_id } else { name };
        let limits = self.entities.limits;
        self.count += 1;
        self.text += name.len() + product_id.len() + instance.as_ref().map_or(0, String::len);
        // `path` holds the node's ancestors, so the node is on level len + 1.
        if self.count > limits.max_parts
            || self.path.len() >= limits.max_levels
            || self.text > limits.max_tree_text
        {
            return Err(StepError::TooLarge);
        }
        self.path.push(definition);
        let mut children = Vec::new();
        for usage in self.children.get(&definition).into_iter().flatten() {
            children.push(self.node(usage.child, usage.instance.clone())?);
        }
        self.path.pop();
        Ok(PartNode {
            name: name.clone(),
            product_id: product_id.clone(),
            instance,
            children,
        })
    }
}

/// Reads ISO 10303-21 statements one at a time. Strings (`'...'`, with `''`
/// for a quote) and comments (`/* ... */`) may hold `;`, so both are tracked
/// while looking for a statement's end.
struct Scanner<'a, R> {
    input: R,
    cancelled: &'a AtomicBool,
    max_record_bytes: usize,
    buffer: Box<[u8]>,
    start: usize,
    end: usize,
}

impl<'a, R: Read> Scanner<'a, R> {
    fn new(input: R, cancelled: &'a AtomicBool, max_record_bytes: usize) -> Self {
        Self {
            input,
            cancelled,
            max_record_bytes,
            buffer: vec![0; READ_CHUNK].into_boxed_slice(),
            start: 0,
            end: 0,
        }
    }

    /// The next byte without taking it. Each refill checks the cancel flag
    /// first, so a long record or comment cannot delay cancellation.
    fn peek(&mut self) -> Result<Option<u8>, StepError> {
        if self.start == self.end {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(StepError::Cancelled);
            }
            self.end = loop {
                match self.input.read(&mut self.buffer) {
                    Ok(read) => break read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            };
            self.start = 0;
            if self.end == 0 {
                return Ok(None);
            }
        }
        Ok(Some(self.buffer[self.start]))
    }

    fn next(&mut self) -> Result<Option<u8>, StepError> {
        let byte = self.peek()?;
        if byte.is_some() {
            self.start += 1;
        }
        Ok(byte)
    }

    /// Skips whitespace and comments; answers the next byte without taking it.
    fn skip_blank(&mut self) -> Result<Option<u8>, StepError> {
        loop {
            match self.peek()? {
                Some(byte) if byte.is_ascii_whitespace() => {
                    self.start += 1;
                }
                Some(b'/') => {
                    self.start += 1;
                    if self.peek()? != Some(b'*') {
                        return Ok(Some(b'/'));
                    }
                    self.start += 1;
                    self.skip_comment()?;
                }
                other => return Ok(other),
            }
        }
    }

    /// Consumes a comment up to its `*/`. An unclosed comment is incomplete.
    fn skip_comment(&mut self) -> Result<(), StepError> {
        let mut star = false;
        while let Some(byte) = self.next()? {
            if star && byte == b'/' {
                return Ok(());
            }
            star = byte == b'*';
        }
        Err(StepError::Incomplete)
    }

    fn starts_with_magic(&mut self) -> Result<bool, StepError> {
        self.skip_blank()?;
        for expected in b"ISO-10303-21" {
            if self.next()? != Some(*expected) {
                return Ok(false);
            }
        }
        Ok(self.skip_blank()? == Some(b';'))
    }

    /// Reads the bytes of a name, `[A-Za-z0-9_-]`, uppercased; at most 64.
    fn word(&mut self) -> Result<Vec<u8>, StepError> {
        let mut word = Vec::new();
        while let Some(byte) = self.peek()? {
            if !(byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-') || word.len() > 64 {
                break;
            }
            word.push(byte.to_ascii_uppercase());
            self.start += 1;
        }
        Ok(word)
    }

    /// The next statement, or `None` at the end of the file.
    fn next_statement(&mut self) -> Result<Option<Statement>, StepError> {
        let Some(first) = self.skip_blank()? else {
            return Ok(None);
        };
        if first != b'#' {
            let word = self.word()?;
            self.rest(None)?;
            return Ok(Some(Statement::Keyword(word)));
        }
        self.start += 1;
        let mut id = 0u64;
        let mut digits = 0;
        while let Some(byte @ b'0'..=b'9') = self.peek()? {
            self.start += 1;
            id = id.saturating_mul(10).saturating_add(u64::from(byte - b'0'));
            digits += 1;
        }
        if digits == 0 || self.skip_blank()? != Some(b'=') {
            self.rest(None)?;
            return Ok(Some(Statement::Other));
        }
        self.start += 1;
        self.skip_blank()?;
        let keyword = self.word()?;
        let Some(kind) = Kind::of(&keyword) else {
            self.rest(None)?;
            return Ok(Some(Statement::Other));
        };
        let mut body = Vec::new();
        self.rest(Some(&mut body))?;
        Ok(Some(Statement::Entity(id, kind, body)))
    }

    /// Consumes up to and including the statement's `;`, copying the bytes
    /// into `keep` when given. The file ending first is
    /// [`StepError::Incomplete`]; a kept statement over the record cap is
    /// [`StepError::TooLarge`].
    fn rest(&mut self, mut keep: Option<&mut Vec<u8>>) -> Result<(), StepError> {
        let mut quoted = false;
        while let Some(byte) = self.next()? {
            if !quoted && byte == b';' {
                return Ok(());
            }
            if !quoted && byte == b'/' && self.peek()? == Some(b'*') {
                self.start += 1;
                self.skip_comment()?;
                continue;
            }
            if byte == b'\'' {
                quoted = !quoted;
            }
            if let Some(kept) = keep.as_deref_mut() {
                if kept.len() == self.max_record_bytes {
                    return Err(StepError::TooLarge);
                }
                kept.push(byte);
            }
        }
        Err(StepError::Incomplete)
    }
}

/// The top-level parameters of `body`, the text after an entity's keyword:
/// `('id', 'name', #12, $, (#1, #2), .T.)`.
fn params(body: &[u8]) -> Vec<Param> {
    let mut params = Vec::new();
    let mut index = match body.iter().position(|byte| *byte == b'(') {
        Some(open) => open + 1,
        None => return params,
    };
    let mut depth = 0usize;
    let mut current: Option<Param> = None;
    while index < body.len() {
        let byte = body[index];
        index += 1;
        match byte {
            b'\'' => {
                let mut raw = Vec::new();
                while index < body.len() {
                    if body[index] == b'\'' {
                        if body.get(index + 1) == Some(&b'\'') {
                            raw.push(b'\'');
                            index += 2;
                            continue;
                        }
                        index += 1;
                        break;
                    }
                    raw.push(body[index]);
                    index += 1;
                }
                if depth == 0 {
                    current = Some(Param::Text(decode(&String::from_utf8_lossy(&raw))));
                }
            }
            b'#' if depth == 0 => {
                let digits = body[index..]
                    .iter()
                    .take_while(|byte| byte.is_ascii_digit())
                    .count();
                current = std::str::from_utf8(&body[index..index + digits])
                    .ok()
                    .and_then(|text| text.parse().ok())
                    .map_or(Some(Param::Other), |id| Some(Param::Ref(id)));
                index += digits;
            }
            b'(' => depth += 1,
            b')' if depth == 0 => {
                params.push(current.take().unwrap_or(Param::Other));
                return params;
            }
            b')' => depth -= 1,
            b',' if depth == 0 => params.push(current.take().unwrap_or(Param::Other)),
            _ => {}
        }
    }
    params
}

/// Decodes the control directives of a STEP string: `\X2\` and `\X4\` runs of
/// UTF-16 and UTF-32 hex ended by `\X0\`, `\X\HH` for one ISO 8859-1 byte,
/// `\S\c` for the upper half of the code page, and `\\` for a backslash. A
/// `\P..\` page switch is dropped. Anything malformed is kept as written.
fn decode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let consumed = directive(rest, &mut out);
        if consumed == 0 {
            out.push('\\');
            rest = &rest[1..];
        } else {
            rest = &rest[consumed..];
        }
    }
    out.push_str(rest);
    out
}

/// Decodes one directive at the start of `text` into `out` and answers how
/// many bytes it used, or 0 when `text` does not start with one.
fn directive(text: &str, out: &mut String) -> usize {
    let hex = |digits: &str| u32::from_str_radix(digits, 16).ok();
    let bytes = text.as_bytes();
    if text.starts_with("\\\\") {
        out.push('\\');
        return 2;
    }
    if let Some(after) = text.strip_prefix("\\X\\")
        && let Some(value) = after.get(..2).and_then(hex)
    {
        out.extend(char::from_u32(value));
        return 5;
    }
    if let Some(after) = text.strip_prefix("\\S\\")
        && let Some(c) = after.chars().next()
        && c.is_ascii()
    {
        out.extend(char::from_u32(c as u32 + 128));
        return 4;
    }
    if bytes.len() >= 4 && bytes[1] == b'P' && bytes[3] == b'\\' {
        return 4;
    }
    for (prefix, width) in [("\\X2\\", 4), ("\\X4\\", 8)] {
        let Some(after) = text.strip_prefix(prefix) else {
            continue;
        };
        let Some(end) = after.find("\\X0\\") else {
            return 0;
        };
        let run = &after[..end];
        if run.len() % width != 0 || !run.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return 0;
        }
        let units: Vec<u32> = (0..run.len())
            .step_by(width)
            .filter_map(|at| hex(&run[at..at + width]))
            .collect();
        if width == 4 {
            let utf16: Vec<u16> = units.iter().map(|unit| *unit as u16).collect();
            out.extend(char::decode_utf16(utf16).map(|c| c.unwrap_or('\u{fffd}')));
        } else {
            out.extend(
                units
                    .into_iter()
                    .map(|unit| char::from_u32(unit).unwrap_or('\u{fffd}')),
            );
        }
        return prefix.len() + end + 4;
    }
    0
}

#[cfg(test)]
mod tests;
