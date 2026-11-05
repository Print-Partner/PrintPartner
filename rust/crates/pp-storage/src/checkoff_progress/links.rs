//! Printer Checkoff links and print outcomes as the Node server stores them.
//! Links are one JSON array in the `printer.checkoff_links` setting, read and
//! normalized like `services/printer-checkoff-store.ts`; outcomes are
//! `print_jobs` and `print_job_parts` rows written like
//! `services/printer-outcomes-store.ts`.

use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Map, Number, Value, json};

const KEY: &str = "printer.checkoff_links";
const MAX_LINKS: usize = 200;
const MAX_SAFE: f64 = 9_007_199_254_740_991.0;
const REASONS: [&str; 10] = [
    "bed_adhesion",
    "layer_shift",
    "warping",
    "stringing",
    "under_extrusion",
    "over_extrusion",
    "dimensional",
    "collision",
    "wrong_filament",
    "other",
];

/// A parsed link. Every field of the Node shape is present in Node's order;
/// `Value::Null` stands for a JavaScript `undefined` and is never serialized.
pub(super) type Link = Map<String, Value>;

pub(super) fn now() -> String {
    crate::auth::timestamp(0)
}

pub(super) fn uuid() -> String {
    let mut b = rand::random::<[u8; 16]>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

pub(super) fn is_reason(value: &Value) -> bool {
    value.as_str().is_some_and(|s| REASONS.contains(&s))
}

/// `String.prototype.trim`.
pub(super) fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// `String.prototype.slice(0, len)`, counted in UTF-16 code units.
/// `String.prototype.slice(0, len)`. A pair split at the limit leaves Node a
/// lone surrogate; Rust strings cannot hold one, so it becomes U+FFFD, as
/// [`js_json`] reads it back.
pub(super) fn js_slice(s: &str, len: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in s.chars() {
        units += c.len_utf16();
        if units > len {
            if units == len + 1 && c.len_utf16() == 2 {
                out.push('\u{FFFD}');
            }
            break;
        }
        out.push(c);
    }
    out
}

/// JSON text as `JSON.stringify` writes it, with each lone surrogate escape
/// replaced by U+FFFD so serde_json accepts what `JSON.parse` accepts.
pub(super) fn js_json(raw: &str) -> String {
    let surrogate = |hex: &str| {
        u16::from_str_radix(hex, 16)
            .ok()
            .filter(|u| (0xD800..0xE000).contains(u))
    };
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('\\') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let escape = rest.get(..6).filter(|e| e.starts_with("\\u"));
        let Some(high) = escape.and_then(|e| surrogate(&e[2..])) else {
            let len = rest[1..].chars().next().map_or(1, |c| 1 + c.len_utf8());
            out.push_str(&rest[..len]);
            rest = &rest[len..];
            continue;
        };
        let low = rest
            .get(6..12)
            .filter(|e| e.starts_with("\\u"))
            .and_then(|e| surrogate(&e[2..]));
        match low {
            Some(low) if high < 0xDC00 && low >= 0xDC00 => {
                out.push_str(&rest[..12]);
                rest = &rest[12..];
            }
            _ => {
                out.push_str("\\ufffd");
                rest = &rest[6..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `Number(value)`; `None` is `NaN`.
pub(super) fn js_number(value: &Value) -> Option<f64> {
    match value {
        Value::Null => Some(0.0),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let s = js_trim(s);
            if s.is_empty() {
                return Some(0.0);
            }
            let radix = match s.get(..2).map(str::to_ascii_lowercase).as_deref() {
                Some("0x") => 16,
                Some("0o") => 8,
                Some("0b") => 2,
                _ => 10,
            };
            if radix != 10 {
                return u64::from_str_radix(&s[2..], radix).ok().map(|n| n as f64);
            }
            match s.trim_start_matches(['+', '-']) {
                "Infinity" if s.starts_with('-') => Some(f64::NEG_INFINITY),
                "Infinity" => Some(f64::INFINITY),
                _ if s
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b".+-eE".contains(&b)) =>
                {
                    s.parse().ok()
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// `Number.isInteger(Number(value)) && n >= min`.
pub(super) fn js_integer(value: Option<&Value>, min: f64) -> Option<i64> {
    value
        .and_then(js_number)
        .filter(|n| n.is_finite() && n.fract() == 0.0 && *n >= min && n.abs() <= MAX_SAFE)
        .map(|n| n as i64)
}

/// JavaScript truthiness.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

pub(super) fn field<'a>(row: &'a Value, key: &str) -> &'a Value {
    row.get(key).unwrap_or(&Value::Null)
}

/// `typeof v === "string" && v.trim()`, optionally sliced.
fn text(value: &Value, len: Option<usize>) -> Option<Value> {
    let s = js_trim(value.as_str()?);
    (!s.is_empty()).then(|| Value::from(len.map_or_else(|| s.to_owned(), |len| js_slice(s, len))))
}

fn object(pairs: Vec<(&str, Option<Value>)>) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .filter_map(|(k, v)| Some((k.to_owned(), v?)))
            .collect(),
    )
}

/// `row && typeof row === "object"`.
fn js_object(value: &Value) -> bool {
    value.is_object() || value.is_array()
}

fn unit(raw: &Value) -> Option<Value> {
    if !js_object(raw) {
        return None;
    }
    let part_id = js_integer(raw.get("part_id"), 1.0)?;
    let unit_index = js_integer(raw.get("unit_index"), 0.0)?;
    Some(object(vec![
        ("part_id", Some(part_id.into())),
        ("unit_index", Some(unit_index.into())),
        ("object_name", text(field(raw, "object_name"), Some(200))),
    ]))
}

fn file_identity(raw: &Value) -> Value {
    if !js_object(raw) {
        return Value::Null;
    }
    let size = field(raw, "size_bytes")
        .as_f64()
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|n| Value::from((n + 0.5).floor() as i64));
    let sha = field(raw, "sha256")
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .map(Value::from);
    let identity = object(vec![
        ("size_bytes", size),
        ("modified_at", text(field(raw, "modified_at"), Some(100))),
        (
            "provider_revision",
            text(field(raw, "provider_revision"), Some(200)),
        ),
        ("sha256", sha),
    ]);
    if identity.as_object().is_some_and(Map::is_empty) {
        Value::Null
    } else {
        identity
    }
}

fn drift(raw: &Value, now: &str) -> Value {
    let reason = field(raw, "reason");
    if !js_object(raw)
        || !reason
            .as_str()
            .is_some_and(|r| ["missing", "size", "modified", "revision", "content"].contains(&r))
    {
        return Value::Null;
    }
    let detected = field(raw, "detected_at")
        .as_str()
        .map(js_trim)
        .unwrap_or("");
    json!({ "reason": reason, "detected_at": if detected.is_empty() { now } else { detected } })
}

fn classification(raw: &Value) -> Value {
    let kind = field(raw, "kind");
    match field(raw, "format").as_str() {
        Some(format @ ("gcode" | "bgcode")) => json!({ "format": format }),
        Some("3mf")
            if kind.as_str().is_some_and(|k| {
                [
                    "slicer_project",
                    "model_package",
                    "toolpath_package",
                    "unsupported",
                ]
                .contains(&k)
            }) =>
        {
            json!({ "format": "3mf", "kind": kind })
        }
        _ => Value::Null,
    }
}

fn resolved(raw: &Value) -> Value {
    let Some(items) = raw.as_array() else {
        return Value::Null;
    };
    let out: Vec<Value> = items
        .iter()
        .filter(|item| js_object(item))
        .filter_map(|row| {
            let part_id = js_integer(row.get("part_id"), 1.0)?;
            let unit_index = js_integer(row.get("unit_index"), 0.0)?;
            let result = field(row, "result");
            if result != "confirmed" && result != "rejected" {
                return None;
            }
            let reason = field(row, "reason");
            Some(object(vec![
                ("part_id", Some(part_id.into())),
                ("unit_index", Some(unit_index.into())),
                ("result", Some(result.clone())),
                ("reason", reason.is_string().then(|| reason.clone())),
                ("note", text(field(row, "note"), Some(500))),
            ]))
        })
        .collect();
    if out.is_empty() {
        Value::Null
    } else {
        out.into()
    }
}

/// `readResult` in `imported-print-inventory.ts`.
pub(super) fn extra_result(raw: &Value) -> Option<Value> {
    if !js_object(raw) {
        return None;
    }
    match field(raw, "result").as_str() {
        Some("confirmed") => Some(json!({ "result": "confirmed" })),
        Some("rejected") if is_reason(field(raw, "reason")) => {
            let note = match raw.get("note") {
                None => None,
                Some(Value::String(s)) => Some(Value::from(js_slice(js_trim(s), 500))),
                Some(_) => return None,
            };
            Some(object(vec![
                ("result", Some("rejected".into())),
                ("reason", Some(field(raw, "reason").clone())),
                ("note", note),
            ]))
        }
        _ => None,
    }
}

/// `readImportedInventory`.
pub(super) fn imported_inventory(raw: &Value) -> Value {
    let Some(items) = raw.get("extras").and_then(Value::as_array) else {
        return Value::Null;
    };
    let mut extras = Vec::with_capacity(items.len());
    for item in items {
        let name = field(item, "name");
        let kind = field(item, "kind");
        let checkoff = field(item, "checkoff");
        if !js_object(item)
            || !name.is_string()
            || (kind != "object" && kind != "file")
            || !checkoff.is_object()
            || checkoff.get("result").is_none()
        {
            return Value::Null;
        }
        let checkoff = if checkoff["result"] == "pending" {
            json!({ "result": "pending" })
        } else {
            let (Some(Value::Object(mut result)), Some(at)) = (
                extra_result(checkoff),
                checkoff.get("checked_at").filter(|v| v.is_string()),
            ) else {
                return Value::Null;
            };
            result.insert("checked_at".into(), at.clone());
            Value::Object(result)
        };
        extras.push(json!({ "name": name, "kind": kind, "checkoff": checkoff }));
    }
    json!({ "extras": extras })
}

fn finite(raw: &Value, key: &str) -> Option<f64> {
    field(raw, key).as_f64().filter(|n| n.is_finite())
}

/// The print file fields of `parseLink`.
fn file_fields(raw: &Value, now: &str) -> [(&'static str, Value); 5] {
    let revision = finite(raw, "plan_revision_id")
        .filter(|n| n.fract() == 0.0 && *n > 0.0 && *n <= MAX_SAFE)
        .map_or(Value::Null, |n| Value::from(n as i64));
    [
        ("plan_revision_id", revision),
        (
            "remote_identity",
            file_identity(field(raw, "remote_identity")),
        ),
        ("remote_drift", drift(field(raw, "remote_drift"), now)),
        (
            "classification",
            classification(field(raw, "classification")),
        ),
        (
            "imported_inventory",
            imported_inventory(field(raw, "imported_inventory")),
        ),
    ]
}

/// The state and timing fields of `parseLink`.
fn status_fields(raw: &Value, state: &str, now: &str) -> [(&'static str, Value); 9] {
    let host_outcome = match (field(raw, "host_outcome").as_str(), state) {
        (Some(o @ ("unknown" | "success" | "failed" | "cancelled")), _) => Value::from(o),
        (_, "verified" | "awaiting_verify") => "success".into(),
        (_, "host_failed") => "failed".into(),
        _ => Value::Null,
    };
    let nonempty = |key: &str| {
        field(raw, key)
            .as_str()
            .filter(|s| !s.is_empty())
            .map(Value::from)
    };
    let progress = finite(raw, "last_progress").map_or(Value::Null, |n| {
        Value::from((n.clamp(0.0, 100.0) + 0.5).floor() as i64)
    });
    let marked = finite(raw, "units_marked").map_or(Value::Null, |_| raw["units_marked"].clone());
    [
        ("state", state.into()),
        ("host_outcome", host_outcome),
        ("saw_active", truthy(field(raw, "saw_active")).into()),
        ("started", truthy(field(raw, "started")).into()),
        ("last_progress", progress),
        (
            "created_at",
            nonempty("created_at").unwrap_or_else(|| now.into()),
        ),
        (
            "completed_at",
            nonempty("completed_at")
                .or_else(|| nonempty("applied_at"))
                .unwrap_or_default(),
        ),
        ("applied_at", nonempty("applied_at").unwrap_or_default()),
        ("units_marked", marked),
    ]
}

/// `parseLink`, including the legacy `applied` state that now reads as verified.
fn parse(raw: &Value, now: &str) -> Option<Link> {
    let state = match raw.get("state")?.as_str()? {
        "applied" => "verified",
        s @ ("watching" | "awaiting_verify" | "host_failed" | "dismissed" | "verified") => s,
        _ => return None,
    };
    let units: Vec<Value> = field(raw, "units")
        .as_array()
        .map(|units| units.iter().filter_map(unit).collect())
        .unwrap_or_default();
    let unlabeled: Vec<Value> = field(raw, "unlabeled_names")
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(|n| text(n, Some(200)))
                .take(200)
                .collect()
        })
        .unwrap_or_default();
    let head = [
        ("id", text(field(raw, "id"), None)?),
        ("profile_id", js_integer(raw.get("profile_id"), 1.0)?.into()),
        ("integration_id", text(field(raw, "integration_id"), None)?),
        ("printer_id", text(field(raw, "printer_id"), None)?),
        (
            "host_name",
            text(field(raw, "host_name"), None).unwrap_or_else(|| "Printer".into()),
        ),
        ("filename", text(field(raw, "filename"), None)?),
        (
            "remote_path",
            text(field(raw, "remote_path"), None).unwrap_or_default(),
        ),
        (
            "upload_job_id",
            text(field(raw, "upload_job_id"), None).unwrap_or_default(),
        ),
    ];
    let lists = [
        ("units", units.into()),
        (
            "unlabeled_names",
            if unlabeled.is_empty() {
                Value::Null
            } else {
                unlabeled.into()
            },
        ),
        ("resolved_units", resolved(field(raw, "resolved_units"))),
    ];
    Some(
        head.into_iter()
            .chain(file_fields(raw, now))
            .chain(lists)
            .chain(status_fields(raw, state, now))
            .map(|(k, v)| (k.to_owned(), v))
            .collect(),
    )
}

/// The link as Node's JSON serializes it: no `undefined` fields.
pub(super) fn public(link: &Link) -> Value {
    Value::Object(
        link.iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

/// `loadPrinterCheckoffLinks`: unreadable settings and entries drop out.
pub(super) fn load(tx: &Transaction<'_>, tenant: &str, now: &str) -> Result<Vec<Link>> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, KEY],
            |r| r.get(0),
        )
        .optional()?;
    let Some(Ok(Value::Array(items))) = raw
        .filter(|raw| !js_trim(raw).is_empty())
        .map(|raw| serde_json::from_str::<Value>(&js_json(&raw)))
    else {
        return Ok(Vec::new());
    };
    Ok(items.iter().filter_map(|item| parse(item, now)).collect())
}

/// The end of `updatePrinterCheckoffLink`: the patched link moves to the end
/// before the list is saved.
pub(super) fn save(
    tx: &Transaction<'_>,
    tenant: &str,
    mut links: Vec<Link>,
    index: usize,
) -> Result<()> {
    let link = links.remove(index);
    links.push(link);
    save_all(tx, tenant, links)
}

/// `savePrinterCheckoffLinks`: `trimPrinterCheckoffLinks` keeps every active
/// link and the newest terminal ones, then the list is written back.
pub(super) fn save_all(tx: &Transaction<'_>, tenant: &str, links: Vec<Link>) -> Result<()> {
    let terminal = |link: &Link| {
        matches!(
            link["state"].as_str(),
            Some("verified" | "dismissed" | "host_failed" | "applied")
        ) && !link["imported_inventory"]["extras"]
            .as_array()
            .is_some_and(|extras| !extras.is_empty())
    };
    let (done, active): (Vec<Link>, Vec<Link>) = links.into_iter().partition(terminal);
    let keep = MAX_LINKS.saturating_sub(active.len());
    let kept: Vec<Value> = active
        .iter()
        .chain(done[done.len().saturating_sub(keep)..].iter())
        .map(public)
        .collect();
    tx.execute(
        "INSERT INTO app_settings (tenant_id, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (tenant_id, key) DO UPDATE SET value=excluded.value",
        params![tenant, KEY, serde_json::to_string(&kept)?],
    )?;
    Ok(())
}

/// One unit result for `appendPrintOutcomes`.
pub(super) struct Outcome {
    pub part_id: i64,
    pub unit_index: i64,
    pub result: &'static str,
    pub reason: Option<String>,
    pub note: Option<String>,
    pub match_key: String,
    pub role: String,
}

/// `appendPrintOutcomes` for one link: one `print_jobs` row and one
/// `print_job_parts` row per result. Returns the events Node returns.
pub(super) fn append_outcomes(
    tx: &Transaction<'_>,
    tenant: &str,
    link: &Link,
    outcomes: &[Outcome],
    now: &str,
) -> Result<Vec<Value>> {
    if outcomes.is_empty() {
        return Ok(Vec::new());
    }
    let profile = link["profile_id"].as_i64();
    let host = link["integration_id"].as_str();
    let filename = link["filename"].as_str();
    let link_id = link["id"].as_str();
    let job = uuid();
    tx.execute(
        "INSERT INTO print_jobs (id, tenant_id, profile_id, host_integration_id, printer_id, material, filename, status, filament_consumed_g, at, completed_at, link_id)
         VALUES (?1, ?2, ?3, ?4, '', '', ?5, 'sent', NULL, ?6, NULL, ?7)",
        params![job, tenant, profile, host, filename, now, link_id],
    )?;
    let present = |s: Option<&str>| s.filter(|s| !s.is_empty()).map(Value::from);
    let mut events = Vec::with_capacity(outcomes.len());
    for o in outcomes {
        let id = uuid();
        tx.execute(
            "INSERT INTO print_job_parts (id, tenant_id, job_id, at, profile_id, part_id, unit_index, result, reason, note, host_integration_id, filename, match_key, role, filament_display, link_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, NULL, ?15)",
            params![
                id, tenant, job, now, profile, o.part_id, o.unit_index, o.result, o.reason, o.note,
                host, filename, o.match_key, o.role, link_id
            ],
        )?;
        events.push(object(vec![
            ("id", Some(id.into())),
            ("at", Some(now.into())),
            ("profile_id", profile.map(Value::from)),
            ("part_id", Some(o.part_id.into())),
            ("unit_index", Some(o.unit_index.into())),
            ("result", Some(o.result.into())),
            (
                "reason",
                o.reason.clone().map(Value::from).filter(is_reason),
            ),
            ("note", present(o.note.as_deref())),
            ("host_integration_id", present(host)),
            ("filename", present(filename)),
            ("match_key", present(Some(&o.match_key))),
            ("role", present(Some(&o.role))),
            ("link_id", present(link_id)),
        ]));
    }
    Ok(events)
}

/// `(current ?? 0) + added`, keeping integers integral.
pub(super) fn add(current: &Value, added: usize) -> Value {
    match current {
        Value::Number(n) if n.is_i64() => Value::from(n.as_i64().unwrap_or(0) + added as i64),
        Value::Number(n) => Number::from_f64(n.as_f64().unwrap_or(0.0) + added as f64)
            .map_or(Value::Null, Value::Number),
        _ => Value::from(added),
    }
}

#[cfg(test)]
mod tests {
    use super::{js_json, js_slice};

    #[test]
    fn js_json_reads_lone_surrogates_as_replacement_and_keeps_pairs() {
        let raw = r#"["a\ud83d","\ud83d\ude00","\ude00x","\\ud83d","\"\u00e9"]"#;
        let parsed: Vec<String> = serde_json::from_str(&js_json(raw)).unwrap();
        assert_eq!(
            parsed,
            ["a\u{FFFD}", "\u{1F600}", "\u{FFFD}x", "\\ud83d", "\"é"]
        );
    }

    #[test]
    fn js_slice_marks_a_pair_split_at_the_limit() {
        assert_eq!(js_slice("ab\u{1F600}", 3), "ab\u{FFFD}");
        assert_eq!(js_slice("ab\u{1F600}", 4), "ab\u{1F600}");
        assert_eq!(js_slice("ab\u{1F600}c", 2), "ab");
    }
}
