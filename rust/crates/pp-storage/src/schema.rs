use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, types::Value};
use serde::Deserialize;
use std::{
    fs::File,
    io::Read,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(feature = "desktop_rebuild")]
pub(crate) const CURRENT_VERSION: u64 = 41;
#[cfg(not(feature = "desktop_rebuild"))]
pub(crate) const CURRENT_VERSION: u64 = 35;

pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct SchemaData {
    migrations: Vec<String>,
    repair: Vec<String>,
    stranded: String,
    extras: Vec<String>,
    seeds: Vec<SeedGroup>,
}
#[derive(Deserialize)]
struct SeedGroup {
    table: String,
    columns: Vec<String>,
    rows: Vec<Vec<serde_json::Value>>,
}
#[derive(Debug, serde::Serialize)]
pub struct SchemaReady {
    pub version: u64,
    pub previous_version: u64,
    pub backup: Option<PathBuf>,
    /// Older verified pre-upgrade copies removed after this upgrade committed.
    pub pruned_backups: Vec<PathBuf>,
    /// Retention problems, such as an unverified copy that was kept. They
    /// never fail startup.
    pub backup_warnings: Vec<String>,
}

struct PreflightCopy(PathBuf);
impl Drop for PreflightCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
thread_local! {
    static PREFLIGHT_COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static PREFLIGHT_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn preflight(
    #[cfg(test)] path: &Path,
    #[cfg(test)] owned: bool,
    open_file: impl Fn(&str) -> std::io::Result<File>,
    validate_directory: impl Fn() -> Result<()>,
) -> Result<u64> {
    #[cfg(test)]
    PREFLIGHT_CALLS.update(|calls| calls + 1);
    validate_directory()?;
    #[cfg(test)]
    if owned {
        capture_tests::arm_owned_swap();
    }
    #[cfg(test)]
    capture_tests::transient_swap(path, 0, false)?;
    let metadata = open_file("print-partner.db").and_then(|file| {
        file.metadata().map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("schema::preflight: fstat print-partner.db: {error}"),
            )
        })
    });
    #[cfg(test)]
    capture_tests::transient_swap(path, 0, true)?;
    #[cfg(test)]
    capture_tests::capture_checkpoint(path, 0)?;
    validate_directory()?;
    match metadata {
        Ok(metadata) if metadata.len() == 0 => return Ok(0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        result => {
            result?;
        }
    }
    let capture = || -> Result<Vec<(String, Option<Vec<u8>>)>> {
        ["", "-wal", "-journal"]
            .into_iter()
            .map(|suffix| {
                let name = format!("print-partner.db{suffix}");
                validate_directory()?;
                #[cfg(test)]
                let checkpoint = match suffix {
                    "" => 1,
                    "-wal" => 2,
                    _ => 3,
                };
                #[cfg(test)]
                capture_tests::transient_swap(path, checkpoint, false)?;
                let read = open_file(&name).and_then(|mut file| {
                    let mut bytes = Vec::new();
                    file.read_to_end(&mut bytes).map_err(|error| {
                        std::io::Error::new(
                            error.kind(),
                            format!("schema::preflight: read {name}: {error}"),
                        )
                    })?;
                    Ok(bytes)
                });
                #[cfg(test)]
                capture_tests::transient_swap(path, checkpoint, true)?;
                #[cfg(test)]
                capture_tests::capture_checkpoint(
                    path,
                    match suffix {
                        "" => 1,
                        "-wal" => 2,
                        _ => 3,
                    },
                )?;
                validate_directory()?;
                let bytes = match read {
                    Ok(bytes) => Some(bytes),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                };
                Ok((suffix.to_owned(), bytes))
            })
            .collect()
    };
    let source = capture()?;
    #[cfg(test)]
    capture_tests::capture_checkpoint(path, 4)?;
    validate_directory()?;
    let copy = PreflightCopy(std::env::temp_dir().join(format!(
        "pp-preflight-{}",
        hex::encode(rand::random::<[u8; 16]>())
    )));
    #[cfg(test)]
    PREFLIGHT_COPIES.update(|copies| copies + 1);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&copy.0)
        .with_context(|| {
            format!(
                "schema::preflight: create temporary directory {}",
                copy.0.display()
            )
        })?;
    for (suffix, bytes) in &source {
        if let Some(bytes) = bytes {
            std::fs::write(copy.0.join(format!("print-partner.db{suffix}")), bytes).with_context(
                || format!("schema::preflight: write captured print-partner.db{suffix}"),
            )?;
        }
    }
    ensure!(
        source == capture()?,
        "Database changed during preflight capture"
    );
    validate_directory()?;
    let conn = Connection::open(copy.0.join("print-partner.db"))?;
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    ensure!(
        integrity == "ok",
        "SQLite integrity_check failed: {integrity}"
    );
    let has_settings: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='app_settings')",
        [],
        |r| r.get(0),
    )?;
    let value: Option<String> = if has_settings {
        conn.query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
            [],
            |r| r.get(0),
        )
        .optional()?
    } else {
        None
    };
    let version = match value {
        None => 0,
        Some(value) => {
            ensure!(
                !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
                "Invalid database schema version"
            );
            value
                .parse::<u64>()
                .map_err(|_| anyhow::anyhow!("Database schema version exceeds supported range"))?
        }
    };
    ensure!(
        version <= CURRENT_VERSION,
        "Database schema version {version} is newer than supported version {CURRENT_VERSION}"
    );
    ensure!(
        version == 0 || version >= 31,
        "Cannot upgrade database schema version {version}; install Print Partner v3.3.0 first"
    );
    crate::jobs::validate_schema(&conn, version)?;
    #[cfg(feature = "desktop_rebuild")]
    crate::plan_publication::validate_schema(&conn, version)?;
    #[cfg(feature = "desktop_rebuild")]
    crate::uploads::validate_schema(&conn, version)?;
    #[cfg(feature = "desktop_rebuild")]
    crate::source_scan::validate_schema(&conn, version)?;
    #[cfg(feature = "desktop_rebuild")]
    crate::source_sync::validate_schema(&conn, version)?;
    Ok(version)
}

pub(crate) fn configure(conn: &Connection, read_only: bool) -> Result<()> {
    if !read_only {
        conn.pragma_update(None, "journal_mode", "WAL")?;
    }
    conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "query_only", i64::from(read_only))?;
    let journal: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    ensure!(journal == "wal", "WAL required");
    for (name, expected) in [
        ("synchronous", 2),
        ("foreign_keys", 1),
        ("busy_timeout", 5000),
        ("query_only", i64::from(read_only)),
    ] {
        let actual: i64 = conn.pragma_query_value(None, name, |r| r.get(0))?;
        ensure!(actual == expected, "SQLite {name} invariant failed");
    }
    Ok(())
}

pub(crate) fn backup(conn: &Connection, path: &Path, replace: bool) -> Result<()> {
    if path.exists()
        && let Some(source) = conn.path()
    {
        ensure!(
            path.canonicalize()
                .context("schema::backup: canonicalize destination")?
                != Path::new(source)
                    .canonicalize()
                    .context("schema::backup: canonicalize source")?,
            "Backup cannot replace the open database"
        );
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Backup parent missing"))?;
    std::fs::create_dir_all(parent).with_context(|| {
        format!(
            "schema::backup: create parent directory {}",
            parent.display()
        )
    })?;
    let temp = parent.join(format!(
        ".backup-{}.db",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let result = (|| -> Result<()> {
        conn.backup("main", &temp, None)?;
        let check = Connection::open_with_flags(&temp, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let integrity: String = check.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        ensure!(integrity == "ok", "Backup integrity failed");
        drop(check);
        File::open(&temp)
            .context("schema::backup: open temporary backup")?
            .sync_all()
            .context("schema::backup: sync temporary backup")?;
        if replace {
            std::fs::rename(&temp, path).context("schema::backup: rename temporary backup")?;
        } else {
            std::fs::hard_link(&temp, path).context("schema::backup: link temporary backup")?;
            std::fs::remove_file(&temp).context("schema::backup: unlink temporary backup")?;
        }
        File::open(parent)
            .context("schema::backup: open parent directory")?
            .sync_all()
            .context("schema::backup: sync parent directory")?;
        Ok(())
    })();
    for suffix in ["", "-wal", "-shm"] {
        let mut member = temp.clone().into_os_string();
        member.push(suffix);
        let _ = std::fs::remove_file(member);
    }
    result
}

pub(crate) fn initialize(
    path: &Path,
    version: u64,
    now: &str,
) -> Result<(Connection, SchemaReady)> {
    let mut conn = Connection::open(path)?;
    let populated = path
        .metadata()
        .with_context(|| {
            format!(
                "schema::initialize: stat {} after SQLite open",
                path.display()
            )
        })?
        .len()
        > 0;
    let legacy_version = if cfg!(feature = "desktop_rebuild") {
        37
    } else {
        CURRENT_VERSION
    };
    // The first backup is the database exactly as opened, before configure
    // or any repair, seed, or step write. A legacy upgrade keeps an earlier
    // copy because its pass is not one transaction. With the feature, that
    // copy is named for its from and to versions, as Node's upgrade guard
    // names its backups, so a database restored at another version gets its
    // own copy instead of reporting an unrelated older one.
    let first_step = if version < legacy_version {
        legacy_version
    } else {
        version + 1
    };
    let backup_path = if version < CURRENT_VERSION && populated {
        let name = if cfg!(feature = "desktop_rebuild") && version < legacy_version {
            format!("backups/pre-schema{version}-to-{CURRENT_VERSION}.db")
        } else {
            format!("backups/pre-schema{first_step}.db")
        };
        let target = path.parent().unwrap().join(name);
        if version >= legacy_version || !target.exists() {
            backup(&conn, &target, true)?;
        }
        Some(target)
    } else {
        None
    };
    configure(&conn, false)?;
    let data: SchemaData = serde_json::from_str(include_str!("../data/schema.json"))?;
    for sql in &data.migrations {
        execute_migration(&conn, sql)?;
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for sql in &data.repair {
        tx.execute_batch(sql)?;
    }
    let stranded: Option<i64> = tx.query_row(&data.stranded, [], |r| r.get(0)).optional()?;
    if let Some(id) = stranded {
        bail!(
            "Active Source revision {id} cannot be moved without conflicting with protected history; manual recovery is required"
        );
    }
    tx.commit()?;
    // Historical Node client.ts rebuilt this table when source_revision_id was NOT NULL,
    // before stamping the current version. Schema 34 already publishes the repaired
    // shape, so this stays inside version 34 instead of allocating a new version.
    rebuild_legacy_plan_revision_inputs(&mut conn, &data.migrations)?;
    realign_equivalent_indexes_and_triggers(&mut conn, &data.migrations)?;
    add_column(
        &conn,
        "parts",
        "spoolman_spool_id",
        "ALTER TABLE parts ADD COLUMN spoolman_spool_id TEXT",
    )?;
    conn.execute_batch(&data.extras[0])?;
    add_column(
        &conn,
        "print_progress",
        "assembled",
        "ALTER TABLE print_progress ADD COLUMN assembled INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute_batch(&data.extras[1])?;
    if version < 34 {
        conn.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES('default','schema_version','34') ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",[])?;
    }
    {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if version < 35 {
            tx.execute_batch(include_str!("jobs/schema.sql"))?;
        }
        tx.execute_batch(include_str!("jobs/printer-start-indexes.sql"))?;
        if version < 35 {
            tx.execute(
                "UPDATE app_settings SET value='35' WHERE tenant_id='default' AND key='schema_version'",
                [],
            )?;
        }
        tx.commit()?;
    }
    // Steps run after the common restoration and tenant repair, whose writes
    // their triggers would refuse, and before starter seeding.
    #[cfg(feature = "desktop_rebuild")]
    let (backup_path, step_copies) = {
        if version < legacy_version {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute_batch(include_str!("plan_publication/schema.sql"))?;
            tx.execute("UPDATE app_settings SET value='37' WHERE tenant_id='default' AND key='schema_version'", [])?;
            tx.commit()?;
        }
        install_steps(&mut conn, path, version, populated, backup_path)?
    };
    #[cfg(not(feature = "desktop_rebuild"))]
    let step_copies: Vec<PathBuf> = Vec::new();
    #[cfg(feature = "desktop_rebuild")]
    {
        crate::plan_publication::validate_schema(&conn, CURRENT_VERSION)?;
        crate::uploads::validate_schema(&conn, CURRENT_VERSION)?;
        crate::source_scan::validate_schema(&conn, CURRENT_VERSION)?;
        crate::source_sync::validate_schema(&conn, CURRENT_VERSION)?;
    }
    // Node's fixture builder connects once to create the input and again to
    // create the expected database. The first Rust open must repeat that
    // INSERT OR IGNORE pass, including the sequence bump for rows that
    // already exist. user_version is unused by Node and is not part of the
    // schema graph, so it records that this one pass already happened.
    // Later opens only insert a missing starter. Another ignore pass would
    // allocate rowids for rows that are already there.
    let replayed: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let groups = data.seeds;
    let last = groups.len().saturating_sub(1);
    for (index, group) in groups.into_iter().enumerate() {
        let tx = conn.transaction()?;
        let sql = if replayed >= STARTER_SEED_REPLAY {
            starter_seed_insert_sql(&group.table, &group.columns)?
        } else {
            format!(
                "INSERT OR IGNORE INTO {}({}) VALUES({})",
                group.table,
                group.columns.join(","),
                vec!["?"; group.columns.len()].join(",")
            )
        };
        for row in group.rows {
            let values = row
                .into_iter()
                .zip(&group.columns)
                .map(|(value, column)| {
                    if column == "imported_at" {
                        return Ok(Value::Text(now.into()));
                    }
                    match value {
                        serde_json::Value::Null => Ok(Value::Null),
                        serde_json::Value::String(s) => Ok(Value::Text(s)),
                        serde_json::Value::Number(n) => n
                            .as_i64()
                            .map(Value::Integer)
                            .ok_or_else(|| anyhow::anyhow!("Invalid seed integer")),
                        _ => bail!("Invalid seed scalar"),
                    }
                })
                .collect::<Result<Vec<_>>>()?;
            tx.execute(&sql, rusqlite::params_from_iter(values))?;
        }
        if replayed < STARTER_SEED_REPLAY && index == last {
            tx.execute_batch(&format!("PRAGMA user_version = {STARTER_SEED_REPLAY}"))?;
        }
        tx.commit()?;
    }
    // The upgrade has committed, so retention may now drop copies left by
    // earlier upgrades. This upgrade's copies all stay, including the
    // from-version copy, the only one the previous release opens.
    let pruned = match &backup_path {
        Some(rollback) => crate::backup_retention::prune(rollback, &step_copies),
        None => Default::default(),
    };
    Ok((
        conn,
        SchemaReady {
            version: CURRENT_VERSION,
            previous_version: version,
            backup: backup_path,
            pruned_backups: pruned.removed,
            backup_warnings: pruned.warnings,
        },
    ))
}

// Each step validates and commits with its version stamp, so a refused or
// interrupted step leaves the from-version intact and a fresh copy is always
// current. `first` is the backup already taken at open, if any.
#[cfg(feature = "desktop_rebuild")]
fn install_steps(
    conn: &mut Connection,
    path: &Path,
    version: u64,
    populated: bool,
    first: Option<PathBuf>,
) -> Result<(Option<PathBuf>, Vec<PathBuf>)> {
    let mut first = first;
    let mut written = Vec::new();
    let install_scan: fn(&Connection) -> Result<()> =
        |connection| Ok(connection.execute_batch(include_str!("source_scan/schema.sql"))?);
    let install_sync: fn(&Connection) -> Result<()> =
        |connection| Ok(connection.execute_batch(include_str!("source_sync/schema.sql"))?);
    for (step, install) in crate::uploads::SCHEMA_STEPS
        .into_iter()
        .chain([(40, install_scan), (41, install_sync)])
    {
        if version < step {
            let target =
                populated.then(|| path.with_file_name(format!("backups/pre-schema{step}.db")));
            if let Some(target) = &target
                && first.as_ref() != Some(target)
            {
                backup(conn, target, true)?;
                written.push(target.clone());
            }
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            install(&tx)?;
            crate::uploads::validate_schema(&tx, step)?;
            crate::source_scan::validate_schema(&tx, step)?;
            crate::source_sync::validate_schema(&tx, step)?;
            tx.execute(
                "UPDATE app_settings SET value=?1 WHERE tenant_id='default' AND key='schema_version'",
                [step.to_string()],
            )?;
            tx.commit()?;
            first = first.or(target);
        }
    }
    Ok((first, written))
}

pub(crate) fn sql_identity(sql: &str) -> String {
    let mut tokens: Vec<&str> = sql.split_whitespace().collect();
    let mut index = 1;
    if matches!(tokens.get(index), Some(&"UNIQUE" | &"TEMP" | &"TEMPORARY")) {
        index += 1;
    }
    if matches!(
        tokens.get(index),
        Some(&"TABLE" | &"INDEX" | &"TRIGGER" | &"VIEW")
    ) {
        index += 1;
        if tokens.get(index) == Some(&"IF")
            && tokens.get(index + 1) == Some(&"NOT")
            && tokens.get(index + 2) == Some(&"EXISTS")
        {
            tokens.drain(index..index + 3);
        }
    }
    tokens.join(" ")
}

/// SQL text SQLite records in `sqlite_master` for a `CREATE` statement.
///
/// The catalog drops one leading `IF NOT EXISTS` and any whitespace before
/// `CREATE`. Indentation and later `NOT EXISTS` predicates stay as written.
fn sqlite_catalog_sql(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut index = 0;
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let start = index;
    let mut words = Vec::new();
    while words.len() < 6 && index < bytes.len() {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        let word_start = index;
        while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        words.push((word_start, index));
    }
    let word = |position: usize| words.get(position).map(|(from, to)| &sql[*from..*to]);
    let mut kind_at = 1;
    if matches!(word(kind_at), Some("UNIQUE" | "TEMP" | "TEMPORARY")) {
        kind_at += 1;
    }
    if word(0) == Some("CREATE")
        && matches!(word(kind_at), Some("TABLE" | "INDEX" | "TRIGGER" | "VIEW"))
        && word(kind_at + 1) == Some("IF")
        && word(kind_at + 2) == Some("NOT")
        && word(kind_at + 3) == Some("EXISTS")
    {
        let kind_end = words[kind_at].1;
        let exists_end = words[kind_at + 3].1;
        return format!("{}{}", &sql[start..kind_end], &sql[exists_end..]);
    }
    sql[start..].to_owned()
}

fn canonical_migration<'a>(migrations: &'a [String], needle: &str) -> Result<&'a str> {
    let mut matches = migrations.iter().filter(|sql| sql.contains(needle));
    let found = matches
        .next()
        .ok_or_else(|| anyhow::anyhow!("Missing migration containing {needle}"))?;
    ensure!(
        matches.next().is_none(),
        "Multiple migrations contain {needle}"
    );
    Ok(found)
}

const STARTER_SEED_REPLAY: i64 = 1;

fn quote_sql_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn starter_seed_insert_sql(table: &str, columns: &[String]) -> Result<String> {
    let tenant = columns
        .iter()
        .position(|column| column == "tenant_id")
        .ok_or_else(|| anyhow::anyhow!("Starter seed {table} has no tenant_id"))?;
    let name = columns
        .iter()
        .position(|column| column == "name")
        .ok_or_else(|| anyhow::anyhow!("Starter seed {table} has no name"))?;
    let quoted_table = quote_sql_ident(table);
    let column_list = columns
        .iter()
        .map(|column| quote_sql_ident(column))
        .collect::<Vec<_>>()
        .join(",");
    let placeholders = (1..=columns.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        "INSERT INTO {quoted_table} ({column_list}) SELECT {placeholders} WHERE NOT EXISTS (SELECT 1 FROM {quoted_table} WHERE {} = ?{} AND {} = ?{})",
        quote_sql_ident("tenant_id"),
        tenant + 1,
        quote_sql_ident("name"),
        name + 1,
    ))
}

fn rebuild_legacy_plan_revision_inputs(conn: &mut Connection, migrations: &[String]) -> Result<()> {
    let notnull: Option<i64> = conn
        .query_row(
            "SELECT \"notnull\" FROM pragma_table_info('plan_revision_inputs') WHERE name = 'source_revision_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if notnull != Some(1) {
        return Ok(());
    }
    let orphan: Option<i64> = conn
        .query_row(
            "SELECT input.id
               FROM plan_revision_inputs input
               LEFT JOIN source_revisions revision ON revision.id = input.source_revision_id
              WHERE revision.id IS NULL
              LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = orphan {
        bail!("Cannot migrate Plan revision input {id}: Source revision is missing");
    }
    let create = canonical_migration(
        migrations,
        "CREATE TABLE IF NOT EXISTS plan_revision_inputs",
    )?;
    let tenant_index = canonical_migration(migrations, "idx_plan_revision_inputs_tenant_set")?;
    let unique_index = canonical_migration(migrations, "uq_plan_revision_inputs_v2_set_source")?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Historical schemaMigrations filled these columns before the client rebuild.
    // Current schema.json publishes the repaired CREATE instead of that UPDATE,
    // so a legacy NOT NULL table still needs the fill before the NOT NULL copy.
    tx.execute_batch(
        "UPDATE plan_revision_inputs
            SET source_id = (
                  SELECT project_id FROM source_revisions
                   WHERE source_revisions.id = plan_revision_inputs.source_revision_id
                ),
                source_layer = 'legacy:' || COALESCE((
                  SELECT project_id FROM source_revisions
                   WHERE source_revisions.id = plan_revision_inputs.source_revision_id
                ), 0)
          WHERE source_id IS NULL OR source_layer IS NULL",
    )?;
    tx.execute_batch("ALTER TABLE plan_revision_inputs RENAME TO plan_revision_inputs_v17")?;
    tx.execute_batch(create)?;
    tx.execute_batch(
        "INSERT INTO plan_revision_inputs (
            id, tenant_id, input_set_id, source_id, source_layer, layer_order,
            tracking_kind, source_revision_id, manifest_digest, effective_naming_digest
         )
         SELECT
            id, tenant_id, input_set_id, source_id, source_layer, layer_order,
            tracking_kind, source_revision_id, manifest_digest, effective_naming_digest
           FROM plan_revision_inputs_v17",
    )?;
    tx.execute_batch("DROP TABLE plan_revision_inputs_v17")?;
    tx.execute_batch(unique_index)?;
    tx.execute_batch(tenant_index)?;
    // Historical Node's empty rebuild records sqlite_sequence.seq = 0. Current
    // fresh initialization never inserts that row. The next AUTOINCREMENT id is
    // 1 either way, so drop only the empty marker and leave a real sequence.
    let has_sequence: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'sqlite_sequence')",
        [],
        |row| row.get(0),
    )?;
    if has_sequence {
        tx.execute_batch(
            "DELETE FROM sqlite_sequence
              WHERE name = 'plan_revision_inputs'
                AND seq = 0
                AND NOT EXISTS (SELECT 1 FROM plan_revision_inputs)",
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn realign_equivalent_indexes_and_triggers(
    conn: &mut Connection,
    migrations: &[String],
) -> Result<()> {
    let mut statement = conn.prepare(
        "SELECT type, name, sql FROM sqlite_master
          WHERE sql IS NOT NULL AND type IN ('index', 'trigger')
            AND name NOT LIKE 'sqlite_%'",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(statement);
    let mut replacements = Vec::new();
    for (kind, name, sql) in rows {
        let identity = sql_identity(&sql);
        let mut matches = migrations
            .iter()
            .filter(|statement| sql_identity(statement) == identity);
        let Some(canonical) = matches.next() else {
            continue;
        };
        // sqlite_master omits the leading IF NOT EXISTS that schema.json keeps.
        // A raw compare is therefore false for every matching object and this
        // path drops and recreates it on every open. Compare the stored form.
        if matches.next().is_some() || sqlite_catalog_sql(canonical) == sqlite_catalog_sql(&sql) {
            continue;
        }
        replacements.push((kind, name, canonical.clone()));
    }
    if replacements.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for (kind, name, statement) in &replacements {
        let drop_sql = match kind.as_str() {
            "index" => format!("DROP INDEX {}", quote_sql_ident(name)),
            "trigger" => format!("DROP TRIGGER {}", quote_sql_ident(name)),
            other => bail!("Refusing to realign {other} {name}"),
        };
        tx.execute_batch(&drop_sql)?;
        tx.execute_batch(statement)?;
    }
    tx.commit()?;
    Ok(())
}

fn execute_migration(conn: &Connection, sql: &str) -> Result<()> {
    match conn.execute_batch(sql) {
        Ok(()) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if sql.starts_with("ALTER TABLE ")
                && message.starts_with("duplicate column name: ") =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn add_column(conn: &Connection, table: &str, column: &str, sql: &str) -> Result<()> {
    let columns = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|name| name == column) {
        conn.execute_batch(sql)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_ignores_only_sqlite_duplicate_column_errors() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE existing (id INTEGER)")
            .unwrap();
        let sql = "ALTER TABLE existing ADD COLUMN added TEXT";
        execute_migration(&conn, sql).unwrap();
        execute_migration(&conn, sql).unwrap();
        let error = execute_migration(
            &conn,
            "ALTER TABLE \"duplicate column name\" ADD COLUMN added TEXT",
        )
        .unwrap_err();
        assert!(error.to_string().starts_with("no such table: "));
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    thread_local! {
        static OWNED_TRANSIENT_SWAP: std::cell::Cell<Option<u8>> = const { std::cell::Cell::new(None) };
        static TRANSIENT_SWAP: std::cell::Cell<Option<(u8, bool)>> = const { std::cell::Cell::new(None) };
        static REPLACE_DURING_CAPTURE: std::cell::Cell<Option<(u8, u8)>> = const { std::cell::Cell::new(None) };
    }

    pub(super) fn arm_owned_swap() {
        if let Some(checkpoint) = OWNED_TRANSIENT_SWAP.take() {
            TRANSIENT_SWAP.set(Some((checkpoint, false)));
        }
    }

    #[test]
    fn ticket_t_12_postlease_writer_uses_admitted_descriptor() {
        for checkpoint in 0..=3 {
            let path = std::env::temp_dir().join(format!(
                "pp-owned-writer-{}",
                hex::encode(rand::random::<[u8; 8]>())
            ));
            let admitted = crate::lease::admit_directory(&path).unwrap();
            drop(admitted);
            let conn = Connection::open(path.join("print-partner.db")).unwrap();
            conn.execute_batch("CREATE TABLE capture_fixture(value TEXT);")
                .unwrap();
            drop(conn);
            let replacement = path.with_extension("replacement");
            std::fs::create_dir(&replacement).unwrap();
            std::fs::write(replacement.join("print-partner.db"), b"decoy").unwrap();
            PREFLIGHT_COPIES.set(0);
            OWNED_TRANSIENT_SWAP.set(Some(checkpoint));
            let result = crate::WriterOwner::open(&path, crate::Limits::default());
            OWNED_TRANSIENT_SWAP.set(None);
            TRANSIENT_SWAP.set(None);
            let (writer, ready) = result.unwrap();
            assert_eq!(ready.previous_version, 0);
            assert_eq!(PREFLIGHT_COPIES.get(), 2);
            writer.shutdown().unwrap();
            assert_eq!(
                std::fs::read(replacement.join("print-partner.db")).unwrap(),
                b"decoy"
            );
            assert!(!path.join(".desktop-owner.json").exists());
            std::fs::remove_dir_all(&path).unwrap();
            std::fs::remove_dir_all(replacement).unwrap();
        }
    }

    #[test]
    fn ticket_t_12_postlease_refuses_changed_directory_before_early_returns() {
        for database in ["missing", "empty", "populated"] {
            for replacement in ["directory", "symlink", "permissions"] {
                let path = std::env::temp_dir().join(format!(
                    "pp-owned-identity-{}",
                    hex::encode(rand::random::<[u8; 8]>())
                ));
                let lease = crate::lease::StorageLease::acquire(&path).unwrap();
                let db = path.join("print-partner.db");
                match database {
                    "empty" => std::fs::write(&db, b"").unwrap(),
                    "populated" => {
                        let conn = Connection::open(&db).unwrap();
                        conn.execute_batch("CREATE TABLE capture_fixture(value TEXT);")
                            .unwrap();
                    }
                    _ => {}
                }
                let before = std::fs::read(&db).ok();
                let retired = path.with_extension("retired");
                if replacement == "permissions" {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777))
                        .unwrap();
                } else {
                    std::fs::rename(&path, &retired).unwrap();
                    if replacement == "symlink" {
                        std::os::unix::fs::symlink(&retired, &path).unwrap();
                    } else {
                        std::fs::DirBuilder::new()
                            .mode(0o700)
                            .create(&path)
                            .unwrap();
                    }
                }
                PREFLIGHT_COPIES.set(0);
                let result = lease.preflight();
                let copies = PREFLIGHT_COPIES.get();
                if replacement == "permissions" {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
                        .unwrap();
                } else {
                    if replacement == "symlink" {
                        std::fs::remove_file(&path).unwrap();
                    } else {
                        std::fs::remove_dir(&path).unwrap();
                    }
                    std::fs::rename(retired, &path).unwrap();
                }
                assert_eq!(std::fs::read(&db).ok(), before);
                let released = lease.release();
                assert!(
                    released.lock_released && released.marker_removed && released.runtime_removed
                );
                std::fs::remove_dir_all(path).unwrap();
                let error = result.unwrap_err().to_string();
                assert!(
                    error.contains(if replacement == "permissions" {
                        "Unsafe data directory permissions"
                    } else {
                        "Data directory was replaced"
                    }),
                    "{database}/{replacement}: {error}"
                );
                assert_eq!(copies, 0);
            }
        }
    }

    pub(super) fn transient_swap(database: &Path, checkpoint: u8, restore: bool) -> Result<()> {
        if TRANSIENT_SWAP.get() != Some((checkpoint, restore)) {
            return Ok(());
        }
        let path = database.parent().unwrap();
        let retired = path.with_extension("retired");
        let replacement = path.with_extension("replacement");
        if restore {
            std::fs::remove_file(path)?;
            std::fs::rename(retired, path)?;
            TRANSIENT_SWAP.set(None);
        } else {
            std::fs::rename(path, retired)?;
            std::os::unix::fs::symlink(replacement, path)?;
            TRANSIENT_SWAP.set(Some((checkpoint, true)));
        }
        Ok(())
    }

    #[test]
    fn ticket_t_12_preflight_descriptor_ignores_transient_metadata_swap() {
        assert_transient_swap_ignored(0, false);
    }

    #[test]
    fn ticket_t_12_preflight_descriptor_ignores_transient_database_swap() {
        assert_transient_swap_ignored(1, false);
    }

    #[test]
    fn ticket_t_12_preflight_descriptor_ignores_transient_wal_swap() {
        assert_transient_swap_ignored(2, false);
    }

    #[test]
    fn ticket_t_12_preflight_descriptor_ignores_transient_journal_swap() {
        assert_transient_swap_ignored(3, false);
    }

    fn preflight_runner(
        admitted: crate::lease::AdmittedDirectory,
        owned: bool,
    ) -> Box<dyn Fn() -> Result<()> + Send> {
        if owned {
            let lease = crate::lease::StorageLease::acquire_admitted(admitted).unwrap();
            Box::new(move || lease.preflight().map(|_| ()))
        } else {
            Box::new(move || admitted.preflight())
        }
    }

    #[test]
    fn ticket_t_12_postlease_descriptor_ignores_transient_swaps() {
        for checkpoint in 0..=3 {
            assert_transient_swap_ignored(checkpoint, true);
        }
    }

    #[test]
    fn ticket_t_12_postlease_descriptor_refuses_symlink_members() {
        assert_symlink_members_refused(true);
    }

    #[test]
    fn ticket_t_12_postlease_descriptor_refuses_special_members_without_blocking() {
        assert_special_members_refused(true);
    }

    fn assert_transient_swap_ignored(checkpoint: u8, owned: bool) {
        let path = std::env::temp_dir().join(format!(
            "pp-descriptor-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let admitted = crate::lease::admit_directory(&path).unwrap();
        let preflight = preflight_runner(admitted, owned);
        let conn = Connection::open(path.join("print-partner.db")).unwrap();
        conn.execute_batch("CREATE TABLE capture_fixture(value TEXT);")
            .unwrap();
        drop(conn);
        for name in ["print-partner.db-wal", "print-partner.db-journal"] {
            std::fs::write(path.join(name), b"").unwrap();
        }
        let before = std::fs::read(path.join("print-partner.db")).unwrap();
        let replacement = path.with_extension("replacement");
        std::fs::create_dir(&replacement).unwrap();
        let decoy = match checkpoint {
            0 => b"".as_slice(),
            1 => b"outside admitted store",
            _ => &before,
        };
        std::fs::write(replacement.join("print-partner.db"), decoy).unwrap();
        if checkpoint >= 2 {
            let name = if checkpoint == 2 {
                "print-partner.db-wal"
            } else {
                "print-partner.db-journal"
            };
            std::fs::create_dir(replacement.join(name)).unwrap();
        }
        PREFLIGHT_COPIES.set(0);
        TRANSIENT_SWAP.set(Some((checkpoint, false)));
        let result = preflight();
        TRANSIENT_SWAP.set(None);
        let copies = PREFLIGHT_COPIES.get();
        assert_eq!(
            std::fs::read(path.join("print-partner.db")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read(replacement.join("print-partner.db")).unwrap(),
            decoy
        );
        drop(preflight);
        std::fs::remove_dir_all(&path).unwrap();
        std::fs::remove_dir_all(replacement).unwrap();
        assert!(result.is_ok(), "checkpoint {checkpoint}: {result:?}");
        assert_eq!(
            copies, 1,
            "checkpoint {checkpoint} read replacement metadata"
        );
    }

    #[test]
    fn ticket_t_12_preflight_descriptor_refuses_symlink_members() {
        assert_symlink_members_refused(false);
    }

    fn assert_symlink_members_refused(owned: bool) {
        for name in [
            "print-partner.db",
            "print-partner.db-wal",
            "print-partner.db-journal",
        ] {
            let path = std::env::temp_dir().join(format!(
                "pp-descriptor-link-{}",
                hex::encode(rand::random::<[u8; 8]>())
            ));
            let admitted = crate::lease::admit_directory(&path).unwrap();
            let preflight = preflight_runner(admitted, owned);
            let conn = Connection::open(path.join("print-partner.db")).unwrap();
            conn.execute_batch("CREATE TABLE capture_fixture(value TEXT);")
                .unwrap();
            drop(conn);
            let external = path.with_extension("external");
            if name == "print-partner.db" {
                std::fs::rename(path.join(name), &external).unwrap();
            } else {
                std::fs::write(&external, b"").unwrap();
            }
            let before = std::fs::read(&external).unwrap();
            std::os::unix::fs::symlink(&external, path.join(name)).unwrap();
            PREFLIGHT_COPIES.set(0);
            let result = preflight();
            let copies = PREFLIGHT_COPIES.get();
            assert_eq!(std::fs::read(&external).unwrap(), before);
            drop(preflight);
            std::fs::remove_dir_all(path).unwrap();
            std::fs::remove_file(external).unwrap();
            assert!(result.is_err(), "preflight followed {name}");
            assert_eq!(copies, 0, "preflight copied symlink member {name}");
        }
    }

    #[test]
    fn ticket_t_12_preflight_descriptor_refuses_special_members_without_blocking() {
        assert_special_members_refused(false);
    }

    fn assert_special_members_refused(owned: bool) {
        use std::os::unix::fs::OpenOptionsExt;
        for name in [
            "print-partner.db",
            "print-partner.db-wal",
            "print-partner.db-journal",
        ] {
            for kind in ["dangling", "directory", "fifo"] {
                let path = std::env::temp_dir().join(format!(
                    "pp-descriptor-special-{}",
                    hex::encode(rand::random::<[u8; 8]>())
                ));
                let admitted = crate::lease::admit_directory(&path).unwrap();
                let preflight = preflight_runner(admitted, owned);
                let conn = Connection::open(path.join("print-partner.db")).unwrap();
                conn.execute_batch("CREATE TABLE capture_fixture(value TEXT);")
                    .unwrap();
                drop(conn);
                let member = path.join(name);
                if name == "print-partner.db" {
                    std::fs::remove_file(&member).unwrap();
                }
                match kind {
                    "dangling" => std::os::unix::fs::symlink(path.join("absent"), &member).unwrap(),
                    "directory" => std::fs::create_dir(&member).unwrap(),
                    _ => {
                        use std::os::unix::ffi::OsStrExt;
                        let member = std::ffi::CString::new(member.as_os_str().as_bytes()).unwrap();
                        assert_eq!(unsafe { libc::mkfifo(member.as_ptr(), 0o600) }, 0);
                    }
                }
                let (tx, rx) = std::sync::mpsc::channel();
                let worker = std::thread::spawn(move || {
                    PREFLIGHT_COPIES.set(0);
                    let result = preflight().map_err(|error| error.to_string());
                    tx.send((result, PREFLIGHT_COPIES.get())).unwrap();
                });
                let result = rx.recv_timeout(Duration::from_secs(2));
                if result.is_err() && kind == "fifo" {
                    drop(
                        std::fs::OpenOptions::new()
                            .write(true)
                            .custom_flags(libc::O_NONBLOCK)
                            .open(&member)
                            .unwrap(),
                    );
                }
                worker.join().unwrap();
                let metadata = std::fs::symlink_metadata(&member).unwrap();
                std::fs::remove_dir_all(path).unwrap();
                let (result, copies) = result.expect("preflight blocked on special member");
                assert!(result.is_err(), "preflight accepted {kind} at {name}");
                assert_eq!(copies, 0);
                use std::os::unix::fs::FileTypeExt;
                assert!(match kind {
                    "dangling" => metadata.file_type().is_symlink(),
                    "directory" => metadata.is_dir(),
                    _ => metadata.file_type().is_fifo(),
                });
            }
        }
    }

    pub(super) fn capture_checkpoint(database: &Path, checkpoint: u8) -> Result<()> {
        let Some((selected, replacement)) = REPLACE_DURING_CAPTURE.get() else {
            return Ok(());
        };
        if selected != checkpoint {
            return Ok(());
        }
        REPLACE_DURING_CAPTURE.set(None);
        let path = database.parent().unwrap();
        if replacement >= 3 {
            let mode = match replacement {
                3 => 0o777,
                4 => 0o770,
                _ => 0o500,
            };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
            return Ok(());
        }
        let retired = path.with_extension("retired");
        std::fs::rename(path, &retired)?;
        let target = if replacement == 2 {
            path.with_extension("replacement")
        } else {
            path.to_path_buf()
        };
        std::fs::DirBuilder::new().mode(0o700).create(&target)?;
        for entry in std::fs::read_dir(&retired)? {
            let entry = entry?;
            std::fs::copy(entry.path(), target.join(entry.file_name()))?;
        }
        if replacement == 2 {
            std::os::unix::fs::symlink(target, path)?;
        }
        Ok(())
    }

    #[test]
    fn ticket_t_12_preflight_capture_admitted_store_creates_one_copy() {
        let path = std::env::temp_dir().join(format!(
            "pp-capture-safe-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let admitted = crate::lease::admit_directory(&path).unwrap();
        let conn = Connection::open(path.join("print-partner.db")).unwrap();
        conn.execute_batch("CREATE TABLE capture_fixture(value TEXT);")
            .unwrap();
        drop(conn);
        let before = std::fs::read(path.join("print-partner.db")).unwrap();
        PREFLIGHT_COPIES.set(0);
        admitted.preflight().unwrap();
        assert_eq!(PREFLIGHT_COPIES.get(), 1);
        assert_eq!(
            std::fs::read(path.join("print-partner.db")).unwrap(),
            before
        );
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 1);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn ticket_t_12_preflight_capture_rejects_store_replacement_before_copy() {
        for checkpoint in [1, 2, 3, 4, 0] {
            for replacement in 1..=5 {
                assert_capture_refused(checkpoint, replacement, 0);
            }
        }
    }

    #[test]
    fn ticket_t_12_preflight_capture_rejects_replacement_before_empty_return() {
        for replacement in 1..=5 {
            assert_capture_refused(0, replacement, 1);
        }
    }

    #[test]
    fn ticket_t_12_preflight_capture_rejects_replacement_before_missing_return() {
        for replacement in 1..=5 {
            assert_capture_refused(0, replacement, 2);
        }
    }

    fn assert_capture_refused(checkpoint: u8, replacement: u8, database: u8) {
        let path = std::env::temp_dir().join(format!(
            "pp-capture-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        if database == 1 {
            std::fs::write(path.join("print-partner.db"), b"").unwrap();
        } else if database == 0 {
            let conn = Connection::open(path.join("print-partner.db")).unwrap();
            conn.execute_batch("CREATE TABLE capture_fixture(value TEXT);")
                .unwrap();
        }
        for (name, bytes) in [
            ("print-partner.db-wal", b"copied wal".as_slice()),
            ("print-partner.db-journal", b"copied journal".as_slice()),
            ("desktop.toml", b"port = 45678\n".as_slice()),
            (
                ".desktop-owner.json",
                br#"{"pid":2147483647,"kind":"standalone"}"#.as_slice(),
            ),
        ] {
            std::fs::write(path.join(name), bytes).unwrap();
        }
        let snapshot = |directory: &Path| -> Vec<_> {
            let mut files: Vec<_> = std::fs::read_dir(directory)
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().file_name() != ".desktop-lease")
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), std::fs::read(entry.path()).unwrap())
                })
                .collect();
            files.sort();
            files
        };
        if database == 0 {
            std::fs::write(path.join("printpartner.lock"), b"").unwrap();
        }
        let before = snapshot(&path);
        PREFLIGHT_COPIES.set(0);
        let admitted = crate::lease::admit_directory(&path).unwrap();
        REPLACE_DURING_CAPTURE.set(Some((checkpoint, replacement)));
        let result = if database == 0 {
            crate::WriterOwner::open(&path, crate::Limits::default()).map(|_| ())
        } else {
            admitted.preflight()
        };
        REPLACE_DURING_CAPTURE.set(None);
        let error = result.err().map(|error| error.to_string());
        let copies = PREFLIGHT_COPIES.get();
        if replacement >= 3 {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        assert_eq!(snapshot(&path), before);
        if replacement <= 2 {
            let retired = path.with_extension("retired");
            assert_eq!(snapshot(&retired), before);
            std::fs::remove_dir_all(retired).unwrap();
        }
        if replacement == 2 {
            std::fs::remove_file(&path).unwrap();
            std::fs::remove_dir_all(path.with_extension("replacement")).unwrap();
        } else {
            std::fs::remove_dir_all(&path).unwrap();
        }
        assert_eq!(
            copies, 0,
            "checkpoint {checkpoint}, replacement {replacement}"
        );
        let expected = if replacement >= 3 {
            "Unsafe data directory permissions"
        } else {
            "Data directory was replaced"
        };
        assert_eq!(error.as_deref(), Some(expected));
    }
}
