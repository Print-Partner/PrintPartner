//! The import's checks on the staged copy: rows that must arrive exactly as
//! the source stored them, and Source paths that must lead into the new
//! folder.

use super::{ImportError, quoted, strings};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

/// Accepted Plans, Required units, Plates, Checkoff and print outcomes. The
/// import copies their rows; no upgrade step or converter may change one.
const VERIFIED: [&str; 15] = [
    "plan_revisions",
    "plan_revision_parts",
    "plan_revision_inputs",
    "plan_revision_input_sets",
    "plan_accepted_input_sets",
    "required_units",
    "plan_revision_required_units",
    "plan_revision_required_unit_sets",
    "accepted_plates",
    "accepted_plate_units",
    "accepted_plate_revisions",
    "accepted_plate_heads",
    "print_progress",
    "print_jobs",
    "print_job_parts",
];

/// The columns of one verified table and the SHA-256 of its rows.
pub(super) type Digests = BTreeMap<&'static str, (Vec<String>, String)>;

/// Digests every verified table that exists. With `like`, it digests the
/// tables and columns `like` holds, so an upgrade that adds a column or a
/// table does not change the result.
pub(super) fn digests(
    connection: &Connection,
    like: Option<&Digests>,
) -> rusqlite::Result<Digests> {
    let mut found = Digests::new();
    for table in VERIFIED {
        let columns = match like {
            Some(like) => like.get(table).map(|(columns, _)| columns.clone()),
            None => Some(strings(
                connection,
                "SELECT name FROM pragma_table_info(?1)",
                Some(table),
            )?),
        };
        let Some(columns) = columns.filter(|columns| !columns.is_empty()) else {
            continue;
        };
        // `quote` writes each value with its type, so 1 and '1' differ.
        let names: Vec<String> = columns.iter().map(|column| quoted(column)).collect();
        let row: Vec<String> = names.iter().map(|name| format!("quote({name})")).collect();
        let sql = format!(
            "SELECT {} FROM {} ORDER BY {}",
            row.join(" || ',' || "),
            quoted(table),
            names.join(", ")
        );
        let mut digest = Sha256::new();
        let mut statement = connection.prepare(&sql)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            digest.update(row.get_ref(0)?.as_bytes()?);
            digest.update(b"\n");
        }
        found.insert(table, (columns, hex::encode(digest.finalize())));
    }
    Ok(found)
}

/// The verified tables whose rows differ between `before` and `after`.
pub(super) fn changed(before: &Digests, after: &Digests) -> Vec<String> {
    let differs = |(table, digest): (&&'static str, &(Vec<String>, String))| {
        (after.get(table) != Some(digest)).then(|| (*table).to_owned())
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
