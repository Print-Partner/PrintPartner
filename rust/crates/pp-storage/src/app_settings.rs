//! The date format, build tracking and default Spoolman settings, stored in
//! `app_settings` as Node's `routes/settings.ts` stores them (GRE-1357).
//!
//! Every command runs on the writer thread and resolves the caller's tenant
//! from its credential in the same transaction. Commands name one of three
//! fixed keys, so this client cannot read or write any other setting.

use crate::{Envelope, SettingSnapshot, Shared, WriterOwner, auth, read_model::Credential};
use anyhow::{Result, anyhow, ensure};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

/// Node's setting key for the integration list.
const INTEGRATIONS_KEY: &str = "integrations";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    DateFormat,
    BuildTracking,
    SpoolmanDefault,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Self::DateFormat => "date_format",
            Self::BuildTracking => "build_tracking_assembly",
            Self::SpoolmanDefault => "default_spoolman_integration_id",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    /// A date format id the caller has checked against Node's presets.
    DateFormat(String),
    /// Stored as `"1"` or `"0"`.
    BuildTracking(bool),
    /// A trimmed id. An empty id clears the default; any other id must name
    /// a stored Spoolman integration.
    SpoolmanDefault(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The stored value; `None` when the row is missing or empty, as Node's
    /// `getSetting` reads it.
    Value(Option<String>),
    Written,
    /// The Spoolman id names no integration, or one of another type.
    NotSpoolman,
}

#[derive(Clone)]
pub struct AppSettingsClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
}

pub(super) struct Reply(mpsc::Sender<Result<Outcome>>);

impl Reply {
    pub(super) fn send(self, outcome: Result<Outcome>) {
        let _ = self.0.send(outcome);
    }
}

pub(super) struct Command {
    credential: Credential,
    policy: auth::AuthPolicy,
    request: Request,
}

enum Request {
    Read(Key),
    Write(Write),
}

impl WriterOwner {
    pub fn app_settings_with_policy(&self, policy: auth::AuthPolicy) -> Result<AppSettingsClient> {
        auth::validate_policy(policy)?;
        Ok(AppSettingsClient {
            shared: self.client.shared.clone(),
            policy,
        })
    }
}

impl AppSettingsClient {
    pub fn read(
        &self,
        credential: Credential,
        key: Key,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Outcome> {
        self.submit(credential, Request::Read(key), cancelled, wait)
    }

    pub fn write(
        &self,
        credential: Credential,
        write: Write,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Outcome> {
        self.submit(credential, Request::Write(write), cancelled, wait)
    }

    fn submit(
        &self,
        credential: Credential,
        request: Request,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Outcome> {
        let deadline = Instant::now() + wait;
        let mut queue = self
            .shared
            .queue
            .lock()
            .map_err(|_| anyhow!("Writer admission poisoned"))?;
        loop {
            ensure!(!queue.closed, "Storage stopped");
            ensure!(
                !cancelled.load(Ordering::Acquire),
                "Cancelled before admission"
            );
            if queue.pending.len() < self.shared.capacity {
                break;
            }
            ensure!(Instant::now() < deadline, "Writer queue full");
            queue = self
                .shared
                .changed
                .wait_timeout(queue, Duration::from_millis(5))
                .map_err(|_| anyhow!("Writer admission poisoned"))?
                .0;
        }
        let (reply, receiver) = mpsc::channel();
        queue.pending.push_back(Envelope::AppSettings {
            command: Command {
                credential,
                policy: self.policy,
                request,
            },
            reply: Reply(reply),
        });
        self.shared.changed.notify_all();
        drop(queue);
        receiver.recv().map_err(|_| anyhow!("Storage stopped"))?
    }
}

pub(super) fn execute(
    connection: &mut Connection,
    command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<Outcome> {
    let tx = match command.request {
        Request::Write(_) => crate::writer_admission::begin_immediate(connection, admission)?,
        Request::Read(_) => connection.transaction_with_behavior(TransactionBehavior::Deferred)?,
    };
    let tenant = match command.credential {
        Credential::Session(secret) => auth::read_session_tenant(&tx, &secret, command.policy)?,
        Credential::ApiKey {
            routed_tenant,
            secret,
        } => auth::read_key_tenant(&tx, &routed_tenant, &secret)?,
    };
    let outcome = match command.request {
        Request::Read(key) => Outcome::Value(read(&tx, &tenant, key.name())?),
        Request::Write(Write::DateFormat(format)) => store(&tx, &tenant, Key::DateFormat, &format)?,
        Request::Write(Write::BuildTracking(on)) => {
            store(&tx, &tenant, Key::BuildTracking, if on { "1" } else { "0" })?
        }
        Request::Write(Write::SpoolmanDefault(id)) => {
            if id.is_empty() || is_spoolman(read(&tx, &tenant, INTEGRATIONS_KEY)?, &id) {
                store(&tx, &tenant, Key::SpoolmanDefault, &id)?
            } else {
                Outcome::NotSpoolman
            }
        }
    };
    tx.commit()?;
    Ok(outcome)
}

fn read(tx: &Transaction<'_>, tenant: &str, key: &str) -> Result<Option<String>> {
    Ok(match crate::snapshot(tx, tenant, key)? {
        SettingSnapshot::Stored { value } if !value.is_empty() => Some(value),
        _ => None,
    })
}

fn store(tx: &Transaction<'_>, tenant: &str, key: Key, value: &str) -> Result<Outcome> {
    tx.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value",
        params![tenant, key.name(), value],
    )?;
    Ok(Outcome::Written)
}

/// Node's `getIntegrationConfig(repo, id)?.type === "spoolman"`: the first
/// array element that is an object with string `id` equal to `id`. Only its
/// `type` is read. Text `serde_json` cannot parse (such as an unpaired UTF-16
/// surrogate escape) holds no integrations here, where `JSON.parse` would
/// accept it.
fn is_spoolman(integrations: Option<String>, id: &str) -> bool {
    let Some(Value::Array(items)) = integrations.and_then(|text| serde_json::from_str(&text).ok())
    else {
        return false;
    };
    items
        .iter()
        .find(|item| item.is_object() && item["id"].as_str() == Some(id))
        .is_some_and(|item| item["type"].as_str() == Some("spoolman"))
}

#[cfg(test)]
mod tests {
    use super::is_spoolman;

    #[test]
    fn spoolman_lookup_takes_the_first_row_with_a_string_id() {
        let stored = r#"[null,{"id":5,"type":"spoolman"},{"id":"a","type":"spoolman"},{"id":"a","type":"moonraker"},{"id":"m","type":"moonraker"}]"#;
        let text = Some(stored.to_owned());
        assert!(is_spoolman(text.clone(), "a"));
        assert!(!is_spoolman(text.clone(), "5"));
        assert!(!is_spoolman(text.clone(), "m"));
        assert!(!is_spoolman(text, "missing"));
        assert!(!is_spoolman(None, "a"));
        assert!(!is_spoolman(Some("not json".into()), "a"));
        assert!(!is_spoolman(Some(r#"{"id":"a"}"#.into()), "a"));
    }
}
