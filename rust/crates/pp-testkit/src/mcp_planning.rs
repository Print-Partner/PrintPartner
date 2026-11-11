//! Controlled edits and durable-state observations for disposable MCP fixtures.
use rusqlite::{Connection, params};
use serde_json::Value;
use std::path::Path;
pub fn edit(path: &Path, sql: &str) {
    Connection::open(path).unwrap().execute_batch(sql).unwrap();
}
pub fn setting(path: &Path, key: &str) -> Value {
    let raw: String = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key=?1",
            [key],
            |r| r.get(0),
        )
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}
pub fn save(path: &Path, key: &str, value: &Value) {
    Connection::open(path)
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO app_settings(tenant_id,key,value) VALUES('default',?1,?2)",
            params![key, value.to_string()],
        )
        .unwrap();
}
pub fn state(path: &Path) -> Vec<Vec<Vec<String>>> {
    let db = Connection::open(path).unwrap();
    [
        "SELECT * FROM profile_layers ORDER BY id",
        "SELECT * FROM app_settings ORDER BY tenant_id,key",
        "SELECT * FROM plan_decisions ORDER BY id",
    ]
    .map(|sql| {
        let mut query = db.prepare(sql).unwrap();
        let columns = query.column_count();
        query
            .query_map([], |row| {
                (0..columns)
                    .map(|i| Ok(format!("{:?}", row.get_ref(i)?)))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
    .to_vec()
}
