//! Storage operations for MCP addon and Build brief proposals. Confirmation
//! rechecks the account and current data in the writer transaction.
use super::*;
use serde_json::{Value, json};
mod brief_update;
#[derive(Clone)]
pub struct Request {
    pub build: i64,
    pub tool: String,
    pub params: Value,
    pub apply: bool,
    pub action: Option<Value>,
    source_stamp: Option<Source>,
    source_allowed: bool,
}
impl Request {
    pub fn new(build: i64, tool: String, params: Value, action: Option<Value>) -> Self {
        Self {
            build,
            tool,
            params,
            apply: action.is_some(),
            action,
            source_stamp: None,
            source_allowed: false,
        }
    }
    fn failure(&self, detail: &str) -> Value {
        if self.apply {
            json!({"ok":false,"detail":detail})
        } else {
            json!({"error":detail})
        }
    }
}
fn setting(tx: &Transaction<'_>, account: &str, key: &str) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![account, key],
            |r| r.get(0),
        )
        .optional()?)
}
fn save(tx: &Transaction<'_>, account: &str, key: &str, value: &Value) -> Result<()> {
    tx.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,?2,?3) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value", params![account,key,value.to_string()])?;
    Ok(())
}
pub(crate) fn execute(
    tx: &Transaction<'_>,
    account: &str,
    request: &Request,
    repos: &std::path::Path,
    cancelled: &AtomicBool,
) -> Result<Value> {
    let Request {
        build,
        params,
        apply,
        ..
    } = request;
    let failure = |detail: &str| request.failure(detail);
    if request.tool == "propose_update_build_brief" {
        return brief(tx, account, request);
    }
    if !build_exists(tx, account, *build)? {
        return Ok(failure(if *apply {
            "Plan not found"
        } else {
            "plan_id and source_name required"
        }));
    }
    let needle =
        crate::read_model::views::js_trim(params["source_name"].as_str().unwrap_or_default());
    let sources = sources(tx, account)?;
    let found = find(&sources, needle);
    let Some(source) = found else {
        if *apply {
            return Ok(failure(&format!("Source not found: {needle}")));
        }
        return Ok(failure(&missing_source(&sources, needle)));
    };
    ensure!(
        request.source_stamp.as_ref() == Some(source),
        "Build inputs changed during MCP preparation"
    );
    let synced = request.source_allowed && source.3.as_ref().is_some_and(|s| !s.is_empty());
    if !synced {
        return Ok(if *apply {
            failure(&format!("Source is not synced: {needle}"))
        } else {
            json!({"error":format!("Source \"{}\" exists but is not synced. Ask the user to sync it before adding as an addon.",source.1),"synced":false})
        });
    }
    if !apply {
        return proposed_addon(tx, account, *build, &source.1, cancelled);
    }
    confirm_addon(tx, account, request, source, repos, cancelled)
}
impl BuildGraphClient {
    pub(super) fn execute_mcp(
        &self,
        credential: read_model::Credential,
        mut request: Request,
        cancelled: Arc<AtomicBool>,
        wait: Duration,
    ) -> Result<BuildOutcome> {
        if request.tool == "add_addon" {
            let captured = {
                let mut reader = self.settings.reader(wait)?;
                let tx = reader
                    .connection
                    .as_mut()
                    .ok_or_else(|| anyhow!("Reader checkout is closed"))?
                    .transaction()?;
                let account = credential_tenant(&tx, &credential, self.policy)?;
                let found = if build_exists(&tx, &account, request.build)? {
                    let rows = sources(&tx, &account)?;
                    find(
                        &rows,
                        crate::read_model::views::js_trim(
                            request.params["source_name"].as_str().unwrap_or_default(),
                        ),
                    )
                    .cloned()
                } else {
                    None
                };
                tx.commit()?;
                found
            };
            if let Some(source) = captured {
                let reads = crate::working_drafts::source_reads::SourceReads::new(
                    self.repos.parent().unwrap_or(&self.repos),
                )
                .with_filesystem_policy(
                    crate::working_drafts::source_reads::SourceFilesystemPolicy::for_auth(
                        &self.policy,
                    ),
                );
                request.source_allowed = reads.node_root(source.0, source.2.as_deref()).is_some();
                request.source_stamp = Some(source);
            }
        }
        self.execute_with_reply(
            credential,
            BuildCommand::Mcp(request),
            None,
            cancelled,
            wait,
            false,
        )
    }
    /// Local domain packs and kit catalog, read off the writer thread.
    pub fn mcp_addon_documents(&self) -> Value {
        crate::working_drafts::source_reads::mcp_documents(
            self.repos.parent().unwrap_or(&self.repos),
        )
    }
}
fn log(
    tx: &Transaction<'_>,
    account: &str,
    build: i64,
    action: Option<&Value>,
    result: &Value,
) -> Result<()> {
    let Some(action) = action else {
        return Ok(());
    };
    tx.execute("INSERT INTO plan_decisions(tenant_id,profile_id,created_at,actor,kind,action_type,params_json,label,summary,result_json) VALUES(?1,?2,?3,'assistant','applied_action',?4,?5,?6,?7,?8)",params![account,build,auth::catalog_timestamp(),action["type"].as_str(),action["params"].to_string(),action["label"].as_str().unwrap_or_default(),action["summary"].as_str().unwrap_or_default(),result.to_string()])?;
    Ok(())
}
fn brief(tx: &Transaction<'_>, account: &str, request: &Request) -> Result<Value> {
    let Request {
        build,
        params,
        apply,
        ..
    } = request;
    let failure = |detail: &str| request.failure(detail);
    // Node's proposal checks the account-scoped setting even when the
    // requested Build no longer exists. Confirm also checks Build ownership.
    if *apply && !build_exists(tx, account, *build)? {
        return Ok(failure("Plan not found"));
    }
    let key = format!("build_planning.v1.{build}");
    let Some(raw) = setting(tx, account, &key)? else {
        return Ok(failure("Build planning brief not found"));
    };
    if let Err(error) = crate::read_model::planning::BuildPlanningBrief::parse(&raw, *build) {
        return Ok(failure(&error.to_string()));
    }
    if !apply {
        return Ok(json!({}));
    }
    let mut brief: Value = serde_json::from_str(&raw)?;
    let result = match brief_update::update(&mut brief, params) {
        Ok(result) => result,
        Err(detail) => return Ok(failure(detail)),
    };
    brief["updated_at"] = json!(auth::catalog_timestamp());
    save(tx, account, &key, &brief)?;
    log(tx, account, *build, request.action.as_ref(), &result)?;
    Ok(json!({"ok":true,"result":result}))
}
fn confirm_addon(
    tx: &Transaction<'_>,
    account: &str,
    request: &Request,
    source: &Source,
    repos: &std::path::Path,
    cancelled: &AtomicBool,
) -> Result<Value> {
    let build = &request.build;
    let params = &request.params;
    let failure = |detail: &str| json!({"ok":false,"detail":detail});
    let build = PositiveId::new(*build as u64).map_err(|e| anyhow!(e))?;
    let outcome = mutation(
        tx,
        account,
        &BuildCommand::AttachAddon {
            build,
            source: PositiveId::new(source.0 as u64).map_err(|e| anyhow!(e))?,
        },
        repos,
        cancelled,
    )?;
    let BuildOutcome::Layers { layers, .. } = outcome else {
        return Ok(failure(&format!(
            "Source \"{}\" is already attached to this build",
            source.1
        )));
    };
    let mut result = json!({"layers":layers.into_iter().map(wire::layer).collect::<Vec<_>>()});
    let excludes = params["suggested_excludes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            if let Some(s) = v.as_str() {
                s.to_owned()
            } else {
                v.to_string()
            }
        })
        .map(|s| crate::read_model::views::js_trim(&s).to_owned())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    if !excludes.is_empty() {
        let key = format!("kit_manifest_{build}", build = build.get());
        let raw = setting(tx, account, &key)?.unwrap_or_default();
        let kit = crate::manifest_options::KitManifest::from_stored(&raw).unwrap_or_default();
        let mut bytes = Vec::new();
        kit.write_json(&mut bytes);
        let mut kit: Value = serde_json::from_slice(&bytes)?;
        let mut merged = kit["exclude"].as_array().cloned().unwrap_or_default();
        for s in excludes {
            let s = json!(s);
            if !merged.contains(&s) {
                merged.push(s);
            }
        }
        kit["exclude"] = json!(merged);
        save(tx, account, &key, &kit)?;
        result["exclude"] = json!(merged);
    }
    log(
        tx,
        account,
        build.get() as i64,
        request.action.as_ref(),
        &result,
    )?;
    Ok(json!({"ok":true,"result":result}))
}
type Source = (i64, String, Option<String>, Option<String>);
fn sources(tx: &Transaction<'_>, account: &str) -> Result<Vec<Source>> {
    let mut statement = tx.prepare("SELECT id,name,local_path,last_synced_at FROM projects WHERE tenant_id=?1 ORDER BY name,id")?;
    let sources = statement
        .query_map([account], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(sources)
}
fn compact(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| {
            (!c.is_whitespace() || *c == '\u{85}') && *c != '_' && *c != '-' && *c != '\u{feff}'
        })
        .collect()
}
fn find<'a>(sources: &'a [Source], needle: &str) -> Option<&'a Source> {
    let norm = |s: &str| {
        s.to_lowercase()
            .split(|c: char| c.is_whitespace() || c == '_' || c == '-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    };
    sources
        .iter()
        .find(|s| s.1 == needle)
        .or_else(|| {
            sources
                .iter()
                .find(|s| s.1.to_lowercase() == needle.to_lowercase())
        })
        .or_else(|| sources.iter().find(|s| compact(&s.1) == compact(needle)))
        .or_else(|| {
            sources
                .iter()
                .filter(|s| {
                    norm(&s.1).contains(&norm(needle)) || norm(needle).contains(&norm(&s.1))
                })
                .reduce(|best, next| {
                    if next.1.encode_utf16().count() > best.1.encode_utf16().count() {
                        next
                    } else {
                        best
                    }
                })
        })
}
fn missing_source(sources: &[Source], needle: &str) -> String {
    let grams = |s: &str| {
        let units = compact(s).encode_utf16().collect::<Vec<_>>();
        units
            .windows(2)
            .map(|w| w.to_vec())
            .collect::<std::collections::HashSet<_>>()
    };
    let target = grams(needle);
    let mut suggestions = sources
        .iter()
        .map(|s| {
            let g = grams(&s.1);
            let score =
                2.0 * target.intersection(&g).count() as f64 / (target.len() + g.len()) as f64;
            (&s.1, score)
        })
        .filter(|(_, score)| *score >= 0.3)
        .collect::<Vec<_>>();
    suggestions.sort_by(|a, b| b.1.total_cmp(&a.1));
    let hint = if suggestions.is_empty() {
        String::new()
    } else {
        format!(
            " Did you mean: {}?",
            suggestions
                .iter()
                .take(5)
                .map(|(s, _)| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "Source not found: \"{needle}\".{hint} Call list_sources and use an existing name — do not invent sources."
    )
}
fn proposed_addon(
    tx: &Transaction<'_>,
    account: &str,
    build: i64,
    name: &str,
    cancelled: &AtomicBool,
) -> Result<Value> {
    let mut stmt=tx.prepare("SELECT action_type,params_json FROM plan_decisions WHERE tenant_id=?1 AND profile_id=?2 AND kind='dismissed_action' AND id IN (SELECT id FROM plan_decisions WHERE tenant_id=?1 AND profile_id=?2 ORDER BY created_at DESC,id DESC LIMIT 80)")?;
    let rows = stmt
        .query_map(params![account, build], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let dismissed = rows.iter().any(|(kind, raw)| {
        let p: Value = serde_json::from_str(raw).unwrap_or_default();
        kind.as_deref() == Some("add_addon")
            && p["source_name"] == name
            && [
                "preset_id",
                "tag",
                "branch",
                "base_id",
                "layer_id",
                "workflow",
            ]
            .iter()
            .all(|k| p.get(*k).is_none_or(|v| v.is_null() || v == ""))
            && p["selections"].as_object().is_none_or(|v| v.is_empty())
    });
    Ok(
        json!({"source_name":name,"layers":layers(tx,account,build,cancelled)?.into_iter().map(wire::layer).collect::<Vec<_>>(),"dismissed":dismissed}),
    )
}
