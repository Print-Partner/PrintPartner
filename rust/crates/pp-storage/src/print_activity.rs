//! Print activity: the plates sent to printers in a time window, counted per
//! printer from `print_jobs`, for MCP `get_print_stats` (GRE-1352).
//!
//! One read on the read seam: it proves the credential and counts in the
//! same snapshot, and never waits in the writer queue. The counts come from
//! one grouped query, so a busy window is counted whole; Node listed the
//! newest 100 jobs and counted those.
//!
//! Errors are [`PrintActivityError`]. This module holds no status codes and
//! no response text.

use crate::{
    SettingsClient, WriterFailure, WriterOwner,
    auth::{self, AuthPolicy},
    read_model::Credential,
    write::authorize,
};
use rusqlite::{Transaction, params};
use std::{sync::atomic::AtomicBool, time::Duration};

/// One printer's plates in the window. `printer_id` is `None` for jobs that
/// name no printer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterActivity {
    pub printer_id: Option<String>,
    /// Every job in the window, whatever its status.
    pub sent: u32,
    pub completed: u32,
    pub failed: u32,
    /// Grams the completed jobs recorded; jobs with no figure add nothing.
    pub filament_consumed_g: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum PrintActivityError {
    #[error(transparent)]
    Reader(#[from] WriterFailure),
}

impl From<rusqlite::Error> for PrintActivityError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Reader(error.into())
    }
}

#[derive(Clone)]
pub struct PrintActivityClient {
    readers: SettingsClient,
    policy: AuthPolicy,
}

impl WriterOwner {
    pub fn print_activity(&self, policy: AuthPolicy) -> anyhow::Result<PrintActivityClient> {
        auth::validate_policy(policy)?;
        Ok(PrintActivityClient {
            readers: self.client(),
            policy,
        })
    }
}

impl PrintActivityClient {
    /// The installation's jobs whose `at` is at or after `since`, an RFC 3339
    /// time with milliseconds as `print_jobs` stores it, counted per printer
    /// in printer order.
    pub fn since(
        &self,
        credential: Credential,
        since: &str,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Vec<PrinterActivity>, PrintActivityError> {
        let name = "print_activity.since";
        self.readers.read(name, cancelled, wait, |tx| {
            let installation = authorize(tx, &credential, self.policy)?;
            count(tx, installation.id(), since)
        })?
    }
}

fn count(
    tx: &Transaction<'_>,
    tenant: &str,
    since: &str,
) -> Result<Vec<PrinterActivity>, PrintActivityError> {
    let mut statement = tx.prepare(
        "SELECT NULLIF(TRIM(printer_id),''), COUNT(*), \
                COALESCE(SUM(status='completed'),0), COALESCE(SUM(status='failed'),0), \
                COALESCE(SUM(filament_consumed_g),0) \
         FROM print_jobs WHERE tenant_id=?1 AND at>=?2 \
         GROUP BY NULLIF(TRIM(printer_id),'') ORDER BY 1",
    )?;
    let rows = statement.query_map(params![tenant, since], |row| {
        Ok(PrinterActivity {
            printer_id: row.get(0)?,
            sent: row.get(1)?,
            completed: row.get(2)?,
            failed: row.get(3)?,
            filament_consumed_g: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
