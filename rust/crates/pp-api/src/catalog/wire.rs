use pp_storage::catalog::{
    CategorySettings, CreateSource, Deletion, NamingCommand, NamingOverride, NamingPreview,
    NamingProfile, SourceActivityEvent, SourcePatch, SourceSummary,
};
use serde_json::{Map, Value, json};
use std::collections::HashMap;

pub(super) fn sources(rows: Vec<SourceSummary>) -> Value {
    json!({ "sources": rows })
}

pub(super) fn source(row: SourceSummary) -> Result<Value, serde_json::Error> {
    serde_json::to_value(row)
}

/// Wrapped `{ categories, tree }` body from `GetCategoriesWithTree` /
/// `SaveCategoriesWithTree`. The unwrapped list-only `GetCategories` /
/// `SaveCategories` responses stay off this router.
pub(super) fn categories(settings: CategorySettings) -> Result<Value, serde_json::Error> {
    serde_json::to_value(settings)
}

type CategorySave = (Vec<String>, HashMap<String, Option<String>>);

/// Node PUT body: `{ use_defaults: true }` or
/// `{ use_defaults: false, override: <NamingProfile> }`. Storage tags the
/// command as `kind`; do not deserialize the HTTP body as `NamingCommand`.
pub(super) fn naming_save(body: &Value) -> Option<NamingCommand> {
    let object = body.as_object()?;
    let use_defaults = object.get("use_defaults")?.as_bool()?;
    if use_defaults {
        return (object.len() == 1).then_some(NamingCommand::UseDefaults);
    }
    if object.len() != 2 {
        return None;
    }
    let profile: NamingProfile = serde_json::from_value(object.get("override")?.clone()).ok()?;
    Some(NamingCommand::Override { profile })
}

/// Node preview body: `{ relative_path, profile? }`. Missing or null
/// `relative_path` becomes `""`; other scalars use Node `String()` coercion.
pub(super) fn naming_preview(body: &Value) -> Option<(String, Option<NamingOverride>)> {
    let object = body.as_object()?;
    let relative_path = match object.get("relative_path") {
        None | Some(Value::Null) => String::new(),
        Some(value) => js_string(value),
    };
    let profile = match object.get("profile") {
        None | Some(Value::Null) => None,
        Some(value) => Some(serde_json::from_value(value.clone()).ok()?),
    };
    Some((relative_path, profile))
}

pub(super) fn naming_preview_body(preview: NamingPreview) -> Result<Value, serde_json::Error> {
    serde_json::to_value(preview)
}

/// Storage returns a bare `NamingProfile`; HTTP wraps `{ "profile": ... }`
/// to match Node `GET`/`PUT /settings/stl-naming`.
pub(super) fn global_naming(profile: Value) -> Value {
    json!({ "profile": profile })
}

/// Node PUT body: `{ profile?: NamingProfile }`. Missing or null `profile`
/// becomes the default profile; a non-object `profile` is refused.
pub(super) fn global_naming_save(body: &Value) -> Option<NamingProfile> {
    match body.get("profile") {
        None | Some(Value::Null) => Some(NamingProfile::default()),
        Some(value) => serde_json::from_value(value.clone()).ok(),
    }
}

/// Node PUT body: `{ rules?: string[] }`. Missing or null `rules` becomes `[]`.
/// Each rule uses Node `String()` coercion; a non-array `rules` is refused.
pub(super) fn import_rules_save(body: &Value) -> Option<Vec<String>> {
    match body.get("rules") {
        None | Some(Value::Null) => Some(Vec::new()),
        Some(Value::Array(items)) => Some(items.iter().map(js_string).collect()),
        _ => None,
    }
}

/// Storage returns a bare activity array; HTTP wraps `{ "events": [...] }`.
pub(super) fn activity(events: Vec<SourceActivityEvent>) -> Result<Value, serde_json::Error> {
    Ok(json!({ "events": events }))
}

/// Node: `Number(query.limit ?? 20)`; non-finite → `20`; then trunc + clamp `1..=100`.
pub(super) fn activity_limit(raw: Option<&str>) -> u8 {
    let requested = match raw {
        None => 20.0,
        Some(value) => match value.parse::<f64>() {
            Ok(number) if number.is_finite() => number,
            _ => 20.0,
        },
    };
    requested.trunc().clamp(1.0, 100.0) as u8
}

/// Node PUT body: `categories` defaults to `[]`, `replacements` to `{}`.
/// Each category must be a string; each replacement value is a string or null.
pub(super) fn category_save(body: &Value) -> Option<CategorySave> {
    let categories = match body.get("categories") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let Value::String(value) = item else {
                    return None;
                };
                out.push(value.clone());
            }
            out
        }
        _ => return None,
    };
    let replacements = match body.get("replacements") {
        None | Some(Value::Null) => HashMap::new(),
        Some(Value::Object(map)) => {
            let mut out = HashMap::with_capacity(map.len());
            for (key, value) in map {
                let mapped = match value {
                    Value::Null => None,
                    Value::String(value) => Some(value.clone()),
                    _ => return None,
                };
                out.insert(key.clone(), mapped);
            }
            out
        }
        _ => return None,
    };
    Some((categories, replacements))
}

/// `String(body.name ?? "")`, with null and missing fields omitted on the other columns.
pub(super) fn source_create(body: &Value) -> CreateSource {
    CreateSource {
        name: text(body, "name").unwrap_or_default(),
        url: text(body, "url"),
        branch: text(body, "branch"),
        tag: text(body, "tag"),
        source_kind: text(body, "source_kind"),
        role: text(body, "role"),
        metadata: object(body, "metadata"),
        ..Default::default()
    }
}

/// Patch omits null columns. An explicit null `tag` clears the stored tag.
/// `metadata_version` is a safe integer, or omitted when null or absent.
pub(super) fn source_patch(body: &Value) -> Result<SourcePatch, &'static str> {
    Ok(SourcePatch {
        name: text(body, "name"),
        url: text(body, "url"),
        branch: text(body, "branch"),
        tag: body
            .get("tag")
            .map(|value| (!value.is_null()).then(|| js_string(value))),
        source_kind: text(body, "source_kind"),
        role: text(body, "role"),
        metadata: object(body, "metadata"),
        metadata_version: metadata_version(body)?,
        ..Default::default()
    })
}

fn metadata_version(body: &Value) -> Result<Option<i64>, &'static str> {
    let Some(value) = body.get("metadata_version") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    match value.as_i64() {
        Some(number) if (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&number) => {
            Ok(Some(number))
        }
        _ => Err("metadata_version must be an integer"),
    }
}

fn text(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .filter(|value| !value.is_null())
        .map(js_string)
}

fn object(body: &Value, key: &str) -> Option<Map<String, Value>> {
    body.get(key).and_then(Value::as_object).cloned()
}

/// JavaScript's `String(value)` for a JSON value; `null` inside an array is empty.
pub(crate) fn js_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".into(),
        Value::Bool(value) => {
            if *value {
                "true".into()
            } else {
                "false".into()
            }
        }
        Value::Number(value) => js_number(value),
        Value::Array(items) => items
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    js_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

pub(super) enum DeletionHttp {
    Deleted,
    Refused { status: u16, detail: &'static str },
}

/// Node delete statuses, plus the catalog busy and reference refusals.
pub(super) fn source_deletion(outcome: Deletion) -> DeletionHttp {
    match outcome {
        Deletion::Deleted { .. } => DeletionHttp::Deleted,
        Deletion::NotFound => DeletionHttp::Refused {
            status: 404,
            detail: "Source not found",
        },
        Deletion::RetainedHistory => DeletionHttp::Refused {
            status: 409,
            detail: "Source has immutable revision history and cannot be deleted",
        },
        Deletion::Referenced => DeletionHttp::Refused {
            status: 409,
            detail: "Source is referenced and cannot be deleted",
        },
        Deletion::ActiveWork => DeletionHttp::Refused {
            status: 409,
            detail: "Source has active work and cannot be deleted",
        },
    }
}

fn js_number(number: &serde_json::Number) -> String {
    match number.as_f64() {
        Some(value) => ryu_js::Buffer::new().format(value).to_owned(),
        None => number.to_string(),
    }
}
