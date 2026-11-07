//! Node's `updateSource` metadata merge: `JSON.parse`, spread, `JSON.stringify`.
//! Strings stay WTF-8 so lone surrogate escapes survive as they do in JavaScript.

use crate::manifest_text::JsText;
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;
use std::collections::{HashMap, hash_map::Entry};
use std::fmt;

/// The text Node stores after a scan failure: the parsed object, or `{raw}` when the stored
/// text is not a JSON object, plus `sync_error` and `sync_required`.
pub(super) fn failure_metadata(raw: Option<&str>, detail: &str) -> String {
    let mut members = raw.and_then(|raw| members(raw, 0)).unwrap_or_else(|| {
        raw.filter(|raw| !raw.is_empty())
            .map(|raw| {
                (
                    JsString(b"raw".to_vec()),
                    serde_json::Value::from(raw).to_string(),
                )
            })
            .into_iter()
            .collect()
    });
    for (key, value) in &mut members {
        if key.0 == b"category"
            && let Ok(category) = serde_json::from_str::<JsString>(value)
        {
            *value = JsString(crate::catalog::normalize_category_path(&category.0)).stringify();
        }
    }
    for (name, value) in [
        ("sync_error", serde_json::Value::from(detail).to_string()),
        ("sync_required", "true".into()),
    ] {
        match members.iter_mut().find(|(key, _)| key.0 == name.as_bytes()) {
            Some(member) => member.1 = value,
            None => members.push((JsString(name.into()), value)),
        }
    }
    object(&members)
}

/// What JavaScript sees reading `key` from `JSON.parse(text)`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Property {
    /// `JSON.parse` throws, or the document nests past 128 levels.
    Unparsable,
    /// The document is `null`, so reading any property throws a `TypeError`.
    Null,
    /// The value has no such own property, so it reads as `undefined`.
    Missing,
    /// `JSON.stringify` of the property's value.
    Value(String),
}

pub(crate) fn property(text: &str, key: &str) -> Property {
    let Ok(raw) = serde_json::from_str::<Box<RawValue>>(text) else {
        return Property::Unparsable;
    };
    // Walk the whole document so every string and nesting level is checked.
    if stringify(&raw, 0).is_none() {
        return Property::Unparsable;
    }
    match raw.get().as_bytes().first() {
        Some(b'n') => Property::Null,
        Some(b'{') => members(raw.get(), 0)
            .and_then(|members| {
                members
                    .into_iter()
                    .find(|(name, _)| name.0 == key.as_bytes())
            })
            .map_or(Property::Missing, |(_, value)| Property::Value(value)),
        // Strings, numbers, booleans and arrays have no such data property
        // while `key` is not an array index or `length`.
        _ => Property::Missing,
    }
}

fn members(text: &str, depth: usize) -> Option<Vec<(JsString, String)>> {
    serde_json::from_str::<JsObject>(text)
        .ok()?
        .0
        .into_iter()
        .map(|(key, value)| Some((key, stringify(&value, depth + 1)?)))
        .collect()
}

fn object(members: &[(JsString, String)]) -> String {
    let members: Vec<_> = members
        .iter()
        .map(|(key, value)| format!("{}:{value}", key.stringify()))
        .collect();
    format!("{{{}}}", members.join(","))
}

/// A JSON object's members in JavaScript property order with UTF-16 keys, or
/// `None` when `text` is not an object.
pub(crate) fn js_members(text: &str) -> Option<Vec<(JsText, Box<RawValue>)>> {
    let JsObject(members) = serde_json::from_str(text).ok()?;
    Some(
        members
            .into_iter()
            .map(|(key, value)| (key.utf16(), value))
            .collect(),
    )
}

/// A JSON string as UTF-16 with its lone surrogates, or `None` when `text` is
/// not a string.
pub(crate) fn js_string(text: &str) -> Option<JsText> {
    Some(serde_json::from_str::<JsString>(text).ok()?.utf16())
}

/// `JSON.stringify(JSON.parse(raw))` for stored text, or `None` when it is not
/// JSON or nests past 128 levels.
pub(crate) fn reserialize(raw: &str) -> Option<String> {
    stringify(&serde_json::from_str::<Box<RawValue>>(raw).ok()?, 0)
}

/// `JSON.stringify(JSON.parse(raw))`, or `None` past 128 levels of nesting.
fn stringify(raw: &RawValue, depth: usize) -> Option<String> {
    if depth >= 128 {
        return None;
    }
    let text = raw.get();
    Some(match text.as_bytes().first()? {
        b'{' => object(&members(text, depth)?),
        b'[' => {
            let items = serde_json::from_str::<Vec<Box<RawValue>>>(text)
                .ok()?
                .iter()
                .map(|value| stringify(value, depth + 1))
                .collect::<Option<Vec<_>>>()?;
            format!("[{}]", items.join(","))
        }
        b'"' => serde_json::from_str::<JsString>(text).ok()?.stringify(),
        b'-' | b'0'..=b'9' => {
            let number = text.parse::<f64>().ok()?;
            if number.is_finite() {
                ryu_js::Buffer::new().format(number).to_owned()
            } else {
                "null".into()
            }
        }
        _ => text.to_owned(),
    })
}

/// A JavaScript string as WTF-8: UTF-8 that may also encode lone surrogates.
struct JsString(Vec<u8>);

impl JsString {
    fn stringify(&self) -> String {
        let mut json = String::from('"');
        let mut surrogate = Vec::new();
        for chunk in self.0.utf8_chunks() {
            let escaped = serde_json::Value::from(chunk.valid()).to_string();
            json.push_str(&escaped[1..escaped.len() - 1]);
            surrogate.extend_from_slice(chunk.invalid());
            if let [_, high, low] = surrogate[..] {
                let unit = 0xd000 | (u32::from(high & 0x3f) << 6) | u32::from(low & 0x3f);
                json.push_str(&format!("\\u{unit:04x}"));
                surrogate.clear();
            }
        }
        json.push('"');
        json
    }

    fn utf16(&self) -> JsText {
        let mut units = Vec::new();
        let mut surrogate = Vec::new();
        for chunk in self.0.utf8_chunks() {
            units.extend(chunk.valid().encode_utf16());
            surrogate.extend_from_slice(chunk.invalid());
            if let [_, high, low] = surrogate[..] {
                units.push(0xd000 | (u16::from(high & 0x3f) << 6) | u16::from(low & 0x3f));
                surrogate.clear();
            }
        }
        JsText::from_units(units)
    }

    fn array_index(&self) -> Option<u32> {
        let key = std::str::from_utf8(&self.0).ok()?;
        key.parse::<u32>()
            .ok()
            .filter(|index| *index != u32::MAX && index.to_string() == key)
    }
}

impl<'de> Deserialize<'de> for JsString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Wtf8;
        impl Visitor<'_> for Wtf8 {
            type Value = JsString;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON string")
            }
            fn visit_bytes<E>(self, bytes: &[u8]) -> Result<JsString, E> {
                Ok(JsString(bytes.to_vec()))
            }
        }
        // serde_json decodes lone surrogate escapes only when asked for bytes.
        deserializer.deserialize_bytes(Wtf8)
    }
}

/// Object members in JavaScript property order: array-index keys ascending, then the rest in
/// first-seen order. A repeated key keeps its first position and its last value.
struct JsObject(Vec<(JsString, Box<RawValue>)>);

impl<'de> Deserialize<'de> for JsObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Members;
        impl<'de> Visitor<'de> for Members {
            type Value = JsObject;
            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<JsObject, A::Error> {
                let mut members: Vec<(JsString, Box<RawValue>)> = Vec::new();
                let mut positions = HashMap::<Vec<u8>, usize>::new();
                while let Some((key, value)) = map.next_entry::<JsString, Box<RawValue>>()? {
                    match positions.entry(key.0.clone()) {
                        Entry::Occupied(position) => members[*position.get()].1 = value,
                        Entry::Vacant(position) => {
                            position.insert(members.len());
                            members.push((key, value));
                        }
                    }
                }
                members.sort_by_key(|(key, _)| key.array_index().map_or(u64::MAX, u64::from));
                Ok(JsObject(members))
            }
        }
        deserializer.deserialize_map(Members)
    }
}
