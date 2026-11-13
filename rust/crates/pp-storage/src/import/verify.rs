//! The import's checks on the staged copy: rows that must arrive exactly as
//! the source stored them, and Source paths that must lead into the new
//! folder.

use super::{
    ImportError,
    inventory::{self, Shape},
    quoted, strings,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

/// What the import, or the upgrade to Rust's schema, does to stored rows on
/// purpose, beside the shapes the inventory marks converted. Every other row
/// of the source must arrive as the source stored it.
enum Change {
    /// The import empties the table: sign-in state is not carried.
    Emptied(&'static str),
    /// The upgrade writes this setting.
    Stamped(&'static str),
    /// The upgrade adds the starter rows the table lacks. Its stored rows stay.
    Seeded(&'static str),
}

const CHANGES: [Change; 6] = [
    Change::Emptied("sessions"),
    Change::Emptied("password_reset_tokens"),
    Change::Stamped("schema_version"),
    Change::Seeded("printer_profiles"),
    Change::Seeded("process_profiles"),
    Change::Seeded("filament_profiles"),
];

const SETTINGS: &str = "app_settings";

/// The compared columns of one source table and the SHA-256 of each of its
/// rows, sorted.
pub(super) type Digests = BTreeMap<String, (Vec<String>, Vec<[u8; 32]>)>;

fn converted() -> impl Iterator<Item = &'static Shape> {
    let rows = inventory::ROWS.iter();
    rows.filter(|row| row.status == inventory::Status::Converted)
        .flat_map(|row| row.shapes)
}

/// The condition that leaves out the settings a converter or the upgrade
/// rewrites.
fn compared_settings() -> String {
    let literal = |text: &str| format!("'{}'", text.replace('\'', "''"));
    let stamped = CHANGES.iter().filter_map(|change| match change {
        Change::Stamped(key) => Some(format!("key <> {}", literal(key))),
        _ => None,
    });
    let rewritten = converted().filter_map(|shape| match shape {
        Shape::Setting(key) => Some(format!("key <> {}", literal(key))),
        Shape::SettingPrefix(prefix) => Some(format!(
            "substr(key, 1, {}) <> {}",
            prefix.chars().count(),
            literal(prefix)
        )),
        _ => None,
    });
    stamped.chain(rewritten).collect::<Vec<_>>().join(" AND ")
}

/// Digests every row of every table in the database, less what [`CHANGES`]
/// and the converted inventory rows name. With `like`, it digests the tables
/// and columns `like` holds, so a table or column the upgrade adds does not
/// change the result. A table that lost one of those columns is left out,
/// which [`changed`] reports.
pub(super) fn digests(
    connection: &Connection,
    like: Option<&Digests>,
) -> rusqlite::Result<Digests> {
    let tables = match like {
        Some(like) => like.keys().cloned().collect(),
        None => strings(
            connection,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            None,
        )?,
    };
    let rewritten = |table: &str| {
        let emptied = |change: &Change| matches!(change, Change::Emptied(name) if *name == table);
        let whole = |shape: &Shape| matches!(shape, Shape::Table(name) if *name == table);
        CHANGES.iter().any(emptied) || converted().any(whole)
    };
    let mut found = Digests::new();
    for table in tables {
        if rewritten(&table) {
            continue;
        }
        let stored = strings(
            connection,
            "SELECT name FROM pragma_table_info(?1)",
            Some(&table),
        )?;
        let columns = match like.and_then(|like| like.get(&table)) {
            Some((columns, _)) if columns.iter().all(|column| stored.contains(column)) => {
                columns.clone()
            }
            Some(_) => continue,
            None => (stored.into_iter())
                .filter(|column| !converted().any(|shape| shape.names_column(&table, column)))
                .collect(),
        };
        if columns.is_empty() {
            continue;
        }
        // `quote` writes each value with its type, so 1 and '1' differ.
        let row: Vec<String> = (columns.iter())
            .map(|column| format!("quote({})", quoted(column)))
            .collect();
        let mut sql = format!("SELECT {} FROM {}", row.join(" || ',' || "), quoted(&table));
        if table == SETTINGS {
            sql = format!("{sql} WHERE {}", compared_settings());
        }
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query([])?;
        let mut hashes = Vec::new();
        while let Some(row) = rows.next()? {
            hashes.push(<[u8; 32]>::from(Sha256::digest(
                row.get_ref(0)?.as_bytes()?,
            )));
        }
        hashes.sort_unstable();
        found.insert(table, (columns, hashes));
    }
    Ok(found)
}

/// The source tables that lost or changed a row between `before` and
/// `after`, or gained one the upgrade does not seed.
pub(super) fn changed(before: &Digests, after: &Digests) -> Vec<String> {
    let seeded = |table: &str| {
        let named = |change: &Change| matches!(change, Change::Seeded(name) if *name == table);
        CHANGES.iter().any(named)
    };
    let arrived = |table: &str, stored: &[[u8; 32]]| {
        let Some((_, found)) = after.get(table) else {
            return false;
        };
        if !seeded(table) {
            return found == stored;
        }
        // Both are sorted, so one pass finds each stored row among the found.
        let mut found = found.iter();
        stored.iter().all(|row| found.any(|other| other == row))
    };
    let differs = |(table, (_, stored)): (&String, &(Vec<String>, Vec<[u8; 32]>))| {
        (!arrived(table, stored)).then(|| table.clone())
    };
    before.iter().filter_map(differs).collect()
}

/// Points every Source at its files under `repos`, which is where the
/// destination will hold `stage_repos`. Node stores `projects.local_path` as
/// an absolute path into its own data folder. A Source with a current
/// revision gets `repos/<snapshot_locator>`, which must exist. One without
/// keeps the part of its path below Node's `repos`. Returns how many paths
/// were written.
pub(super) fn rebase_sources(
    connection: &Connection,
    stage_repos: &Path,
    repos: &Path,
) -> Result<u64, ImportError> {
    let sources: Vec<(i64, Option<String>, Option<String>)> = {
        let mut statement = connection.prepare(
            "SELECT p.id, p.local_path, r.snapshot_locator FROM projects p
             LEFT JOIN source_revisions r ON r.id = p.current_source_revision_id ORDER BY p.id",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut rebased = 0;
    for (source, stored, locator) in sources {
        let below = match (locator, stored.filter(|path| !path.is_empty())) {
            (Some(locator), _) => {
                let inside = relative(&locator).ok_or(ImportError::SourcePath(source))?;
                if !stage_repos.join(inside).is_dir() {
                    return Err(ImportError::MissingRevision(source));
                }
                locator
            }
            (None, None) => continue,
            // Node on Windows stores backslashes; both separators are read.
            (None, Some(stored)) => stored
                .rsplit_once("/repos/")
                .or_else(|| stored.rsplit_once("\\repos\\"))
                .and_then(|(_, below)| relative(below).map(|_| below.replace('\\', "/")))
                .ok_or(ImportError::SourcePath(source))?,
        };
        let path = pp_os::display_path(&repos.join(below));
        connection.execute(
            "UPDATE projects SET local_path = ?1 WHERE id = ?2",
            (path.to_string_lossy(), source),
        )?;
        rebased += 1;
    }
    Ok(rebased)
}

/// `path` when it is relative and stays below its start.
fn relative(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    let normal = |component| matches!(component, Component::Normal(_));
    (path.components().next().is_some() && path.components().all(normal)).then_some(path)
}
