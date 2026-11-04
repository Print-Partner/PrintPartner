use crate::archive::{self, ArchiveError, ArchiveLimits};
use crate::{
    Directory, FileKind, LocalFiles, PathCollisions, SelectedFile, SourcePath, SourceRoot,
    compare_paths,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::{LazyLock, atomic::AtomicBool},
};
use unicode_normalization::UnicodeNormalization;

const CANDIDATE: &str = ".candidate";
const MAX_RECEIPT_BYTES: u64 = 8 * 1024 * 1024;
#[derive(Debug)]
pub enum MediaError {
    Source(crate::Error),
    Archive(ArchiveError),
    Invalid(&'static str),
    Busy,
    Cleanup {
        operation: Box<MediaError>,
        cleanup: Box<MediaError>,
    },
}
impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(e) => write!(f, "{e}"),
            Self::Archive(e) => write!(f, "{e}"),
            Self::Invalid(message) => write!(f, "{message}"),
            Self::Busy => write!(f, "media-busy"),
            Self::Cleanup { operation, cleanup } => write!(f, "{operation}; cleanup: {cleanup}"),
        }
    }
}
impl std::error::Error for MediaError {}
impl From<crate::Error> for MediaError {
    fn from(e: crate::Error) -> Self {
        Self::Source(e)
    }
}
impl From<io::Error> for MediaError {
    fn from(e: io::Error) -> Self {
        Self::Source(e.into())
    }
}
impl From<ArchiveError> for MediaError {
    fn from(e: ArchiveError) -> Self {
        Self::Archive(e)
    }
}
impl From<zip::result::ZipError> for MediaError {
    fn from(e: zip::result::ZipError) -> Self {
        Self::Archive(e.into())
    }
}
type Result<T> = std::result::Result<T, MediaError>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaLimits {
    pub max_model_bytes: u64,
    pub max_objects: usize,
    pub max_vertices: usize,
    pub max_triangles: usize,
    pub max_output_bytes: u64,
    pub max_total_bytes: u64,
}
impl Default for MediaLimits {
    fn default() -> Self {
        Self {
            max_model_bytes: 64 * 1024 * 1024,
            max_objects: 2_000,
            max_vertices: 5_000_000,
            max_triangles: 10_000_000,
            max_output_bytes: 256 * 1024 * 1024,
            max_total_bytes: crate::MAX_CONTENT_BYTES,
        }
    }
}
impl MediaLimits {
    fn validate(self) -> Result<()> {
        let hard = Self::default();
        let bounds = [
            (self.max_model_bytes, hard.max_model_bytes),
            (self.max_objects as u64, hard.max_objects as u64),
            (self.max_vertices as u64, hard.max_vertices as u64),
            (self.max_triangles as u64, hard.max_triangles as u64),
            (self.max_output_bytes, hard.max_output_bytes),
            (self.max_total_bytes, hard.max_total_bytes),
        ];
        if bounds.iter().any(|&(value, max)| value == 0 || value > max) {
            return Err(MediaError::Invalid("media-limit"));
        }
        Ok(())
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedFile {
    pub relative_path: SourcePath,
    pub object_id: String,
    pub object_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_number: Option<String>,
    pub triangle_count: usize,
    pub byte_size: u64,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionReceipt {
    pub object_count: usize,
    pub files: Vec<DerivedFile>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReadStats {
    pub read_bytes: u64,
    pub seek_count: u64,
    pub model_bytes: u64,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaConversion {
    pub original: SourcePath,
    pub result: ConversionReceipt,
    pub model_read: ModelReadStats,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaReceipt {
    pub conversions: Vec<MediaConversion>,
    pub selected_files: Vec<SelectedFile>,
    pub suggested_import_rules: Vec<String>,
    pub original_bytes: u64,
    pub derived_bytes: u64,
}
pub struct PreparedMedia {
    parent: Directory,
    files: LocalFiles,
    receipt: MediaReceipt,
    cleaned: bool,
}
impl PreparedMedia {
    pub fn files(&self) -> &LocalFiles {
        &self.files
    }
    pub fn receipt(&self) -> &MediaReceipt {
        &self.receipt
    }
    fn cleanup(&mut self) -> Result<()> {
        self.parent.remove_tree(CANDIDATE)?;
        self.parent.sync()?;
        self.cleaned = true;
        Ok(())
    }
    pub fn discard(mut self) -> Result<()> {
        self.cleanup()
    }
}
impl Drop for PreparedMedia {
    fn drop(&mut self) {
        if !self.cleaned {
            let _ = self.cleanup();
        }
    }
}

impl SourceRoot {
    pub fn prepare_media(
        &mut self,
        originals: &LocalFiles,
        paths: &[SourcePath],
        directories: &[SourcePath],
        limits: MediaLimits,
        cancelled: &AtomicBool,
    ) -> Result<PreparedMedia> {
        limits.validate()?;
        archive::check_cancel(cancelled)?;
        let mut collisions = PathCollisions::default();
        for path in directories {
            collisions.insert(path, true)?;
        }
        for path in paths {
            collisions.insert(path, false)?;
        }
        let parent = self.revisions.child(".pp-source-media", true)?;
        match parent.try_lock() {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Err(MediaError::Busy),
            Err(e) => return Err(e.into()),
        }
        parent.remove_tree(CANDIDATE)?;
        let files = LocalFiles {
            root: parent.child(CANDIDATE, true)?,
        };
        let mut prepared = PreparedMedia {
            parent,
            files,
            receipt: MediaReceipt {
                conversions: vec![],
                selected_files: vec![],
                suggested_import_rules: vec![],
                original_bytes: 0,
                derived_bytes: 0,
            },
            cleaned: false,
        };
        let outcome = (|| {
            for directory in directories {
                let (parent, name) = prepared.files.root.parent(directory.as_str(), true)?;
                parent.child(&name, true)?;
            }
            prepare(
                &mut prepared,
                originals,
                paths,
                limits,
                cancelled,
                &mut collisions,
            )
        })();
        match outcome {
            Ok(()) => Ok(prepared),
            Err(operation) => match prepared.cleanup() {
                Ok(()) => Err(operation),
                Err(cleanup) => Err(MediaError::Cleanup {
                    operation: Box::new(operation),
                    cleanup: Box::new(cleanup),
                }),
            },
        }
    }
}
fn prepare(
    prepared: &mut PreparedMedia,
    originals: &LocalFiles,
    paths: &[SourcePath],
    limits: MediaLimits,
    cancelled: &AtomicBool,
    collisions: &mut PathCollisions,
) -> Result<()> {
    let mut ordered = paths.iter().collect::<Vec<_>>();
    ordered.sort_by(|a, b| a.as_str().split('/').cmp(b.as_str().split('/')));
    for path in &ordered {
        archive::check_cancel(cancelled)?;
        let mut input = originals.root.file(path.as_str(), false)?;
        if is_model(path) {
            let _ = read_model(&mut input, limits, cancelled)?;
            input.seek(SeekFrom::Start(0))?;
        }
        let mut output = prepared.files.root.file(path.as_str(), true)?;
        let (bytes, _) = archive::transfer(
            &mut input,
            &mut output,
            limits.max_total_bytes - prepared.receipt.original_bytes,
            cancelled,
        )?;
        output.sync_all()?;
        prepared.receipt.original_bytes += bytes;
        prepared.receipt.selected_files.push(SelectedFile {
            path: (*path).clone(),
            kind: if path.as_str().to_ascii_lowercase().ends_with(".stl") {
                FileKind::Stl
            } else {
                FileKind::Artifact
            },
            size_hint_bytes: Some(bytes),
        });
    }
    let mut receipt_budget = ReceiptBudget(MAX_RECEIPT_BYTES);
    for path in ordered {
        if !is_model(path) {
            continue;
        }
        let mut input = prepared.files.root.file(path.as_str(), false)?;
        let (document, model_read) = read_model(&mut input, limits, cancelled)?;
        let remaining = limits
            .max_output_bytes
            .min(limits.max_total_bytes - prepared.receipt.original_bytes)
            - prepared.receipt.derived_bytes;
        let result = convert(
            document,
            &prepared.files.root,
            path.as_str(),
            MediaLimits {
                max_output_bytes: remaining,
                ..limits
            },
            cancelled,
            collisions,
            &mut receipt_budget,
        )?;
        for file in &result.files {
            prepared.receipt.derived_bytes += file.byte_size;
            prepared.receipt.selected_files.push(SelectedFile {
                path: file.relative_path.clone(),
                kind: FileKind::Stl,
                size_hint_bytes: Some(file.byte_size),
            });
        }
        prepared.receipt.conversions.push(MediaConversion {
            original: path.clone(),
            result,
            model_read,
        });
    }
    prepared
        .receipt
        .selected_files
        .sort_by(|a, b| compare_paths(a.path.as_str(), b.path.as_str()));
    prepared.receipt.suggested_import_rules = discover_import_rules(&prepared.files)?;
    archive::check_cancel(cancelled)?;
    Ok(())
}
fn is_model(path: &SourcePath) -> bool {
    path.as_str().to_ascii_lowercase().ends_with(".3mf")
        && !path.as_str().split('/').any(|s| s == "_3mf")
}

pub fn discover_import_rules(files: &LocalFiles) -> Result<Vec<String>> {
    let mut names = files.root.entries()?;
    names.sort();
    let mut directories = Vec::new();
    let mut printable = Vec::new();
    for name in names {
        if files.root.child(&name, false).is_ok() {
            directories.push(format!("{name}/"));
        } else if (name.to_ascii_lowercase().ends_with(".stl")
            || name.to_ascii_lowercase().ends_with(".3mf"))
            && files.root.file(&name, false).is_ok()
        {
            printable.push(name);
        }
    }
    directories.extend(printable);
    Ok(directories)
}
struct CountingReader<R> {
    inner: R,
    bytes: u64,
    seeks: u64,
}
impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buffer)?;
        self.bytes += n as u64;
        Ok(n)
    }
}
impl<R: Seek> Seek for CountingReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.seeks += 1;
        self.inner.seek(pos)
    }
}
struct ModelDocument(String);
fn read_model(
    input: &mut File,
    limits: MediaLimits,
    cancelled: &AtomicBool,
) -> Result<(ModelDocument, ModelReadStats)> {
    let length = input.metadata()?.len();
    let mut reader = CountingReader {
        inner: input,
        bytes: 0,
        seeks: 0,
    };
    let entries = archive::inspect_index(&mut reader, length, ArchiveLimits::default(), cancelled)?;
    let (index, inflated_bytes) = entries
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.raw_name.to_ascii_lowercase().ends_with(".model"))
        .min_by_key(|(_, e)| e.local_header_offset)
        .map(|(index, entry)| (index, entry.inflated_bytes))
        .ok_or(MediaError::Invalid("3MF model document is missing"))?;
    if inflated_bytes > limits.max_model_bytes {
        return Err(MediaError::Invalid(
            "3MF model document exceeds the size limit",
        ));
    }
    // Bind a private copy of the validated prefix. The caller's 3MF stays
    // intact, and a ZIP64 extra cannot redirect this read.
    let bytes = archive::read_bound_entry(
        &mut reader,
        &entries,
        index,
        limits.max_model_bytes,
        cancelled,
    )?;
    if bytes.len() as u64 != inflated_bytes {
        return Err(MediaError::Invalid("3MF model size mismatch"));
    }
    let stats = ModelReadStats {
        read_bytes: reader.bytes.saturating_add(bytes.len() as u64),
        seek_count: reader.seeks,
        model_bytes: bytes.len() as u64,
    };
    Ok((
        ModelDocument(String::from_utf8_lossy(&bytes).into_owned()),
        stats,
    ))
}

struct Grammar {
    model: Regex,
    objects: Regex,
    mesh: Regex,
    vertices: Regex,
    triangles: Regex,
    attributes: Regex,
    entities: Regex,
    decimal: Regex,
}
const XML_SPACE: &str = r"[\x09-\x0d\x20\x{a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]";
fn compiled_regex(pattern: &str) -> Regex {
    match Regex::new(pattern) {
        Ok(regex) => regex,
        Err(error) => panic!("3MF grammar regex is invalid: {error}"),
    }
}
static GRAMMAR: LazyLock<Grammar> = LazyLock::new(|| Grammar {
    model: compiled_regex(r"(?i-u:<model\b)([^>]*)>"),
    objects: compiled_regex(
        &r"(?i-u:<object\b)([^>]*)>(?s:(.*?))(?i-u:</object)\s*>".replace(r"\s", XML_SPACE),
    ),
    mesh: compiled_regex(r"(?i-u:<mesh\b)"),
    vertices: compiled_regex(&r"(?i-u:<vertex\b)([^>]*)/?\s*>".replace(r"\s", XML_SPACE)),
    triangles: compiled_regex(&r"(?i-u:<triangle\b)([^>]*)/?\s*>".replace(r"\s", XML_SPACE)),
    attributes: compiled_regex(
        &r#"(?:^|\s)([a-zA-Z0-9_:.-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')"#.replace(r"\s", XML_SPACE),
    ),
    entities: compiled_regex(r"(?i-u:&(?:#x([0-9a-f]+)|#([0-9]+)|(amp|quot|apos|lt|gt));)"),
    decimal: compiled_regex(r"^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?$"),
});
fn attributes(tag: &str) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for matched in GRAMMAR.attributes.captures_iter(tag) {
        let Some(value_match) = matched.get(2).or_else(|| matched.get(3)) else {
            return Err(MediaError::Invalid("3MF attribute value is missing"));
        };
        let value = value_match.as_str();
        let mut decoded = String::new();
        let mut end = 0;
        for entity in GRAMMAR.entities.captures_iter(value) {
            let Some(whole) = entity.get(0) else {
                return Err(MediaError::Invalid("3MF entity match is missing"));
            };
            decoded.push_str(&value[end..whole.start()]);
            let ch = if let Some(hex) = entity.get(1) {
                u32::from_str_radix(hex.as_str(), 16)
                    .ok()
                    .and_then(char::from_u32)
            } else if let Some(decimal) = entity.get(2) {
                decimal
                    .as_str()
                    .parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
            } else {
                match entity[3].to_ascii_lowercase().as_str() {
                    "amp" => Some('&'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    _ => None,
                }
            };
            decoded.push(ch.ok_or(MediaError::Invalid("3MF invalid XML character reference"))?);
            end = whole.end();
        }
        decoded.push_str(&value[end..]);
        result.insert(matched[1].to_ascii_lowercase(), decoded);
    }
    Ok(result)
}
/// Generated names are pp-source's own, so on Windows a stem that names a
/// device (`con`, `com1`) gets a `_` instead of failing the import. Unix keeps
/// every stem unchanged.
fn portable_stem(stem: String) -> String {
    if pp_os::reinterpreted_here(&format!("{stem}.stl")) {
        format!("{stem}_")
    } else {
        stem
    }
}

fn slug(value: &str, fallback: &str) -> String {
    let lowered = value
        .chars()
        .map(|c| if c == '\u{a7f1}' { 'S' } else { c })
        .nfkd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .collect::<String>()
        .to_lowercase();
    let mut output = String::new();
    let mut separator = false;
    for c in lowered.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if separator && !output.is_empty() {
                output.push('-');
            }
            output.push(c);
            separator = false;
        } else {
            separator = true;
        }
    }
    if output.is_empty() {
        fallback.to_owned()
    } else {
        output
    }
}
fn js_whitespace(c: char) -> bool {
    matches!(c,'\u{9}'..='\u{d}'|'\u{20}'|'\u{a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
fn number(value: Option<&String>) -> Result<f64> {
    let text = value
        .ok_or(MediaError::Invalid("3MF contains an invalid number"))?
        .trim_matches(js_whitespace);
    let parsed = if text.is_empty() {
        0.0
    } else if ["0x", "0X", "0b", "0B", "0o", "0O"]
        .iter()
        .any(|prefix| text.starts_with(prefix))
    {
        let radix = match text.as_bytes()[1] {
            b'x' | b'X' => 16,
            b'b' | b'B' => 2,
            _ => 8,
        };
        let digits = &text[2..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return Err(MediaError::Invalid("3MF contains an invalid number"));
        }
        radix_number(digits, radix)
    } else if GRAMMAR.decimal.is_match(text) {
        text.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        f64::NAN
    };
    if !parsed.is_finite() {
        return Err(MediaError::Invalid("3MF contains an invalid number"));
    }
    Ok(parsed)
}
fn radix_number(digits: &str, radix: u32) -> f64 {
    let width = radix.trailing_zeros();
    let mut bits = 0usize;
    let mut significand = 0u64;
    let mut round = false;
    let mut sticky = false;
    for digit in digits.chars() {
        let Some(digit) = digit.to_digit(radix) else {
            return f64::NAN;
        };
        for shift in (0..width).rev() {
            let bit = (digit >> shift) & 1;
            if bits == 0 && bit == 0 {
                continue;
            }
            bits += 1;
            if bits <= 53 {
                significand = significand * 2 + bit as u64;
            } else if bits == 54 {
                round = bit != 0;
            } else {
                sticky |= bit != 0;
            }
        }
    }
    if bits <= 53 {
        return significand as f64;
    }
    if bits > 1024 {
        return f64::INFINITY;
    }
    if round && (sticky || significand & 1 != 0) {
        significand += 1;
    }
    significand as f64 * f64::from_bits(((bits - 53 + 1023) as u64) << 52)
}
struct MeshObject {
    id: String,
    name: String,
    part_number: Option<String>,
    vertices: Vec<[f64; 3]>,
    faces: Vec<[usize; 3]>,
}
fn convert(
    document: ModelDocument,
    directory: &Directory,
    source_name: &str,
    limits: MediaLimits,
    cancelled: &AtomicBool,
    collisions: &mut PathCollisions,
    receipt_budget: &mut ReceiptBudget,
) -> Result<ConversionReceipt> {
    let xml = &document.0;
    let model = GRAMMAR
        .model
        .captures(xml)
        .map(|m| m[1].to_owned())
        .unwrap_or_default();
    let attrs = attributes(&model)?;
    let unit = attrs
        .get("unit")
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| "millimeter".into());
    let scale = match unit.as_str() {
        "micron" => 0.001,
        "millimeter" => 1.0,
        "centimeter" => 10.0,
        "meter" => 1000.0,
        "inch" => 25.4,
        "foot" => 304.8,
        _ => return Err(MediaError::Invalid("3MF uses unsupported unit")),
    };
    let basename = source_name.rsplit('/').next().unwrap_or(source_name);
    let project = portable_stem(slug(
        basename.strip_suffix(".3mf").unwrap_or(basename),
        "project",
    ));
    let mut objects = 0;
    let mut vertices = 0;
    let mut triangles = 0;
    let mut output_bytes = 0;
    let mut names = BTreeMap::<String, usize>::new();
    let mut files = Vec::new();
    for matched in GRAMMAR
        .objects
        .captures_iter(xml)
        .filter(|m| GRAMMAR.mesh.is_match(&m[2]))
    {
        archive::check_cancel(cancelled)?;
        objects += 1;
        if objects > limits.max_objects {
            return Err(MediaError::Invalid("3MF has too many mesh objects"));
        }
        let attrs = attributes(&matched[1])?;
        let id = attrs
            .get("id")
            .cloned()
            .unwrap_or_else(|| objects.to_string());
        let name = attrs
            .get("name")
            .or_else(|| attrs.get("partnumber"))
            .cloned()
            .unwrap_or_else(|| format!("object-{id}"));
        let part_number = attrs.get("partnumber").filter(|s| !s.is_empty()).cloned();
        let mut object = MeshObject {
            id,
            name,
            part_number,
            vertices: vec![],
            faces: vec![],
        };
        for vertex in GRAMMAR.vertices.captures_iter(&matched[2]) {
            archive::check_cancel(cancelled)?;
            vertices += 1;
            if vertices > limits.max_vertices {
                return Err(MediaError::Invalid("3MF has too many vertices"));
            }
            let attrs = attributes(&vertex[1])?;
            let point = [
                number(attrs.get("x"))? * scale,
                number(attrs.get("y"))? * scale,
                number(attrs.get("z"))? * scale,
            ];
            if !point.iter().all(|v| v.is_finite()) {
                return Err(MediaError::Invalid("3MF non-finite derived geometry"));
            }
            object.vertices.push(point);
        }
        for triangle in GRAMMAR.triangles.captures_iter(&matched[2]) {
            archive::check_cancel(cancelled)?;
            triangles += 1;
            if triangles > limits.max_triangles {
                return Err(MediaError::Invalid("3MF has too many triangles"));
            }
            let attrs = attributes(&triangle[1])?;
            let mut face = [0; 3];
            for (i, key) in ["v1", "v2", "v3"].iter().enumerate() {
                let n = number(attrs.get(*key))?;
                if n < 0.0 || n.fract() != 0.0 || n >= object.vertices.len() as f64 {
                    return Err(MediaError::Invalid(
                        "3MF triangle references an invalid vertex",
                    ));
                }
                face[i] = n as usize;
            }
            object.faces.push(face);
        }
        if object.vertices.is_empty() || object.faces.is_empty() {
            continue;
        }
        let solid = slug(&object.name, &format!("object-{}", object.id));
        let base = portable_stem(solid.clone());
        let count = names.entry(base.clone()).or_default();
        *count += 1;
        let suffix = if *count > 1 {
            format!("-{count}")
        } else {
            String::new()
        };
        let relative_path = SourcePath::try_from(format!("_3mf/{project}/{base}{suffix}.stl"))?;
        collisions.insert(&relative_path, false)?;
        let mut output = directory.file(relative_path.as_str(), true)?;
        let byte_size = write_stl(
            &mut output,
            &solid,
            &object,
            limits.max_output_bytes - output_bytes,
            cancelled,
        )?;
        output.sync_all()?;
        output_bytes += byte_size;
        let derived = DerivedFile {
            relative_path,
            object_id: object.id,
            object_name: object.name,
            part_number: object.part_number,
            triangle_count: object.faces.len(),
            byte_size,
        };
        serde_json::to_writer(&mut *receipt_budget, &derived)
            .map_err(|_| MediaError::Invalid("3MF receipt metadata exceeds the size limit"))?;
        files.push(derived);
    }
    if files.is_empty() {
        return Err(MediaError::Invalid(
            "3MF contains no non-empty printable mesh objects",
        ));
    }
    Ok(ConversionReceipt {
        object_count: files.len(),
        files,
    })
}
struct ReceiptBudget(u64);
impl Write for ReceiptBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("receipt metadata limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn normal(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> Result<[f64; 3]> {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let cross = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    if !cross.iter().all(|n| n.is_finite()) {
        return Err(MediaError::Invalid("3MF non-finite derived geometry"));
    }
    let max = cross.iter().map(|n| n.abs()).fold(0.0, f64::max);
    if max == 0.0 {
        return Ok([0.0; 3]);
    }
    let [a, b, c] = cross.map(|n| n.abs() / max);
    let power_a = a * a;
    let power_b = b * b;
    let compensation = (power_a + power_b) - power_a - power_b;
    let power_c = c * c - compensation;
    let length = (power_a + power_b + power_c).sqrt() * max;
    Ok(cross.map(|n| n / length))
}
fn point(point: [f64; 3]) -> String {
    let mut buffer = ryu_js::Buffer::new();
    point
        .into_iter()
        .map(|n| buffer.format(n).to_owned())
        .collect::<Vec<_>>()
        .join(" ")
}
fn write_stl(
    output: &mut File,
    name: &str,
    object: &MeshObject,
    limit: u64,
    cancelled: &AtomicBool,
) -> Result<u64> {
    let mut bytes = 0;
    let mut append = |text: String| -> Result<()> {
        archive::check_cancel(cancelled)?;
        if text.len() as u64 > limit - bytes {
            return Err(MediaError::Invalid(
                "3MF derived STL output exceeds the size limit",
            ));
        }
        output.write_all(text.as_bytes())?;
        bytes += text.len() as u64;
        Ok(())
    };
    append(format!("solid {name}\n"))?;
    for face in &object.faces {
        let [a, b, c] = face.map(|i| object.vertices[i]);
        append(format!(
            "  facet normal {}\n    outer loop\n      vertex {}\n      vertex {}\n      vertex {}\n    endloop\n  endfacet\n",
            point(normal(a, b, c)?),
            point(a),
            point(b),
            point(c)
        ))?;
    }
    append(format!("endsolid {name}\n"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn slug_matches_node_unicode_17_for_every_scalar() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("../../../tests/source-media/slug-oracle.json"))
                .unwrap();
        assert_eq!(oracle["unicode"], "17.0");
        let mapped = oracle["mapped"].as_object().unwrap();
        let mut mismatches = Vec::new();
        for n in 0..=0x10ffff {
            if let Some(c) = char::from_u32(n) {
                let expected = mapped
                    .get(&n.to_string())
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let actual = super::slug(&c.to_string(), "");
                if actual != expected {
                    mismatches.push((n, actual, expected.to_owned()));
                }
                let expected_context = oracle["context"]
                    .get(n.to_string())
                    .and_then(|v| v.as_str())
                    .unwrap_or("a-b");
                let actual_context = super::slug(&format!("a{c}b"), "");
                if actual_context != expected_context {
                    mismatches.push((n, actual_context, expected_context.to_owned()));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{mismatches:?}; Rust Unicode {:?}",
            char::UNICODE_VERSION
        );
    }
}
