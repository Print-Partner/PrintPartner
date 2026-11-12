//! Validate inventory matches and save confirmed role assignments atomically.
use super::*;
use crate::read_model::views::js_trim;
use rusqlite::OptionalExtension;

fn text(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        crate::checkoff_progress::links::js_string(value)
    }
}

pub(super) fn base(row: &Value) -> Result<Value, String> {
    if !row.is_object() && !row.is_array() {
        return Err("brief and assignment required".into());
    }
    let role = text(&row["role"]);
    let role = js_trim(&role);
    let kind = text(&row["inventory_kind"]);
    let color = text(&row["color_hex"]);
    if role.is_empty()
        || !["catalog", "spoolman", "custom", "substitute"].contains(&kind.as_str())
        || color.len() != 7
        || !color.starts_with('#')
        || !color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
    {
        return Err("Invalid filament assignment".into());
    }
    if ["custom", "substitute"].contains(&kind.as_str()) && row["substitution_confirmed"] != true {
        return Err("Filament substitution requires explicit confirmation".into());
    }
    let mut assignment = json!({"role":role,"inventory_kind":kind,"color_hex":color,"substitution_confirmed":row["substitution_confirmed"] == true});
    let id = row["inventory_id"]
        .as_str()
        .map(js_trim)
        .unwrap_or_default();
    if !id.is_empty() {
        assignment["inventory_id"] = json!(id);
    }
    for key in ["requested_brand", "requested_name"] {
        if row[key].is_string() {
            assignment[key] = row[key].clone();
        }
    }
    Ok(assignment)
}

pub(super) fn normalize(row: &Value, inventory: Option<&Value>) -> Result<Value, String> {
    let assignment = base(row)?;
    let kind = assignment["inventory_kind"].as_str().unwrap_or_default();
    let color = assignment["color_hex"].as_str().unwrap_or_default();
    if ["catalog", "spoolman"].contains(&kind) {
        let inventory = inventory.ok_or(if kind == "catalog" {
            "Catalog inventory filament not found"
        } else {
            "Spoolman inventory filament could not be verified"
        })?;
        let source = if kind == "catalog" {
            "Catalog"
        } else {
            "Spoolman"
        };
        if inventory["hex"].as_str().unwrap_or_default().to_lowercase() != color.to_lowercase() {
            return Err(format!("{source} inventory filament color does not match"));
        }
        for (requested, field, noun) in [
            ("requested_name", "display_name", "color"),
            (
                "requested_brand",
                if kind == "catalog" {
                    "product_line"
                } else {
                    "brand"
                },
                "brand",
            ),
        ] {
            if let Some(requested) = row[requested]
                .as_str()
                .map(js_trim)
                .filter(|s| !s.is_empty())
                && inventory[field].as_str().unwrap_or_default().to_lowercase()
                    != requested.to_lowercase()
            {
                let source = if kind == "catalog" {
                    "catalog"
                } else {
                    "Spoolman"
                };
                return Err(format!(
                    "Requested {noun} is not an exact {source} inventory match; confirm a substitute instead"
                ));
            }
        }
    }
    Ok(assignment)
}

pub(super) fn catalog(id: &str) -> Option<Value> {
    static CATALOG: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let catalog = CATALOG.get_or_init(|| {
        serde_json::from_str(include_str!(
            // ast-grep-ignore: no-writer-family-literal -- Read-only shipped filament catalog.
            "../../../../../../../web/apps/server/src/data/ambrosia_fallback.json"
        ))
        .unwrap_or(Value::Null)
    });
    catalog["colors"]
        .as_array()?
        .iter()
        .find(|row| row["id"] == id)
        .cloned()
}

pub(crate) fn assign(
    tx: &Transaction<'_>,
    account: &str,
    build: i64,
    brief: &mut Value,
    assignment: Value,
) -> Result<Value> {
    let role = assignment["role"]
        .as_str()
        .ok_or_else(|| anyhow!("Assignment role is missing"))?;
    let kind = assignment["inventory_kind"].as_str().unwrap_or_default();
    let key = format!("role_filaments_{build}");
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![account, key],
            |row| row.get(0),
        )
        .optional()?;
    let saved: Value = raw
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null);
    let mut defaults = json!({});
    for (role, row) in saved.as_object().into_iter().flatten() {
        if !row.is_object() && !row.is_array() {
            continue;
        }
        let mut fields = json!({});
        for field in [
            "filament_color_id",
            "filament_custom_hex",
            "spoolman_spool_id",
        ] {
            fields[field] = row[field].as_str().map_or(Value::Null, |s| json!(s));
        }
        defaults[role] = fields;
    }
    let inventory = ["catalog", "spoolman"].contains(&kind);
    defaults[role] = json!({
        "filament_color_id":if inventory {assignment["inventory_id"].clone()} else {Value::Null},
        "filament_custom_hex":if inventory {Value::Null} else {assignment["color_hex"].clone()},
        "spoolman_spool_id":null
    });
    let mut normalized = String::new();
    let mut separator = false;
    for c in role.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            normalized.push(c);
            separator = false;
        } else if !separator {
            normalized.push('_');
            separator = true;
        }
    }
    let requirement = format!("color_{normalized}");
    for row in brief["requirements"].as_array_mut().into_iter().flatten() {
        if row["key"] == requirement {
            row["status"] = json!("satisfied");
            row["detail"] = json!(format!(
                "Assigned {kind} filament {}",
                assignment["inventory_id"]
                    .as_str()
                    .or_else(|| assignment["color_hex"].as_str())
                    .unwrap_or_default()
            ));
        }
    }
    let mut roles = brief["role_filaments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    roles.retain(|row| row["role"] != role);
    let result = json!({"ok":true,"result":{"role":role}});
    roles.push(assignment);
    brief["role_filaments"] = json!(roles);
    if let Some(fields) = brief.as_object_mut() {
        fields.remove("draft_id");
    }
    brief["updated_at"] = json!(auth::catalog_timestamp());
    for (key, value) in [
        (key, defaults),
        (format!("build_planning.v1.{build}"), brief.clone()),
    ] {
        tx.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value", params![account,key,value.to_string()])?;
    }
    Ok(result)
}
