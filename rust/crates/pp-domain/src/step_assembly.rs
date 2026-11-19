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
//! skipped as it streams past, so memory follows the number of parts, not the
//! file size.

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
/// The longest product-structure record buffered. Geometry records are never
/// buffered, so this bounds only names and ids.
const MAX_RECORD_BYTES: usize = 64 * 1024;

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
    /// No product definition resolves to a product.
    NoProducts,
    /// Over [`MAX_PARTS`] nodes or [`MAX_LEVELS`] levels.
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

/// Reads the part tree of the STEP file `input`. `cancelled` is checked
/// before each record; setting it ends the read with [`StepError::Cancelled`].
pub fn part_tree(input: impl Read, cancelled: &AtomicBool) -> Result<PartTree, StepError> {
    let mut scanner = Scanner::new(input);
    if !scanner.starts_with_magic()? {
        return Err(StepError::NotStep);
    }
    let mut entities = Entities::default();
    while let Some(record) = scanner.next_record()? {
        if cancelled.load(Ordering::Relaxed) {
            return Err(StepError::Cancelled);
        }
        if let Some((id, kind, body)) = record {
            entities.add(id, kind, &params(&body));
        }
    }
    entities.tree()
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

/// A product-structure record: its entity id, kind, and parameter text.
type Entity = (u64, Kind, Vec<u8>);

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

#[derive(Default)]
struct Entities {
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
    fn add(&mut self, id: u64, kind: Kind, params: &[Param]) {
        let text = |index: usize| match params.get(index) {
            Some(Param::Text(value)) => Some(value.clone()),
            _ => None,
        };
        let reference = |index: usize| match params.get(index) {
            Some(Param::Ref(value)) => Some(*value),
            _ => None,
        };
        match kind {
            Kind::Product => {
                if let (Some(product_id), Some(name)) = (text(0), text(1)) {
                    self.products.insert(id, (product_id, name));
                }
            }
            Kind::Formation => {
                if let Some(product) = reference(2) {
                    self.formations.insert(id, product);
                }
            }
            Kind::Definition => {
                if let Some(formation) = reference(2) {
                    self.definitions.insert(id, formation);
                }
            }
            Kind::Usage => {
                if let (Some(parent), Some(child)) = (reference(3), reference(4)) {
                    let instance = [text(1), text(0)]
                        .into_iter()
                        .flatten()
                        .find(|value| !value.is_empty());
                    self.usages.insert(
                        id,
                        Usage {
                            instance,
                            parent,
                            child,
                        },
                    );
                }
            }
        }
    }

    /// The product a definition names, through its formation.
    fn product(&self, definition: u64) -> Option<&(String, String)> {
        let formation = self.definitions.get(&definition)?;
        self.products.get(self.formations.get(formation)?)
    }

    fn tree(&self) -> Result<PartTree, StepError> {
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
            path: Vec::new(),
            count: 0,
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
    /// The definitions from the root down to the node being built.
    path: Vec<u64>,
    count: usize,
}

impl Build<'_> {
    fn node(&mut self, definition: u64, instance: Option<String>) -> Result<PartNode, StepError> {
        if self.path.contains(&definition) {
            return Err(StepError::Cycle);
        }
        self.count += 1;
        // `path` holds the node's ancestors, so the node is on level len + 1.
        if self.count > MAX_PARTS || self.path.len() >= MAX_LEVELS {
            return Err(StepError::TooLarge);
        }
        let (product_id, name) = self
            .entities
            .product(definition)
            .ok_or(StepError::NoProducts)?;
        self.path.push(definition);
        let mut children = Vec::new();
        for usage in self.children.get(&definition).into_iter().flatten() {
            children.push(self.node(usage.child, usage.instance.clone())?);
        }
        self.path.pop();
        Ok(PartNode {
            name: if name.is_empty() {
                product_id.clone()
            } else {
                name.clone()
            },
            product_id: product_id.clone(),
            instance,
            children,
        })
    }
}

/// Reads ISO 10303-21 records one at a time. Strings (`'...'`, with `''` for
/// a quote) and comments (`/* ... */`) may hold `;`, so both are tracked
/// while looking for a record's end.
struct Scanner<R> {
    input: R,
    buffer: Box<[u8]>,
    start: usize,
    end: usize,
}

impl<R: Read> Scanner<R> {
    fn new(input: R) -> Self {
        Self {
            input,
            buffer: vec![0; 64 * 1024].into_boxed_slice(),
            start: 0,
            end: 0,
        }
    }

    fn peek(&mut self) -> io::Result<Option<u8>> {
        if self.start == self.end {
            self.end = loop {
                match self.input.read(&mut self.buffer) {
                    Ok(read) => break read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error),
                }
            };
            self.start = 0;
            if self.end == 0 {
                return Ok(None);
            }
        }
        Ok(Some(self.buffer[self.start]))
    }

    fn next(&mut self) -> io::Result<Option<u8>> {
        let byte = self.peek()?;
        if byte.is_some() {
            self.start += 1;
        }
        Ok(byte)
    }

    /// Skips whitespace and comments; answers the next byte without taking it.
    fn skip_blank(&mut self) -> io::Result<Option<u8>> {
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

    fn skip_comment(&mut self) -> io::Result<()> {
        let mut star = false;
        while let Some(byte) = self.next()? {
            if star && byte == b'/' {
                return Ok(());
            }
            star = byte == b'*';
        }
        Ok(())
    }

    fn starts_with_magic(&mut self) -> io::Result<bool> {
        self.skip_blank()?;
        for expected in b"ISO-10303-21" {
            if self.next()? != Some(*expected) {
                return Ok(false);
            }
        }
        Ok(self.skip_blank()? == Some(b';'))
    }

    /// The next record: `Some(Some(..))` for a product-structure entity with
    /// its parameter text, `Some(None)` for any other record, `None` at the
    /// end of the file.
    fn next_record(&mut self) -> io::Result<Option<Option<Entity>>> {
        let Some(first) = self.skip_blank()? else {
            return Ok(None);
        };
        if first != b'#' {
            self.rest(None)?;
            return Ok(Some(None));
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
            return Ok(Some(None));
        }
        self.start += 1;
        self.skip_blank()?;
        let mut keyword = Vec::new();
        while let Some(byte) = self.peek()? {
            if !(byte.is_ascii_alphanumeric() || byte == b'_') || keyword.len() > 64 {
                break;
            }
            keyword.push(byte.to_ascii_uppercase());
            self.start += 1;
        }
        let Some(kind) = Kind::of(&keyword) else {
            self.rest(None)?;
            return Ok(Some(None));
        };
        let mut body = Vec::new();
        let whole = self.rest(Some(&mut body))?;
        Ok(Some(whole.then_some((id, kind, body))))
    }

    /// Consumes up to and including the record's `;`, copying the bytes into
    /// `keep` when given. Answers false when `keep` would pass
    /// [`MAX_RECORD_BYTES`]; the rest of that record is then skipped.
    fn rest(&mut self, mut keep: Option<&mut Vec<u8>>) -> io::Result<bool> {
        let mut quoted = false;
        let mut whole = true;
        while let Some(byte) = self.next()? {
            if !quoted && byte == b';' {
                return Ok(whole);
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
                if kept.len() == MAX_RECORD_BYTES {
                    whole = false;
                    keep = None;
                } else {
                    kept.push(byte);
                }
            }
        }
        Ok(false)
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
