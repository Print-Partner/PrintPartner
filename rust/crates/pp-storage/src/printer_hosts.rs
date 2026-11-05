//! The Moonraker hosts in the stored `integrations` setting, projected the way
//! Node's private `/__desktop/moonraker-hosts` route does
//! (`web/apps/server/src/moonraker-host-snapshot.ts`). The Rust-only runtime
//! feeds this to the status watchers in place of that Node route. Read-only.
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

/// Read and project the stored setting. `Ok(None)` means the setting is
/// malformed and monitoring must fail closed; `Err` means storage was busy.
pub fn read(client: &SettingsClient, wait: Duration) -> Result<Option<Vec<MoonrakerHost>>> {
    let raw = match client.reader(wait)?.snapshot(TENANT, INTEGRATIONS_KEY)? {
        SettingSnapshot::Stored { value } => Some(value),
        SettingSnapshot::Missing => None,
    };
    Ok(project(raw.as_deref()))
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
}
