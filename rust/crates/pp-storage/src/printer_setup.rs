//! Printer setup: adding and removing integrations, and reading printers.
//!
//! Each kind lives in one `app_settings` document with one typed shape
//! ([`integrations`], [`printers`]). A write runs through the write seam: it
//! proves the credential, parses the whole stored document, changes it and
//! stores it, in one transaction. A read runs through the read seam. A
//! document that is not the typed shape refuses every operation and is left
//! as it is.
//!
//! Errors are [`PrinterSetupError`]. This module holds no status codes and no
//! response text; `pp-api` maps the variants.

pub(crate) mod integrations;
pub(crate) mod printers;

pub use integrations::{IntegrationSummary, NewIntegration, ServiceKind};
pub use printers::{LoadedFilament, Printer};

use crate::{
    SettingsClient, Shared, WriterFailure, WriterOwner,
    auth::AuthPolicy,
    read_model::Credential,
    tls_pins,
    write::{Installation, authorize},
};
use integrations::{Entry, Integration};
use rusqlite::{OptionalExtension, Transaction, params};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const INTEGRATIONS_KEY: &str = "integrations";
const SPOOLMAN_DEFAULT_KEY: &str = "default_spoolman_integration_id";
/// Rows of every kind count. One person's farm stays far below this.
pub const MAX_INTEGRATIONS: usize = 64;

/// A request field with a value this module refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Kind,
    Name,
    BaseUrl,
    Username,
    ApiKey,
    Password,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Empty,
    TooLong,
    /// Not an absolute `http` or `https` URL with a host.
    NotHttpUrl,
    /// The URL carries a username or password.
    HasCredentials,
    /// A kind Rust keeps but cannot create.
    NotCreatable,
    /// The kind does not sign in with this field.
    NotUsedByKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidField {
    pub field: Field,
    pub reason: Reason,
}

/// Why the stored document is not the typed shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentFault {
    NotAList,
    /// The entry has no `type` this code knows.
    UnknownKind,
    /// The entry does not match the shape of its kind, or has no id.
    WrongShape,
    DuplicateId,
}

/// One of the two stored documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupDocument {
    Integrations,
    Printers,
}

/// Where a stored document stops being the typed shape. It names the entry
/// and never holds a stored value, so it is safe to log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidDocument {
    pub document: SetupDocument,
    /// The entry's position in the list; `None` when the text is not a list.
    pub entry: Option<usize>,
    pub reason: DocumentFault,
}

#[derive(Debug, thiserror::Error)]
pub enum PrinterSetupError {
    /// The request has these invalid fields. Nothing was stored.
    #[error("{} request field(s) are invalid", .0.len())]
    Invalid(Vec<InvalidField>),
    #[error("no stored integration or printer has that id")]
    NotFound,
    #[error("the installation already has {MAX_INTEGRATIONS} integrations")]
    LimitReached,
    /// Every operation is refused and the stored text is untouched.
    #[error("a stored printer setup document is not the typed shape")]
    StoredDocumentInvalid(InvalidDocument),
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

impl From<rusqlite::Error> for PrinterSetupError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Writer(error.into())
    }
}

impl From<anyhow::Error> for PrinterSetupError {
    fn from(error: anyhow::Error) -> Self {
        Self::Writer(error.into())
    }
}

#[derive(Clone)]
pub struct PrinterSetupClient {
    shared: Arc<Shared>,
    /// The read-only pool, for reads through the read seam.
    readers: SettingsClient,
    policy: AuthPolicy,
}

impl WriterOwner {
    pub fn printer_setup(&self, policy: AuthPolicy) -> anyhow::Result<PrinterSetupClient> {
        crate::auth::validate_policy(policy)?;
        Ok(PrinterSetupClient {
            shared: self.client.shared.clone(),
            readers: self.client(),
            policy,
        })
    }
}

impl PrinterSetupClient {
    /// Stores a new integration and answers it without its secrets.
    pub fn create_integration(
        &self,
        credential: Credential,
        new: NewIntegration,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<IntegrationSummary, PrinterSetupError> {
        let policy = self.policy;
        let name = "printer_setup.create_integration";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let mut entries = load(tx, &installation)?;
            let id = hex::encode(rand::random::<[u8; 16]>());
            let row = Integration::new(new, id, now(tx)?).map_err(PrinterSetupError::Invalid)?;
            if entries.len() >= MAX_INTEGRATIONS {
                return Err(PrinterSetupError::LimitReached);
            }
            let summary = row.summary();
            entries.push(Entry::Host(row));
            save(tx, &installation, &entries)?;
            Ok(summary)
        })?
    }

    /// Removes the integration of any kind with `id`, together with its TLS
    /// pin and a Spoolman default that names it, and answers what it removed.
    /// A printer that linked it keeps the id and reads as unlinked.
    pub fn delete_integration(
        &self,
        credential: Credential,
        id: String,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<IntegrationSummary, PrinterSetupError> {
        let policy = self.policy;
        let name = "printer_setup.delete_integration";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let mut entries = load(tx, &installation)?;
            let index = entries
                .iter()
                .position(|entry| entry.id() == id)
                .ok_or(PrinterSetupError::NotFound)?;
            let removed = entries.remove(index).summary();
            save(tx, &installation, &entries)?;
            tls_pins::execute(tx, tls_pins::Command::Reset { integration: id })?;
            tx.execute(
                "UPDATE app_settings SET value='' WHERE tenant_id=?1 AND key=?2 AND value=?3",
                params![installation.id(), SPOOLMAN_DEFAULT_KEY, removed.id],
            )?;
            Ok(removed)
        })?
    }
}

/// The whole stored integrations document.
fn load(
    tx: &Transaction<'_>,
    installation: &Installation,
) -> Result<Vec<Entry>, PrinterSetupError> {
    let Some(text) = stored(tx, installation, INTEGRATIONS_KEY)? else {
        return Ok(Vec::new());
    };
    integrations::parse(&text).map_err(invalid_document)
}

fn save(
    tx: &Transaction<'_>,
    installation: &Installation,
    entries: &[Entry],
) -> Result<(), PrinterSetupError> {
    store(tx, installation, INTEGRATIONS_KEY, entries)
}

/// A stored document's text. No row and an empty value are both no document.
fn stored(
    tx: &Transaction<'_>,
    installation: &Installation,
    key: &str,
) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2 AND value<>''",
        params![installation.id(), key],
        |row| row.get(0),
    )
    .optional()
}

fn store(
    tx: &Transaction<'_>,
    installation: &Installation,
    key: &str,
    document: &(impl serde::Serialize + ?Sized),
) -> Result<(), PrinterSetupError> {
    let text = serde_json::to_string(document).map_err(anyhow::Error::from)?;
    tx.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
        params![installation.id(), key, text],
    )?;
    Ok(())
}

fn invalid_document(fault: InvalidDocument) -> PrinterSetupError {
    tracing::error!(document = ?fault.document, entry = ?fault.entry, reason = ?fault.reason, "stored printer setup document is invalid");
    PrinterSetupError::StoredDocumentInvalid(fault)
}

/// UTC now as SQLite writes it, with milliseconds.
fn now(tx: &Transaction<'_>) -> rusqlite::Result<String> {
    tx.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
        row.get(0)
    })
}

#[cfg(test)]
mod tests;
