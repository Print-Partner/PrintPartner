//! The Moonraker hosts in the stored `integrations` setting, projected the way
//! Node's private `/__desktop/moonraker-hosts` route does
//! (`web/apps/server/src/moonraker-host-snapshot.ts`). The Rust-only runtime
//! feeds this to the status watchers in place of that Node route, together
//! with the stored PrusaLink hosts (GRE-1175), which Node polls on demand
//! instead. Read-only.
//!
//! One deliberate difference: a setting that `serde_json` cannot parse, such as
//! one with an unpaired UTF-16 surrogate escape, is invalid here, so monitoring
//! fails closed where Node's `JSON.parse` would accept it.
use crate::{SettingSnapshot, SettingsClient};
use anyhow::Result;
use serde_json::Value;
use std::time::Duration;

/// Node's setting key for the integration list.
const INTEGRATIONS_KEY: &str = "integrations";
/// Node's setting key for the printer fleet.
const FLEET_KEY: &str = "printer.fleet";
/// The single-account tenant Node's `repository.getSetting` reads.
const TENANT: &str = "default";
/// More hosts than this are sent as ids only, so every Moonraker id fails
/// closed instead of polling a partial set. Matches `pp_drivers::moonraker::MAX_HOSTS`.
pub const MAX_HOSTS: usize = 128;
/// Past this many Moonraker rows the whole snapshot is invalid.
pub const MAX_SNAPSHOT_IDS: usize = 2048;
pub const MAX_ID_BYTES: usize = 256;
/// A larger `base_url` or `api_key` keeps its id but gets no endpoint.
pub const MAX_CONFIG_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoonrakerHost {
    pub id: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub enabled: bool,
}

/// The hosts the Rust-only status watchers poll, from one read of the setting.
/// Not `Debug`: it holds PrusaLink passwords.
#[derive(Clone, PartialEq, Eq)]
pub struct WatchedHosts {
    /// Node's Moonraker host snapshot.
    pub moonraker: Vec<MoonrakerHost>,
    /// The PrusaLink rows the watchers take, in stored order: the first row
    /// per usable id, skipping an id a Moonraker host already has, while the
    /// total stays within [`MAX_HOSTS`], so PrusaLink rows never push the
    /// Moonraker snapshot over the cap. A row with an oversized `base_url`,
    /// username or password keeps its id with an empty `base_url`, so it is
    /// never polled.
    pub prusalink: Vec<StoredIntegration>,
}

/// Read and project the stored setting. `Ok(None)` means the setting is
/// malformed and monitoring must fail closed; `Err` means storage was busy.
pub fn read(client: &SettingsClient, wait: Duration) -> Result<Option<WatchedHosts>> {
    let raw = match client.reader(wait)?.snapshot(TENANT, INTEGRATIONS_KEY)? {
        SettingSnapshot::Stored { value } => Some(value),
        SettingSnapshot::Missing => None,
    };
    Ok(watched(raw.as_deref()))
}

/// The ids of the hosts the watchers actually poll: a watched Moonraker or
/// PrusaLink host with an address. `None` for an unusable setting.
pub fn polled_ids(raw: Option<&str>) -> Option<Vec<String>> {
    let hosts = watched(raw)?;
    let moonraker = hosts
        .moonraker
        .into_iter()
        .map(|host| (host.id, host.base_url));
    let prusalink = hosts
        .prusalink
        .into_iter()
        .map(|host| (host.id, host.base_url));
    Some(
        moonraker
            .chain(prusalink)
            .filter(|(_, base_url)| !base_url.trim().is_empty())
            .map(|(id, _)| id)
            .collect(),
    )
}

/// [`project`] plus the PrusaLink rows of the same setting that the
/// watchers take.
pub fn watched(raw: Option<&str>) -> Option<WatchedHosts> {
    let moonraker = project(raw)?;
    let items = raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let mut prusalink: Vec<StoredIntegration> = Vec::new();
    for item in items
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if item["type"].as_str() != Some("prusalink") {
            continue;
        }
        let Some(id) = item["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES)
        else {
            continue;
        };
        if moonraker.len() + prusalink.len() >= MAX_HOSTS {
            break;
        }
        if moonraker.iter().any(|host| host.id == id) || prusalink.iter().any(|host| host.id == id)
        {
            continue;
        }
        let mut host = stored(item, id);
        let oversized = [
            Some(&host.base_url),
            Some(&host.username),
            host.password.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| value.len() > MAX_CONFIG_BYTES);
        if oversized {
            host.base_url.clear();
        }
        prusalink.push(host);
    }
    Some(WatchedHosts {
        moonraker,
        prusalink,
    })
}

/// Node's `moonrakerHostSnapshot`: `None` for an unusable setting.
pub fn project(raw: Option<&str>) -> Option<Vec<MoonrakerHost>> {
    let items = match raw.filter(|raw| !raw.is_empty()) {
        None => return Some(Vec::new()),
        Some(raw) => serde_json::from_str::<Value>(raw).ok()?,
    };
    let mut rows = Vec::new();
    for item in items.as_array()? {
        let Some(item) = item.as_object() else {
            continue;
        };
        if item.get("type").and_then(Value::as_str) != Some("moonraker") {
            continue;
        }
        let id = item
            .get("id")?
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES)?;
        let config = match item.get("config") {
            None | Some(Value::Null) => None,
            Some(Value::Object(config)) => Some(config),
            Some(_) => return None,
        };
        rows.push((id.to_owned(), config));
        if rows.len() > MAX_SNAPSHOT_IDS {
            return None;
        }
    }
    let ids_only = rows.len() > MAX_HOSTS;
    Some(
        rows.into_iter()
            .map(|(id, config)| {
                let field = |names: [&str; 2]| {
                    let config = config?;
                    names
                        .iter()
                        .find_map(|name| config.get(*name).filter(|value| !value.is_null()))
                };
                let enabled = field(["enabled", "enabled"]) != Some(&Value::Bool(false));
                if ids_only {
                    return projected(id, false);
                }
                let base_url = field(["base_url", "baseUrl"])
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let api_key = field(["api_key", "apiKey"]).and_then(Value::as_str);
                if base_url.len() > MAX_CONFIG_BYTES
                    || api_key.is_some_and(|key| key.len() > MAX_CONFIG_BYTES)
                {
                    return projected(id, enabled);
                }
                MoonrakerHost {
                    id,
                    base_url: base_url.to_owned(),
                    api_key: api_key.map(str::to_owned),
                    enabled,
                }
            })
            .collect(),
    )
}

fn projected(id: String, enabled: bool) -> MoonrakerHost {
    MoonrakerHost {
        id,
        base_url: String::new(),
        api_key: None,
        enabled,
    }
}

/// A stored integration, unredacted, as Node's `getIntegrationConfig` reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredIntegration {
    pub id: String,
    /// The stored `type`, empty when it is not a string.
    pub kind: String,
    pub base_url: String,
    pub api_key: Option<String>,
    /// PrusaLink's `username ?? user`, as JavaScript's `String()` reads it.
    pub username: String,
    /// PrusaLink's `password ?? api_key` when it is a string.
    pub password: Option<String>,
    /// PrusaLink's pinned `storage ?? storage_path` when it is a string.
    pub storage: Option<String>,
    /// False only when `config.enabled` is exactly `false`.
    pub enabled: bool,
}

/// How a fleet printer reaches its host, as Node's `printerHostCapability`
/// resolves it before checking the adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrinterLink {
    PrinterNotFound,
    /// No `integration_id`, or a blank one.
    NotLinked,
    /// The linked integration is missing or disabled.
    NotAvailable,
    Linked(StoredIntegration),
}

/// The integration with `id`, or `None` when no stored row has it.
pub fn integration(
    client: &SettingsClient,
    wait: Duration,
    id: &str,
) -> Result<Option<StoredIntegration>> {
    Ok(find_integration(
        &setting(client, wait, INTEGRATIONS_KEY)?,
        id,
    ))
}

/// Resolve fleet printer `printer_id` to its linked integration.
pub fn printer_link(
    client: &SettingsClient,
    wait: Duration,
    printer_id: &str,
) -> Result<PrinterLink> {
    let reader = client.reader(wait)?;
    let read = |key: &str| -> Result<Value> {
        Ok(match reader.snapshot(TENANT, key)? {
            SettingSnapshot::Stored { value } => {
                serde_json::from_str(&value).unwrap_or(Value::Null)
            }
            SettingSnapshot::Missing => Value::Null,
        })
    };
    let fleet = read(FLEET_KEY)?;
    let Some(printer) = fleet
        .as_array()
        .into_iter()
        .flatten()
        .find(|printer| js_string(&printer["id"]).as_deref() == Some(printer_id))
    else {
        return Ok(PrinterLink::PrinterNotFound);
    };
    let Some(linked) = js_string(&printer["integration_id"])
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
    else {
        return Ok(PrinterLink::NotLinked);
    };
    Ok(match find_integration(&read(INTEGRATIONS_KEY)?, &linked) {
        Some(integration) if integration.enabled => PrinterLink::Linked(integration),
        _ => PrinterLink::NotAvailable,
    })
}

/// How Node's printer upload labels a send: the fleet printer's `name` and
/// its linked integration's `name`, each `None` when missing or not a string.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SendLabels {
    pub printer_name: Option<String>,
    pub host_name: Option<String>,
}

/// The labels for fleet printer `printer_id`.
pub fn send_labels(
    client: &SettingsClient,
    wait: Duration,
    printer_id: &str,
) -> Result<SendLabels> {
    let fleet = setting(client, wait, FLEET_KEY)?;
    let Some(printer) = fleet
        .as_array()
        .into_iter()
        .flatten()
        .find(|printer| js_string(&printer["id"]).as_deref() == Some(printer_id))
    else {
        return Ok(SendLabels::default());
    };
    let name = |value: &Value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    };
    let integration = js_string(&printer["integration_id"]).map(|id| id.trim().to_owned());
    let host_name = match integration {
        Some(id) if !id.is_empty() => setting(client, wait, INTEGRATIONS_KEY)?
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["id"].as_str() == Some(id.as_str()))
            .and_then(|item| name(&item["name"])),
        _ => None,
    };
    Ok(SendLabels {
        printer_name: name(&printer["name"]),
        host_name,
    })
}

fn setting(client: &SettingsClient, wait: Duration, key: &str) -> Result<Value> {
    Ok(match client.reader(wait)?.snapshot(TENANT, key)? {
        SettingSnapshot::Stored { value } => serde_json::from_str(&value).unwrap_or(Value::Null),
        SettingSnapshot::Missing => Value::Null,
    })
}

/// Node's `loadRaw(...).find((x) => x.id === id)`: the first object whose
/// `id` is that string.
fn find_integration(items: &Value, id: &str) -> Option<StoredIntegration> {
    let item = items
        .as_array()?
        .iter()
        .find(|item| item.is_object() && item["id"].as_str() == Some(id))?;
    Some(stored(item, id))
}

/// One stored integration row with id `id`, read as Node's adapters read it.
fn stored(item: &Value, id: &str) -> StoredIntegration {
    let config = item["config"].as_object();
    let field = |names: [&str; 2]| {
        names
            .iter()
            .find_map(|name| config?.get(*name).filter(|value| !value.is_null()))
    };
    StoredIntegration {
        id: id.to_owned(),
        kind: item["type"].as_str().unwrap_or_default().to_owned(),
        base_url: field(["base_url", "baseUrl"])
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        api_key: field(["api_key", "apiKey"])
            .and_then(Value::as_str)
            .map(str::to_owned),
        username: field(["username", "user"])
            .and_then(js_string)
            .unwrap_or_default(),
        password: field(["password", "api_key"])
            .and_then(Value::as_str)
            .map(str::to_owned),
        storage: field(["storage", "storage_path"])
            .and_then(Value::as_str)
            .map(str::to_owned),
        enabled: config.and_then(|config| config.get("enabled")) != Some(&Value::Bool(false)),
    }
}

/// JavaScript's `String(value)` for the scalar ids Node's fleet parser reads.
fn js_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn host(id: &str, base_url: &str, api_key: Option<&str>, enabled: bool) -> MoonrakerHost {
        MoonrakerHost {
            id: id.into(),
            base_url: base_url.into(),
            api_key: api_key.map(str::to_owned),
            enabled,
        }
    }

    /// Replays the corpus the Node `moonraker-host-snapshot` test also reads.
    #[test]
    fn ticket_t_4_watcher_proof_host_snapshot_parity_corpus() {
        let corpus: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../../../../fixtures/drivers/moonraker/host-snapshot-parity.json"
        ))
        .unwrap();
        let cases = corpus["cases"].as_array().unwrap();
        assert!(cases.len() >= 13);
        for case in cases {
            let expected = case["expected"].as_array().map(|hosts| {
                hosts
                    .iter()
                    .map(|h| {
                        host(
                            h["id"].as_str().unwrap(),
                            h["base_url"].as_str().unwrap(),
                            h["api_key"].as_str(),
                            h["enabled"].as_bool().unwrap(),
                        )
                    })
                    .collect::<Vec<_>>()
            });
            assert_eq!(
                project(case["setting"].as_str()),
                expected,
                "{}",
                case["name"]
            );
        }
    }

    #[test]
    fn ticket_t_4_watcher_proof_host_projection_bounds() {
        // Oversized config isolates its own id and keeps the enabled choice.
        let big = "h".repeat(MAX_CONFIG_BYTES + 1);
        let stored = json!([
            {"id": "big", "type": "moonraker", "config": {"base_url": big}},
            {"id": "key", "type": "moonraker", "config": {"base_url": "http://p.test", "api_key": big, "enabled": false}},
            {"id": "ok", "type": "moonraker", "config": {"base_url": "http://p.test"}},
        ]);
        assert_eq!(
            project(Some(&stored.to_string())),
            Some(vec![
                host("big", "", None, true),
                host("key", "", None, false),
                host("ok", "http://p.test", None, true),
            ])
        );
        // Past the host cap every row is id-only and disabled; unrelated rows
        // never count toward it.
        let rows = |count: usize| {
            let mut rows: Vec<_> = (0..count)
                .map(|i| json!({"id": format!("m{i}"), "type": "moonraker", "config": {"base_url": "http://p.test"}}))
                .collect();
            rows.extend((0..500).map(|i| json!({"id": format!("s{i}"), "type": "spoolman"})));
            json!(rows).to_string()
        };
        let at_cap = project(Some(&rows(MAX_HOSTS))).unwrap();
        assert_eq!(at_cap.len(), MAX_HOSTS);
        assert!(
            at_cap
                .iter()
                .all(|h| h.enabled && h.base_url == "http://p.test")
        );
        let over = project(Some(&rows(MAX_HOSTS + 1))).unwrap();
        assert_eq!(over.len(), MAX_HOSTS + 1);
        assert!(over.iter().all(|h| *h == host(&h.id, "", None, false)));
        assert_eq!(
            project(Some(&rows(MAX_SNAPSHOT_IDS))).unwrap().len(),
            MAX_SNAPSHOT_IDS
        );
        assert_eq!(project(Some(&rows(MAX_SNAPSHOT_IDS + 1))), None);
    }

    #[test]
    fn ticket_t_21_integration_lookup_reads_like_node() {
        let items = json!([
            null,
            {"id": 7, "type": "moonraker"},
            {"id": "moon", "type": "moonraker",
             "config": {"baseUrl": "http://p.test", "apiKey": "k", "base_url": null}},
            {"id": "moon", "type": "prusalink"},
            {"id": "off", "type": "moonraker", "config": {"enabled": false}},
            {"id": "odd", "type": 3, "config": "text"}
        ]);
        let moon = find_integration(&items, "moon").unwrap();
        assert_eq!(
            moon,
            StoredIntegration {
                id: "moon".into(),
                kind: "moonraker".into(),
                base_url: "http://p.test".into(),
                api_key: Some("k".into()),
                username: String::new(),
                password: None,
                storage: None,
                enabled: true,
            },
            "the first matching row wins, and a null field falls back to its alias"
        );
        assert!(!find_integration(&items, "off").unwrap().enabled);
        let prusa = json!([{"id": "mk4", "type": "prusalink", "config": {
            "user": 42, "username": null, "password": "secret", "api_key": "key",
            "storage_path": "/usb/", "storage": 7}}]);
        let prusa = find_integration(&prusa, "mk4").unwrap();
        assert_eq!(
            (
                prusa.username.as_str(),
                prusa.password.as_deref(),
                prusa.storage
            ),
            ("42", Some("secret"), None),
            "a present non-string storage wins over storage_path, as with ??"
        );
        let odd = find_integration(&items, "odd").unwrap();
        assert_eq!((odd.kind.as_str(), odd.enabled), ("", true));
        assert_eq!(
            find_integration(&items, "7"),
            None,
            "Node matches string ids only"
        );
        assert_eq!(find_integration(&json!({"id": "moon"}), "moon"), None);
        assert_eq!(js_string(&json!(7)).as_deref(), Some("7"));
        assert_eq!(js_string(&Value::Null), None);
    }

    #[test]
    fn ticket_t_22_watched_hosts_add_prusalink_rows() {
        let big = "p".repeat(MAX_CONFIG_BYTES + 1);
        let stored = json!([
            {"id": "moon", "type": "moonraker", "config": {"base_url": "http://m.test"}},
            {"id": "mk4", "type": "prusalink", "config": {"base_url": "http://p.test", "username": "maker", "password": "secret"}},
            {"id": "mk4", "type": "prusalink", "config": {"base_url": "http://other.test"}},
            {"id": "off", "type": "prusalink", "config": {"baseUrl": "http://q.test", "user": 7, "api_key": "key", "enabled": false}},
            {"id": "big", "type": "prusalink", "config": {"base_url": "http://r.test", "password": big}},
            {"id": "", "type": "prusalink", "config": {"base_url": "http://s.test"}},
            {"id": 9, "type": "prusalink", "config": {"base_url": "http://t.test"}},
            {"id": "spool", "type": "spoolman", "config": {"base_url": "http://u.test"}},
        ])
        .to_string();
        let hosts = watched(Some(&stored)).unwrap();
        assert_eq!(hosts.moonraker, project(Some(&stored)).unwrap());
        let read: Vec<_> = hosts
            .prusalink
            .iter()
            .map(|host| {
                (
                    host.id.as_str(),
                    host.base_url.as_str(),
                    host.username.as_str(),
                    host.password.as_deref(),
                    host.enabled,
                )
            })
            .collect();
        assert_eq!(
            read,
            [
                ("mk4", "http://p.test", "maker", Some("secret")),
                ("off", "http://q.test", "7", Some("key")),
                ("big", "", "", Some(big.as_str())),
            ]
            .into_iter()
            .zip([true, false, true])
            .map(|((id, url, user, password), enabled)| (id, url, user, password, enabled))
            .collect::<Vec<_>>()
        );
        assert_eq!(
            polled_ids(Some(&stored)).unwrap(),
            ["moon", "mk4", "off"],
            "a row without a usable address is never polled"
        );
        // PrusaLink rows never take a Moonraker id or push the total past the cap.
        let capped = |moonraker: usize| {
            let mut rows: Vec<Value> = (0..moonraker)
                .map(|i| json!({"id": format!("m{i}"), "type": "moonraker", "config": {"base_url": "http://m.test"}}))
                .collect();
            rows.push(
                json!({"id": "m0", "type": "prusalink", "config": {"base_url": "http://p.test"}}),
            );
            rows.push(
                json!({"id": "mk4", "type": "prusalink", "config": {"base_url": "http://p.test"}}),
            );
            watched(Some(&Value::Array(rows).to_string())).map(|hosts| {
                hosts
                    .prusalink
                    .into_iter()
                    .map(|host| host.id)
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(capped(1).unwrap(), ["mk4"]);
        assert_eq!(capped(MAX_HOSTS - 1).unwrap(), ["mk4"]);
        assert_eq!(capped(MAX_HOSTS).unwrap(), Vec::<String>::new());
        // A malformed setting still fails closed as a whole.
        assert!(watched(Some("{")).is_none());
        assert_eq!(watched(None).unwrap().prusalink, []);
    }
}
