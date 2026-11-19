//! The product structure of a STEP file (ISO 10303-21), checked against an
//! input contract (GRE-1007).
//!
//! A STEP assembly tree is defined by four record kinds: each
//! `PRODUCT_DEFINITION` points through a `PRODUCT_DEFINITION_FORMATION` to its
//! `PRODUCT`, and each `NEXT_ASSEMBLY_USAGE_OCCURRENCE` places one definition
//! inside another. [`product_structure`] reads the file once, checks every
//! statement against the contract below, keeps those four kinds and drops the
//! rest (the geometry) as it streams past. GRE-1855 builds the tree.
//!
//! # Input contract
//!
//! The reader accepts this exchange and nothing else:
//!
//! ```text
//! ISO-10303-21;
//! HEADER; FILE_DESCRIPTION(..); FILE_NAME(..); FILE_SCHEMA(..); <header entities> ENDSEC;
//! DATA; <entity instances> ENDSEC;      one or more; DATA may carry a parameter list
//! END-ISO-10303-21;
//! ```
//!
//! starting at the file's first byte and followed only by whitespace and
//! closed comments.
//!
//! - Whitespace is space, tab, CR and LF. A comment `/* .. */` may stand
//!   wherever whitespace may.
//! - A keyword is `[A-Z_][A-Z0-9_]*`, or a user-defined `!` and the same.
//! - A header entity is a keyword and a parameter list. The three required
//!   ones come first, in that order.
//! - An entity instance is `#N = KEYWORD(..);`, or a complex instance
//!   `#N = (A(..) B(..) ..);`. `N` is a decimal number below 2^64.
//! - A parameter is `$` (omitted), `*` (derived), an integer, a real (`1.`,
//!   `-2.5E-3`), a string, an enumeration (`.T.`), a binary (`"0F"`), a
//!   reference `#N`, a list `(a, b, ..)` (possibly empty), or a typed
//!   parameter `KEYWORD(value)`. Lists nest.
//! - A string is `'..'` with `''` for a quote. CR and LF inside it are line
//!   folding and are dropped; any other control byte is refused. In a kept
//!   record, the string's other bytes must be UTF-8, and its directives are
//!   decoded: `\X\HH`, `\X2\..\X0\` (UTF-16), `\X4\..\X0\`, `\S\c`, `\PA\`
//!   and `\\`. Strings in dropped records are checked for control bytes and
//!   length only.
//!
//! The kept records must have these parameters, and every reference must
//! name a kept record of the kind shown, or the read fails as
//! [`StepError::InvalidStructure`]:
//!
//! | Record | Parameters | Read |
//! | --- | --- | --- |
//! | `PRODUCT` | 4 | id (string), name (string or `$`) |
//! | `PRODUCT_DEFINITION_FORMATION` | 3 | third: `#N` of a `PRODUCT` |
//! | `.._WITH_SPECIFIED_SOURCE` | 4 | as above |
//! | `PRODUCT_DEFINITION` | 4 | third: `#N` of a formation |
//! | `.._WITH_ASSOCIATED_DOCUMENTS` | 5 | as above |
//! | `NEXT_ASSEMBLY_USAGE_OCCURRENCE` | 6 | id and name (string or `$`); fourth (parent) and fifth (child): `#N` of a definition |
//!
//! Everything else is refused with a typed [`StepError`] before anything is
//! returned. The reader never repairs a file, guesses, or returns part of one.
//!
//! Only kept records' instance numbers are checked for reuse: storing every
//! number would cost memory per record, and a dropped record cannot change
//! the result, since a reference to anything but a kept record is refused.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

/// The longest keyword, in bytes; the longest AP242 entity name is under 70.
const MAX_KEYWORD: usize = 128;
/// How many bytes are read between checks of the cancel flag.
const READ_CHUNK: usize = 64 * 1024;

/// Bounds on one read, each checked per byte taken: no refusal comes more
/// than one 64 KiB read past the first byte over its cap, at most one byte
/// is read past the file cap, and nothing over a cap is retained. Numbers,
/// enumerations, binaries and blanks are not buffered; outside a kept record
/// only the file cap bounds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes in the file.
    pub max_file_bytes: u64,
    /// Entity instances in the DATA sections, kept or not.
    pub max_entities: u64,
    /// Nesting of one record's parameter lists; its own list is level 1.
    pub max_nesting: usize,
    /// Bytes between a string's quotes, as written.
    pub max_string_bytes: usize,
    /// Bytes in one comment, `/*` through `*/`.
    pub max_comment_bytes: usize,
    /// Bytes in one kept record, `#` through `;`, comments included. Every
    /// record is held to it until its keyword shows whether it is kept.
    pub max_record_bytes: usize,
    /// Kept records, checked at the keyword of the one over the cap.
    pub max_records: usize,
    /// Bytes of the strings in kept records, as buffered.
    pub max_kept_text: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 2 << 30,
            max_entities: 50_000_000,
            max_nesting: 32,
            max_string_bytes: 1 << 20,
            max_comment_bytes: 64 << 10,
            max_record_bytes: 64 << 10,
            max_records: 400_000,
            max_kept_text: 16 << 20,
        }
    }
}

/// Which bound a [`StepError::TooLarge`] read passed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Limit {
    FileBytes,
    Entities,
    Nesting,
    StringBytes,
    CommentBytes,
    RecordBytes,
    Records,
    KeptText,
    /// A keyword over 128 bytes.
    KeywordBytes,
    /// An instance number of 2^64 or more.
    InstanceNumber,
}

#[derive(Debug)]
pub enum StepError {
    Io(io::Error),
    /// The file does not start with `ISO-10303-21;`.
    NotStep,
    /// The file ends before `END-ISO-10303-21;`: inside a token, a string,
    /// a comment, a statement or a section.
    Incomplete,
    /// Not in the grammar of the input contract, at this byte offset; also a
    /// kept instance number used twice.
    Malformed {
        at: u64,
    },
    /// An edition 3 form this reader does not read, at this byte offset (see
    /// the module docs).
    Unsupported {
        at: u64,
    },
    /// This kept record has the wrong parameters, or refers to a record that
    /// is missing or of the wrong kind.
    InvalidStructure {
        entity: u64,
    },
    /// Over this one of the [`Limits`].
    TooLarge(Limit),
    Cancelled,
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::NotStep => f.write_str("not a STEP file"),
            Self::Incomplete => f.write_str("the STEP file is incomplete"),
            Self::Malformed { at } => write!(f, "the STEP file is not well formed at byte {at}"),
            Self::Unsupported { at } => write!(f, "unsupported STEP form at byte {at}"),
            Self::InvalidStructure { entity } => write!(f, "invalid product record #{entity}"),
            Self::TooLarge(limit) => write!(f, "over the {limit:?} limit"),
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

/// The kept records of one STEP file, every reference checked.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProductStructure {
    /// Entity id to (product id, name); a `$` name is empty.
    products: HashMap<u64, (String, String)>,
    /// Formation to its product.
    formations: HashMap<u64, u64>,
    /// Definition to its formation, in entity order.
    definitions: BTreeMap<u64, u64>,
    /// In entity order.
    usages: BTreeMap<u64, Usage>,
}

/// One `NEXT_ASSEMBLY_USAGE_OCCURRENCE`.
#[derive(Debug, PartialEq, Eq)]
struct Usage {
    /// Its name, or its id when the name is empty; `None` when both are.
    instance: Option<String>,
    parent: u64,
    child: u64,
}

/// Reads and checks the product structure of the STEP file `input`.
/// `cancelled` is checked before every 64 KiB read; setting it ends the read
/// with [`StepError::Cancelled`].
pub fn product_structure(
    input: impl Read,
    limits: Limits,
    cancelled: &AtomicBool,
) -> Result<ProductStructure, StepError> {
    let mut parser = Parser {
        lexer: Lexer::new(input, cancelled, limits),
        structure: ProductStructure::default(),
        ids: HashSet::new(),
        instances: 0,
        records: 0,
    };
    parser.exchange()?;
    parser.structure.check_references()?;
    Ok(parser.structure)
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
    /// The kind a keyword names, and how many parameters its record has.
    fn of(keyword: &[u8]) -> Option<(Self, usize)> {
        Some(match keyword {
            b"PRODUCT" => (Self::Product, 4),
            b"PRODUCT_DEFINITION_FORMATION" => (Self::Formation, 3),
            b"PRODUCT_DEFINITION_FORMATION_WITH_SPECIFIED_SOURCE" => (Self::Formation, 4),
            b"PRODUCT_DEFINITION" => (Self::Definition, 4),
            b"PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS" => (Self::Definition, 5),
            b"NEXT_ASSEMBLY_USAGE_OCCURRENCE" => (Self::Usage, 6),
            _ => return None,
        })
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Keyword(Vec<u8>),
    /// `END-ISO-10303-21`.
    End,
    /// `#N`, as an instance's name or a reference.
    Instance(u64),
    /// A string, decoded inside a kept record and empty elsewhere.
    Text(String),
    /// `$`.
    Null,
    /// `*`, an integer, a real, an enumeration or a binary.
    Value,
    Open,
    Close,
    Comma,
    Equals,
    Semicolon,
}

/// One top-level parameter of a kept record.
#[derive(Debug, PartialEq)]
enum Param {
    Text(String),
    Ref(u64),
    Null,
    Other,
}

/// Where a parameter list is: just opened, after a `,`, or after a value.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    Opened,
    Comma,
    Value,
}

struct Parser<'a, R> {
    lexer: Lexer<'a, R>,
    structure: ProductStructure,
    /// Every kept instance number, to refuse one used twice.
    ids: HashSet<u64>,
    /// Entity instances read, and records and text bytes kept.
    instances: u64,
    records: usize,
}

impl<R: Read> Parser<'_, R> {
    fn token(&mut self) -> Result<Token, StepError> {
        self.lexer.token()?.ok_or(StepError::Incomplete)
    }

    fn malformed(&self) -> StepError {
        StepError::Malformed { at: self.lexer.at }
    }

    fn eat(&mut self, want: Token) -> Result<(), StepError> {
        if self.token()? == want {
            Ok(())
        } else {
            Err(self.malformed())
        }
    }

    /// The whole exchange, from its first byte through the blanks after its
    /// end.
    fn exchange(&mut self) -> Result<(), StepError> {
        if !self.lexer.starts_with_magic()? {
            return Err(StepError::NotStep);
        }
        self.eat(Token::Keyword(b"HEADER".to_vec()))?;
        self.eat(Token::Semicolon)?;
        for name in [&b"FILE_DESCRIPTION"[..], b"FILE_NAME", b"FILE_SCHEMA"] {
            self.eat(Token::Keyword(name.to_vec()))?;
            self.skip_list()?;
        }
        loop {
            match self.token()? {
                Token::Keyword(word) if word == b"ENDSEC" => break,
                Token::Keyword(_) => self.skip_list()?,
                _ => return Err(self.malformed()),
            }
        }
        self.eat(Token::Semicolon)?;
        let mut data = false;
        loop {
            match self.token()? {
                Token::Keyword(word) if word == b"DATA" => {
                    match self.token()? {
                        Token::Semicolon => {}
                        Token::Open => {
                            self.list(false)?;
                            self.eat(Token::Semicolon)?;
                        }
                        _ => return Err(self.malformed()),
                    }
                    self.data()?;
                    data = true;
                }
                Token::Keyword(word) if word == b"ANCHOR" || word == b"REFERENCE" => {
                    return Err(StepError::Unsupported { at: self.lexer.at });
                }
                Token::End if data => break,
                _ => return Err(self.malformed()),
            }
        }
        self.eat(Token::Semicolon)?;
        match self.lexer.token()? {
            None => Ok(()),
            Some(Token::Keyword(word)) if word == b"SIGNATURE" => {
                Err(StepError::Unsupported { at: self.lexer.at })
            }
            Some(_) => Err(self.malformed()),
        }
    }

    /// The entity instances of a DATA section, through its `ENDSEC;`.
    fn data(&mut self) -> Result<(), StepError> {
        loop {
            // Every record is capped as kept from its `#` until its keyword
            // shows the kind, so its instance number counts too.
            self.lexer.cap_record_at_hash()?;
            let id = match self.token()? {
                Token::Instance(id) => id,
                Token::Keyword(word) if word == b"ENDSEC" => {
                    return self.eat(Token::Semicolon);
                }
                _ => return Err(self.malformed()),
            };
            let begin = self.lexer.at;
            self.instances += 1;
            if self.instances > self.lexer.limits.max_entities {
                return Err(StepError::TooLarge(Limit::Entities));
            }
            self.eat(Token::Equals)?;
            let token = self.token()?;
            let kind = match &token {
                Token::Keyword(word) => Kind::of(word),
                _ => None,
            };
            if kind.is_none() {
                self.lexer.record_end = None;
            } else if self.records == self.lexer.limits.max_records {
                return Err(StepError::TooLarge(Limit::Records));
            }
            match token {
                Token::Keyword(_) => {
                    self.eat(Token::Open)?;
                    let params = self.list(kind.is_some())?;
                    self.eat(Token::Semicolon)?;
                    self.lexer.record_end = None;
                    if let Some((kind, arity)) = kind {
                        self.keep(id, begin, kind, arity, params)?;
                    }
                }
                Token::Open => {
                    let mut parts = false;
                    loop {
                        match self.token()? {
                            Token::Keyword(word) if Kind::of(&word).is_some() => {
                                return Err(StepError::Unsupported { at: self.lexer.at });
                            }
                            Token::Keyword(_) => {
                                self.eat(Token::Open)?;
                                self.list(false)?;
                                parts = true;
                            }
                            Token::Close if parts => break,
                            _ => return Err(self.malformed()),
                        }
                    }
                    self.eat(Token::Semicolon)?;
                }
                _ => return Err(self.malformed()),
            }
        }
    }

    /// Reads `(..);` and drops it.
    fn skip_list(&mut self) -> Result<(), StepError> {
        self.eat(Token::Open)?;
        self.list(false)?;
        self.eat(Token::Semicolon)
    }

    /// Reads a parameter list whose `(` was just read, through its `)`.
    /// Answers its top-level parameters when `keep`.
    fn list(&mut self, keep: bool) -> Result<Vec<Param>, StepError> {
        // One entry per open list: true for a typed parameter's, which holds
        // exactly one value.
        let mut open = Vec::new();
        self.nest(&mut open, false)?;
        let mut place = Place::Opened;
        let mut params = Vec::new();
        loop {
            let typed = open.last() == Some(&true);
            let value = match (place, self.token()?) {
                (Place::Value, Token::Comma) if !typed => {
                    place = Place::Comma;
                    continue;
                }
                (Place::Value, Token::Close) | (Place::Opened, Token::Close)
                    if !typed || place == Place::Value =>
                {
                    open.pop();
                    if open.is_empty() {
                        return Ok(params);
                    }
                    Param::Other
                }
                (Place::Value, _) => return Err(self.malformed()),
                (_, Token::Open) => {
                    self.nest(&mut open, false)?;
                    place = Place::Opened;
                    continue;
                }
                (_, Token::Keyword(_)) => {
                    self.eat(Token::Open)?;
                    self.nest(&mut open, true)?;
                    place = Place::Opened;
                    continue;
                }
                (_, Token::Text(text)) => Param::Text(text),
                (_, Token::Instance(id)) => Param::Ref(id),
                (_, Token::Null) => Param::Null,
                (_, Token::Value) => Param::Other,
                _ => return Err(self.malformed()),
            };
            if keep && open.len() == 1 {
                params.push(value);
            }
            place = Place::Value;
        }
    }

    fn nest(&self, open: &mut Vec<bool>, typed: bool) -> Result<(), StepError> {
        if open.len() >= self.lexer.limits.max_nesting {
            return Err(StepError::TooLarge(Limit::Nesting));
        }
        open.push(typed);
        Ok(())
    }

    /// Keeps one record, read at byte `at`, after checking its parameters.
    fn keep(
        &mut self,
        id: u64,
        at: u64,
        kind: Kind,
        arity: usize,
        params: Vec<Param>,
    ) -> Result<(), StepError> {
        if !self.ids.insert(id) {
            return Err(StepError::Malformed { at });
        }
        let invalid = StepError::InvalidStructure { entity: id };
        if params.len() != arity {
            return Err(invalid);
        }
        // A string, or `$` as an empty one.
        let label = |param: &Param| match param {
            Param::Text(text) => Some(text.clone()),
            Param::Null => Some(String::new()),
            _ => None,
        };
        let reference = |param: &Param| match param {
            Param::Ref(id) => Some(*id),
            _ => None,
        };
        let structure = &mut self.structure;
        match kind {
            Kind::Product => {
                let (Param::Text(product_id), Some(name)) = (&params[0], label(&params[1])) else {
                    return Err(invalid);
                };
                structure.products.insert(id, (product_id.clone(), name));
            }
            Kind::Formation | Kind::Definition => {
                let target = reference(&params[2]).ok_or(invalid)?;
                match kind {
                    Kind::Formation => structure.formations.insert(id, target),
                    _ => structure.definitions.insert(id, target),
                };
            }
            Kind::Usage => {
                let (Some(usage_id), Some(name), Some(parent), Some(child)) = (
                    label(&params[0]),
                    label(&params[1]),
                    reference(&params[3]),
                    reference(&params[4]),
                ) else {
                    return Err(invalid);
                };
                let instance = [name, usage_id].into_iter().find(|value| !value.is_empty());
                let usage = Usage {
                    instance,
                    parent,
                    child,
                };
                structure.usages.insert(id, usage);
            }
        }
        self.records += 1;
        Ok(())
    }
}

impl ProductStructure {
    /// Checks that every reference names a kept record of the right kind.
    fn check_references(&self) -> Result<(), StepError> {
        let invalid = |entity| Err(StepError::InvalidStructure { entity });
        for (&id, product) in &self.formations {
            if !self.products.contains_key(product) {
                return invalid(id);
            }
        }
        for (&id, formation) in &self.definitions {
            if !self.formations.contains_key(formation) {
                return invalid(id);
            }
        }
        for (&id, usage) in &self.usages {
            if ![usage.parent, usage.child]
                .iter()
                .all(|id| self.definitions.contains_key(id))
            {
                return invalid(id);
            }
        }
        Ok(())
    }
}

/// Splits the exchange into tokens, enforcing the byte-level limits as each
/// byte is taken.
struct Lexer<'a, R> {
    input: R,
    cancelled: &'a AtomicBool,
    limits: Limits,
    buffer: Box<[u8]>,
    /// Bytes of the file before `buffer`.
    base: u64,
    start: usize,
    end: usize,
    /// Where the last token started.
    at: u64,
    /// While a kept record is read: the offset it may not pass. Strings are
    /// decoded only then.
    record_end: Option<u64>,
    /// Bytes of strings kept so far.
    kept_text: usize,
}

impl<'a, R: Read> Lexer<'a, R> {
    fn new(input: R, cancelled: &'a AtomicBool, limits: Limits) -> Self {
        Self {
            input,
            cancelled,
            limits,
            buffer: vec![0; READ_CHUNK].into_boxed_slice(),
            base: 0,
            start: 0,
            end: 0,
            at: 0,
            record_end: None,
            kept_text: 0,
        }
    }

    /// The next byte without taking it. Each refill checks the cancel flag
    /// first, so a long record or comment cannot delay cancellation.
    fn peek(&mut self) -> Result<Option<u8>, StepError> {
        if self.start == self.end {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(StepError::Cancelled);
            }
            self.base += self.end as u64;
            // Read at most one byte past the file cap.
            let room = self.limits.max_file_bytes.saturating_sub(self.base);
            let size = room.saturating_add(1).min(READ_CHUNK as u64) as usize;
            self.end = loop {
                match self.input.read(&mut self.buffer[..size]) {
                    Ok(read) => break read,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => return Err(error.into()),
                }
            };
            self.start = 0;
            if self.base + self.end as u64 > self.limits.max_file_bytes {
                return Err(StepError::TooLarge(Limit::FileBytes));
            }
            if self.end == 0 {
                return Ok(None);
            }
        }
        Ok(Some(self.buffer[self.start]))
    }

    /// How many bytes of the file have been taken.
    fn position(&self) -> u64 {
        self.base + self.start as u64
    }

    /// Takes the byte `peek` answered, checking the kept record's cap.
    fn advance(&mut self) -> Result<(), StepError> {
        self.start += 1;
        match self.record_end {
            Some(end) if self.position() > end => Err(StepError::TooLarge(Limit::RecordBytes)),
            _ => Ok(()),
        }
    }

    /// Takes the next byte; the file ending here is [`StepError::Incomplete`].
    fn take(&mut self) -> Result<u8, StepError> {
        let byte = self.peek()?.ok_or(StepError::Incomplete)?;
        self.advance()?;
        Ok(byte)
    }

    /// Skips blanks; when a `#` follows, the record it starts is kept from
    /// that byte until `record_end` is cleared: its bytes are capped and its
    /// strings decoded.
    fn cap_record_at_hash(&mut self) -> Result<(), StepError> {
        self.skip_blank()?;
        if self.peek()? == Some(b'#') {
            let end = self
                .position()
                .saturating_add(self.limits.max_record_bytes as u64);
            self.record_end = Some(end);
        }
        Ok(())
    }

    /// Skips whitespace and comments.
    fn skip_blank(&mut self) -> Result<(), StepError> {
        loop {
            match self.peek()? {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.advance()?,
                Some(b'/') => {
                    self.at = self.position();
                    self.advance()?;
                    if self.take()? != b'*' {
                        return Err(StepError::Malformed { at: self.at });
                    }
                    let (mut length, mut star) = (2, false);
                    loop {
                        let byte = self.take()?;
                        length += 1;
                        if length > self.limits.max_comment_bytes {
                            return Err(StepError::TooLarge(Limit::CommentBytes));
                        }
                        if star && byte == b'/' {
                            break;
                        }
                        star = byte == b'*';
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn starts_with_magic(&mut self) -> Result<bool, StepError> {
        for expected in b"ISO-10303-21" {
            if self.peek()? != Some(*expected) {
                return Ok(false);
            }
            self.advance()?;
        }
        self.skip_blank()?;
        if self.peek()? != Some(b';') {
            return Ok(false);
        }
        self.advance()?;
        Ok(true)
    }

    /// Takes bytes while `pick` accepts them, and answers how many it took.
    /// A run that reaches the end of the file is [`StepError::Incomplete`],
    /// since no valid file ends inside one.
    fn run(&mut self, mut pick: impl FnMut(u8) -> bool) -> Result<usize, StepError> {
        let mut count = 0;
        loop {
            match self.peek()? {
                None => return Err(StepError::Incomplete),
                Some(byte) if pick(byte) => {
                    self.advance()?;
                    count += 1;
                }
                Some(_) => return Ok(count),
            }
        }
    }

    /// The next token, or `None` at the end of the file.
    fn token(&mut self) -> Result<Option<Token>, StepError> {
        self.skip_blank()?;
        self.at = self.position();
        let malformed = StepError::Malformed { at: self.at };
        let Some(first) = self.peek()? else {
            return Ok(None);
        };
        let upper = |byte: u8| byte.is_ascii_uppercase() || byte == b'_';
        let word = |byte: u8| upper(byte) || byte.is_ascii_digit();
        let hex = |byte: u8| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte);
        Ok(Some(match first {
            b';' => self.single(Token::Semicolon)?,
            b'(' => self.single(Token::Open)?,
            b')' => self.single(Token::Close)?,
            b',' => self.single(Token::Comma)?,
            b'=' => self.single(Token::Equals)?,
            b'$' => self.single(Token::Null)?,
            b'*' => self.single(Token::Value)?,
            b'A'..=b'Z' | b'_' | b'!' => self.keyword()?,
            b'#' => {
                self.advance()?;
                if self.peek()?.is_some_and(upper) {
                    return Err(StepError::Unsupported { at: self.at });
                }
                let mut id = Some(0u64);
                let digits = self.run(|byte| {
                    let digit = byte.is_ascii_digit();
                    if digit {
                        let value = u64::from(byte - b'0');
                        id = id.and_then(|id| id.checked_mul(10)?.checked_add(value));
                    }
                    // Stop at the first digit that overflows, without taking it.
                    digit && id.is_some()
                })?;
                match (digits, id) {
                    (0, _) => return Err(malformed),
                    (_, None) => return Err(StepError::TooLarge(Limit::InstanceNumber)),
                    (_, Some(id)) => Token::Instance(id),
                }
            }
            b'\'' => Token::Text(self.string()?),
            b'"' => self.delimited(|byte| matches!(byte, b'0'..=b'3'), hex)?,
            b'.' => self.delimited(upper, word)?,
            b'+' | b'-' | b'0'..=b'9' => {
                self.number()?;
                Token::Value
            }
            b'@' | b'<' => return Err(StepError::Unsupported { at: self.at }),
            _ => return Err(malformed),
        }))
    }

    fn single(&mut self, token: Token) -> Result<Token, StepError> {
        self.advance()?;
        Ok(token)
    }

    /// A binary `"0F"` or an enumeration `.T.`: a delimiter, one byte `first`
    /// accepts, bytes `rest` accepts, and the same delimiter.
    fn delimited(
        &mut self,
        first: impl Fn(u8) -> bool,
        rest: impl FnMut(u8) -> bool,
    ) -> Result<Token, StepError> {
        let malformed = StepError::Malformed { at: self.at };
        let delimiter = self.take()?;
        if !first(self.take()?) {
            return Err(malformed);
        }
        self.run(rest)?;
        if self.take()? != delimiter {
            return Err(malformed);
        }
        Ok(Token::Value)
    }

    /// A keyword, or `END-ISO-10303-21`.
    fn keyword(&mut self) -> Result<Token, StepError> {
        let mut keyword = Vec::new();
        if self.peek()? == Some(b'!') {
            keyword.push(b'!');
            self.advance()?;
        }
        if !matches!(self.peek()?, Some(b'A'..=b'Z' | b'_')) {
            self.run(|_| false)?;
            return Err(StepError::Malformed { at: self.at });
        }
        let mut long = false;
        self.run(|byte| {
            let word = byte.is_ascii_uppercase() || byte == b'_' || byte.is_ascii_digit();
            if word {
                long |= keyword.len() == MAX_KEYWORD;
                if !long {
                    keyword.push(byte);
                }
            }
            word && !long
        })?;
        if long {
            return Err(StepError::TooLarge(Limit::KeywordBytes));
        }
        if keyword == b"END" && self.peek()? == Some(b'-') {
            for expected in b"-ISO-10303-21" {
                if self.take()? != *expected {
                    return Err(StepError::Malformed { at: self.at });
                }
            }
            return Ok(Token::End);
        }
        Ok(Token::Keyword(keyword))
    }

    /// `[+-]digits[.digits[E[+-]digits]]`, starting at its first byte.
    fn number(&mut self) -> Result<(), StepError> {
        let digit = |byte: u8| byte.is_ascii_digit();
        let malformed = StepError::Malformed { at: self.at };
        if matches!(self.peek()?, Some(b'+' | b'-')) {
            self.advance()?;
        }
        if self.run(digit)? == 0 {
            return Err(malformed);
        }
        if self.peek()? == Some(b'.') {
            self.advance()?;
            self.run(digit)?;
            if self.peek()? == Some(b'E') {
                self.advance()?;
                if matches!(self.peek()?, Some(b'+' | b'-')) {
                    self.advance()?;
                }
                if self.run(digit)? == 0 {
                    return Err(malformed);
                }
            }
        }
        Ok(())
    }

    /// A string, starting at its opening quote. Decoded only in a kept record.
    fn string(&mut self) -> Result<String, StepError> {
        let keep = self.record_end.is_some();
        let malformed = StepError::Malformed { at: self.at };
        self.advance()?;
        let (mut raw, mut length) = (Vec::new(), 0);
        loop {
            let byte = self.take()?;
            if byte == b'\'' {
                if self.peek()? != Some(b'\'') {
                    break;
                }
                self.advance()?;
                length += 1;
            }
            length += 1;
            if length > self.limits.max_string_bytes {
                return Err(StepError::TooLarge(Limit::StringBytes));
            }
            match byte {
                b'\r' | b'\n' => continue,
                ..=0x1f | 0x7f => return Err(malformed),
                _ => {}
            }
            if keep {
                if self.kept_text == self.limits.max_kept_text {
                    return Err(StepError::TooLarge(Limit::KeptText));
                }
                self.kept_text += 1;
                raw.push(byte);
            }
        }
        if !keep {
            return Ok(String::new());
        }
        let text = String::from_utf8(raw).map_err(|_| malformed)?;
        decode(&text).map_err(|unsupported| {
            if unsupported {
                StepError::Unsupported { at: self.at }
            } else {
                StepError::Malformed { at: self.at }
            }
        })
    }
}

/// Decodes the control directives of a STEP string: `\X2\` and `\X4\` runs of
/// UTF-16 and UTF-32 hex ended by `\X0\`, `\X\HH` for one ISO 8859-1 byte,
/// `\S\c` for the upper half of ISO 8859-1, `\PA\` (the default page) and `\\`
/// for a backslash. Fails with `true` for another `\P` page and `false` for
/// anything else after a backslash.
fn decode(raw: &str) -> Result<String, bool> {
    // Uppercase ASCII hex, read as bytes so that no slice can split a
    // multi-byte character.
    let hex = |digits: &[u8]| {
        digits.iter().try_fold(0u32, |value, &byte| {
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'A'..=b'F' => byte - b'A' + 10,
                _ => return None,
            };
            Some(value << 4 | u32::from(digit))
        })
    };
    let mut out = String::new();
    let mut rest = raw;
    // Each `\` starts a directive; every branch answers the text after it.
    while let Some((plain, directive)) = rest.split_once('\\') {
        out.push_str(plain);
        rest = if let Some(after) = directive.strip_prefix('\\') {
            out.push('\\');
            after
        } else if let Some(after) = directive.strip_prefix("PA\\") {
            after
        } else if directive.starts_with('P') {
            return Err(true);
        } else if let Some(after) = directive.strip_prefix("X\\") {
            let value = after.as_bytes().get(..2).and_then(hex).ok_or(false)?;
            out.extend(char::from_u32(value));
            after.get(2..).ok_or(false)?
        } else if let Some(after) = directive.strip_prefix("S\\") {
            let byte = after
                .bytes()
                .next()
                .filter(|byte| (0x20..=0x7e).contains(byte));
            out.extend(char::from_u32(u32::from(byte.ok_or(false)?) + 128));
            after.get(1..).ok_or(false)?
        } else {
            let (width, after) = match (
                directive.strip_prefix("X2\\"),
                directive.strip_prefix("X4\\"),
            ) {
                (Some(after), _) => (4, after),
                (_, Some(after)) => (8, after),
                _ => return Err(false),
            };
            let (run, after) = after.split_once("\\X0\\").ok_or(false)?;
            let run = run.as_bytes();
            if run.is_empty() || run.len() % width != 0 {
                return Err(false);
            }
            let units = run
                .chunks_exact(width)
                .map(|unit| hex(unit).ok_or(false))
                .collect::<Result<Vec<u32>, bool>>()?;
            if width == 4 {
                let units = units.iter().map(|unit| *unit as u16);
                for c in char::decode_utf16(units) {
                    out.push(c.map_err(|_| false)?);
                }
            } else {
                for unit in units {
                    out.push(char::from_u32(unit).ok_or(false)?);
                }
            }
            after
        };
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests;
