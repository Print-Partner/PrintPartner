//! A marked installation with Builds, a draft and History, and a dump of a
//! table's rows, for backup and restore tests in `pp-storage` and `pp-core`.

use crate::{Installation, MarkerState, Spec};
use rusqlite::Connection;
use std::{fs, io};

/// Builds 7 and 29, Plan snapshot 3 (History) and open draft 41 on Build 29.
const SEED: &str = "INSERT INTO build_profiles(id, name) VALUES (7, 'Seven'), (29, 'Twenty-nine');
INSERT INTO plan_snapshots(id, profile_id, name, created_at, payload_json)
  VALUES (3, 29, 'before', '2026-10-01T00:00:00.000Z', '{\"layers\":2}');
INSERT INTO plan_drafts(id, profile_id, base_plan_version, state, digest_format,
  snapshot_digest, created_by, idempotency_key, created_at)
  VALUES (41, 29, 0, 'open', 'v1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
  'user:1', 'seed', '2026-10-01T00:00:00.000Z');";

impl Installation {
    /// A marked installation with a Node schema 34 database holding Builds 7
    /// and 29, a Plan snapshot and an open draft, plus the Source file
    /// `repos/source-a/part.stl` and the export `exports/plate.3mf`.
    pub fn with_builds_drafts_and_history() -> io::Result<Self> {
        let installation = Self::create(Spec {
            node_database: true,
            marker: MarkerState::Valid,
            ..Spec::default()
        })?;
        Connection::open(installation.database_path())
            .and_then(|db| db.execute_batch(SEED))
            .map_err(io::Error::other)?;
        let root = installation.path();
        fs::create_dir_all(root.join("repos/source-a"))?;
        fs::write(
            root.join("repos/source-a/part.stl"),
            b"solid a\nendsolid a\n",
        )?;
        fs::create_dir_all(root.join("exports"))?;
        fs::write(root.join("exports/plate.3mf"), b"plate")?;
        Ok(installation)
    }
}

/// Every row of `table`, each formatted with `Debug`, sorted, one per line.
pub fn table_rows(db: &Connection, table: &str) -> rusqlite::Result<String> {
    let mut select = db.prepare(&format!("SELECT * FROM \"{table}\""))?;
    let width = select.column_count();
    let mut found = Vec::new();
    let mut rows = select.query([])?;
    while let Some(row) = rows.next()? {
        let values = (0..width)
            .map(|i| row.get_ref(i))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        found.push(format!("{values:?}"));
    }
    found.sort();
    Ok(found.join("\n"))
}
