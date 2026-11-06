//! The snapshot Accept saves of the Plan before it publishes, so History can
//! restore it. The payload has the shape of Node's `capturePlanSnapshotPayload`
//! (`services/plan-snapshots.ts`), which Node's restore reads, plus the
//! Accepted Plan revision and version it was taken from.

use super::*;
use rusqlite::OptionalExtension;

/// Inserts one `pre_apply` `plan_snapshots` row for Build `profile` inside
/// Accept's transaction, before the publication writes.
pub(super) fn save_before_accept(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    draft_id: i64,
    accepted: (Option<i64>, i64),
    at: &str,
) -> Result<()> {
    let layers = tx
        .prepare(
            "SELECT layer.layer_type,layer.project_id,source.name,source.tag,source.branch,layer.layer_order FROM profile_layers layer LEFT JOIN projects source ON source.tenant_id=layer.tenant_id AND source.id=layer.project_id WHERE layer.tenant_id=?1 AND layer.profile_id=?2 ORDER BY layer.layer_order,layer.id",
        )?
        .query_map(params![tenant, profile], |row| {
            Ok(json!({
                "layer_type": row.get::<_, String>(0)?,
                "project_id": row.get::<_, Option<i64>>(1)?,
                "source_name": row.get::<_, Option<String>>(2)?,
                "tag": row.get::<_, Option<String>>(3)?,
                "branch": row.get::<_, Option<String>>(4)?,
                "order": row.get::<_, i64>(5)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let manifest: Value = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, format!("kit_manifest_{profile}")],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|stored| serde_json::from_str(&stored).ok())
        .unwrap_or_default();
    let field = |key: &str, keep: fn(&Value) -> bool, empty: Value| {
        manifest
            .get(key)
            .filter(|value| keep(value))
            .cloned()
            .unwrap_or(empty)
    };
    let payload = json!({
        "layers": layers,
        "kit_manifest": {
            "name": field("name", Value::is_string, Value::Null),
            "selections": field("selections", Value::is_object, json!({})),
            "include": field("include", Value::is_array, json!([])),
            "exclude": field("exclude", Value::is_array, json!([])),
            "replacements": field("replacements", Value::is_object, json!({})),
            "base_source_id": field("base_source_id", Value::is_string, Value::Null),
            "addon_source_ids": field("addon_source_ids", Value::is_array, json!([])),
        },
        "recipe_summary": null,
        "captured_at": at,
        "accepted_plan_revision_id": accepted.0,
        "accepted_plan_version": accepted.1,
    });
    tx.execute(
        "INSERT INTO plan_snapshots(tenant_id,profile_id,name,created_at,source,payload_json) VALUES(?1,?2,?3,?4,'pre_apply',?5)",
        params![
            tenant,
            profile,
            format!("Before accepting draft {draft_id}"),
            at,
            payload.to_string()
        ],
    )?;
    Ok(())
}
