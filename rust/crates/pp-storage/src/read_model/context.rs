use super::{
    AcceptedRead,
    graph::{Budget, Row, check, one, owned, required, rows, sha},
    views,
    workflow::{
        self, Accepted, Build, Checkoff, Facts, PlateState, Production, Sources, Stock, Working,
    },
};
use anyhow::Result;
use rusqlite::Transaction;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

#[cfg(feature = "desktop_rebuild")]
#[derive(Debug, Clone)]
pub(crate) struct BuildSummaryFacts {
    pub id: i64,
    pub name: String,
    pub order_number: Option<String>,
    pub special_request: Option<String>,
    pub part_count: i64,
    pub accepted_progress: AcceptedProgressFacts,
    pub freshness: FreshnessFacts,
    pub archived_at: Option<String>,
    pub last_used_at: Option<String>,
}

#[cfg(feature = "desktop_rebuild")]
#[derive(Debug, Clone)]
pub(crate) enum AcceptedProgressFacts {
    Ready {
        total_units: i64,
        remaining_units: i64,
    },
    Empty,
    CompatibilityDirty,
    Uninitialized,
    Integrity,
}

#[derive(Debug, Clone)]
pub(crate) enum FreshnessFacts {
    #[cfg(feature = "desktop_rebuild")]
    Unavailable { code: String },
    Current {
        accepted_input_set_id: i64,
        accepted_at: String,
    },
    Stale {
        accepted_input_set_id: i64,
        accepted_at: String,
        reasons: Vec<StaleReasonFacts>,
        untracked_sources: Vec<UntrackedReasonFacts>,
    },
    Untracked {
        accepted_input_set_id: Option<i64>,
        accepted_at: Option<String>,
        reasons: Vec<UntrackedReasonFacts>,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum StaleReasonFacts {
    SourceRevisionChanged {
        source_id: i64,
        source_name: String,
        accepted_revision_id: i64,
        current_revision_id: i64,
    },
    SourceRevisionUnavailable {
        source_id: i64,
        source_name: String,
        accepted_revision_id: i64,
    },
    NamingRulesChanged {
        source_id: i64,
        source_name: String,
        accepted_digest: String,
        current_digest: String,
    },
    PlanInputsInvalid,
    PlanConfigurationChanged,
}

#[derive(Debug, Clone)]
pub(crate) enum UntrackedReasonFacts {
    NoAcceptedInputs,
    SourceRevisionUntracked { source_id: i64, source_name: String },
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedContext {
    pub profile_summary: Value,
    pub profile_summary_v1: Value,
    pub profile_summary_v2: Value,
    pub workflow: Value,
    pub history: Vec<RevisionRef>,
    pub required_units: Vec<UnitHistory>,
    pub plate_revisions: Vec<PlateRef>,
    /// The facts `workflow` was resolved from, for the workflow read.
    #[cfg(feature = "desktop_rebuild")]
    #[serde(skip)]
    pub(crate) facts: Facts,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionRef {
    pub id: i64,
    pub parent_revision_id: Option<i64>,
    pub revision_number: i64,
    pub snapshot_digest: String,
    pub accepted_at: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitHistory {
    pub token: String,
    pub object_name: String,
    pub created_in_revision_id: i64,
    pub member_revision_ids: Vec<i64>,
    pub retired: bool,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlateRef {
    pub id: i64,
    pub plan_revision_id: i64,
    pub plan_version: i64,
    pub layout_digest: String,
    pub current: bool,
}
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct Role {
    id: String,
    label: String,
    markers: Vec<String>,
}
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct Quantity {
    regex: String,
    default: i64,
}
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct Slug {
    strip_markers: bool,
    strip_quantity: bool,
}
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct Folder {
    path_contains: String,
    role_id: String,
    functional_class: Option<String>,
}
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
struct Naming {
    roles: Vec<Role>,
    quantity: Quantity,
    slug: Slug,
    folder_rules: Vec<Folder>,
    export_role_order: Vec<String>,
}
fn default_naming() -> Value {
    json!({"roles":[{"id":"primary","label":"Primary","markers":[]},{"id":"accent","label":"Accent","markers":["[a]"]},{"id":"clear","label":"Clear","markers":["[c]"]},{"id":"opaque","label":"Opaque","markers":["[o]"]}],"quantity":{"regex":"[ _]x([0-9]+)\\.stl$","default":1},"slug":{"strip_markers":true,"strip_quantity":true},"folder_rules":[],"export_role_order":["primary","accent","clear","opaque"]})
}
fn trim(s: &str) -> &str {
    s.trim_matches(|c:char|matches!(c,'\u{0009}'..='\u{000d}'|' '| '\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))
}
fn naming(value: Value) -> Result<Naming> {
    let mut n: Naming = serde_json::from_value(value)?;
    let mut ids = HashSet::new();
    let allowed = ["primary", "accent", "clear", "opaque"];
    for r in &mut n.roles {
        r.label = trim(&r.label).into();
        check(
            allowed.contains(&r.id.as_str()) && ids.insert(r.id.clone()) && !r.label.is_empty(),
            "accepted_inputs",
            "Invalid stored naming roles",
        )?;
    }
    check(
        ids.contains("primary")
            && n.export_role_order.len() == 4
            && n.export_role_order.iter().collect::<HashSet<_>>().len() == 4
            && n.export_role_order
                .iter()
                .all(|r| allowed.contains(&r.as_str())),
        "accepted_inputs",
        "Invalid stored naming role order",
    )?;
    n.quantity.regex = trim(&n.quantity.regex).into();
    check(
        !n.quantity.regex.is_empty() && n.quantity.default > 0,
        "accepted_inputs",
        "Invalid stored naming quantity",
    )?;
    for f in &mut n.folder_rules {
        f.path_contains = trim(&f.path_contains).into();
        check(
            !f.path_contains.is_empty()
                && allowed.contains(&f.role_id.as_str())
                && f.functional_class
                    .as_deref()
                    .is_none_or(|v| v == "functional" || v == "cosmetic"),
            "accepted_inputs",
            "Invalid stored naming folder rule",
        )?;
    }
    Ok(n)
}
fn naming_digest(global: &Naming, metadata: Option<&str>) -> Result<String> {
    let metadata: Value = metadata
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let mut value = serde_json::to_value(global)?;
    if metadata["naming"]["use_defaults"] == false {
        let over = metadata["naming"]["override"].clone();
        // A complete valid profile replaces the global rules. Sparse overrides
        // still merge, matching catalog get() and Node resolveNamingProfile.
        if naming(over.clone()).is_ok() {
            value = over;
        } else {
            if let Some(roles) = over["roles"].as_array() {
                let target = value["roles"]
                    .as_array_mut()
                    .ok_or_else(|| anyhow::Error::msg("Naming roles are missing"))?;
                for r in roles {
                    if let Some(existing) = target.iter_mut().find(|e| e["id"] == r["id"]) {
                        for k in ["label", "markers"] {
                            if !r[k].is_null() {
                                existing[k] = r[k].clone();
                            }
                        }
                    } else {
                        target.push(json!({"id":r["id"],"label":r.get("label").unwrap_or(&r["id"]),"markers":r.get("markers").cloned().unwrap_or(json!([]))}));
                    }
                }
            }
            for k in ["quantity", "slug"] {
                if let Some(fields) = over[k].as_object() {
                    for (name, v) in fields {
                        value[k][name] = v.clone();
                    }
                }
            }
            for k in ["folder_rules", "export_role_order"] {
                if !over[k].is_null() {
                    value[k] = over[k].clone();
                }
            }
        }
    }
    let n = naming(value)?;
    #[derive(Serialize)]
    struct DigestNaming<'a> {
        version: u8,
        roles: &'a [Role],
        quantity: &'a Quantity,
        slug: &'a Slug,
        folder_rules: &'a [Folder],
        export_role_order: &'a [String],
    }
    Ok(sha(&serde_json::to_string(&DigestNaming {
        version: 1,
        roles: &n.roles,
        quantity: &n.quantity,
        slug: &n.slug,
        folder_rules: &n.folder_rules,
        export_role_order: &n.export_role_order,
    })?))
}
fn setting(
    tx: &Transaction<'_>,
    tenant: &str,
    key: &str,
    b: &mut Budget,
) -> Result<Option<String>> {
    one(
        tx,
        "app_settings",
        "tenant_id=?1 AND key=?2",
        &[&tenant, &key],
        "pointer",
        b,
    )?
    .map(|r| r.os("value"))
    .transpose()
    .map(Option::flatten)
}
fn stale_reason_json(reason: &StaleReasonFacts) -> Value {
    match reason {
        StaleReasonFacts::SourceRevisionChanged {
            source_id,
            source_name,
            accepted_revision_id,
            current_revision_id,
        } => {
            json!({"kind":"source_revision_changed","source_id":source_id,"source_name":source_name,"accepted_revision_id":accepted_revision_id,"current_revision_id":current_revision_id})
        }
        StaleReasonFacts::SourceRevisionUnavailable {
            source_id,
            source_name,
            accepted_revision_id,
        } => {
            json!({"kind":"source_revision_unavailable","source_id":source_id,"source_name":source_name,"accepted_revision_id":accepted_revision_id})
        }
        StaleReasonFacts::NamingRulesChanged {
            source_id,
            source_name,
            accepted_digest,
            current_digest,
        } => {
            json!({"kind":"naming_rules_changed","source_id":source_id,"source_name":source_name,"accepted_digest":accepted_digest,"current_digest":current_digest})
        }
        StaleReasonFacts::PlanInputsInvalid => json!({"kind":"plan_inputs_invalid"}),
        StaleReasonFacts::PlanConfigurationChanged => {
            json!({"kind":"plan_configuration_changed"})
        }
    }
}

fn untracked_reason_json(reason: &UntrackedReasonFacts) -> Value {
    match reason {
        UntrackedReasonFacts::NoAcceptedInputs => json!({"kind":"no_accepted_inputs"}),
        UntrackedReasonFacts::SourceRevisionUntracked {
            source_id,
            source_name,
        } => {
            json!({"kind":"source_revision_untracked","source_id":source_id,"source_name":source_name})
        }
    }
}

fn freshness_json(freshness: &FreshnessFacts) -> Value {
    match freshness {
        #[cfg(feature = "desktop_rebuild")]
        FreshnessFacts::Unavailable { code } => json!({"status":"unavailable","code":code}),
        FreshnessFacts::Current {
            accepted_input_set_id,
            accepted_at,
        } => {
            json!({"status":"current","accepted_input_set_id":accepted_input_set_id,"accepted_at":accepted_at})
        }
        FreshnessFacts::Stale {
            accepted_input_set_id,
            accepted_at,
            reasons,
            untracked_sources,
        } => json!({
            "status":"stale",
            "accepted_input_set_id":accepted_input_set_id,
            "accepted_at":accepted_at,
            "reasons":reasons.iter().map(stale_reason_json).collect::<Vec<_>>(),
            "untracked_sources":untracked_sources.iter().map(untracked_reason_json).collect::<Vec<_>>()
        }),
        FreshnessFacts::Untracked {
            accepted_input_set_id,
            accepted_at,
            reasons,
        } => json!({
            "status":"untracked",
            "accepted_input_set_id":accepted_input_set_id,
            "accepted_at":accepted_at,
            "reasons":reasons.iter().map(untracked_reason_json).collect::<Vec<_>>()
        }),
    }
}

fn freshness(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    p: &Row,
    b: &mut Budget,
) -> Result<(FreshnessFacts, u64)> {
    let mut layers = rows(
        tx,
        "profile_layers",
        "profile_id=?1 AND tenant_id=?2",
        &[&id, &tenant],
        "accepted_inputs",
        b,
    )?;
    layers.sort_by_key(|r| r.n("layer_order").unwrap_or(0));
    let attached = layers
        .iter()
        .filter(|r| r.v("project_id").as_i64().is_some_and(|n| n != 0))
        .count() as u64;
    let Some(a) = one(
        tx,
        "plan_accepted_input_sets",
        "profile_id=?1 AND tenant_id=?2",
        &[&id, &tenant],
        "accepted_inputs",
        b,
    )?
    else {
        return Ok((
            FreshnessFacts::Untracked {
                accepted_input_set_id: None,
                accepted_at: None,
                reasons: vec![UntrackedReasonFacts::NoAcceptedInputs],
            },
            attached,
        ));
    };
    let sid = a.n("input_set_id")?;
    let s = required(
        one(
            tx,
            "plan_revision_input_sets",
            "id=?1 AND tenant_id=?2",
            &[&sid, &tenant],
            "accepted_inputs",
            b,
        )?,
        "accepted_inputs",
    )?;
    if s.n("format_version")? != 2 {
        return Ok((
            FreshnessFacts::Untracked {
                accepted_input_set_id: Some(sid),
                accepted_at: Some(a.s("accepted_at")?.into()),
                reasons: vec![UntrackedReasonFacts::NoAcceptedInputs],
            },
            attached,
        ));
    }
    let global = match setting(tx, tenant, "stl_naming_defaults", b)?
        .and_then(|s| serde_json::from_str(&s).ok())
        .and_then(|v| naming(v).ok())
    {
        Some(naming) => naming,
        None => naming(default_naming())?,
    };
    let mut current = HashMap::new();
    let mut invalid = false;
    for l in layers {
        let Some(source) = l.on("project_id")? else {
            continue;
        };
        if source == 0 {
            continue;
        }
        if current.contains_key(&source) {
            invalid = true;
            break;
        }
        let Some(row) = one(
            tx,
            "projects",
            "id=?1 AND tenant_id=?2",
            &[&source, &tenant],
            "source_revision",
            b,
        )?
        else {
            invalid = true;
            break;
        };
        if let Some(rid) = row.on("current_source_revision_id")? {
            let sr = one(
                tx,
                "source_revisions",
                "id=?1 AND tenant_id=?2",
                &[&rid, &tenant],
                "source_revision",
                b,
            )?;
            if sr
                .as_ref()
                .is_none_or(|r| r.n("project_id").ok() != Some(source))
            {
                invalid = true;
                break;
            }
        }
        let digest = naming_digest(&global, row.os("metadata_json")?.as_deref())?;
        current.insert(source, (row, digest));
    }
    if invalid {
        current.clear();
    }
    let inputs = rows(
        tx,
        "plan_revision_inputs",
        "input_set_id=?1 AND tenant_id=?2",
        &[&sid, &tenant],
        "accepted_inputs",
        b,
    )?;
    let mut stale = Vec::new();
    let mut untracked = Vec::new();
    if invalid {
        stale.push(StaleReasonFacts::PlanInputsInvalid);
    }
    let config = p.os("config_modified_at")?;
    let recomputed = p.os("last_recomputed_at")?;
    if recomputed.is_none()
        || config
            .as_ref()
            .zip(recomputed.as_ref())
            .is_some_and(|(c, r)| c > r)
        || inputs.len() != current.len()
        || inputs
            .iter()
            .any(|i| !current.contains_key(&i.n("source_id").unwrap_or(0)))
    {
        stale.push(StaleReasonFacts::PlanConfigurationChanged);
    }
    for i in inputs {
        let source = i.n("source_id")?;
        let Some((now, digest)) = current.get(&source) else {
            continue;
        };
        let name = now.s("name")?;
        let accepted_revision = i.on("source_revision_id")?;
        let current_revision = now.on("current_source_revision_id")?;
        if i.s("tracking_kind")? == "untracked" || accepted_revision.is_none() {
            untracked.push(UntrackedReasonFacts::SourceRevisionUntracked {
                source_id: source,
                source_name: name.into(),
            });
        } else if let (Some(accepted_revision_id), None) = (accepted_revision, current_revision) {
            stale.push(StaleReasonFacts::SourceRevisionUnavailable {
                source_id: source,
                source_name: name.into(),
                accepted_revision_id,
            });
        } else if let (Some(accepted_revision_id), Some(current_revision_id)) =
            (accepted_revision, current_revision)
            && current_revision_id != accepted_revision_id
        {
            stale.push(StaleReasonFacts::SourceRevisionChanged {
                source_id: source,
                source_name: name.into(),
                accepted_revision_id,
                current_revision_id,
            });
        }
        if i.s("effective_naming_digest")? != digest {
            stale.push(StaleReasonFacts::NamingRulesChanged {
                source_id: source,
                source_name: name.into(),
                accepted_digest: i.s("effective_naming_digest")?.into(),
                current_digest: digest.clone(),
            });
        }
    }
    let accepted_at = a.s("accepted_at")?.into();
    let value = if !stale.is_empty() {
        FreshnessFacts::Stale {
            accepted_input_set_id: sid,
            accepted_at,
            reasons: stale,
            untracked_sources: untracked,
        }
    } else if !untracked.is_empty() {
        FreshnessFacts::Untracked {
            accepted_input_set_id: Some(sid),
            accepted_at: Some(accepted_at),
            reasons: untracked,
        }
    } else {
        FreshnessFacts::Current {
            accepted_input_set_id: sid,
            accepted_at,
        }
    };
    Ok((value, attached))
}

#[cfg(feature = "desktop_rebuild")]
fn accepted_progress_facts(accepted: &AcceptedRead) -> AcceptedProgressFacts {
    match accepted {
        AcceptedRead::Ready { snapshot } => {
            let units = snapshot
                .parts
                .iter()
                .filter(|part| part.included)
                .flat_map(|part| part.units.iter())
                .collect::<Vec<_>>();
            AcceptedProgressFacts::Ready {
                total_units: units.len() as i64,
                remaining_units: units.iter().filter(|unit| !unit.completed).count() as i64,
            }
        }
        AcceptedRead::Empty { .. } => AcceptedProgressFacts::Empty,
        AcceptedRead::CompatibilityDirty => AcceptedProgressFacts::CompatibilityDirty,
        AcceptedRead::Uninitialized => AcceptedProgressFacts::Uninitialized,
        AcceptedRead::IntegrityFailure { .. } => AcceptedProgressFacts::Integrity,
        AcceptedRead::Missing => unreachable!("profile exists"),
    }
}

#[cfg(feature = "desktop_rebuild")]
fn summary_facts(
    id: i64,
    profile: &Row,
    part_count: i64,
    accepted: &AcceptedRead,
    freshness: FreshnessFacts,
) -> Result<BuildSummaryFacts> {
    Ok(BuildSummaryFacts {
        id,
        name: profile.s("name")?.into(),
        order_number: profile.os("order_number")?,
        special_request: profile
            .os("special_request")?
            .as_deref()
            .map(trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        part_count,
        accepted_progress: accepted_progress_facts(accepted),
        freshness,
        archived_at: profile.os("archived_at")?,
        last_used_at: profile.os("last_used_at")?,
    })
}

#[cfg(feature = "desktop_rebuild")]
pub(super) fn capture_build_summary(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    accepted: &AcceptedRead,
    budget: &mut Budget,
) -> Result<Option<BuildSummaryFacts>> {
    let Some(profile) = one(
        tx,
        "build_profiles",
        "id=?1 AND tenant_id=?2",
        &[&id, &tenant],
        "pointer",
        budget,
    )?
    else {
        return Ok(None);
    };
    let freshness = match freshness(tx, tenant, id, &profile, budget) {
        Ok((freshness, _)) => freshness,
        Err(error) => match error.downcast_ref::<super::graph::Integrity>() {
            Some(integrity) => FreshnessFacts::Unavailable {
                code: integrity.code.to_owned(),
            },
            None => return Err(error),
        },
    };
    let part_count = tx.query_row(
        "SELECT count(*) FROM parts WHERE profile_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let unavailable = matches!(freshness, FreshnessFacts::Unavailable { .. });
    let mut facts = summary_facts(id, &profile, part_count, accepted, freshness)?;
    if unavailable {
        facts.accepted_progress = AcceptedProgressFacts::Integrity;
    }
    Ok(Some(facts))
}

fn working(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    p: &Row,
    b: &mut Budget,
) -> Result<Working> {
    let drafts = rows(
        tx,
        "plan_drafts",
        "profile_id=?1 AND tenant_id=?2 AND state='open'",
        &[&id, &tenant],
        "revision",
        b,
    )?;
    let Some(d) = drafts.into_iter().max_by_key(|r| r.n("id").unwrap_or(0)) else {
        return Ok(Working::None);
    };
    let did = d.n("id")?;
    let base = d.on("base_revision_id")?;
    let before = if let Some(base) = base {
        let revision = required(
            one(tx, "plan_revisions", "id=?1", &[&base], "revision", b)?,
            "revision",
        )?;
        owned(&revision, tenant, Some(id), "revision")?;
        rows(
            tx,
            "plan_revision_parts",
            "revision_id=?1",
            &[&base],
            "revision",
            b,
        )?
    } else {
        Vec::new()
    };
    let after = rows(
        tx,
        "plan_draft_parts",
        "draft_id=?1",
        &[&did],
        "revision",
        b,
    )?;
    for part in &before {
        owned(part, tenant, None, "revision")?;
    }
    let mut linked = HashSet::new();
    let mut changes = 0;
    for r in after {
        owned(&r, tenant, None, "revision")?;
        let predecessor = r.on("base_revision_part_id")?;
        let original = before.iter().find(|r| r.n("id").ok() == predecessor);
        if let Some(original) = original {
            linked.insert(original.n("id")?);
            if [
                "part_key",
                "relative_path",
                "filename",
                "source_layer",
                "status",
                "role_inferred",
                "role_override",
                "filament_color_id",
                "filament_custom_hex",
                "spoolman_spool_id",
                "quantity_inferred",
                "quantity_override",
                "quantity_effective",
                "included",
                "notes",
                "github_blob_url",
                "geometry_same",
                "requirement",
                "option_group_id",
                "manifest_source",
                "artifact_digest",
            ]
            .iter()
            .any(|k| !original.same_field(&r, k))
            {
                changes += 1;
            }
        } else {
            changes += 1;
        }
    }
    changes += before
        .iter()
        .filter(|r| !linked.contains(&r.n("id").unwrap_or(0)))
        .count() as u64;
    let mut issues = 0;
    if let Some(rid) = d.on("current_required_unit_reconciliation_id")? {
        let r = required(
            one(
                tx,
                "plan_draft_required_unit_reconciliations",
                "id=?1",
                &[&rid],
                "required_unit_map",
                b,
            )?,
            "required_unit_map",
        )?;
        owned(&r, tenant, Some(id), "required_unit_map")?;
        check(
            r.n("draft_id")? == did,
            "required_unit_map",
            "Reconciliation draft mismatch",
        )?;
        if r.s("result_kind")? == "unresolved" {
            let v: Value = serde_json::from_str(r.s("result_json")?)?;
            issues = v["conflicts"].as_array().map_or(0, |a| a.len() as u64);
        }
    }
    if base != p.on("accepted_plan_revision_id")?
        || d.n("base_plan_version")? != p.n("accepted_plan_version")?
    {
        Ok(Working::Stale {
            draft_id: did,
            change_count: changes,
            issue_count: issues.max(1),
        })
    } else if issues > 0 {
        Ok(Working::NeedsAttention {
            draft_id: did,
            change_count: changes,
            issue_count: issues,
        })
    } else {
        Ok(Working::Ready {
            draft_id: did,
            change_count: changes,
        })
    }
}
fn numeric(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| {
        v.as_str()
            .and_then(|s| trim(s).parse::<f64>().ok())
            .filter(|n| {
                n.is_finite() && n.fract() == 0.0 && *n > 0.0 && *n <= 9_007_199_254_740_991.0
            })
            .map(|n| n as i64)
    })
}
fn jobs(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    b: &mut Budget,
) -> Result<(Production, Checkoff)> {
    let mut p = Production {
        plate_state: PlateState::NotStarted,
        queued_jobs: 0,
        sending_jobs: 0,
        printing_jobs: 0,
        failed_jobs: 0,
    };
    let mut c = Checkoff {
        awaiting_verification: 0,
        failed_verifications: 0,
        printer_links: 0,
    };
    for (key, link) in [
        ("printer.send_queue", false),
        ("printer.checkoff_links", true),
    ] {
        #[cfg(feature = "desktop_rebuild")]
        let table = if link {
            None
        } else {
            crate::send_queue::rows(tx, tenant)?
        };
        #[cfg(not(feature = "desktop_rebuild"))]
        let table = None;
        let values = match table {
            Some(values) => values,
            None => setting(tx, tenant, key, b)?
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default(),
        };
        for v in values {
            if numeric(&v["profile_id"]) != Some(id) {
                continue;
            }
            let required_fields = if link {
                ["id", "filename", "printer_id", "integration_id"]
            } else {
                ["id", "filename", "printer_id", "artifact_path"]
            };
            if required_fields
                .iter()
                .any(|k| v[k].as_str().is_none_or(|s| trim(s).is_empty()))
            {
                continue;
            }
            let state = v["state"].as_str().unwrap_or("");
            if link && state != "dismissed" {
                c.printer_links += 1;
            }
            match (link, state) {
                (false, "queued") => p.queued_jobs += 1,
                (false, "sending") => p.sending_jobs += 1,
                (false, "error") => p.failed_jobs += 1,
                (true, "watching") => p.printing_jobs += 1,
                (true, "awaiting_verify") => c.awaiting_verification += 1,
                (true, "host_failed") => c.failed_verifications += 1,
                _ => {}
            }
        }
    }
    Ok((p, c))
}
pub(super) fn capture(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    accepted: &AcceptedRead,
    b: &mut Budget,
) -> Result<Option<CapturedContext>> {
    let Some(p) = one(
        tx,
        "build_profiles",
        "id=?1 AND tenant_id=?2",
        &[&id, &tenant],
        "pointer",
        b,
    )?
    else {
        return Ok(None);
    };
    let (freshness, attached) = freshness(tx, tenant, id, &p, b)?;
    let freshness = freshness_json(&freshness);
    let part_count: i64 = tx.query_row(
        "SELECT count(*) FROM parts WHERE profile_id=?1",
        [id],
        |r| r.get(0),
    )?;
    let header = json!({"id":id,"name":p.v("name"),"order_number":p.v("order_number"),"special_request":p.os("special_request")?.as_deref().map(trim).filter(|s|!s.is_empty()),"part_count":part_count,"build_stale":freshness["status"]=="stale","freshness":freshness,"archived_at":p.v("archived_at"),"last_used_at":p.v("last_used_at")});
    let mut progress = views::progress(id, accepted);
    progress
        .as_object_mut()
        .ok_or_else(|| anyhow::Error::msg("Accepted progress is missing"))?
        .remove("profileId");
    let sources = if attached == 0 {
        Sources::Empty
    } else if freshness["status"] == "stale" {
        let reasons = freshness["reasons"].as_array().map_or(0, Vec::len);
        let untracked = freshness["untracked_sources"]
            .as_array()
            .map_or(0, Vec::len);
        Sources::Stale {
            attached_count: attached,
            issue_count: ((reasons + untracked) as u64).max(1),
        }
    } else {
        Sources::Ready {
            attached_count: attached,
        }
    };
    let a=match accepted{AcceptedRead::Ready{snapshot}=>Accepted::Ready{revision_id:snapshot.revision_id,plan_version:snapshot.plan_version,total_units:progress["totalUnits"].as_u64().ok_or_else(||anyhow::Error::msg("Accepted progress is missing a unit count"))?,remaining_units:progress["remainingUnits"].as_u64().ok_or_else(||anyhow::Error::msg("Accepted progress is missing a unit count"))?},AcceptedRead::Empty{..}=>Accepted::None,AcceptedRead::CompatibilityDirty=>Accepted::Unavailable{reason:"This Build's Accepted Plan cannot be read. Restart PrintPartner so it can repair the Plan data.".into()},AcceptedRead::Uninitialized=>Accepted::Unavailable{reason:"The Accepted Plan has not been initialized.".into()},_=>Accepted::Unavailable{reason:"Accepted Plan data is inconsistent.".into()}};
    let work = working(tx, tenant, id, &p, b)?;
    let (mut production, checkoff) = jobs(tx, tenant, id, b)?;
    let history_rows = rows(tx, "plan_revisions", "profile_id=?1", &[&id], "revision", b)?;
    let mut history = Vec::new();
    let mut revision_ids = HashSet::new();
    for r in history_rows {
        owned(&r, tenant, Some(id), "revision")?;
        revision_ids.insert(r.n("id")?);
        history.push(RevisionRef {
            id: r.n("id")?,
            parent_revision_id: r.on("parent_revision_id")?,
            revision_number: r.n("revision_number")?,
            snapshot_digest: r.s("snapshot_digest")?.into(),
            accepted_at: r.s("accepted_at")?.into(),
        });
    }
    history.sort_by_key(|r| r.id);
    let mut revision_numbers = HashSet::new();
    for revision in &history {
        check(
            revision.id > 0
                && revision.revision_number > 0
                && revision_numbers.insert(revision.revision_number)
                && revision
                    .parent_revision_id
                    .is_none_or(|parent| parent < revision.id && revision_ids.contains(&parent))
                && super::graph::timestamp(&revision.accepted_at)
                && super::graph::digest(&revision.snapshot_digest),
            "revision",
            "Accepted Plan history header is corrupt",
        )?;
    }
    let mut units = Vec::new();
    let unit_rows = rows(
        tx,
        "required_units",
        "profile_id=?1",
        &[&id],
        "required_unit_map",
        b,
    )?;
    for u in unit_rows {
        owned(&u, tenant, Some(id), "required_unit_map")?;
        let token = u.s("token")?;
        check(
            revision_ids.contains(&u.n("created_in_revision_id")?),
            "required_unit_map",
            "Required-unit creation history belongs to another Build",
        )?;
        let membership = rows(
            tx,
            "plan_revision_required_units",
            "required_unit_token=?1",
            &[&token],
            "required_unit_map",
            b,
        )?;
        let mut ids = Vec::new();
        for m in membership {
            owned(&m, tenant, None, "required_unit_map")?;
            let rid = m.n("revision_id")?;
            check(
                revision_ids.contains(&rid),
                "required_unit_map",
                "Required-unit history belongs to another Build",
            )?;
            ids.push(rid);
        }
        ids.sort();
        ids.dedup();
        let retired = p
            .on("accepted_plan_revision_id")?
            .is_none_or(|r| !ids.contains(&r));
        units.push(UnitHistory {
            token: token.into(),
            object_name: u.s("object_name")?.into(),
            created_in_revision_id: u.n("created_in_revision_id")?,
            member_revision_ids: ids,
            retired,
        });
    }
    units.sort_by(|a, b| a.token.cmp(&b.token));
    let head = one(
        tx,
        "accepted_plate_heads",
        "profile_id=?1",
        &[&id],
        "artifact_linkage",
        b,
    )?;
    if let Some(h) = &head {
        owned(h, tenant, Some(id), "artifact_linkage")?;
    }
    let head_id = head
        .as_ref()
        .map(|h| h.n("current_revision_id"))
        .transpose()?;
    let mut plates = Vec::new();
    for r in rows(
        tx,
        "accepted_plate_revisions",
        "profile_id=?1",
        &[&id],
        "artifact_linkage",
        b,
    )? {
        owned(&r, tenant, Some(id), "artifact_linkage")?;
        let rid = r.n("id")?;
        let plan = r.n("plan_revision_id")?;
        check(
            revision_ids.contains(&plan),
            "artifact_linkage",
            "Plate history belongs to another Build",
        )?;
        let current = Some(rid) == head_id;
        if current {
            production.plate_state = if let AcceptedRead::Ready { snapshot } = accepted {
                if plan == snapshot.revision_id
                    && r.n("plan_version")? == snapshot.plan_version
                    && r.s("plan_revision_digest")? == snapshot.revision_digest
                    && r.s("required_unit_mapping_digest")? == snapshot.required_unit_mapping_digest
                {
                    validate_current_plates(tx, tenant, &r, snapshot, b)?;
                    PlateState::Ready
                } else {
                    PlateState::Stale
                }
            } else {
                PlateState::Error
            };
        }
        plates.push(PlateRef {
            id: rid,
            plan_revision_id: plan,
            plan_version: r.n("plan_version")?,
            layout_digest: r.s("layout_digest")?.into(),
            current,
        });
    }
    check(
        head_id.is_none() || plates.iter().any(|p| p.current),
        "artifact_linkage",
        "Accepted Plate head is missing",
    )?;
    plates.sort_by_key(|p| p.id);
    let f = Facts {
        build: Build {
            id,
            name: p.s("name")?.into(),
        },
        sources,
        accepted_plan: a,
        working_plan: work,
        production,
        checkoff,
        stock: Stock::NotReady,
    };
    let (accepted_progress, legacy) = match accepted {
        AcceptedRead::Ready { .. } => (
            json!({"kind":"ready","total_units":progress["totalUnits"],"remaining_units":progress["remainingUnits"]}),
            Some((
                progress["totalUnits"].clone(),
                progress["remainingUnits"].clone(),
            )),
        ),
        AcceptedRead::Empty { .. } => (json!({"kind":"empty"}), Some((json!(0), json!(0)))),
        AcceptedRead::CompatibilityDirty => (
            json!({"kind":"unavailable","reason":"compatibility_dirty"}),
            None,
        ),
        AcceptedRead::Uninitialized => {
            (json!({"kind":"unavailable","reason":"uninitialized"}), None)
        }
        _ => (json!({"kind":"unavailable","reason":"integrity"}), None),
    };
    let mut profile_summary_v2 = header.clone();
    profile_summary_v2["accepted_progress"] = accepted_progress;
    let profile_summary_v1 = if let Some((total, remaining)) = legacy {
        let mut profile = header.clone();
        profile["total_units"] = total;
        profile["remaining_units"] = remaining;
        json!({"kind":"ready","profile":profile})
    } else {
        let failure = match accepted {
            AcceptedRead::CompatibilityDirty => {
                json!({"kind":"unavailable","reason":"compatibility_dirty"})
            }
            AcceptedRead::Uninitialized => json!({"kind":"unavailable","reason":"uninitialized"}),
            AcceptedRead::IntegrityFailure { code, .. } => {
                json!({"kind":"integrity_failure","code":code})
            }
            _ => unreachable!("profile exists"),
        };
        json!({"kind":"unavailable","failure":failure})
    };
    Ok(Some(CapturedContext {
        profile_summary_v1,
        profile_summary_v2,
        profile_summary: json!({"kind":"found","summary":{"header":header,"progress":progress}}),
        workflow: json!({"kind":"ready","workspace":workflow::resolve(&f)?}),
        #[cfg(feature = "desktop_rebuild")]
        facts: f,
        history,
        required_units: units,
        plate_revisions: plates,
    }))
}

fn validate_current_plates(
    tx: &Transaction<'_>,
    tenant: &str,
    r: &Row,
    s: &super::Snapshot,
    b: &mut Budget,
) -> Result<()> {
    let rid = r.n("id")?;
    let mut plates = rows(
        tx,
        "accepted_plates",
        "revision_id=?1",
        &[&rid],
        "artifact_linkage",
        b,
    )?;
    let units = rows(
        tx,
        "accepted_plate_units",
        "revision_id=?1",
        &[&rid],
        "artifact_linkage",
        b,
    )?;
    check(
        !plates.is_empty()
            && plates.len() <= 65534
            && plates.len() as i64 == r.n("expected_plate_count")?
            && !units.is_empty()
            && units.len() as i64 == r.n("expected_unit_count")?,
        "artifact_linkage",
        "Accepted Plate counts are corrupt",
    )?;
    plates.sort_by_key(|p| p.n("ordinal").unwrap_or(0));
    let expected = s
        .parts
        .iter()
        .filter(|p| p.included)
        .flat_map(|p| p.units.iter().map(|u| u.token.as_str()))
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut plate_ids = HashSet::new();
    let mut digests = [Vec::new(), Vec::new()];
    let mut overlaps = false;
    let mut clearance = false;
    let mut pair_checks = 0usize;
    for (i, p) in plates.iter().enumerate() {
        owned(p, tenant, None, "artifact_linkage")?;
        let id = trim(p.s("plate_id")?);
        check(
            p.n("ordinal")? == i as i64 + 1 && plate_ids.insert(id.to_owned()),
            "artifact_linkage",
            "Accepted Plate ordinal is corrupt",
        )?;
        for field in ["plate_id", "printer_id", "printer_name", "printer_model"] {
            let text = trim(p.s(field)?);
            check(
                !text.is_empty() && text.encode_utf16().count() <= 200,
                "artifact_linkage",
                "Accepted Plate text is corrupt",
            )?;
        }
        for field in ["bed_width_um", "bed_depth_um", "bed_height_um", "margin_um"] {
            let n = p.n(field)?;
            check(
                (0..=2_147_483_647).contains(&n) && (field == "margin_um" || n > 0),
                "artifact_linkage",
                "Accepted Plate geometry is corrupt",
            )?;
        }
        let (w, d, h, m) = (
            p.n("bed_width_um")?,
            p.n("bed_depth_um")?,
            p.n("bed_height_um")?,
            p.n("margin_um")?,
        );
        check(
            m <= w / 2 && m <= d / 2,
            "artifact_linkage",
            "Accepted Plate margin is corrupt",
        )?;
        let mut selected = units
            .iter()
            .filter(|u| u.s("plate_id").ok() == Some(p.s("plate_id").unwrap_or_default()))
            .collect::<Vec<_>>();
        selected.sort_by(|a, b| {
            a.s("required_unit_token")
                .unwrap_or_default()
                .cmp(b.s("required_unit_token").unwrap_or_default())
        });
        let mut serialized = [Vec::new(), Vec::new()];
        let mut footprints = Vec::new();
        for u in selected {
            owned(u, tenant, None, "artifact_linkage")?;
            let token = u.s("required_unit_token")?;
            check(
                expected.contains(token) && seen.insert(token.to_owned()),
                "artifact_linkage",
                "Accepted Plate unit membership is corrupt",
            )?;
            for field in ["x_um", "y_um", "width_um", "depth_um", "height_um"] {
                let n = u.n(field)?;
                check(
                    (0..=2_147_483_647).contains(&n) && (!field.ends_with("th_um") || n > 0),
                    "artifact_linkage",
                    "Accepted Plate unit geometry is corrupt",
                )?;
            }
            let (x, y, uw, ud, uh) = (
                u.n("x_um")?,
                u.n("y_um")?,
                u.n("width_um")?,
                u.n("depth_um")?,
                u.n("height_um")?,
            );
            check(
                uw > 0 && ud > 0 && uh > 0,
                "artifact_linkage",
                "Accepted Plate unit dimensions are corrupt",
            )?;
            let placement = match u.s("placement")? {
                "manual" => "manual",
                "unplaced" => "unplaced",
                _ => "auto",
            };
            let pinned = placement != "unplaced" && u.b("pinned")?;
            if placement != "unplaced" {
                check(
                    x >= m
                        && y >= m
                        && x <= w - m
                        && uw <= w - m - x
                        && y <= d - m
                        && ud <= d - m - y
                        && uh <= h,
                    "artifact_linkage",
                    "Accepted Plate unit is outside build area",
                )?;
                footprints.push((x, y, uw, ud));
            }
            let fields = vec![
                ("token", json!(token)),
                ("xUm", json!(x)),
                ("yUm", json!(y)),
                ("widthUm", json!(uw)),
                ("depthUm", json!(ud)),
                ("heightUm", json!(uh)),
            ];
            serialized[0].push(super::graph::object(fields.clone()));
            let mut current = fields;
            current.push(("placement", json!(placement)));
            current.push(("pinned", json!(pinned)));
            serialized[1].push(super::graph::object(current));
        }
        pair_checks += footprints
            .len()
            .saturating_mul(footprints.len().saturating_sub(1))
            / 2;
        #[cfg(feature = "desktop_rebuild")]
        super::graph::budget_check(
            pair_checks <= 1_000_000,
            crate::build_graph::BudgetResource::PlateOverlapChecks,
        )?;
        #[cfg(not(feature = "desktop_rebuild"))]
        check(
            pair_checks <= 1_000_000,
            "artifact_linkage",
            "Accepted Plate overlap validation budget exceeded",
        )?;
        for (idx, a) in footprints.iter().enumerate() {
            for other in &footprints[idx + 1..] {
                let hit = |margin| {
                    a.0 < other.0 + other.2 + margin
                        && other.0 < a.0 + a.2 + margin
                        && a.1 < other.1 + other.3 + margin
                        && other.1 < a.1 + a.3 + margin
                };
                overlaps |= hit(0);
                clearance |= hit(m);
            }
        }
        let base = super::graph::object(vec![
            ("ordinal", json!(i + 1)),
            ("plateId", json!(id)),
            ("printerId", json!(trim(p.s("printer_id")?))),
            ("printerName", json!(trim(p.s("printer_name")?))),
            ("printerModel", json!(trim(p.s("printer_model")?))),
            ("bedWidthUm", json!(w)),
            ("bedDepthUm", json!(d)),
            ("bedHeightUm", json!(h)),
            ("marginUm", json!(m)),
        ]);
        for version in 0..2 {
            digests[version].push(format!(
                "{},\"units\":[{}]}}",
                &base[..base.len() - 1],
                serialized[version].join(",")
            ));
        }
    }
    check(
        seen.len() == units.len() && !overlaps,
        "artifact_linkage",
        "Accepted Plate layout is corrupt",
    )?;
    let legacy = sha(&format!(
        "{{\"format\":1,\"plates\":[{}]}}",
        digests[0].join(",")
    ));
    let current = sha(&format!(
        "{{\"format\":2,\"plates\":[{}]}}",
        digests[1].join(",")
    ));
    check(
        legacy == r.s("layout_digest")? || (!clearance && current == r.s("layout_digest")?),
        "artifact_linkage",
        "Accepted Plate layout digest is corrupt",
    )?;
    Ok(())
}

#[cfg(all(test, feature = "desktop_rebuild"))]
mod tests {
    use super::*;
    use crate::build_graph::{BudgetResource, Failure};

    #[test]
    fn plate_overlap_budget_is_distinct_from_integrity() {
        let root = std::env::temp_dir().join(format!("pp-plate-budget-{}", rand::random::<u64>()));
        std::fs::create_dir_all(root.join("repos/1/revisions/accepted")).unwrap();
        let database = root.join("fixture.db");
        std::fs::write(
            &database,
            include_bytes!("../../tests/fixtures/accepted-plate-node.db"),
        )
        .unwrap();
        let mut connection = rusqlite::Connection::open(database).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        let tx = connection.transaction().unwrap();
        let AcceptedRead::Ready { mut snapshot } = super::super::graph::read(
            &tx,
            "default",
            1,
            &root.join("repos"),
            &mut Budget::default(),
        )
        .unwrap() else {
            panic!("accepted fixture")
        };
        let triggers = tx
            .prepare("SELECT name FROM sqlite_master WHERE type='trigger'")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for trigger in triggers {
            tx.execute_batch(&format!("DROP TRIGGER \"{trigger}\";"))
                .unwrap();
        }
        let part = snapshot.parts.iter_mut().find(|p| p.included).unwrap();
        let template = part.units[0].clone();
        part.units = (0..1415)
            .map(|index| {
                let mut unit = template.clone();
                unit.token = format!("budget-unit-{index}");
                unit
            })
            .collect();
        tx.execute_batch("DELETE FROM accepted_plate_units; UPDATE accepted_plate_revisions SET expected_unit_count=1415;").unwrap();
        for unit in &part.units {
            tx.execute("INSERT INTO accepted_plate_units(tenant_id,revision_id,plate_id,required_unit_token,x_um,y_um,width_um,depth_um,height_um) VALUES('default',1,'fixture-plate',?1,5000,5000,50000,40000,30000)", [&unit.token]).unwrap();
        }
        let revision = one(
            &tx,
            "accepted_plate_revisions",
            "id=1",
            &[],
            "artifact_linkage",
            &mut Budget::default(),
        )
        .unwrap()
        .unwrap();
        let error =
            validate_current_plates(&tx, "default", &revision, &snapshot, &mut Budget::default())
                .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref(),
                Some(Failure::BudgetExhausted {
                    resource: BudgetResource::PlateOverlapChecks
                })
            ),
            "{error:?}"
        );
        drop(tx);
        drop(connection);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(feature = "desktop_rebuild")]
pub(super) fn draft_freshness_current(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    b: &mut Budget,
) -> Result<bool> {
    let p = required(
        one(
            tx,
            "build_profiles",
            "id=?1 AND tenant_id=?2",
            &[&id, &tenant],
            "profile",
            b,
        )?,
        "profile",
    )?;
    Ok(matches!(
        freshness(tx, tenant, id, &p, b)?.0,
        FreshnessFacts::Current { .. }
    ))
}
