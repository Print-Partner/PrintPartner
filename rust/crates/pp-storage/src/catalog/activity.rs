use super::*;
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct SourceActivityEvent {
    pub id: i64,
    pub at: String,
    pub kind: String,
    /// Node writes an integer Source ID; the client schema says so.
    #[cfg_attr(feature = "codegen", schemars(with = "Option<i64>"))]
    pub source_id: Option<serde_json::Number>,
    pub source_name: String,
    pub detail: Option<String>,
}
pub(super) fn list(
    tx: &Transaction<'_>,
    tenant: &str,
    limit: u8,
) -> Result<Vec<SourceActivityEvent>> {
    let mut statement = tx.prepare(
        "SELECT id,at,kind,payload_json FROM app_events WHERE tenant_id=?1
         AND kind IN ('source.update_available','source.updated','source.sync_failed')
         ORDER BY id DESC LIMIT ?2",
    )?;
    Ok(statement
        .query_map(params![tenant, limit.clamp(1, 100)], |row| {
            let raw: Option<String> = row.get(3)?;
            let payload = raw
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .filter(Value::is_object)
                .unwrap_or(json!({}));
            Ok(SourceActivityEvent {
                id: row.get(0)?,
                at: row.get(1)?,
                kind: row.get(2)?,
                source_id: payload["source_id"].as_number().cloned(),
                source_name: payload["source_name"].as_str().unwrap_or("Source").into(),
                detail: payload["error"].as_str().map(str::to_owned),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
