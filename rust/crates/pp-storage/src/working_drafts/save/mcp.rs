//! MCP adapters use the existing draft Part edit transaction and snapshot format.
use super::super::*;
use std::collections::BTreeMap;

pub(crate) fn read(
    tx: &Transaction<'_>,
    account: &str,
    build: i64,
    id: i64,
) -> Result<Option<Value>> {
    draft(tx, account, build, id)?
        .map(|draft| body(&draft))
        .transpose()
}

fn body(draft: &Draft) -> Result<Value> {
    Ok(serde_json::from_slice(
        &snapshot(draft)?.into_json_body().into_bytes(),
    )?)
}

pub(crate) fn edit_parts(
    tx: &Transaction<'_>,
    credential: &Credential,
    policy: auth::AuthPolicy,
    build: PositiveId,
    account: &str,
    params: &Value,
) -> Result<Value> {
    let refuse = |detail: String| json!({"ok":false,"detail":detail});
    let Some(id) = params["draft_id"].as_i64() else {
        return Ok(refuse("draft_id and parts required".into()));
    };
    let Some(parts) = params["parts"].as_array() else {
        return Ok(refuse("draft_id and parts required".into()));
    };
    let Some(before) = draft(tx, account, build.get() as i64, id)? else {
        return Ok(refuse("Plan draft not found".into()));
    };
    let mut edits = BTreeMap::<i64, Map<String, Value>>::new();
    for part in parts {
        let Some(part_id) = part["part_id"]
            .as_i64()
            .filter(|id| before.parts.iter().any(|part| part["id"] == *id))
        else {
            return Ok(refuse(format!("Draft part not found: {}", part["part_id"])));
        };
        for (key, accepted) in [
            ("included", part["included"].is_boolean()),
            ("quantity_override", part.get("quantity_override").is_some()),
        ] {
            if accepted {
                edits
                    .entry(part_id)
                    .or_default()
                    .insert(key.into(), part[key].clone());
            }
        }
    }
    let mut groups = BTreeMap::<String, Value>::new();
    for (id, fields) in edits {
        for (field, value) in fields {
            let kind = if field == "included" {
                "set_included"
            } else {
                "set_quantity_override"
            };
            let decision = groups
                .entry(format!("{kind}:{value}"))
                .or_insert_with(|| json!({"kind":kind,"draft_part_ids":[],"value":value}));
            if let Some(ids) = decision["draft_part_ids"].as_array_mut() {
                ids.push(json!(id));
            }
        }
    }
    let expected = params
        .get("expected_snapshot_digest")
        .filter(|v| !v.is_null())
        .unwrap_or(&before.header["snapshotDigest"]);
    let expected =
        crate::read_model::views::js_trim(expected.as_str().unwrap_or_default()).to_lowercase();
    if Digest::new(expected.clone()).is_err() {
        return Ok(
            json!({"error":"Expected Plan draft snapshot digest must be a SHA-256 hex digest"}),
        );
    }
    let request: EditRequest = serde_json::from_value(json!({
        "expected_snapshot_digest":expected,
        "decisions":groups.into_values().collect::<Vec<_>>()
    }))?;
    let result = edit(
        tx,
        credential,
        policy,
        build,
        PositiveId::new(id as u64).map_err(anyhow::Error::msg)?,
        &request,
    )?;
    let kind = match result {
        Edited::Unselected(_) | Edited::Done(WorkspaceOutcome::Ready { .. }) => None,
        Edited::Done(WorkspaceOutcome::DraftChanged { .. }) => Some("conflict"),
        Edited::Done(WorkspaceOutcome::BaseChanged { .. }) => Some("base_changed"),
        Edited::Done(WorkspaceOutcome::NotOpen { .. }) => Some("not_open"),
        Edited::Done(WorkspaceOutcome::AcceptedBaselineRequired) => {
            Some("accepted_baseline_required")
        }
        _ => Some("not_found"),
    };
    if let Some(kind) = kind {
        let mut result = json!({});
        if ["conflict", "base_changed"].contains(&kind) {
            result["draft"] = body(&before)?;
        }
        return Ok(
            json!({"ok":false,"detail":format!("Plan draft edit failed: {kind}"),"result":result}),
        );
    }
    let after = draft(tx, account, build.get() as i64, id)?
        .ok_or_else(|| anyhow!("Edited draft is missing"))?;
    Ok(json!({"ok":true,"result":{
        "edit_result":if after.header["snapshotDigest"] == before.header["snapshotDigest"] {"unchanged"} else {"updated"},
        "draft":body(&after)?
    }}))
}

mod filaments;
mod rebuild;
use crate::build_graph::mcp::Request as McpRequest;
use crate::read_model::planning::BuildPlanningBrief;
use rusqlite::OptionalExtension;
use sha2::{Digest as _, Sha256};

pub(crate) fn handles(tool: &str) -> bool {
    matches!(
        tool,
        "propose_edit_plan_draft_parts" | "propose_rebuild_plan" | "propose_assign_role_filament"
    )
}
fn failure(apply: bool, detail: &str) -> Value {
    if apply {
        json!({"ok":false,"detail":detail})
    } else {
        json!({"error":detail})
    }
}
fn setting(tx: &Transaction<'_>, account: &str, key: &str) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![account, key],
            |row| row.get(0),
        )
        .optional()?)
}
fn spoolman_access(tx: &Transaction<'_>, account: &str, build: i64, apply: bool) -> Option<Value> {
    let view = PositiveId::new(build as u64)
        .map_err(anyhow::Error::msg)
        .and_then(|id| crate::features::resolve_for_tenant(tx, account, Some(id)));
    let (status, title, detail, code) = match view {
        Ok(view)
            if view
                .effective()
                .enabled(pp_domain::features::Feature::Spoolman) =>
        {
            return None;
        }
        Ok(_) => (
            403,
            "Forbidden",
            "Feature spoolman is disabled.",
            "FEATURE_DISABLED",
        ),
        Err(error) if error.to_string() == "Plan not found" || build <= 0 => {
            (404, "Not Found", "Profile not found", "PROFILE_NOT_FOUND")
        }
        Err(_) => (
            500,
            "Internal Server Error",
            "Stored feature toggles are invalid.",
            "FEATURE_TOGGLES_INVALID",
        ),
    };
    Some(if apply {
        json!({"ok":false,"status":status,"title":title,"detail":detail,"code":code})
    } else {
        json!({"error":detail,"code":code})
    })
}
fn integration(tx: &Transaction<'_>, account: &str, params: &Value) -> Result<Option<Value>> {
    let Some((id, _)) = inventory_id(params) else {
        return Ok(None);
    };
    let integrations: Value = setting(tx, account, "integrations")?
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null);
    Ok(integrations
        .as_array()
        .and_then(|items| items.iter().find(|item| item["id"] == id))
        .cloned())
}
fn inventory_id(params: &Value) -> Option<(&str, i64)> {
    let raw = crate::read_model::views::js_trim(params["assignment"]["inventory_id"].as_str()?)
        .strip_prefix("spoolman:")?;
    let (integration, id) = raw.rsplit_once(":filament:")?;
    if integration.contains(':') || id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id = id
        .parse::<i64>()
        .ok()
        .filter(|id| *id >= 0 && *id <= 9_007_199_254_740_991)?;
    (!integration.is_empty()).then_some((integration, id))
}
fn guard(
    tx: &Transaction<'_>,
    account: &str,
    build: i64,
    raw: &str,
    connector: &Option<Value>,
) -> Result<Value> {
    let layers = rows(
        tx,
        "SELECT * FROM profile_layers WHERE tenant_id=? AND profile_id=? ORDER BY layer_order,id",
        &[&account, &build],
    )?;
    let brief: Value = serde_json::from_str(raw)?;
    let ids: HashSet<i64> = brief["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["source_id"].as_i64())
        .chain(layers.iter().filter_map(|row| row["projectId"].as_i64()))
        .collect();
    let sources: Vec<_> = rows(tx,"SELECT id,name,local_path,last_synced_at,last_commit_sha FROM projects WHERE tenant_id=? ORDER BY id", &[&account])?.into_iter().filter(|row| row["id"].as_i64().is_some_and(|id| ids.contains(&id))).collect();
    let accepted = one(
        tx,
        "SELECT accepted_plan_revision_id,accepted_plan_version FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&account, &build],
    )?;
    let drafts = rows(
        tx,
        "SELECT id,state,lifecycle_version,snapshot_digest FROM plan_drafts WHERE tenant_id=? AND profile_id=? AND state='open' ORDER BY id",
        &[&account, &build],
    )?;
    let defaults = setting(tx, account, &format!("role_filaments_{build}"))?;
    Ok(json!(hex::encode(Sha256::digest(
        json!([raw, accepted, layers, sources, connector, drafts, defaults])
            .to_string()
            .as_bytes()
    ))))
}

pub(crate) fn execute(
    tx: &Transaction<'_>,
    account: &str,
    request: &McpRequest,
    repos: &std::path::Path,
    flag: &AtomicBool,
) -> Result<Value> {
    let build = request.build;
    let params = &request.params;
    let apply = request.apply;
    let role = request.tool == "propose_assign_role_filament";
    let mut spoolman = role && params["assignment"]["inventory_kind"] == "spoolman";
    if !apply
        && spoolman
        && let Some(denied) = spoolman_access(tx, account, build, false)
    {
        return Ok(denied);
    }
    let raw = setting(tx, account, &format!("build_planning.v1.{build}"))?;
    let brief = raw
        .as_deref()
        .and_then(|raw| BuildPlanningBrief::parse(raw, build).ok());
    if request.tool == "propose_edit_plan_draft_parts" {
        if !apply && brief.is_none() {
            return Ok(failure(false, "Build planning brief not found"));
        }
        let Some(id) = params["draft_id"].as_i64() else {
            return Ok(failure(apply, "plan_id and draft_id required"));
        };
        if !apply {
            return Ok(read(tx, account, build, id)?.map_or_else(
                || failure(false, "Plan draft not found"),
                |draft| json!({"draft":draft}),
            ));
        }
        let (credential, policy) = request
            .authority
            .as_ref()
            .ok_or_else(|| anyhow!("Missing MCP authority"))?;
        return edit_parts(
            tx,
            credential,
            *policy,
            PositiveId::new(build as u64).map_err(anyhow::Error::msg)?,
            account,
            params,
        );
    }
    let (Some(raw), Some(brief)) = (raw, brief) else {
        return Ok(failure(
            apply,
            if !apply {
                "Build planning brief not found"
            } else if role {
                "brief and assignment required"
            } else {
                "Planning brief not found"
            },
        ));
    };
    if apply && role {
        match filaments::base(&params["assignment"]) {
            Ok(assignment) => spoolman = assignment["inventory_kind"] == "spoolman",
            Err(detail) => return Ok(failure(true, &detail)),
        }
    }
    if apply
        && spoolman
        && let Some(denied) = spoolman_access(tx, account, build, true)
    {
        return Ok(denied);
    }
    let connector = if role {
        integration(tx, account, params)?
    } else {
        None
    };
    let basis = guard(tx, account, build, &raw, &connector)?;
    if !apply {
        return Ok(json!({"guard":basis}));
    }
    if request.confirmation_guard.as_ref() != Some(&basis) {
        return Ok(failure(
            true,
            "Build planning changed since the proposal; propose again",
        ));
    }
    if request.verify_only && !spoolman {
        return Ok(json!({}));
    }
    if role {
        return assign_role(tx, account, request, &raw, &connector);
    }
    let (_, policy) = request
        .authority
        .as_ref()
        .ok_or_else(|| anyhow!("Missing MCP authority"))?;
    rebuild::run(rebuild::Args {
        tx,
        account,
        build,
        raw: &raw,
        brief,
        request,
        repos,
        flag,
        policy: *policy,
    })
}

fn assign_role(
    tx: &Transaction<'_>,
    account: &str,
    request: &McpRequest,
    raw: &str,
    connector: &Option<Value>,
) -> Result<Value> {
    let params = &request.params;
    let build = request.build;
    let id = params["assignment"]["inventory_id"]
        .as_str()
        .map(crate::read_model::views::js_trim)
        .unwrap_or_default();
    let catalog = filaments::catalog(id);
    if request.verify_only {
        let usable = connector
            .as_ref()
            .filter(|item| item["type"] == "spoolman" && item["config"]["enabled"] != false);
        if catalog.is_none() && usable.is_none() {
            return Ok(failure(
                true,
                "Spoolman inventory filament could not be verified",
            ));
        }
        return Ok(
            json!({"integration":usable,"filament_id":inventory_id(params).map(|(_,id)| id),"catalog":catalog}),
        );
    }
    let proof = if params["assignment"]["inventory_kind"] == "catalog" {
        catalog.as_ref()
    } else {
        request.inventory_proof.as_ref().or(catalog.as_ref())
    };
    let assignment = match filaments::normalize(&params["assignment"], proof) {
        Ok(assignment) => assignment,
        Err(detail) => return Ok(failure(true, &detail)),
    };
    let mut brief: Value = serde_json::from_str(&raw)?;
    filaments::assign(tx, account, build, &mut brief, assignment)
}
