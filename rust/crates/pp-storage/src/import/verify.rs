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

/// The stored path and current revision of one Source.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct SourceRow {
    pub id: i64,
    pub stored: Option<String>,
    pub locator: Option<String>,
}

/// What the import does with one Source's stored path.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Decision {
    /// No stored path and no current revision: nothing to rewrite.
    Keep,
    /// Its files are in the copy at this `/`-separated path below `repos`.
    Rewrite(String),
    /// Its current revision names a snapshot the copy does not hold.
    MissingRevision,
    /// The stored path is not below the installation's own `repos`. It is
    /// kept byte for byte.
    Outside,
    /// The stored path is below the installation's `repos`, but the copy
    /// holds no such folder. It is kept byte for byte.
    Missing,
}

/// How the Sources' paths came out, by count. A path is never reported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourcePaths {
    /// Paths that now lead into the destination's `repos`.
    pub rebased: u64,
    /// Paths outside the installation, kept as stored.
    pub outside: u64,
    /// Paths inside the installation whose folder the copy lacks, kept as
    /// stored.
    pub missing: u64,
}

impl SourcePaths {
    /// Counts what `decisions` would do.
    pub(super) fn of(decisions: &[(i64, Decision)]) -> Self {
        let count = |wanted: fn(&Decision) -> bool| {
            decisions
                .iter()
                .filter(|(_, decision)| wanted(decision))
                .count() as u64
        };
        Self {
            rebased: count(|decision| matches!(decision, Decision::Rewrite(_))),
            outside: count(|decision| matches!(decision, Decision::Outside)),
            missing: count(|decision| matches!(decision, Decision::Missing)),
        }
    }
}

/// Every Source's stored path and the snapshot of its current revision.
pub(super) fn source_rows(connection: &Connection) -> rusqlite::Result<Vec<SourceRow>> {
    let mut statement = connection.prepare(
        "SELECT p.id, p.local_path, r.snapshot_locator FROM projects p
         LEFT JOIN source_revisions r ON r.id = p.current_source_revision_id ORDER BY p.id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(SourceRow {
            id: row.get(0)?,
            stored: row.get(1)?,
            locator: row.get(2)?,
        })
    })?;
    rows.collect()
}

/// `path` split at both separators: Node on Windows stores backslashes. The
/// split comes before every check, so `..\..` cannot pass as one name.
fn segments(path: &str) -> Vec<&str> {
    path.split(['/', '\\']).collect()
}

/// `parts` as a path below `repos`, when every part is a plain name: not
/// empty, not `.` or `..`, and not a drive letter.
fn below(parts: &[&str]) -> Option<String> {
    let plain = |part: &&str| {
        let drive = part.len() == 2 && part.ends_with(':');
        !part.is_empty() && *part != "." && *part != ".." && !drive
    };
    (!parts.is_empty() && parts.iter().all(plain)).then(|| parts.join("/"))
}

/// Decides, for every Source, what its stored path becomes. `root` is the
/// Node data folder the import reads, as given and not resolved; an archive
/// has none. `held` says whether the copy holds a folder at a path below
/// `repos`.
///
/// A Source with a current revision is at `repos/<snapshot_locator>`. One
/// without is rewritten only when its stored path lies under `root`'s own
/// `repos`, compared part by part, and the copy holds the rest of the path.
pub(super) fn decide(
    rows: &[SourceRow],
    root: Option<&Path>,
    held: impl Fn(&str) -> bool,
) -> Result<Vec<(i64, Decision)>, ImportError> {
    let root = root.and_then(|root| std::path::absolute(root).ok());
    let root = root.as_ref().map(|root| root.to_string_lossy());
    let own: Option<Vec<&str>> = root.as_ref().map(|root| {
        let mut parts = segments(root.trim_end_matches(['/', '\\']));
        parts.push("repos");
        parts
    });
    let decided = |row: &SourceRow| -> Result<Decision, ImportError> {
        if let Some(locator) = &row.locator {
            let inside = below(&segments(locator)).ok_or(ImportError::SourcePath(row.id))?;
            return Ok(match held(&inside) {
                true => Decision::Rewrite(inside),
                false => Decision::MissingRevision,
            });
        }
        let Some(stored) = row.stored.as_deref().filter(|path| !path.is_empty()) else {
            return Ok(Decision::Keep);
        };
        let parts = segments(stored);
        let rest = (own.as_ref()).and_then(|own| parts.strip_prefix(own.as_slice()));
        Ok(match rest.and_then(below) {
            Some(inside) if held(&inside) => Decision::Rewrite(inside),
            Some(_) => Decision::Missing,
            None => Decision::Outside,
        })
    };
    rows.iter().map(|row| Ok((row.id, decided(row)?))).collect()
}

/// Writes what [`decide`] decided. A Source whose current revision the copy
/// lacks stops the import. Two guards follow, each a refusal, because each
/// means the mapping itself is wrong: no two Sources may land on one path,
/// and every rewritten path must lie under the staged `repos` with links
/// resolved. `repos` is where the destination will hold `stage_repos`.
pub(super) fn rebase_sources(
    connection: &Connection,
    root: Option<&Path>,
    stage_repos: &Path,
    repos: &Path,
) -> Result<SourcePaths, ImportError> {
    let rows = source_rows(connection)?;
    let decisions = decide(&rows, root, |inside| stage_repos.join(inside).is_dir())?;
    let staged = stage_repos.canonicalize().ok();
    let mut taken = BTreeMap::new();
    for (source, decision) in &decisions {
        let inside = match decision {
            Decision::Rewrite(inside) => inside,
            Decision::MissingRevision => return Err(ImportError::MissingRevision(*source)),
            Decision::Keep | Decision::Outside | Decision::Missing => continue,
        };
        if let Some(first) = taken.insert(inside.as_str(), *source) {
            return Err(ImportError::SourcesShare(first, *source));
        }
        let resolved = stage_repos.join(inside).canonicalize().ok();
        let contained =
            matches!((&staged, &resolved), (Some(staged), Some(path)) if path.starts_with(staged));
        if !contained {
            return Err(ImportError::SourceEscapes(*source));
        }
        let path = pp_os::display_path(&repos.join(inside));
        connection.execute(
            "UPDATE projects SET local_path = ?1 WHERE id = ?2",
            (path.to_string_lossy(), source),
        )?;
    }
    Ok(SourcePaths::of(&decisions))
}

/// Checks every Source's path in the copy against `before`, the paths the
/// source stored, without using [`decide`]: a path is either what the source
/// stored, byte for byte, or `repos` joined with the snapshot of the current
/// revision, or with what the stored path holds below `root`'s own `repos`.
/// A Source with a current revision must have been rewritten.
pub(super) fn check_sources(
    connection: &Connection,
    before: &BTreeMap<i64, Option<String>>,
    root: Option<&Path>,
    repos: &Path,
) -> Result<(), ImportError> {
    let own = root.and_then(|root| std::path::absolute(root).ok());
    let own = own.map(|root| root.join("repos"));
    for row in source_rows(connection)? {
        let wrong = || ImportError::SourceRebase(row.id);
        let stored = before.get(&row.id).ok_or_else(wrong)?;
        // Only a Source without a current revision may keep what it stored.
        if row.locator.is_none() && row.stored == *stored {
            continue;
        }
        let under = |path: &str| Path::new(path).strip_prefix(repos).ok().map(Path::to_owned);
        let found = row.stored.as_deref().and_then(under).ok_or_else(wrong)?;
        let found = found.as_path();
        let from_revision = row.locator.as_deref().map(Path::new) == Some(found);
        let unix = stored.as_deref().map(|path| path.replace('\\', "/"));
        let from_stored = row.locator.is_none()
            && (own.as_deref())
                .zip(unix.as_deref())
                .is_some_and(|(own, path)| {
                    !path.contains("//") && Path::new(path).strip_prefix(own) == Ok(found)
                });
        let plain = found.components().next().is_some()
            && found
                .components()
                .all(|part| matches!(part, Component::Normal(_)));
        if !plain || !(from_revision || from_stored) {
            return Err(wrong());
        }
    }
    Ok(())
}
