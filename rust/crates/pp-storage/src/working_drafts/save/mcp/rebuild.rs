//! Atomic planning rebuild over the existing Source preparation and draft writer.
use super::*;
use crate::build_graph::{BuildCommand, BuildOutcome};
use crate::read_model::planning::{self, *};
use crate::source_scan::node_json::RawObject;
use std::collections::BTreeSet;

pub(super) struct Args<'a, 'conn> {
    pub tx: &'a Transaction<'conn>,
    pub account: &'a str,
    pub build: i64,
    pub raw: &'a str,
    pub brief: BuildPlanningBrief,
    pub request: &'a McpRequest,
    pub repos: &'a std::path::Path,
    pub flag: &'a AtomicBool,
    pub policy: auth::AuthPolicy,
}
struct Sql<'a, 'conn>(&'a Transaction<'conn>);
impl preparation::PreparationSql for Sql<'_, '_> {
    fn read<T>(&mut self, read: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        read(self.0)
    }
}
fn without_nulls(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|_, v| !v.is_null());
            for value in fields.values_mut() {
                without_nulls(value)
            }
        }
        Value::Array(values) => {
            for value in values {
                without_nulls(value)
            }
        }
        _ => {}
    }
}
fn layers(tx: &Transaction<'_>, account: &str, build: i64) -> Result<Vec<Value>> {
    rows(
        tx,
        "SELECT id,layer_type,project_id FROM profile_layers WHERE tenant_id=? AND profile_id=? ORDER BY layer_order,id",
        &[&account, &build],
    )
}
fn command(args: &Args<'_, '_>, command: BuildCommand) -> Result<bool> {
    Ok(matches!(
        crate::build_graph::mutation(args.tx, args.account, &command, args.repos, args.flag)?,
        BuildOutcome::Layers { .. } | BuildOutcome::Deleted
    ))
}
fn sources(args: &Args<'_, '_>) -> Result<HashMap<i64, PlanningSource>> {
    let reads = source_reads::SourceReads::new(args.repos.parent().unwrap_or(args.repos))
        .with_filesystem_policy(source_reads::SourceFilesystemPolicy::for_auth(&args.policy));
    rows(args.tx,"SELECT id,name,local_path,last_synced_at,last_commit_sha FROM projects WHERE tenant_id=? ORDER BY id", &[&args.account])?.into_iter().map(|row| {
        let id=num(&row,"id")?;
        Ok((id,PlanningSource::resolve(&reads,id,text(&row,"name")?.to_owned(),row["localPath"].as_str(),(row["lastSyncedAt"].as_str().map(str::to_owned),row["lastCommitSha"].as_str().map(str::to_owned)))))
    }).collect()
}
fn exclusions(
    brief: &BuildPlanningBrief,
    sources: &HashMap<i64, PlanningSource>,
) -> Vec<(i64, String)> {
    let named: HashMap<_, _> = brief
        .evidence
        .iter()
        .filter_map(|e| {
            let id = e.source_id?;
            Some((sources.get(&id)?.name.as_str(), id))
        })
        .collect();
    let mut out = BTreeSet::new();
    for difference in &brief.differences {
        let Some(resolution) = brief.resolutions.get(&difference.group_id) else {
            continue;
        };
        for (source, path, excluded) in [
            (
                &difference.source_b,
                &difference.path_b,
                matches!(
                    resolution.resolution,
                    DifferenceResolution::ChooseSourceA | DifferenceResolution::NotApplicable
                ),
            ),
            (
                &difference.source_a,
                &difference.path_a,
                matches!(
                    resolution.resolution,
                    DifferenceResolution::ChooseSourceB | DifferenceResolution::NotApplicable
                ),
            ),
        ] {
            if excluded
                && let Some(path) = path.as_ref().filter(|p| p.to_lowercase().ends_with(".stl"))
                && let Some(id) = named.get(source.as_str())
            {
                out.insert((*id, path.clone()));
            }
        }
    }
    let mut out: Vec<_> = out.into_iter().collect();
    out.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| crate::read_model::views::folder_compare(&a.1, &b.1))
    });
    out
}
fn input_digest(
    brief: &BuildPlanningBrief,
    raw: &str,
    exclusions: &[(i64, String)],
    targets: &BTreeSet<i64>,
) -> Result<String> {
    #[derive(serde::Serialize)]
    struct Evidence<'a> {
        id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_id: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        source_role: Option<SourceRole>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pinned_revision: Option<&'a str>,
    }
    let mut resolutions = RawObject::parse(
        RawObject::parse(raw)
            .and_then(|r| r.get("resolutions").map(str::to_owned))
            .as_deref()
            .unwrap_or("{}"),
    )
    .unwrap_or_default();
    let original = BuildPlanningBrief::parse(raw, brief.build_id)?;
    for key in original.resolutions.keys() {
        if !brief.resolutions.contains_key(key) {
            resolutions.remove(key)
        }
    }
    let mut ids: Vec<_> = brief.differences.iter().map(|d| &d.id).collect();
    ids.sort();
    let mut input = RawObject::default();
    input.set(
        "evidence",
        &serde_json::to_string(
            &brief
                .evidence
                .iter()
                .map(|e| Evidence {
                    id: &e.id,
                    source_id: e.source_id,
                    source_role: e.source_role,
                    pinned_revision: e.pinned_revision.as_deref(),
                })
                .collect::<Vec<_>>(),
        )?,
    );
    input.set(
        "contributions",
        RawObject::parse(raw)
            .as_ref()
            .and_then(|r| r.get("contributions"))
            .unwrap_or("[]"),
    );
    input.set("resolutions", &resolutions.text());
    input.set("difference_ids", &serde_json::to_string(&ids)?);
    input.set(
        "exclusions",
        &serde_json::to_string(
            &exclusions
                .iter()
                .map(|(id, path)| json!({"sourceId":id,"path":path}))
                .collect::<Vec<_>>(),
        )?,
    );
    input.set("managed_source_ids", &serde_json::to_string(&targets)?);
    Ok(hex::encode(Sha256::digest(input.text().as_bytes()))[..16].into())
}

pub(super) fn run(args: Args<'_, '_>) -> Result<Value> {
    let fail = |detail: &str| failure(true, detail);
    let sources = sources(&args)?;
    let brief = planning::hydrate(args.brief.clone(), &sources)?;
    let bases: Vec<_> = brief
        .evidence
        .iter()
        .filter_map(|e| {
            (e.source_role == Some(SourceRole::StructuralBase))
                .then_some(e.source_id)
                .flatten()
        })
        .collect();
    if bases.len() != 1 {
        return Ok(fail(
            "Planning rebuild requires exactly one structural base Source",
        ));
    }
    let targets: BTreeSet<_> = brief
        .evidence
        .iter()
        .filter(|e| {
            matches!(
                e.source_role,
                Some(SourceRole::StructuralBase | SourceRole::Overlay | SourceRole::Addon)
            )
        })
        .filter_map(|e| e.source_id)
        .collect();
    if let Some(detail) = configure_layers(&args, &brief, bases[0], &targets)? {
        return Ok(fail(detail));
    }
    let blockers = planning::source_choice_blockers(&brief, &sources);
    if !blockers.is_empty() {
        return Ok(fail(&format!(
            "Planning rebuild cannot apply source choices: {}",
            blockers.join("; ")
        )));
    }
    let exclusions = exclusions(&brief, &sources);
    let digest = input_digest(&brief, args.raw, &exclusions, &targets)?;
    let action = args
        .request
        .action
        .as_ref()
        .and_then(|a| a["id"].as_str())
        .unwrap_or_default();
    let key = format!(
        "{}:{digest}",
        args.request.params["idempotency_key"]
            .as_str()
            .map(crate::read_model::views::js_trim)
            .filter(|key| !key.is_empty())
            .unwrap_or(action)
    );
    let (id, kind) = match recompute(&args, &brief, &exclusions, &key)? {
        Ok(ready) => ready,
        Err(kind) => return Ok(fail(&format!("Plan draft recompute failed: {kind}"))),
    };
    let mut saved: Value = serde_json::from_str(args.raw)?;
    let hydrated = serde_json::to_value(&brief)?;
    for field in ["evidence", "differences", "resolutions"] {
        saved[field] = hydrated[field].clone();
        without_nulls(&mut saved[field]);
    }
    saved["draft_id"] = json!(id);
    saved["draft_review_blockers"] = json!([]);
    saved["managed_source_ids"] = json!(targets);
    saved["draft_source_revisions"] = json!(
        brief
            .evidence
            .iter()
            .filter_map(|e| Some((e.source_id?.to_string(), e.pinned_revision.clone()?)))
            .collect::<BTreeMap<_, _>>()
    );
    saved["updated_at"] = json!(auth::catalog_timestamp());
    args.tx.execute(
        "UPDATE app_settings SET value=?1 WHERE tenant_id=?2 AND key=?3",
        params![
            saved.to_string(),
            args.account,
            format!("build_planning.v1.{}", args.build)
        ],
    )?;
    Ok(json!({"ok":true,"result":{"draft_id":id,"recompute":kind}}))
}

fn configure_layers(
    args: &Args<'_, '_>,
    brief: &BuildPlanningBrief,
    base: i64,
    targets: &BTreeSet<i64>,
) -> Result<Option<&'static str>> {
    let managed = args.brief.managed_source_ids.as_deref().unwrap_or_default();
    let build = PositiveId::new(args.build as u64).map_err(anyhow::Error::msg)?;
    for layer in layers(args.tx, args.account, args.build)? {
        if let Some(id) = layer["projectId"].as_i64()
            && managed.contains(&id)
            && !targets.contains(&id)
        {
            if !command(
                args,
                BuildCommand::Detach {
                    build,
                    layer: PositiveId::new(num(&layer, "id")? as u64)
                        .map_err(anyhow::Error::msg)?,
                },
            )? {
                return Ok(Some("Planning rebuild could not remove a managed Source"));
            }
        }
    }
    if !layers(args.tx, args.account, args.build)?
        .iter()
        .any(|row| row["layerType"] == "base" && row["projectId"] == base)
        && !command(
            args,
            BuildCommand::SetBase {
                build,
                source: PositiveId::new(base as u64).map_err(anyhow::Error::msg)?,
            },
        )?
    {
        return Ok(Some("Planning rebuild structural base Source not found"));
    }
    let mut attached: BTreeSet<_> = layers(args.tx, args.account, args.build)?
        .iter()
        .filter_map(|row| row["projectId"].as_i64())
        .collect();
    for evidence in &brief.evidence {
        if matches!(
            evidence.source_role,
            Some(SourceRole::Overlay | SourceRole::Addon)
        ) && let Some(id) = evidence.source_id
            && attached.insert(id)
            && !command(
                args,
                BuildCommand::AttachAddon {
                    build,
                    source: PositiveId::new(id as u64).map_err(anyhow::Error::msg)?,
                },
            )?
        {
            return Ok(Some("Planning rebuild addon Source not found"));
        }
    }
    Ok(None)
}
fn prepare(
    args: &Args<'_, '_>,
    brief: &BuildPlanningBrief,
    exclusions: &[(i64, String)],
    profile: &Value,
    reads: &source_reads::SourceReads,
    budget: &mut observation::PreparationBudget<'_>,
) -> Result<preparation::PreparedDraftSnapshot> {
    let base = json!({"baseRevisionId":profile["acceptedPlanRevisionId"],"basePlanVersion":profile["acceptedPlanVersion"]});
    let mut options = preparation::PreparationOptions::default();
    let mut excluded = BTreeMap::<i64, Vec<String>>::new();
    for (id, path) in exclusions {
        excluded.entry(*id).or_default().push(path.clone());
    }
    options.excluded_paths_by_source_id = Some(excluded);
    let mut included = BTreeMap::<i64, Vec<String>>::new();
    for contribution in &brief.contributions {
        if contribution.status == ContributionStatus::Confirmed
            && contribution.responsibility == ContributionResponsibility::PrintableParts
            && let Some(id) = brief
                .evidence
                .iter()
                .find(|e| e.id == contribution.evidence_id)
                .and_then(|e| e.source_id)
        {
            included
                .entry(id)
                .or_default()
                .extend(contribution.path_scopes.clone());
        }
    }
    options.included_paths_by_source_id = Some(included);
    preparation::prepare(
        &mut Sql(args.tx),
        args.account,
        args.build,
        base,
        preparation::PreparationContext {
            options: &options,
            repos: args.repos,
        },
        reads,
        budget,
    )
}
fn recompute(
    args: &Args<'_, '_>,
    brief: &BuildPlanningBrief,
    exclusions: &[(i64, String)],
    key: &str,
) -> Result<std::result::Result<(i64, &'static str), &'static str>> {
    let profile = one(
        args.tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&args.account, &args.build],
    )?
    .ok_or_else(|| anyhow!("Build not found"))?;
    let ready = if let Some(winner) = winner(
        args.tx,
        args.account,
        "assistant:build-planning",
        args.build,
        key,
    )? {
        match winner {
            RecomputeOutcome::Existing { draft } => {
                (draft.identity().draft_id().get() as i64, "existing")
            }
            _ => return Ok(Err("idempotency_conflict")),
        }
    } else {
        if baseline_required(args.tx, args.account, &profile)? {
            return Ok(Err("accepted_baseline_required"));
        }
        let reads = source_reads::SourceReads::new(args.repos.parent().unwrap_or(args.repos))
            .with_filesystem_policy(source_reads::SourceFilesystemPolicy::for_auth(&args.policy));
        let mut budget = observation::PreparationBudget::new(args.flag, Default::default())?;
        let prepared = prepare(args, brief, exclusions, &profile, &reads, &mut budget);
        let mut prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                return match error.downcast_ref::<preparation::Failure>() {
                    Some(preparation::Failure::NoLayers) => Ok(Err("no_layers")),
                    Some(preparation::Failure::NoStls) => Ok(Err("no_stls")),
                    Some(preparation::Failure::WouldWipe) => Ok(Err("would_wipe")),
                    None => Err(error),
                };
            }
        };
        if preparation::capture(args.tx, args.account, args.build, &reads, &mut budget)?.fingerprint
            != prepared.capture.fingerprint
        {
            return Ok(Err("inputs_changed"));
        }
        if carry_open_edits(
            args.tx,
            args.account,
            args.build,
            &mut prepared.parts,
            false,
            &mut budget,
        )? {
            prepared.digest =
                model::planning(&prepared.base, &prepared.capture.inputs, &prepared.parts)?;
        }
        let open = abandon_open(args.tx, args.account, args.build)?;
        (
            insert_snapshot(
                args.tx,
                args.account,
                "assistant:build-planning",
                args.build,
                key,
                prepared.rows(),
                open.last().copied().map_or(Origin::New, Origin::Draft),
            )?,
            "created",
        )
    };
    Ok(Ok(ready))
}
