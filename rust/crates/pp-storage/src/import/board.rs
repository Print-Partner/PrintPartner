use crate::build_graph::board::{IMPORTED_POSTS, ImportedPost};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;

pub(super) fn convert(connection: &Connection) -> rusqlite::Result<usize> {
    let owner: Option<String> = connection
        .query_row("SELECT id FROM users", [], |row| row.get(0))
        .optional()?;
    let Some(owner) = owner else { return Ok(0) };
    let posts = visible_posts(connection, &owner)?;
    if posts.is_empty() {
        return Ok(0);
    }
    let value = serde_json::to_string(&posts)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    connection.execute(
        "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', ?1, ?2)",
        params![IMPORTED_POSTS, value],
    )
}

fn visible_posts(
    connection: &Connection,
    owner: &str,
) -> rusqlite::Result<BTreeMap<String, ImportedPost>> {
    let mut query = connection.prepare(
        "SELECT id, caption, title, created_at FROM board_posts
         WHERE author_user_id = ?1 AND hidden_at IS NULL",
    )?;
    query
        .query_map([owner], |row| {
            Ok((
                row.get(0)?,
                ImportedPost {
                    caption: row.get(1)?,
                    title: row.get(2)?,
                    created_at: row.get(3)?,
                },
            ))
        })?
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_import_projects_only_the_selected_owners_visible_posts() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE board_posts (
                id TEXT, caption TEXT, title TEXT, created_at TEXT, author_user_id TEXT, hidden_at TEXT
             );
             INSERT INTO board_posts VALUES
                ('own', 'Own caption', 'Own title', '2026-11-23', 'owner', NULL),
                ('hidden', 'Hidden', 'Hidden', '2026-11-24', 'owner', '2026-11-25'),
                ('foreign', 'Foreign', 'Foreign', '2026-11-25', 'other', NULL);",
        )
        .unwrap();
        let posts = visible_posts(&db, "owner").unwrap();
        assert_eq!(posts.keys().collect::<Vec<_>>(), ["own"]);
        assert!(visible_posts(&db, "missing").unwrap().is_empty());
    }
}
