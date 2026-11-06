//! Pre-upgrade backup retention (T-8-archive). After an upgrade commits, keep
//! every copy it wrote, including its rollback copy, plus the three newest
//! verified `pre-schema*.db` copies left by earlier upgrades. The count is
//! per file, not per upgrade.
//!
//! A `pre-schema{N}-to-{M}.db` copy is never pruned. It is written only when
//! a database older than schema 37 upgrades, one per source and target
//! version. After an upgrade interrupted between steps it can be the only
//! copy the previous release opens, even though a later attempt pinned a
//! newer rollback.
//!
//! A step copy written by an attempt that was interrupted before the upgrade
//! committed is an ordinary candidate. The run marker ([`RUN_MARKER`]) keeps
//! the one copy that matters: before its first step, an upgrade names its
//! rollback copy there, and every restart of that upgrade keeps the named copy
//! until the upgrade commits and the marker is removed. Releases before the
//! marker copied at most steps 38 to 41, so a restart from 41 without a marker
//! may hold four copies with the untouched input the oldest; `schema.rs`
//! skips retention there.
//!
//! Only this crate's own files are candidates: Node's pre-update and manual
//! `.tar.gz` archives, temporary `.backup-*.db` files and SQLite sidecars are
//! never touched. A copy that fails verification is kept and reported, never
//! deleted. A path that is not UTF-8 cannot be verified, so nothing is
//! pruned there.

use rusqlite::{Connection, OpenFlags};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub(crate) const KEEP_VERIFIED: usize = 3;

/// Names the rollback copy of the upgrade in progress, one file name per line
/// of text, in the backups directory.
#[cfg(feature = "desktop_rebuild")]
pub(crate) const RUN_MARKER: &str = "pre-schema-run";

/// Starts or continues an upgrade whose rollback copy is `rollback`. Returns
/// the copy an earlier, interrupted attempt named, which retention must keep;
/// otherwise names `rollback` in a new marker, durably, and returns `None`.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn begin_run(rollback: &Path) -> anyhow::Result<Option<PathBuf>> {
    use anyhow::Context;
    let directory = rollback
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Backup parent missing"))?;
    let marker = directory.join(RUN_MARKER);
    match std::fs::read_to_string(&marker) {
        Ok(name) => {
            let name = name.trim();
            anyhow::ensure!(
                parse(name).is_some(),
                "Upgrade run marker {} names no pre-upgrade copy",
                marker.display()
            );
            return Ok(Some(directory.join(name)));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(error).context("backup_retention: read the upgrade run marker");
        }
        Err(_) => {}
    }
    let name = rollback
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("Rollback copy name is not UTF-8"))?;
    let temp = directory.join(format!(
        ".{RUN_MARKER}-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    std::fs::write(&temp, format!("{name}\n")).context("backup_retention: write the run marker")?;
    pp_os::sync_file(&temp).context("backup_retention: sync the run marker")?;
    pp_os::publish_rename(&temp, &marker).context("backup_retention: publish the run marker")?;
    pp_os::sync_directory(directory).context("backup_retention: sync the backups directory")?;
    Ok(None)
}

/// Ends the upgrade run in `directory`: its marker goes once the upgrade has
/// committed, or when an open finds nothing to upgrade.
#[cfg(feature = "desktop_rebuild")]
pub(crate) fn end_run(directory: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    match std::fs::remove_file(directory.join(RUN_MARKER)) {
        Ok(()) => pp_os::sync_directory(directory).context("backup_retention: sync after the run"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("backup_retention: remove the upgrade run marker"),
    }
}

#[derive(Debug, Default)]
pub(crate) struct Pruned {
    pub removed: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

enum Name {
    /// `pre-schema{N}.db`, the input to step N.
    Step(u64),
    /// `pre-schema{N}-to-{M}.db`, a database from before schema 37.
    Legacy,
}

/// Parses exactly `pre-schema{N}.db` or `pre-schema{N}-to-{M}.db`.
fn parse(name: &str) -> Option<Name> {
    let versions = name.strip_prefix("pre-schema")?.strip_suffix(".db")?;
    let number = |part: &str| {
        (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse::<u64>().ok())
            .flatten()
    };
    match versions.split_once("-to-") {
        Some((from, to)) => {
            (number(from).is_some() && number(to).is_some()).then_some(Name::Legacy)
        }
        None => number(versions).map(Name::Step),
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Opens the copy as an immutable read-only URI, so verification creates no
/// `-wal` or `-shm` file, and requires `integrity_check` to return `ok`. An
/// immutable open ignores a `-wal` or `-journal`, so a copy that has either
/// is not verified.
fn verified(path: &Path) -> bool {
    if ["-wal", "-journal"]
        .iter()
        .any(|suffix| sidecar(path, suffix).symlink_metadata().is_ok())
    {
        return false;
    }
    let Some(uri) = crate::sqlite_uri::immutable_uri(path) else {
        return false;
    };
    Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .and_then(|check| check.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0)))
    .is_ok_and(|result| result == "ok")
}

/// Removes the sidecars first, so a failure never leaves one without its
/// database.
fn remove_with_sidecars(path: &Path) -> std::io::Result<()> {
    for suffix in ["-wal", "-journal", "-shm"] {
        match std::fs::remove_file(sidecar(path, suffix)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    std::fs::remove_file(path)
}

/// Prunes the backups directory that holds `rollback`, the copy of the
/// database as this run opened it (`SchemaReady.backup`). `written` lists
/// the step copies this run wrote; they are kept too. Errors become
/// warnings; they never fail startup.
pub(crate) fn prune(rollback: &Path, written: &[PathBuf]) -> Pruned {
    let mut pruned = Pruned::default();
    let Some(directory) = rollback.parent() else {
        return pruned;
    };
    // Every path names a file in `directory`, so file names identify them.
    let protected: Vec<OsString> = std::iter::once(rollback)
        .chain(written.iter().map(PathBuf::as_path))
        .filter_map(|path| path.file_name().map(ToOwned::to_owned))
        .collect();
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            pruned
                .warnings
                .push(format!("Read {}: {error}", directory.display()));
            return pruned;
        }
    };
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                pruned
                    .warnings
                    .push(format!("Read {}: {error}", directory.display()));
                continue;
            }
        };
        let file_name = entry.file_name();
        let Some(Name::Step(step)) = file_name.to_str().and_then(parse) else {
            continue;
        };
        if protected.contains(&file_name) {
            continue;
        }
        let path = entry.path();
        match path.symlink_metadata() {
            Ok(metadata) if metadata.is_file() => {
                let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                candidates.push((modified, step, path));
            }
            Ok(_) => {}
            Err(error) => pruned
                .warnings
                .push(format!("Inspect {}: {error}", path.display())),
        }
    }
    // Newest first; equal times keep the higher step, so the result is stable.
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then(right.1.cmp(&left.1)));
    let mut kept = 0;
    for (_, _, path) in candidates {
        if !verified(&path) {
            pruned.warnings.push(format!(
                "Kept unverified backup {}; it does not count toward the {KEEP_VERIFIED} retained",
                path.display()
            ));
        } else if kept < KEEP_VERIFIED {
            kept += 1;
        } else {
            match remove_with_sidecars(&path) {
                Ok(()) => pruned.removed.push(path),
                Err(error) => pruned
                    .warnings
                    .push(format!("Remove {}: {error}", path.display())),
            }
        }
    }
    pruned
}

#[cfg(test)]
mod tests {
    use super::{Name, parse};

    #[test]
    fn ticket_t_8_archive_retention_matches_only_pre_schema_copies() {
        assert!(matches!(parse("pre-schema38.db"), Some(Name::Step(38))));
        assert!(matches!(parse("pre-schema100.db"), Some(Name::Step(100))));
        assert!(matches!(parse("pre-schema34-to-41.db"), Some(Name::Legacy)));
        for name in [
            "pre-schema38.db-wal",
            "pre-schema38.db-shm",
            "pre-schema.db",
            "pre-schema38-to-.db",
            "pre-schemaX.db",
            "pre-schema38.db.bak",
            ".backup-0011aa.db",
            "print-partner-pre-update-to-3.9.0-schema-34-to-41.tar.gz",
            "print-partner-backup-2026-10-29T12-00-00-000Z.tar.gz",
            "PRE-SCHEMA38.db",
        ] {
            assert!(parse(name).is_none(), "{name}");
        }
    }
}
