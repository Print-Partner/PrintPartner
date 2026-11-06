use rusqlite::Connection;

/// `part_id:unit_index:completed:assembled` for every progress row.
pub fn progress(sql: &Connection) -> Vec<String> {
    let mut statement = sql
        .prepare("SELECT part_id,unit_index,completed,assembled FROM print_progress ORDER BY part_id,unit_index")
        .unwrap();
    statement
        .query_map([], |r| {
            Ok(format!(
                "{}:{}:{}:{}",
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
