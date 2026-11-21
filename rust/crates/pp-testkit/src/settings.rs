//! Seeding settings rows that the generic setting write refuses.
//!
//! `SettingsClient::submit` refuses the documents another module owns, such
//! as `integrations` and `printer.fleet` (GRE-1480). A test that needs one in
//! place before it runs stores the row here, with raw SQL, as the Node import
//! or an older build would have left it.

use crate::DATABASE_FILE;
use std::{path::Path, time::Duration};

/// Stores `value` under `key` for installation `default` in the database in
/// the data directory `data`, replacing any stored row. The schema must
/// already exist. A running writer may hold the database; the insert waits
/// up to five seconds for it.
pub fn seed_setting(data: &Path, key: &str, value: &str) -> rusqlite::Result<()> {
    let database = rusqlite::Connection::open(data.join(DATABASE_FILE))?;
    database.busy_timeout(Duration::from_secs(5))?;
    database.execute(
        "INSERT OR REPLACE INTO app_settings (tenant_id, key, value) VALUES ('default', ?1, ?2)",
        [key, value],
    )?;
    Ok(())
}
