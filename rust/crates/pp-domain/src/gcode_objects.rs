//! Pure port of `web/apps/server/src/services/gcode-object-parser.ts` and the
//! parts of `web/packages/domain/src/sliced-object-matching.ts` it uses.
//!
//! Object names come from EXCLUDE_OBJECT (OrcaSlicer/BambuStudio and quoted
//! PrusaSlicer), M486 and PrusaSlicer objects_info. They are grouped per part
//! and matched to library STL filenames. Parity with the TypeScript output is
//! pinned by the goldens in `fixtures/parity/gcode-objects`.

mod key_table;

use std::cmp::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcodeObjectFormat {
    ExcludeObjectOrca,
    ExcludeObjectPrusa,
    M486,
    Unknown,
}

impl GcodeObjectFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExcludeObjectOrca => "exclude_object_orca",
            Self::ExcludeObjectPrusa => "exclude_object_prusa",
            Self::M486 => "m486",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedGcodeObject {
    /// Raw NAME from gcode or the printer API.
    pub name: String,
    /// Extracted filename, e.g. `a_drive_frame_lower.stl`.
    pub stl_basename: String,
    /// 0-based copy index. A float like the TS `number`: digit runs are unbounded.
    pub copy_index: f64,
    pub format: GcodeObjectFormat,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlateMatch {
    /// Lowercased `stl_basename`; also the grouping key.
    pub stl_basename: String,
    pub count: usize,
    pub objects: Vec<ParsedGcodeObject>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlicedObjectNameWrapper {
    OrcaCopy,
    PrusaInstance,
    ObjectsInfoInstance,
    Plain,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterpretedSlicedObjectName {
    pub raw_name: String,
    pub unwrapped_name: String,
    pub path_key: String,
    pub basename_key: String,
    pub unit_stem_key: String,
    pub wrapper: SlicedObjectNameWrapper,
    pub copy_index: Option<f64>,
}

/// Groups raw object names by part, in first-seen order. Keys are unique.
pub fn group_objects_by_part<S: AsRef<str>>(names: &[S]) -> Vec<PlateMatch> {
    let mut result: Vec<PlateMatch> = Vec::new();
    for raw in names {
        let parsed = parse_gcode_object_name(raw.as_ref());
        let key = parsed.stl_basename.to_lowercase();
        match result.iter_mut().find(|plate| plate.stl_basename == key) {
            Some(existing) => {
                existing.count += 1;
                existing.objects.push(parsed);
            }
            None => result.push(PlateMatch {
                stl_basename: key,
                count: 1,
                objects: vec![parsed],
            }),
        }
    }
    result
}

/// Matches each plate key to library filenames: one filename when matched,
/// every candidate when ambiguous, none when unmatched.
pub fn match_objects_to_filenames<S: AsRef<str>>(
    plate_matches: &[PlateMatch],
    library_filenames: &[S],
) -> Vec<(String, Vec<String>)> {
    let index = index_filenames(library_filenames);
    plate_matches
        .iter()
        .map(|plate| {
            let filenames = matched_filenames(&plate.stl_basename, &index);
            (plate.stl_basename.clone(), filenames)
        })
        .collect()
}

/// Matches one raw object name to library filenames, as
/// `createSlicedObjectMatcher(filenames)(rawName)` does: one filename when
/// matched, every candidate when ambiguous, none when unmatched.
pub fn match_object_name<S: AsRef<str>>(raw_name: &str, filenames: &[S]) -> Vec<String> {
    matched_filenames(raw_name, &index_filenames(filenames))
}

fn matched_filenames(raw_name: &str, index: &[IndexedFilename<'_>]) -> Vec<String> {
    match match_indexed_name(&interpret_sliced_object_name(raw_name), index) {
        SlicedObjectNameMatch::Matched(filename) => vec![filename.to_owned()],
        SlicedObjectNameMatch::Ambiguous(filenames) => {
            filenames.into_iter().map(str::to_owned).collect()
        }
        SlicedObjectNameMatch::Unmatched => Vec::new(),
    }
}

fn parse_gcode_object_name(raw: &str) -> ParsedGcodeObject {
    let interpreted = interpret_sliced_object_name(raw);
    let is_quoted = raw.starts_with('\'') && raw.ends_with('\'');
    let format = if interpreted.wrapper == SlicedObjectNameWrapper::OrcaCopy {
        GcodeObjectFormat::ExcludeObjectOrca
    } else if is_quoted || interpreted.wrapper != SlicedObjectNameWrapper::Plain {
        GcodeObjectFormat::ExcludeObjectPrusa
    } else {
        GcodeObjectFormat::M486
    };
    let stl_basename = match strip_suffix_ascii_ci(&interpreted.unwrapped_name, "_stl") {
        Some(stem) => format!("{stem}.stl"),
        None => interpreted.unwrapped_name.clone(),
    };
    ParsedGcodeObject {
        name: raw.to_owned(),
        stl_basename,
        copy_index: interpreted.copy_index.unwrap_or(0.0),
        format,
    }
}

pub fn interpret_sliced_object_name(raw_name: &str) -> InterpretedSlicedObjectName {
    let decoded = decode_uri_component(raw_name);
    let mut unwrapped = strip_enclosing_quotes(decoded.as_deref().unwrap_or(raw_name));
    let mut wrapper = SlicedObjectNameWrapper::Plain;
    let mut copy_index = None;

    if let Some((stem, copy)) = orca_copy(unwrapped) {
        unwrapped = stem;
        copy_index = Some(parse_digits(copy));
        wrapper = SlicedObjectNameWrapper::OrcaCopy;
    } else if let Some((index, instance)) = objects_info_instance(unwrapped) {
        unwrapped = &unwrapped[..index];
        copy_index = Some((parse_digits(instance) - 1.0).max(0.0));
        wrapper = SlicedObjectNameWrapper::ObjectsInfoInstance;
    } else if let Some((index, instance)) = prusa_instance(unwrapped) {
        unwrapped = unwrapped[..index].trim_end_matches('_');
        copy_index = Some((parse_digits(instance) - 1.0).max(0.0));
        wrapper = SlicedObjectNameWrapper::PrusaInstance;
    }

    let path_without_extensions = strip_mesh_extensions(&unwrapped.replace('\\', "/"));
    let path_key = comparison_path(&path_without_extensions);
    let basename_key = match path_key.rfind('/') {
        Some(slash) => path_key[slash + 1..].to_owned(),
        None => path_key.clone(),
    };
    let unit_stem_key = match strip_unit_suffix(&basename_key) {
        "" => basename_key.clone(),
        stem => stem.to_owned(),
    };

    InterpretedSlicedObjectName {
        raw_name: raw_name.to_owned(),
        unwrapped_name: unwrapped.to_owned(),
        path_key,
        basename_key,
        unit_stem_key,
        wrapper,
        copy_index,
    }
}

/// ECMAScript `\s`: WhiteSpace plus LineTerminator. Differs from
/// `char::is_whitespace` (U+0085 excluded, U+FEFF included).
fn is_js_space(c: char) -> bool {
    ('\t'..='\r').contains(&c)
        || ('\u{2000}'..='\u{200A}').contains(&c)
        || matches!(
            c,
            ' ' | '\u{A0}'
                | '\u{1680}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
        )
}

/// JS `.` excludes line terminators.
fn is_js_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `decodeURIComponent`, returning `None` where JS would throw.
fn decode_uri_component(value: &str) -> Option<String> {
    if !value.contains('%') {
        return None;
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let high = (hex[0] as char).to_digit(16)?;
            let low = (hex[1] as char).to_digit(16)?;
            decoded.push((high * 16 + low) as u8);
            i += 3;
        } else {
            decoded.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn strip_enclosing_quotes(value: &str) -> &str {
    let trimmed = value.trim_matches(is_js_space);
    let mut chars = trimmed.chars();
    match (chars.next(), chars.next_back()) {
        (Some('\''), Some('\'')) | (Some('"'), Some('"')) => {
            trimmed[1..trimmed.len() - 1].trim_matches(is_js_space)
        }
        _ => trimmed,
    }
}

fn strip_suffix_ascii_ci<'a>(value: &'a str, suffix: &str) -> Option<&'a str> {
    let split = value.len().checked_sub(suffix.len())?;
    let tail = value.as_bytes().get(split..)?;
    // An ASCII-equal tail starts on a char boundary.
    tail.eq_ignore_ascii_case(suffix.as_bytes())
        .then(|| &value[..split])
}

/// Splits a trailing run of ASCII digits: `(rest, digits)`.
fn split_trailing_digits(value: &str) -> (&str, &str) {
    let rest = value.trim_end_matches(|c: char| c.is_ascii_digit());
    (rest, &value[rest.len()..])
}

/// `^(.+)_id_(\d+)_copy_(\d+)$` (case-insensitive): `(stem, copy digits)`.
fn orca_copy(value: &str) -> Option<(&str, &str)> {
    let (rest, copy) = split_trailing_digits(value);
    let rest = strip_suffix_ascii_ci(rest, "_copy_").filter(|_| !copy.is_empty())?;
    let (rest, id) = split_trailing_digits(rest);
    let stem = strip_suffix_ascii_ci(rest, "_id_").filter(|_| !id.is_empty())?;
    (!stem.is_empty() && !stem.contains(is_js_line_terminator)).then_some((stem, copy))
}

/// `\s*\(instance\s+(\d+)\)\s*$` (case-insensitive): `(match start, digits)`.
fn objects_info_instance(value: &str) -> Option<(usize, &str)> {
    let rest = value.trim_end_matches(is_js_space).strip_suffix(')')?;
    let (rest, digits) = split_trailing_digits(rest);
    let spaced = rest.trim_end_matches(is_js_space);
    if digits.is_empty() || spaced.len() == rest.len() {
        return None;
    }
    let rest = strip_suffix_ascii_ci(spaced, "(instance")?;
    Some((rest.trim_end_matches(is_js_space).len(), digits))
}

/// `_+instance_(\d+)_?$` (case-insensitive): `(match start, digits)`.
fn prusa_instance(value: &str) -> Option<(usize, &str)> {
    let value = value.strip_suffix('_').unwrap_or(value);
    let (rest, digits) = split_trailing_digits(value);
    let rest = strip_suffix_ascii_ci(rest, "instance_").filter(|_| !digits.is_empty())?;
    let start = rest.trim_end_matches('_').len();
    (start < rest.len()).then_some((start, digits))
}

/// `Number.parseInt` of a nonempty ASCII digit run.
fn parse_digits(digits: &str) -> f64 {
    digits.parse().unwrap_or(f64::NAN)
}

fn strip_mesh_extensions(value: &str) -> String {
    const EXTENSIONS: [&str; 5] = [".stl", ".3mf", ".gcode", ".gco", ".bgcode"];
    let mut result = value;
    for _ in 0..4 {
        match EXTENSIONS
            .iter()
            .find_map(|extension| strip_suffix_ascii_ci(result, extension))
        {
            Some(next) => result = next,
            None => break,
        }
    }
    strip_suffix_ascii_ci(result, "_stl")
        .unwrap_or(result)
        .to_owned()
}

/// NFKD, strip U+0300..U+036F, lowercase, `[^a-z0-9]+` -> `_`, trim one `_`.
fn comparison_segment(value: &str) -> String {
    let mut key = String::with_capacity(value.len());
    let mut push = |part: &str| {
        for c in part.chars() {
            if c != '_' || !key.ends_with('_') {
                key.push(c);
            }
        }
    };
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            push(c.to_ascii_lowercase().encode_utf8(&mut [0; 4]));
        } else if ('\u{300}'..='\u{36F}').contains(&c) {
        } else if c.is_ascii() {
            push("_");
        } else {
            let exception = key_table::KEY_EXCEPTIONS
                .binary_search_by_key(&u32::from(c), |&(code_point, _)| code_point)
                .map(|found| key_table::KEY_EXCEPTIONS[found].1);
            push(exception.unwrap_or("_"));
        }
    }
    let key = key.strip_prefix('_').unwrap_or(&key);
    key.strip_suffix('_').unwrap_or(key).to_owned()
}

fn comparison_path(value: &str) -> String {
    value
        .replace('\\', "/")
        .split('/')
        .map(comparison_segment)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Strips `_(\d{1,3})$`.
fn strip_unit_suffix(key: &str) -> &str {
    let (rest, digits) = split_trailing_digits(key);
    match rest.strip_suffix('_') {
        Some(stem) if (1..=3).contains(&digits.len()) => stem,
        _ => key,
    }
}

enum SlicedObjectNameMatch<'a> {
    Matched(&'a str),
    Ambiguous(Vec<&'a str>),
    Unmatched,
}

struct IndexedFilename<'a> {
    filename: &'a str,
    interpreted: InterpretedSlicedObjectName,
}

fn index_filenames<S: AsRef<str>>(filenames: &[S]) -> Vec<IndexedFilename<'_>> {
    filenames
        .iter()
        .map(|filename| IndexedFilename {
            filename: filename.as_ref(),
            interpreted: interpret_sliced_object_name(filename.as_ref()),
        })
        .collect()
}

fn exact_outcome<'a>(rows: Vec<&'a str>) -> Option<SlicedObjectNameMatch<'a>> {
    match rows.as_slice() {
        [] => None,
        [filename] => Some(SlicedObjectNameMatch::Matched(filename)),
        _ => Some(SlicedObjectNameMatch::Ambiguous(rows)),
    }
}

fn exact_rows<'a>(
    indexed: &[IndexedFilename<'a>],
    key: impl Fn(&InterpretedSlicedObjectName) -> bool,
) -> Vec<&'a str> {
    indexed
        .iter()
        .filter(|row| key(&row.interpreted))
        .map(|row| row.filename)
        .collect()
}

/// Keys are `[a-z0-9_]`, so bytes equal UTF-16 code units.
fn levenshtein(left: &[u8], right: &[u8]) -> usize {
    if left == right {
        return 0;
    }
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (left_index, left_byte) in left.iter().enumerate() {
        let mut current = vec![left_index + 1];
        for (right_index, right_byte) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_byte != right_byte);
            current.push(
                (current[right_index] + 1)
                    .min(previous[right_index + 1] + 1)
                    .min(substitution),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

const CONTRAST_GROUPS: [[&str; 2]; 5] = [
    ["left", "right"],
    ["front", "rear"],
    ["top", "bottom"],
    ["upper", "lower"],
    ["inner", "outer"],
];

/// Distinct nonempty `_` tokens in first-seen order, like a JS `Set`.
fn semantic_tokens(key: &str) -> Vec<&str> {
    let mut tokens: Vec<&str> = Vec::new();
    for token in key.split('_').filter(|token| !token.is_empty()) {
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

fn semantic_tokens_agree(left_key: &str, right_key: &str) -> bool {
    let left = semantic_tokens(left_key);
    let right = semantic_tokens(right_key);
    let filtered = |tokens: &[&'_ str], keep: fn(&str) -> bool| -> Vec<String> {
        tokens
            .iter()
            .filter(|token| keep(token))
            .map(|token| (*token).to_owned())
            .collect()
    };
    let is_number = |token: &str| token.bytes().all(|byte| byte.is_ascii_digit());
    let is_axis = |token: &str| matches!(token, "x" | "y" | "z");
    if filtered(&left, is_number) != filtered(&right, is_number)
        || filtered(&left, is_axis) != filtered(&right, is_axis)
    {
        return false;
    }
    CONTRAST_GROUPS.iter().all(|group| {
        let left_token = group.iter().find(|token| left.contains(token));
        let right_token = group.iter().find(|token| right.contains(token));
        left_token == right_token
    })
}

fn fuzzy_score(left_key: &str, right_key: &str) -> (usize, f64) {
    let left = left_key.replace('_', "");
    let right = right_key.replace('_', "");
    let distance = levenshtein(left.as_bytes(), right.as_bytes());
    let longest = left.len().max(right.len()).max(1);
    (distance, 1.0 - distance as f64 / longest as f64)
}

/// `String.prototype.localeCompare` under ICU root/en-US collation. Exact for
/// ASCII: controls are ignorable; spaces, punctuation and symbols sort before
/// digits and case-folded letters; lowercase wins ties. Non-ASCII characters
/// fall back to code point order after every ASCII character.
fn locale_compare(left: &str, right: &str) -> Ordering {
    const ORDER: &[u8] =
        b"\t\n\x0B\x0C\r _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789abcdefghijklmnopqrstuvwxyz";
    let weights = |value: &str| -> Vec<(u32, bool)> {
        value
            .chars()
            .filter_map(|c| {
                if !c.is_ascii() {
                    return Some((ORDER.len() as u32 + u32::from(c), false));
                }
                let folded = c.to_ascii_lowercase() as u8;
                let primary = ORDER.iter().position(|&byte| byte == folded)?;
                Some((primary as u32, c.is_ascii_uppercase()))
            })
            .collect()
    };
    let (left, right) = (weights(left), weights(right));
    let primary = |weights: &[(u32, bool)]| weights.iter().map(|w| w.0).collect::<Vec<_>>();
    primary(&left)
        .cmp(&primary(&right))
        .then_with(|| left.iter().map(|w| w.1).cmp(right.iter().map(|w| w.1)))
}

struct FuzzyRow<'a> {
    filename: &'a str,
    distance: usize,
    score: f64,
    semantic: bool,
}

fn match_indexed_name<'a>(
    observed: &InterpretedSlicedObjectName,
    indexed: &[IndexedFilename<'a>],
) -> SlicedObjectNameMatch<'a> {
    if observed.basename_key.is_empty() {
        return SlicedObjectNameMatch::Unmatched;
    }
    if observed.path_key.contains('/')
        && let Some(found) =
            exact_outcome(exact_rows(indexed, |row| row.path_key == observed.path_key))
    {
        return found;
    }
    if let Some(found) = exact_outcome(exact_rows(indexed, |row| {
        row.basename_key == observed.basename_key
    })) {
        return found;
    }
    if observed.unit_stem_key != observed.basename_key
        && let Some(found) = exact_outcome(exact_rows(indexed, |row| {
            row.basename_key == observed.unit_stem_key
        }))
    {
        return found;
    }

    let mut fuzzy: Vec<FuzzyRow<'a>> = indexed
        .iter()
        .map(|row| {
            let (distance, score) =
                fuzzy_score(&observed.unit_stem_key, &row.interpreted.basename_key);
            FuzzyRow {
                filename: row.filename,
                distance,
                score,
                semantic: semantic_tokens_agree(
                    &observed.unit_stem_key,
                    &row.interpreted.basename_key,
                ),
            }
        })
        .collect();
    fuzzy.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| locale_compare(left.filename, right.filename))
    });
    let Some(best) = fuzzy.first() else {
        return SlicedObjectNameMatch::Unmatched;
    };
    let compact_length = observed.unit_stem_key.replace('_', "").len();
    let max_distance = if compact_length >= 12 { 2 } else { 1 };
    let clears_threshold =
        compact_length >= 8 && best.semantic && best.distance <= max_distance && best.score >= 0.9;
    let clears_margin = fuzzy
        .get(1)
        .is_none_or(|runner_up| best.score - runner_up.score >= 0.08);
    if clears_threshold && clears_margin {
        return SlicedObjectNameMatch::Matched(best.filename);
    }
    let close: Vec<&'a str> = fuzzy
        .iter()
        .filter(|row| best.score - row.score < 0.08)
        .map(|row| row.filename)
        .collect();
    if clears_threshold && close.len() > 1 {
        return SlicedObjectNameMatch::Ambiguous(close);
    }
    SlicedObjectNameMatch::Unmatched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrappers_follow_the_ts_regexes() {
        assert_eq!(orca_copy("a.stl_ID_2_Copy_3"), Some(("a.stl", "3")));
        assert_eq!(orca_copy("_id_0_copy_0"), None);
        assert_eq!(orca_copy("a\nb_id_0_copy_0"), None);
        assert_eq!(
            objects_info_instance("a.stl  (Instance\u{A0}2) "),
            Some((5, "2"))
        );
        assert_eq!(objects_info_instance("a.stl (instance2)"), None);
        assert_eq!(prusa_instance("a_stl__Instance_12_"), Some((5, "12")));
        assert_eq!(prusa_instance("ainstance_1"), None);
        assert_eq!(strip_unit_suffix("a_007"), "a");
        assert_eq!(strip_unit_suffix("a_1234"), "a_1234");
    }

    #[test]
    fn uri_decoding_fails_like_js() {
        assert_eq!(decode_uri_component("f%C3%BCr").as_deref(), Some("für"));
        assert_eq!(decode_uri_component("f%C3"), None);
        assert_eq!(decode_uri_component("%zz"), None);
        assert_eq!(decode_uri_component("%C0%AF"), None);
    }

    #[test]
    fn comparison_keys_fold_compatibility_forms() {
        assert_eq!(comparison_segment("ＢＲＡＣＫＥＴ"), "bracket");
        assert_eq!(comparison_segment("ﬁlament ①"), "filament_1");
        assert_eq!(comparison_segment("_Cafe\u{301}__Lid_"), "cafe_lid");
        assert_eq!(comparison_segment("支架"), "");
    }

    #[test]
    fn key_table_is_sorted() {
        assert!(
            key_table::KEY_EXCEPTIONS
                .windows(2)
                .all(|pair| pair[0].0 < pair[1].0)
        );
    }

    #[test]
    fn ascii_collation_matches_icu_order() {
        let mut names = ["b", "B", "a-b", "a_b", "ab", "A", "a1", "a b", "a.b"];
        names.sort_by(|left, right| locale_compare(left, right));
        assert_eq!(
            names,
            ["A", "a b", "a_b", "a-b", "a.b", "a1", "ab", "b", "B"]
        );
    }
}
