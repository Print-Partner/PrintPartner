//! Replays the Node `schemaMigrations` list from a pinned `schema.ts`.
//!
//! The schema 34 source is the copy pinned in
//! `pp-storage/data/historical/schema-34.ts`, released at commit
//! [`SCHEMA_34_COMMIT`]. Replaying it gives the tables, indexes and triggers a
//! Node server at schema 34 creates. It does not run Node's post-migration
//! repairs or starter seeds, and it writes no user rows.

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Commit whose `web/apps/server/src/db/schema.ts` released schema 34.
pub const SCHEMA_34_COMMIT: &str = "71e25e7731feddd0eaec05a0d1070dc13de790a7";
/// SHA-256 of [`SCHEMA_34_SOURCE`].
pub const SCHEMA_34_SHA256: &str =
    "f5755e7a045fcb8b638bbeb17321c57ff6737d0d6db6b317518c41a14780d8b1";
/// The pinned schema 34 `schema.ts` bytes.
pub const SCHEMA_34_SOURCE: &[u8] = include_bytes!("../../pp-storage/data/historical/schema-34.ts");
/// SHA-256 of [`SCHEMA_33_SOURCE`], released at commit
/// `ac0153b682d9565b8b2255b2b2747bee7f9b82d2`.
pub const SCHEMA_33_SHA256: &str =
    "3023074597eb6462866892aceaa1433dcb69b0cc68abfa8479cf920e2be0130f";
/// The pinned schema 33 `schema.ts` bytes.
pub const SCHEMA_33_SOURCE: &[u8] = include_bytes!("../../pp-storage/data/historical/schema-33.ts");

/// One `schemaMigrations` entry and the `//` comments directly above it.
#[derive(Clone, Debug)]
pub struct Statement {
    pub comments: Vec<String>,
    pub sql: String,
}

fn template_literal(source: &str, start: usize) -> Result<(String, usize), String> {
    let bytes = source.as_bytes();
    if bytes.get(start) != Some(&b'`') {
        return Err(format!("no template literal at byte {start}"));
    }
    let mut index = start + 1;
    let mut sql = String::new();
    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'\\' => {
                let escaped = bytes.get(index + 1).ok_or("dangling escape")?;
                sql.push(*escaped as char);
                index += 2;
            }
            b'`' => return Ok((sql, index + 1)),
            _ => {
                sql.push(byte as char);
                index += 1;
            }
        }
    }
    Err("unterminated historical template".into())
}

fn exported_template(source: &str, name: &str) -> Result<String, String> {
    let marker = format!("export const {name} = `");
    let start = source.find(&marker).ok_or(format!("missing {name}"))?;
    Ok(template_literal(source, start + marker.len() - 1)?.0)
}

/// Parses `export const schemaMigrations: string[] = [...]`. Entries are
/// template literals or names of exported template constants.
pub fn parse_schema_migrations(source: &str) -> Result<Vec<Statement>, String> {
    let marker = "export const schemaMigrations: string[] = [";
    let mut index = source.find(marker).ok_or("missing schemaMigrations")? + marker.len();
    let mut statements = Vec::new();
    let mut comments = Vec::new();
    while index < source.len() {
        index += source[index..]
            .chars()
            .take_while(|ch| ch.is_whitespace() || *ch == ',')
            .map(char::len_utf8)
            .sum::<usize>();
        let rest = &source[index..];
        if rest.starts_with("];") {
            return Ok(statements);
        }
        if rest.starts_with("//") {
            let end = rest.find('\n').ok_or("unterminated comment")?;
            comments.push(rest[..end].trim().to_owned());
            index += end + 1;
            continue;
        }
        if rest.starts_with('`') {
            let (sql, next) = template_literal(source, index)?;
            statements.push(Statement {
                comments: std::mem::take(&mut comments),
                sql,
            });
            index = next;
            continue;
        }
        let length = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .map(char::len_utf8)
            .sum::<usize>();
        if length == 0 {
            return Err(format!("unexpected schemaMigrations token at byte {index}"));
        }
        statements.push(Statement {
            comments: std::mem::take(&mut comments),
            sql: exported_template(source, &rest[..length])?,
        });
        index += length;
    }
    Err("unterminated schemaMigrations".into())
}

/// Runs each statement in order. Like Node's `runMigrations`, it skips an
/// `ALTER TABLE` that fails with "duplicate column name".
pub fn execute(connection: &Connection, statements: &[Statement]) -> Result<(), String> {
    for statement in statements {
        if let Err(error) = connection.execute_batch(&statement.sql) {
            let message = error.to_string();
            if statement.sql.trim_start().starts_with("ALTER TABLE ")
                && message.to_lowercase().contains("duplicate column name")
            {
                continue;
            }
            return Err(message);
        }
    }
    Ok(())
}

/// Sets `app_settings.schema_version` for the default tenant, as Node does.
pub fn stamp_schema_version(connection: &Connection, version: u64) -> rusqlite::Result<()> {
    connection.execute(
        "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'schema_version', ?1)
         ON CONFLICT(tenant_id, key) DO UPDATE SET value = excluded.value",
        [version.to_string()],
    )?;
    Ok(())
}

/// Opens the database at `path` and sets its `schema_version` row, for tests
/// of versions no fixture writes.
pub fn restamp_schema_version(path: &Path, version: u64) -> std::io::Result<()> {
    let connection = Connection::open(path).map_err(std::io::Error::other)?;
    stamp_schema_version(&connection, version).map_err(std::io::Error::other)
}

/// Inserts one `source_revisions` row that breaks its CHECK constraint, with
/// CHECK and foreign-key enforcement off, so `PRAGMA integrity_check` on
/// the file reports it.
pub fn break_check_constraint(path: &Path) -> std::io::Result<()> {
    let connection = Connection::open(path).map_err(std::io::Error::other)?;
    connection
        .execute_batch(
            "PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=1;
             INSERT INTO source_revisions
               (project_id, upstream_revision_key, manifest_digest, snapshot_locator, synced_at, completeness)
               VALUES (1, 'k', 'd', 'l', '2026-11-10T00:00:00Z', 'partial');",
        )
        .map_err(std::io::Error::other)
}

/// The bytes of `print-partner.db` and its `-wal`, `-shm` and `-journal`
/// files in `directory`, `None` for each one that is absent.
pub fn database_files(directory: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    ["", "-wal", "-shm", "-journal"]
        .into_iter()
        .map(|suffix| {
            let name = format!("print-partner.db{suffix}");
            let bytes = std::fs::read(directory.join(&name)).ok();
            (name, bytes)
        })
        .collect()
}

/// The parsed migration list of a pinned `schema.ts`, after checking its digest.
fn pinned_statements(source: &[u8], sha256: &str) -> Result<Vec<Statement>, String> {
    let digest = hex::encode(Sha256::digest(source));
    if digest != sha256 {
        return Err(format!("pinned schema source changed: sha256 {digest}"));
    }
    parse_schema_migrations(std::str::from_utf8(source).map_err(|e| e.to_string())?)
}

/// The parsed schema 34 migration list, after checking the pinned digest.
pub fn schema_34_statements() -> Result<Vec<Statement>, String> {
    pinned_statements(SCHEMA_34_SOURCE, SCHEMA_34_SHA256)
}

/// Creates a new schema 34 database at `path` with Node's connection settings
/// (WAL, foreign keys on). Refuses to replace an existing file.
pub fn write_schema_34_database(path: &Path) -> std::io::Result<()> {
    write_database(path, schema_34_statements(), 34)
}

/// Creates a new schema 33 database at `path`, as Print Partner v3.3.0's
/// Node server does. Refuses to replace an existing file.
pub fn write_schema_33_database(path: &Path) -> std::io::Result<()> {
    write_database(
        path,
        pinned_statements(SCHEMA_33_SOURCE, SCHEMA_33_SHA256),
        33,
    )
}

fn write_database(
    path: &Path,
    statements: Result<Vec<Statement>, String>,
    version: u64,
) -> std::io::Result<()> {
    if path.symlink_metadata().is_ok() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display()),
        ));
    }
    let connection = Connection::open(path).map_err(std::io::Error::other)?;
    connection
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .map_err(std::io::Error::other)?;
    execute(&connection, &statements.map_err(std::io::Error::other)?)
        .map_err(std::io::Error::other)?;
    stamp_schema_version(&connection, version).map_err(std::io::Error::other)?;
    connection
        .close()
        .map_err(|(_, error)| std::io::Error::other(error))
}
