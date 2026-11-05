use rusqlite::{Connection, types::ValueRef};
use serde_json::{Map, Value, json};

/// Runs each named query and returns `{ name: [row, ...] }`, each row an
/// object keyed by column with SQLite NULL, integer, real and text values.
pub fn query_rows(connection: &Connection, queries: &Map<String, Value>) -> Value {
    let mut rows = Map::new();
    for (name, sql) in queries {
        let mut statement = connection.prepare(sql.as_str().unwrap()).unwrap();
        let columns: Vec<String> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let table: Vec<Value> = statement
            .query_map([], |row| {
                let mut object = Map::new();
                for (i, column) in columns.iter().enumerate() {
                    let value = match row.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Real(n) => json!(n),
                        value => json!(value.as_str().unwrap()),
                    };
                    object.insert(column.clone(), value);
                }
                Ok(Value::Object(object))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        rows.insert(name.clone(), table.into());
    }
    Value::Object(rows)
}
