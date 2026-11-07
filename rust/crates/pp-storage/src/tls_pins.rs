//! TLS certificate pins for printers that the Rust runtime reaches over TLS
//! (Bambu LAN MQTT, GRE-1245). A pin is the SHA-256 of the certificate the
//! printer presented the first time the user ran Test connection for that
//! integration, at that host and port. Only that explicit user action writes
//! a pin ([`SettingsClient::pin_tls`]); a background status read never does.
//! A pin for another host or port does not apply, and nothing replaces a pin
//! except a deliberate reset ([`SettingsClient::reset_tls_pin`]). The table is
//! Rust-only and installed by schema step [`SCHEMA_STEP`]; Node never reads it.

use crate::{SettingsClient, Work, WriteReply};
use anyhow::{Context, Result, anyhow};
use rusqlite::{OptionalExtension, Transaction, params};
use std::{sync::atomic::AtomicBool, time::Duration};

/// The schema step that installs the table.
pub(crate) const SCHEMA_STEP: u64 = 47;
/// The single-account tenant the printer settings live under.
const TENANT: &str = "default";

/// The certificate pinned for one integration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pin {
    pub host: String,
    pub port: u16,
    /// SHA-256 of the certificate's DER encoding.
    pub sha256: [u8; 32],
    /// When the pin was taken, as SQLite's UTC ISO-8601 text. Empty in a pin
    /// that is about to be stored; storage sets it.
    pub pinned_at: String,
}

pub(crate) enum Command {
    Pin { integration: String, pin: Pin },
    Reset { integration: String },
}

/// The pin stored for `integration`, if any.
pub fn read(client: &SettingsClient, wait: Duration, integration: &str) -> Result<Option<Pin>> {
    let checkout = client.reader(wait)?;
    let connection = checkout
        .connection
        .as_ref()
        .context("checked out connection")?;
    let row = connection
        .query_row(
            "SELECT host, port, sha256, pinned_at FROM printer_tls_pins WHERE tenant_id=?1 AND integration_id=?2",
            params![TENANT, integration],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    row.map(|(host, port, sha256, pinned_at)| {
        Ok(Pin {
            host,
            port: u16::try_from(port)?,
            sha256: sha256
                .try_into()
                .map_err(|_| anyhow!("Stored certificate pin is not 32 bytes"))?,
            pinned_at,
        })
    })
    .transpose()
}

impl SettingsClient {
    /// Stores `pin` for `integration` unless a pin is already stored. The
    /// reply is `true` when this call stored it.
    pub fn pin_tls(
        &self,
        integration: &str,
        pin: Pin,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<WriteReply> {
        let integration = integration.to_owned();
        self.enqueue(
            Work::TlsPin(Command::Pin { integration, pin }),
            cancelled,
            wait,
        )
    }

    /// Deletes the pin for `integration`. The reply is `true` when one was
    /// stored.
    pub fn reset_tls_pin(
        &self,
        integration: &str,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<WriteReply> {
        let integration = integration.to_owned();
        self.enqueue(
            Work::TlsPin(Command::Reset { integration }),
            cancelled,
            wait,
        )
    }
}

pub(crate) fn execute(tx: &Transaction<'_>, command: Command) -> Result<bool> {
    let changed = match command {
        Command::Pin { integration, pin } => tx.execute(
            "INSERT INTO printer_tls_pins(tenant_id,integration_id,host,port,sha256,pinned_at) VALUES(?1,?2,?3,?4,?5,strftime('%Y-%m-%dT%H:%M:%fZ','now')) ON CONFLICT(tenant_id,integration_id) DO NOTHING",
            params![TENANT, integration, pin.host, pin.port, pin.sha256.as_slice()],
        )?,
        Command::Reset { integration } => tx.execute(
            "DELETE FROM printer_tls_pins WHERE tenant_id=?1 AND integration_id=?2",
            params![TENANT, integration],
        )?,
    };
    Ok(changed > 0)
}

#[cfg(test)]
mod tests;
