//! Node's proposal normalization for reviewed draft Part edits.
use super::reads::as_int;
use serde_json::{Map, Value, json};

pub(super) fn proposal(
    build: i64,
    draft_id: i64,
    args: &Map<String, Value>,
    draft: &Value,
) -> Result<Value, String> {
    let parts = args
        .get("parts")
        .and_then(Value::as_array)
        .filter(|parts| !parts.is_empty())
        .ok_or("parts required")?;
    let parts = parts
        .iter()
        .map(|row| {
            if !row.is_object() && !row.is_array() {
                return Err("Invalid draft part edit".into());
            }
            let id = as_int(
                row.get("part_id")
                    .filter(|id| !id.is_null())
                    .or_else(|| row.get("id")),
            );
            let id = id
                .filter(|id| {
                    draft["parts"]
                        .as_array()
                        .is_some_and(|parts| parts.iter().any(|part| part["id"] == *id))
                })
                .ok_or_else(|| {
                    format!(
                        "Draft part not found: {}",
                        id.map_or("missing".into(), |id| id.to_string())
                    )
                })?;
            let mut edit = json!({"part_id":id});
            if row["included"].is_boolean() {
                edit["included"] = row["included"].clone();
            }
            if let Some(quantity) = row.get("quantity_override")
                && (quantity.is_null()
                    || quantity.as_f64().is_some_and(|n| {
                        n >= 0.0 && n <= 9_007_199_254_740_991.0 && n.fract() == 0.0
                    }))
            {
                edit["quantity_override"] =
                    quantity.as_f64().map_or(Value::Null, |n| json!(n as i64));
            }
            if edit.get("included").is_none() && edit.get("quantity_override").is_none() {
                return Err("Each edit must set included or quantity_override".into());
            }
            Ok(edit)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(json!({
        "plan_id":build,
        "draft_id":draft_id,
        "expected_snapshot_digest":args.get("expected_snapshot_digest").filter(|value| value.is_string()).unwrap_or(&draft["snapshotDigest"]),
        "parts":parts
    }))
}
