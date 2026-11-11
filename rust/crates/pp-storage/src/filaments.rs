//! Credentialed custom-filament mutations and catalog inputs on the writer.
use crate::{Envelope, Shared, WriterOwner, auth, read_model::Credential};
use anyhow::{Result, anyhow};
use pp_domain::{PositiveId, features::Feature};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    io::Write,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool, mpsc},
    time::Duration,
};

/// Node-compatible custom filament JSON format.
pub const FORMAT_VERSION: u32 = 1;
// ast-grep-ignore: no-writer-family-literal -- Version 1 is declared by generate-filament-format.py; Rust-only writer.
const STORE_FILE: &str = "custom_filaments.json";

#[derive(Clone)]
pub struct FilamentClient {
    shared: Arc<Shared>,
    policy: auth::AuthPolicy,
    directory: PathBuf,
}
pub enum Request {
    Add(Value),
    Delete(String),
    Catalog(std::collections::HashMap<String, String>),
}
pub enum Outcome {
    Added(Value),
    Deleted,
    Catalog {
        custom: String,
        default_id: Option<String>,
        integration: Option<Value>,
        allowed: bool,
    },
    Refused(u16, &'static str, &'static str),
}
pub(super) struct Command {
    policy: auth::AuthPolicy,
    directory: PathBuf,
    credential: Credential,
    request: Request,
}
pub(super) struct Reply(mpsc::Sender<Result<Outcome>>);
impl Reply {
    pub(super) fn send(self, result: Result<Outcome>) {
        let _ = self.0.send(result);
    }
}
impl WriterOwner {
    pub fn filaments_with_policy(&self, policy: auth::AuthPolicy) -> Result<FilamentClient> {
        auth::validate_policy(policy)?;
        Ok(FilamentClient {
            shared: self.client.shared.clone(),
            policy,
            directory: self
                .lease
                .as_ref()
                .ok_or_else(|| anyhow!("Storage owner is closed"))?
                .data_dir()
                .to_owned(),
        })
    }
}
impl FilamentClient {
    pub fn submit(
        &self,
        credential: Credential,
        request: Request,
        wait: Duration,
    ) -> Result<Outcome> {
        let (sender, receiver) = mpsc::channel();
        self.shared.admit(
            Envelope::Filaments {
                command: Command {
                    policy: self.policy,
                    directory: self.directory.clone(),
                    credential,
                    request,
                },
                reply: Reply(sender),
            },
            &AtomicBool::new(false),
            wait,
        )?;
        receiver.recv().map_err(|_| anyhow!("Storage stopped"))?
    }
}
pub(super) fn execute(
    connection: &mut Connection,
    command: Command,
    deadline: crate::writer_admission::Deadline,
) -> Result<Outcome> {
    let tx = crate::writer_admission::begin_immediate(connection, deadline)?;
    let tenant = crate::read_model::read_tenant(&tx, &command.credential, command.policy)?;
    if let Request::Catalog(query) = command.request {
        return catalog_inputs(&tx, &tenant, &command.directory, query);
    }
    let path = command.directory.join(STORE_FILE);
    let mut store = match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(store) => store,
            Err(_) => match serde_json::from_str::<Box<serde_json::value::RawValue>>(
                &String::from_utf8_lossy(&bytes),
            ) {
                Err(_) => json!({"filaments":[]}),
                Ok(_) => {
                    return Ok(Outcome::Refused(
                        400,
                        "Custom filament store cannot be safely updated",
                        "",
                    ));
                }
            },
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({"filaments":[]}),
        Err(error) => return Err(error.into()),
    };
    let entries = match store.get_mut("filaments").and_then(Value::as_array_mut) {
        Some(entries) => entries,
        None => return Ok(Outcome::Refused(400, "Invalid custom filament store", "")),
    };
    let outcome = match command.request {
        Request::Add(body) => {
            let name = js_string(&body["display_name"]).trim().to_owned();
            if name.is_empty() {
                return Ok(Outcome::Refused(400, "display_name is required", ""));
            }
            let hex = js_string(&body["hex"]);
            let hex = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
            if hex.len() != 6 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Ok(Outcome::Refused(400, "hex must be 6-digit RGB", ""));
            }
            let line = match &body["product_line"] {
                Value::Null => "",
                Value::String(s) => s.trim(),
                _ => {
                    return Ok(Outcome::Refused(
                        400,
                        "body.product_line?.trim is not a function",
                        "",
                    ));
                }
            };
            let entry = json!({"id":format!("custom-{}", hex::encode(rand::random::<[u8;4]>())), "display_name":name, "product_line":if line.is_empty() {"Custom"} else {line}, "hex":format!("#{hex}"), "combo_label":name});
            entries.push(entry.clone());
            Outcome::Added(entry)
        }
        Request::Delete(id) => {
            let before = entries.len();
            entries.retain(|entry| entry["id"].as_str() != Some(&id));
            if before == entries.len() {
                return Ok(Outcome::Refused(404, "Filament not found", ""));
            }
            store = json!({"filaments": entries});
            Outcome::Deleted
        }
        Request::Catalog(_) => unreachable!(),
    };
    publish(
        &command.directory,
        &serde_json::to_vec_pretty(&store)?,
        || Ok(()),
    )?;
    tx.commit()?;
    Ok(outcome)
}
fn catalog_inputs(
    tx: &rusqlite::Transaction<'_>,
    tenant: &str,
    directory: &std::path::Path,
    query: std::collections::HashMap<String, String>,
) -> Result<Outcome> {
    let plan = match query.get("profile_id") {
        None => None,
        Some(id) => match id
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0 && *n <= 9_007_199_254_740_991 && id == &n.to_string())
            .and_then(|n| PositiveId::new(n as u64).ok())
        {
            Some(id) => Some(id),
            None => {
                return Ok(Outcome::Refused(
                    400,
                    "profile_id must be a positive integer",
                    "INVALID_PROFILE_ID",
                ));
            }
        },
    };
    let view = match crate::features::resolve_for_tenant(tx, tenant, plan) {
        Ok(view) => view,
        Err(error) if error.to_string() == "Plan not found" => {
            return Ok(Outcome::Refused(
                404,
                "Profile not found",
                "PROFILE_NOT_FOUND",
            ));
        }
        Err(_) => {
            return Ok(Outcome::Refused(
                500,
                "Stored feature toggles are invalid.",
                "FEATURE_TOGGLES_INVALID",
            ));
        }
    };
    let allowed = view.effective().enabled(Feature::Spoolman);
    let setting = |key| -> Result<Option<String>> {
        Ok(match crate::snapshot(tx, tenant, key)? {
            crate::SettingSnapshot::Stored { value } if !value.is_empty() => Some(value),
            _ => None,
        })
    };
    let default_id = if allowed {
        setting("default_spoolman_integration_id")?
    } else {
        None
    };
    let selected = query
        .get("spoolman_integration_id")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| default_id.clone());
    let integration = if allowed {
        selected
            .map(|id| -> Result<Value> {
                let items: Value = setting("integrations")?
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or(Value::Null);
                Ok(items
                    .as_array()
                    .and_then(|items| items.iter().find(|item| item["id"].as_str() == Some(&id)))
                    .cloned()
                    .unwrap_or_else(|| json!({"id":id})))
            })
            .transpose()?
    } else {
        None
    };
    let custom = catalog_custom(directory)?;
    Ok(Outcome::Catalog {
        custom,
        default_id,
        integration,
        allowed,
    })
}
// Keep JSON strings as raw JSON: serde's String cannot hold lone UTF-16 surrogates.
fn catalog_custom(directory: &std::path::Path) -> Result<String> {
    use crate::source_scan::node_json::{Property, property};
    let crate::admin::CustomFilaments::Body(body) = crate::admin::custom_filaments(directory)
    else {
        return Err(anyhow!("Invalid custom filament store"));
    };
    let Property::Value(list) = property(&body, "filaments") else {
        return Err(anyhow!("Invalid custom filament store"));
    };
    let entries: Vec<Box<serde_json::value::RawValue>> = serde_json::from_str(&list)?;
    let mut colors = Vec::new();
    for entry in entries {
        let mut fields = Vec::new();
        for key in ["id", "display_name", "product_line", "hex", "combo_label"] {
            match property(entry.get(), key) {
                Property::Value(value) => fields.push(format!("\"{key}\":{value}")),
                Property::Missing => {}
                _ => return Err(anyhow!("Invalid custom filament entry")),
            }
        }
        fields.push("\"swatch_url\":\"\"".into());
        colors.push(format!("{{{}}}", fields.join(",")));
    }
    Ok(format!("[{}]", colors.join(",")))
}
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(items) => items.iter().map(js_string).collect::<Vec<_>>().join(","),
        _ => value.to_string(),
    }
}
fn publish(
    directory: &std::path::Path,
    bytes: &[u8],
    mid_write: impl FnOnce() -> std::io::Result<()>,
) -> Result<()> {
    let temporary = directory.join(format!(
        ".custom-filaments-{}",
        hex::encode(rand::random::<[u8; 8]>())
    ));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let half = bytes.len() / 2;
        file.write_all(&bytes[..half])?;
        mid_write()?;
        file.write_all(&bytes[half..])?;
        file.sync_all()?;
        pp_os::publish_rename(&temporary, &directory.join(STORE_FILE))?;
        pp_os::sync_directory(directory)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interrupted_publication_preserves_previous_file() {
        let directory = tempfile::tempdir().unwrap();
        let old = br#"{"filaments":[{"id":"keep"}]}"#;
        publish(directory.path(), old, || Ok(())).unwrap();
        assert!(
            publish(directory.path(), br#"{"filaments":[]}"#, || Err(
                std::io::Error::other("injected crash mid-write")
            ))
            .is_err()
        );
        assert_eq!(
            std::fs::read(directory.path().join(STORE_FILE)).unwrap(),
            old
        );
        publish(directory.path(), br#"{"filaments":[]}"#, || Ok(())).unwrap();
        assert_eq!(
            crate::admin::custom_filaments(directory.path()),
            crate::admin::CustomFilaments::Body("{\"filaments\":[]}".into())
        );
    }
}

#[cfg(test)]
mod crash_tests {
    use super::*;
    #[test]
    fn crash_child() {
        if let Some(directory) = std::env::var_os("PP_FILAMENT_CRASH_TEST") {
            publish(
                std::path::Path::new(&directory),
                b"{\"filaments\":[]}",
                || {
                    if std::env::var_os("PP_FILAMENT_CRASH").is_some() {
                        std::process::exit(73);
                    }
                    Ok(())
                },
            )
            .unwrap();
        }
    }
}
