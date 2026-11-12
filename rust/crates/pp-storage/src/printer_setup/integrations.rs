//! The typed `integrations` settings document: one row per stored
//! integration, in stored order.
//!
//! Rust creates three kinds ([`ServiceKind::creatable`]) and writes them with
//! the field names the readers in `printer_hosts`, `read_model::farm` and
//! `read_model::stock` already accept. A row of another known kind is kept
//! byte for byte and can only be deleted.
//!
//! The shape is strict (`deny_unknown_fields`) and has no version field: a
//! stored document's shape changes only through a numbered schema step that
//! rewrites it. A document that is not this shape takes no write. Values are
//! checked when they arrive, in [`Integration::new`], and not again when a
//! stored row is read. A stored row may carry a credential field its kind does
//! not use; the readers ignore it.

use super::{DocumentFault, Field, InvalidDocument, InvalidField, Reason, SetupDocument};
use crate::auth::Secret;
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};

pub const MAX_NAME_BYTES: usize = 200;
pub const MAX_BASE_URL_BYTES: usize = 2048;
/// The status watchers skip a host whose key or password is longer.
pub const MAX_CREDENTIAL_BYTES: usize = crate::printer_hosts::MAX_CONFIG_BYTES;

/// The service an integration connects to; the stored `type`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum ServiceKind {
    Moonraker,
    Prusalink,
    Spoolman,
    Bambu,
    HomeAssistant,
    AiAssistant,
    SlicerSidecar,
}

impl ServiceKind {
    /// The kinds Rust creates. The rest are kept and can only be deleted.
    pub fn creatable(self) -> bool {
        matches!(self, Self::Moonraker | Self::Prusalink | Self::Spoolman)
    }

    /// PrusaLink signs in with a username and password; Moonraker and
    /// Spoolman take an API key.
    fn takes(self, field: Field) -> bool {
        match field {
            Field::Username | Field::Password => self == Self::Prusalink,
            Field::ApiKey => self != Self::Prusalink,
            Field::Kind | Field::Name | Field::BaseUrl => true,
        }
    }
}

/// One stored row of a creatable kind. No `Debug`: a stored `base_url` that
/// predates the checks can hold a password.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Integration {
    pub(crate) id: String,
    #[serde(rename = "type")]
    pub(crate) kind: ServiceKind,
    pub(crate) name: String,
    pub(crate) config: HostConfig,
    /// UTC, as `2026-11-12T03:04:05.678Z`.
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostConfig {
    pub(crate) base_url: String,
    pub(crate) enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) username: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "stored_secret"
    )]
    pub(crate) api_key: Option<Secret>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "stored_secret"
    )]
    pub(crate) password: Option<Secret>,
    /// Where PrusaLink uploads go. The upload path reads it; no route sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) storage: Option<String>,
}

/// The only path that writes a [`Secret`] out: into the stored document.
/// `Secret` has no `Serialize`, so a response type cannot carry one.
mod stored_secret {
    use super::Secret;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(
        secret: &Option<Secret>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        secret.as_ref().map(Secret::expose).serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Secret>, D::Error> {
        Ok(Option::<String>::deserialize(deserializer)?.map(Secret::new))
    }
}

/// One stored integration without its secrets: what every write answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegrationSummary {
    pub id: String,
    pub kind: ServiceKind,
    pub name: String,
    pub enabled: bool,
    /// `None` for a kept kind, whose address is not a `base_url`.
    pub base_url: Option<String>,
    pub username: Option<String>,
    pub has_api_key: bool,
    pub has_password: bool,
}

/// A new integration, before validation. No `Debug`: an unchecked `base_url`
/// can hold a password.
pub struct NewIntegration {
    pub kind: ServiceKind,
    pub name: String,
    pub base_url: String,
    pub enabled: bool,
    pub username: Option<String>,
    pub api_key: Option<Secret>,
    pub password: Option<Secret>,
}

/// One row of the document. No `Debug`: a kept row's text holds its secrets.
pub(crate) enum Entry {
    Host(Integration),
    /// A kind Rust cannot create: its stored text, written back unchanged.
    Kept {
        summary: IntegrationSummary,
        raw: Box<RawValue>,
    },
}

impl Entry {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Host(row) => &row.id,
            Self::Kept { summary, .. } => &summary.id,
        }
    }

    pub(crate) fn summary(&self) -> IntegrationSummary {
        match self {
            Self::Host(row) => row.summary(),
            Self::Kept { summary, .. } => summary.clone(),
        }
    }

    fn parse(raw: Box<RawValue>) -> Result<Self, DocumentFault> {
        #[derive(Deserialize)]
        struct Head {
            #[serde(rename = "type")]
            kind: ServiceKind,
        }
        /// What a summary reads from a kept row; its other fields are not Rust's.
        #[derive(Deserialize)]
        struct KeptRow {
            id: String,
            #[serde(default)]
            name: Value,
            #[serde(default)]
            config: Value,
        }
        let Ok(Head { kind }) = serde_json::from_str(raw.get()) else {
            return Err(DocumentFault::UnknownKind);
        };
        if kind.creatable() {
            return serde_json::from_str(raw.get())
                .ok()
                .filter(|row: &Integration| !row.id.is_empty())
                .map(Self::Host)
                .ok_or(DocumentFault::WrongShape);
        }
        let row = serde_json::from_str::<KeptRow>(raw.get())
            .ok()
            .filter(|row| !row.id.is_empty())
            .ok_or(DocumentFault::WrongShape)?;
        let summary = IntegrationSummary {
            id: row.id,
            kind,
            name: row.name.as_str().unwrap_or_default().to_owned(),
            enabled: row.config["enabled"] != Value::Bool(false),
            base_url: None,
            username: None,
            has_api_key: row.config["api_key"].is_string(),
            has_password: row.config["password"].is_string(),
        };
        Ok(Self::Kept { summary, raw })
    }
}

impl Serialize for Entry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Host(row) => row.serialize(serializer),
            Self::Kept { raw, .. } => raw.serialize(serializer),
        }
    }
}

/// The stored document, or which entry is not the typed shape and why. The
/// fault never holds a stored value.
pub(crate) fn parse(text: &str) -> Result<Vec<Entry>, InvalidDocument> {
    let fault = |entry, reason| InvalidDocument {
        document: SetupDocument::Integrations,
        entry,
        reason,
    };
    let rows: Vec<Box<RawValue>> =
        serde_json::from_str(text).map_err(|_| fault(None, DocumentFault::NotAList))?;
    let mut entries: Vec<Entry> = Vec::with_capacity(rows.len());
    for (index, raw) in rows.into_iter().enumerate() {
        let entry = Entry::parse(raw).map_err(|reason| fault(Some(index), reason))?;
        if entries.iter().any(|known| known.id() == entry.id()) {
            return Err(fault(Some(index), DocumentFault::DuplicateId));
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Checks new values for one integration and collects every invalid field.
struct Checks {
    kind: ServiceKind,
    invalid: Vec<InvalidField>,
}

impl Checks {
    fn fail(&mut self, field: Field, reason: Reason) {
        self.invalid.push(InvalidField { field, reason });
    }

    /// The name without surrounding whitespace.
    fn name(&mut self, name: &str) -> String {
        let name = name.trim();
        if name.is_empty() {
            self.fail(Field::Name, Reason::Empty);
        } else if name.len() > MAX_NAME_BYTES {
            self.fail(Field::Name, Reason::TooLong);
        }
        name.to_owned()
    }

    fn base_url(&mut self, value: &str) {
        // The parser drops whitespace and control characters; the stored
        // text would keep them.
        let clean = !value.contains(|c: char| c.is_whitespace() || c.is_control());
        let parsed = url::Url::parse(value).ok().filter(|url| {
            clean
                && matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some_and(|host| !host.is_empty())
        });
        match parsed {
            _ if value.len() > MAX_BASE_URL_BYTES => self.fail(Field::BaseUrl, Reason::TooLong),
            None => self.fail(Field::BaseUrl, Reason::NotHttpUrl),
            Some(url) if !url.username().is_empty() || url.password().is_some() => {
                self.fail(Field::BaseUrl, Reason::HasCredentials);
            }
            Some(_) => {}
        }
    }

    /// A username, API key or password, when the request sets one. Which of
    /// them a kind takes is decided here and nowhere else.
    fn credential(&mut self, field: Field, value: Option<&str>) {
        let Some(value) = value else { return };
        if !self.kind.takes(field) {
            self.fail(field, Reason::NotUsedByKind);
        } else if value.is_empty() {
            self.fail(field, Reason::Empty);
        } else if value.len() > MAX_CREDENTIAL_BYTES {
            self.fail(field, Reason::TooLong);
        }
    }
}

impl Integration {
    /// The row for `new`, created at `now` with id `id`. Every new row is
    /// built here, so every stored value passed these checks.
    pub(crate) fn new(
        new: NewIntegration,
        id: String,
        now: String,
    ) -> Result<Self, Vec<InvalidField>> {
        let mut checks = Checks {
            kind: new.kind,
            invalid: Vec::new(),
        };
        if !new.kind.creatable() {
            checks.fail(Field::Kind, Reason::NotCreatable);
        }
        let name = checks.name(&new.name);
        checks.base_url(&new.base_url);
        checks.credential(Field::Username, new.username.as_deref());
        checks.credential(Field::ApiKey, new.api_key.as_ref().map(Secret::expose));
        checks.credential(Field::Password, new.password.as_ref().map(Secret::expose));
        if !checks.invalid.is_empty() {
            return Err(checks.invalid);
        }
        Ok(Self {
            id,
            kind: new.kind,
            name,
            config: HostConfig {
                base_url: new.base_url,
                enabled: new.enabled,
                username: new.username,
                api_key: new.api_key,
                password: new.password,
                storage: None,
            },
            created_at: now.clone(),
            updated_at: now,
        })
    }

    pub(crate) fn summary(&self) -> IntegrationSummary {
        IntegrationSummary {
            id: self.id.clone(),
            kind: self.kind,
            name: self.name.clone(),
            enabled: self.config.enabled,
            base_url: Some(self.config.base_url.clone()),
            username: self.config.username.clone(),
            has_api_key: self.config.api_key.is_some(),
            has_password: self.config.password.is_some(),
        }
    }
}
