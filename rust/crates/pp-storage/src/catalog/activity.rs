use super::*;
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct SourceActivityEvent {
    pub id: i64,
    pub at: String,
    pub kind: String,
    /// Node writes an integer Source ID; the client schema says so.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<i64>")
    )]
    pub source_id: Option<serde_json::Number>,
    pub source_name: String,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<String>")
    )]
    pub detail: Option<String>,
}

pub(crate) enum SourceOutcome {
    Updated,
    SyncFailed,
}

/// Call from the transaction that commits the Source effect or failed target.
/// The event carries only the tenant's Source ID and display name.
pub(crate) fn record(
    tx: &Transaction<'_>,
    tenant: &str,
    source_id: i64,
    outcome: SourceOutcome,
) -> Result<()> {
    let kind = match outcome {
        SourceOutcome::Updated => "source.updated",
        SourceOutcome::SyncFailed => "source.sync_failed",
    };
    ensure!(
        tx.execute(
            "INSERT INTO app_events (tenant_id, at, kind, payload_json)
             SELECT tenant_id, ?3, ?4, json_object('source_id', id, 'source_name', name)
             FROM projects WHERE tenant_id=?1 AND id=?2",
            params![tenant, source_id, auth::catalog_timestamp(), kind],
        )? == 1,
        "Source activity requires an existing Source"
    );
    Ok(())
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
