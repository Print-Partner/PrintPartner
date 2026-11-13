//! Controlled edits and durable-state observations for disposable MCP fixtures.
use rusqlite::{Connection, params};
use serde_json::Value;
use std::path::Path;
pub fn edit(path: &Path, sql: &str) -> rusqlite::Result<()> {
    Connection::open(path)?.execute_batch(sql)
}
pub fn setting(path: &Path, key: &str) -> Result<Value, Box<dyn std::error::Error>> {
    let raw: String = Connection::open(path)?.query_row(
        "SELECT value FROM app_settings WHERE tenant_id='default' AND key=?1",
        [key],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&raw)?)
}
pub fn save(path: &Path, key: &str, value: &Value) -> rusqlite::Result<()> {
    Connection::open(path)?.execute(
        "INSERT OR REPLACE INTO app_settings(tenant_id,key,value) VALUES('default',?1,?2)",
        params![key, value.to_string()],
    )?;
    Ok(())
}
// API-key last-used bookkeeping is outside the proposed Build operation.
pub fn state(path: &Path) -> rusqlite::Result<Vec<Vec<Vec<String>>>> {
    let db = Connection::open(path)?;
    [
        "SELECT * FROM profile_layers ORDER BY id",
        "SELECT * FROM app_settings WHERE key <> 'api_keys_v1' ORDER BY tenant_id,key",
        "SELECT * FROM plan_decisions ORDER BY id",
    ]
    .into_iter()
    .map(|sql| {
        let mut query = db.prepare(sql)?;
        let columns = query.column_count();
        query
            .query_map([], |row| {
                (0..columns)
                    .map(|i| Ok(format!("{:?}", row.get_ref(i)?)))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
    })
    .collect()
}
/// A draft's Parts as `(id, included, quantity_override)`, in id order.
pub fn draft_parts(path: &Path, draft_id: i64) -> rusqlite::Result<Vec<(i64, bool, Option<i64>)>> {
    let db = Connection::open(path)?;
    let mut query = db.prepare(
        "SELECT id,included,quantity_override FROM plan_draft_parts WHERE draft_id=?1 ORDER BY id",
    )?;
    query
        .query_map([draft_id], |row| {
            Ok((row.get(0)?, row.get::<_, i64>(1)? != 0, row.get(2)?))
        })?
        .collect()
}
