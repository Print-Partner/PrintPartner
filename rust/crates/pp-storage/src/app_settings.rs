//! Date format, build tracking, default Spoolman, help mode and bindings in
//! `app_settings` as Node's `routes/settings.ts` stores them (GRE-1357).
//!
//! Every command runs on the writer thread and resolves the caller's tenant
//! from its credential in the same transaction. Commands name
//! fixed keys and the binding list; it cannot access arbitrary settings.

use crate::{Envelope, SettingSnapshot, Shared, WriterOwner, auth, read_model::Credential};
use anyhow::{Result, anyhow};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde_json::Value;
use std::{
    sync::{Arc, atomic::AtomicBool, mpsc},
    time::Duration,
};

/// Node's setting key for the integration list.
const INTEGRATIONS_KEY: &str = "integrations";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    DateFormat,
    BuildTracking,
    SpoolmanDefault,
    ExternalAccess,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Self::DateFormat => "date_format",
            Self::BuildTracking => "build_tracking_assembly",
            Self::SpoolmanDefault => "default_spoolman_integration_id",
            Self::ExternalAccess => "external_access_mode",
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
    Json(Value),
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

/// One atomic read or mutation of Node's printer binding list.
#[derive(Clone, Debug)]
pub enum Bindings {
    Get,
    Put(Value),
    Delete(String),
}

enum Request {
    Read(Key),
    Write(Write),
    Bindings(Bindings),
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

    pub fn bindings(
        &self,
        credential: Credential,
        change: Bindings,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Outcome> {
        self.submit(credential, Request::Bindings(change), cancelled, wait)
    }

    fn submit(
        &self,
        credential: Credential,
        request: Request,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Outcome> {
        let (reply, receiver) = mpsc::channel();
        let command = Command {
            credential,
            policy: self.policy,
            request,
        };
        let envelope = Envelope::AppSettings {
            command,
            reply: Reply(reply),
        };
        self.shared.admit(envelope, cancelled, wait)?;
        receiver.recv().map_err(|_| anyhow!("Storage stopped"))?
    }
}

pub(super) fn execute(
    connection: &mut Connection,
    command: Command,
    admission: crate::writer_admission::Deadline,
) -> Result<Outcome> {
    let tx = match command.request {
        Request::Write(_) | Request::Bindings(Bindings::Put(_) | Bindings::Delete(_)) => {
            crate::writer_admission::begin_immediate(connection, admission)?
        }
        Request::Read(_) | Request::Bindings(Bindings::Get) => {
            connection.transaction_with_behavior(TransactionBehavior::Deferred)?
        }
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
        Request::Bindings(change) => bindings(&tx, &tenant, change)?,
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

/// Node stores the whole list in one app_settings row. Keep read-modify-write
/// and credential proof in one transaction, including replacement order.
fn bindings(tx: &Transaction<'_>, tenant: &str, change: Bindings) -> Result<Outcome> {
    const KEY: &str = "printer.plan_bindings";
    let raw = read(tx, tenant, KEY)?;
    let mut list: Value = raw
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or(serde_json::json!([]));
    if matches!(change, Bindings::Get) {
        return Ok(Outcome::Json(serde_json::json!({"bindings": list})));
    }
    let items = list
        .as_array_mut()
        .ok_or_else(|| anyhow!("Invalid stored bindings"))?;
    let deleting = matches!(change, Bindings::Delete(_));
    match change {
        Bindings::Put(body) => {
            let id = js_string(&body["integration_id"]);
            let id =
                id.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
            let value = &body["profile_id"];
            let number = if value.is_null() {
                Value::Null
            } else {
                let number = match value {
                    Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                    Value::Number(n) => n.as_f64(),
                    Value::String(s) if s.trim().is_empty() => Some(0.0),
                    Value::String(s) => s.trim().parse::<f64>().ok(),
                    _ => None,
                };
                number
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            };
            let stamp = crate::auth::timestamp_at(time::OffsetDateTime::now_utc());
            let entry = serde_json::json!({"integration_id": id, "profile_id": number, "updated_at": stamp});
            // Accessing a member of null throws in Node; don't mutate a broken list.
            if items.iter().any(Value::is_null) {
                return Err(anyhow!("Invalid stored bindings"));
            }
            if let Some(index) = items
                .iter()
                .position(|b| b["integration_id"].as_str() == Some(id))
            {
                items[index] = entry;
            } else {
                items.push(entry);
            }
        }
        Bindings::Delete(id) => {
            if items.iter().any(Value::is_null) {
                return Err(anyhow!("Invalid stored bindings"));
            }
            items.retain(|b| b["integration_id"].as_str() != Some(id.as_str()));
        }
        Bindings::Get => unreachable!(),
    }
    let text = serde_json::json!({"value": list}).to_string();
    let stored = match crate::source_scan::node_json::property(&text, "value") {
        crate::source_scan::node_json::Property::Value(text) => text,
        _ => return Err(anyhow!("Invalid stored bindings")),
    };
    tx.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value", params![tenant, KEY, stored])?;
    // Use the same JavaScript-normalized numbers in the reply and stored row.
    list = serde_json::from_str(&stored)?;
    Ok(Outcome::Json(if deleting {
        serde_json::json!({"ok":true})
    } else {
        serde_json::json!({"bindings":list})
    }))
}

fn js_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(number) => ryu_js::Buffer::new()
            .format(number.as_f64().unwrap_or(f64::NAN))
            .into(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(items) => items.iter().map(js_string).collect::<Vec<_>>().join(","),
        _ => value.to_string(),
    }
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
