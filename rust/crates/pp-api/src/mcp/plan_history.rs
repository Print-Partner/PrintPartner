//! Rust MCP views for the current Build, its Accepted revisions, and Part identity.

use super::{
    ProductMcp, positive_id,
    write_result::{Code, Refusal, writer_failure},
};
use crate::loopback::PresentedCredential;
use pp_services::mcp_domain::ToolResult;
use pp_storage::{
    build_graph::{ManifestOptionsOutcome, PositiveId},
    read_model::{
        AcceptedRead, Credential,
        history::plan::{PartIdentity, PlanHistory, RevisionRead},
        views::{self, CatalogOnly, ReviewObservations},
    },
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::AtomicBool,
};

const SNAPSHOT: &str = "get_plan_snapshot";
const SEARCH: &str = "search_plan_parts";
const REVIEW: &str = "get_plan_review";
const OPTIONS: &str = "get_plan_option_groups";
const COMPARE: &str = "compare_plans";
pub(super) const TOOLS: [&str; 5] = [SNAPSHOT, SEARCH, REVIEW, OPTIONS, COMPARE];

const INTERNAL: Refusal = Refusal::new(Code::Internal, "Plan data could not be read.");
const MISSING: Refusal = Refusal::new(Code::PlanNotFound, "No Build has this Plan ID.");
const INVALID: Refusal = Refusal::new(Code::InvalidPlanId, "Use positive whole Plan IDs.");
const STALE: Refusal = Refusal::new(
    Code::StalePlan,
    "Plan inputs changed during the read; try again.",
);

pub(super) fn describe_revision_selector(catalog: &mut Value) {
    if let Some(tools) = catalog["tools"].as_array_mut() {
        for tool in tools {
            if matches!(tool["name"].as_str(), Some(SNAPSHOT | SEARCH)) {
                tool["inputSchema"]["properties"]["revision_id"] = json!({
                    "type": "integer", "minimum": 1,
                    "description": "Read a specific Accepted revision of this Plan instead of the current head.",
                });
            }
        }
    }
}

enum Read {
    Options(PositiveId),
    Review(PositiveId),
    Snapshot(PositiveId, Option<PositiveId>),
    Search(PositiveId, Option<PositiveId>, String, usize),
    Compare(PositiveId, PositiveId),
}

impl Read {
    fn parse(name: &str, args: &Value) -> Result<Self, Refusal> {
        if name == COMPARE {
            return Ok(Self::Compare(
                positive_id(args, "plan_a_id").ok_or(INVALID)?,
                positive_id(args, "plan_b_id").ok_or(INVALID)?,
            ));
        }
        let plan = positive_id(args, "plan_id").ok_or(INVALID)?;
        let revision = || {
            args.get("revision_id")
                .map(|_| {
                    positive_id(args, "revision_id").ok_or(Refusal::new(
                        Code::InvalidInput,
                        "revision_id must be a positive whole number.",
                    ))
                })
                .transpose()
        };
        Ok(match name {
            OPTIONS => Self::Options(plan),
            REVIEW => Self::Review(plan),
            SNAPSHOT => Self::Snapshot(plan, revision()?),
            SEARCH => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|query| !query.is_empty())
                    .ok_or(Refusal::new(
                        Code::InvalidInput,
                        "A filename or path query is required.",
                    ))?;
                let limit = args
                    .get("limit")
                    .and_then(Value::as_i64)
                    .unwrap_or(20)
                    .clamp(1, 50);
                Self::Search(plan, revision()?, query.to_owned(), limit as usize)
            }
            _ => return Err(INTERNAL),
        })
    }
}

impl ProductMcp {
    pub(super) async fn plan_history_tool(
        &self,
        name: &str,
        args: &Value,
        presented: PresentedCredential,
    ) -> ToolResult {
        let result = match Read::parse(name, args) {
            Err(refusal) => Err(refusal),
            Ok(read) => {
                let reads = self.reads.clone();
                tokio::task::spawn_blocking(move || read.run(&reads, presented.into_credential()))
                    .await
                    .unwrap_or(Err(INTERNAL))
            }
        };
        match result {
            Ok(text) => ToolResult::ok(text),
            Err(refusal) => ToolResult {
                text: refusal.proposal(),
                is_error: true,
            },
        }
    }
}

impl Read {
    fn run(self, reads: &super::Reads, credential: Credential) -> Result<String, Refusal> {
        let stop = AtomicBool::new(false);
        let wait = reads.wait;
        let result = match self {
            Self::Options(build) => {
                return match reads
                    .builds
                    .plan_option_groups(credential, build, &stop, wait)
                    .map_err(|error| writer_failure(&error))?
                {
                    ManifestOptionsOutcome::Builder(body) => {
                        String::from_utf8(body.into_bytes()).map_err(|_| INTERNAL)
                    }
                    ManifestOptionsOutcome::MissingBuild => Err(MISSING),
                    ManifestOptionsOutcome::StaleObservation => Err(STALE),
                    _ => Err(INTERNAL),
                };
            }
            Self::Review(build) => {
                let accepted = reads
                    .accepted
                    .read_plan(credential, Some(build.get() as i64), &stop, wait)
                    .map_err(|error| writer_failure(&error))?;
                accepted_profile(&accepted)?;
                let observations = match (&accepted, &reads.review_observer) {
                    (AcceptedRead::Ready { snapshot }, Some(observer)) => {
                        observer.observe(snapshot, &stop).map_err(|_| INTERNAL)?
                    }
                    _ => ReviewObservations {
                        available_input_roots: Default::default(),
                        media_by_part_id: Default::default(),
                    },
                };
                let body = views::review_json(&accepted, false, &observations, &CatalogOnly)
                    .map_err(|_| INTERNAL)?
                    .summary_json()
                    .ok_or(INTERNAL)?;
                let mut summary: Value =
                    serde_json::from_slice(&body.into_bytes()).map_err(|_| INTERNAL)?;
                summary["basis"] = basis(&accepted);
                summary
            }
            Self::Snapshot(build, revision) => {
                if let Some(revision) = revision {
                    let (basis, parts) = revision_view(reads, credential, build, revision)?;
                    json!({"id": build.get(), "basis": basis, "part_count": parts.len()})
                } else {
                    let (plan, _) = reads
                        .accepted
                        .plan_history(credential, build, None, &stop, wait)
                        .map_err(|error| writer_failure(&error))?;
                    snapshot(&plan.ok_or(MISSING)?)?
                }
            }
            Self::Search(build, revision, query, limit) => {
                let (basis, parts) = if let Some(revision) = revision {
                    revision_view(reads, credential, build, revision)?
                } else {
                    let accepted = reads
                        .accepted
                        .read_plan(credential, Some(build.get() as i64), &stop, wait)
                        .map_err(|error| writer_failure(&error))?;
                    accepted_profile(&accepted)?;
                    (
                        basis(&accepted),
                        accepted_parts(&accepted)
                            .iter()
                            .map(PartIdentity::from)
                            .collect(),
                    )
                };
                let needle = query.to_lowercase();
                let parts = parts
                    .into_iter()
                    .filter(|part| {
                        part.filename.to_lowercase().contains(&needle)
                            || part.relative_path.to_lowercase().contains(&needle)
                    })
                    .take(limit)
                    .collect::<Vec<_>>();
                json!({"plan_id": build.get(), "query": query, "basis": basis, "count": parts.len(), "parts": parts})
            }
            Self::Compare(a, b) => {
                let (a, b) = reads
                    .accepted
                    .plan_history(credential, a, Some(b), &stop, wait)
                    .map_err(|error| writer_failure(&error))?;
                compare(&a.ok_or(MISSING)?, &b.ok_or(MISSING)?)?
            }
        };
        Ok(result.to_string())
    }
}

fn accepted_profile(read: &AcceptedRead) -> Result<&pp_storage::read_model::Profile, Refusal> {
    match read {
        AcceptedRead::Ready { snapshot } => Ok(&snapshot.profile),
        AcceptedRead::Empty { profile } => Ok(profile),
        AcceptedRead::Missing => Err(MISSING),
        AcceptedRead::CompatibilityDirty | AcceptedRead::Uninitialized => Err(Refusal::new(
            Code::StalePlan,
            "The Accepted Plan is not ready; repair or accept the Plan first.",
        )),
        AcceptedRead::IntegrityFailure { .. } => Err(INTERNAL),
    }
}

fn accepted_parts(read: &AcceptedRead) -> &[pp_storage::read_model::Part] {
    match read {
        AcceptedRead::Ready { snapshot } => &snapshot.parts,
        _ => &[],
    }
}

fn basis(read: &AcceptedRead) -> Value {
    match read {
        AcceptedRead::Ready { snapshot } => json!({
        "kind": "current", "revision_id": snapshot.revision_id,
        "revision_number": snapshot.revision_number, "plan_version": snapshot.plan_version,
        "snapshot_digest": snapshot.revision_digest,
        }),
        _ => json!({"kind": "empty"}),
    }
}

fn revision_view(
    reads: &super::Reads,
    credential: Credential,
    build: PositiveId,
    id: PositiveId,
) -> Result<(Value, Vec<PartIdentity>), Refusal> {
    match reads
        .accepted
        .plan_revision(credential, build, id, &AtomicBool::new(false), reads.wait)
        .map_err(|error| writer_failure(&error))?
    {
        RevisionRead::MissingBuild => Err(MISSING),
        RevisionRead::MissingRevision => Err(Refusal::new(
            Code::NotFound,
            "This Plan has no such Accepted revision.",
        )),
        RevisionRead::Ready {
            revision,
            current,
            parts,
        } => Ok((
            json!({
                "kind": if current { "current" } else { "historical" },
                "revision_id": revision.id, "revision_number": revision.revision_number,
                "snapshot_digest": revision.snapshot_digest, "accepted_at": revision.accepted_at,
                "parent_revision_id": revision.parent_revision_id,
            }),
            parts,
        )),
    }
}

fn kit(plan: &PlanHistory) -> Result<Value, Refusal> {
    let mut bytes = Vec::new();
    plan.kit.write_json(&mut bytes);
    serde_json::from_slice(&bytes).map_err(|_| INTERNAL)
}

fn snapshot(plan: &PlanHistory) -> Result<Value, Refusal> {
    let profile = accepted_profile(&plan.accepted)?;
    let kit = kit(plan)?;
    let layers = plan
        .layers
        .iter()
        .map(|layer| {
            json!({
                "id": layer.id, "order": layer.layer_order, "type": layer.layer_type,
                "source": layer.project_name, "project_id": layer.project_id,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "id": profile.id, "name": profile.name, "build_stale": plan.build_stale,
        "part_count": accepted_parts(&plan.accepted).len(), "layers": layers,
        "kit_selections": kit["selections"], "kit_name": kit["name"],
        "inferred_stack_preset": plan.inferred_stack_preset,
        "basis": basis(&plan.accepted), "revisions": plan.revisions, "draft": plan.draft,
    }))
}

fn base_and_addons(plan: &PlanHistory) -> (Option<&str>, BTreeSet<&str>) {
    let (mut base, mut addons) = (None, BTreeSet::new());
    for layer in &plan.layers {
        if layer.layer_type == "base" {
            base = base.or(layer.project_name.as_deref());
        } else if let Some(name) = layer.project_name.as_deref() {
            addons.insert(name);
        }
    }
    (base, addons)
}

fn source_refs(plan: &PlanHistory) -> BTreeMap<&str, Option<String>> {
    plan.source_refs
        .iter()
        .map(|source| {
            let reference = source
                .tag
                .as_ref()
                .map(|tag| format!("tag:{tag}"))
                .or_else(|| {
                    source
                        .branch
                        .as_ref()
                        .map(|branch| format!("branch:{branch}"))
                });
            (source.name.as_str(), reference)
        })
        .collect()
}

fn selected_variants(selection: &Value) -> BTreeSet<&str> {
    match selection {
        Value::Array(variants) => variants.iter().filter_map(Value::as_str).collect(),
        _ => selection.as_str().into_iter().collect(),
    }
}

fn selection_diff(a: &Value, b: &Value) -> Value {
    let (mut only_a, mut only_b, mut changed) = (Map::new(), Map::new(), Vec::new());
    for (key, value) in a.as_object().into_iter().flatten() {
        match b.get(key) {
            Some(other) if selected_variants(other) != selected_variants(value) => {
                changed.push(json!({"key": key, "a": value, "b": other}))
            }
            None => {
                only_a.insert(key.clone(), value.clone());
            }
            _ => {}
        }
    }
    for (key, value) in b
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| a.get(*key).is_none())
    {
        only_b.insert(key.clone(), value.clone());
    }
    json!({"only_a": only_a, "only_b": only_b, "changed": changed})
}

fn compare(a: &PlanHistory, b: &PlanHistory) -> Result<Value, Refusal> {
    let (profile_a, profile_b) = (
        accepted_profile(&a.accepted)?,
        accepted_profile(&b.accepted)?,
    );
    let ((base_a, addons_a), (base_b, addons_b)) = (base_and_addons(a), base_and_addons(b));
    let (refs_a, refs_b) = (source_refs(a), source_refs(b));
    let sources: BTreeSet<_> = refs_a.keys().chain(refs_b.keys()).collect();
    let refs = sources
        .into_iter()
        .map(|name| {
            let (a, b) = (
                refs_a.get(name).and_then(Option::as_deref),
                refs_b.get(name).and_then(Option::as_deref),
            );
            json!({"source": name, "a": a, "b": b, "same": a == b})
        })
        .collect::<Vec<_>>();
    let base = |name: Option<&str>, refs: &BTreeMap<&str, Option<String>>| {
        name.map(|name| {
            match refs
                .get(name)
                .and_then(Option::as_deref)
                .and_then(|r| r.strip_prefix("tag:"))
            {
                Some(tag) => format!("{name}@{tag}"),
                None => name.to_owned(),
            }
        })
    };
    let (base_a, base_b) = (base(base_a, &refs_a), base(base_b, &refs_b));
    let decisions = |p: &PlanHistory| {
        p.decisions
            .iter()
            .map(|d| json!({"label": d.label, "kind": d.kind, "summary": d.summary}))
            .collect::<Vec<_>>()
    };
    let parts = |left: &AcceptedRead, right: &AcceptedRead| {
        let other: BTreeSet<_> = accepted_parts(right)
            .iter()
            .map(|part| part.part_key.as_str())
            .collect();
        let differing = accepted_parts(left)
            .iter()
            .filter(|part| !other.contains(part.part_key.as_str()));
        json!({"count": differing.clone().count(), "sample": differing.take(20)
            .map(PartIdentity::from).collect::<Vec<_>>()})
    };
    Ok(json!({
        "plan_a": {"id": profile_a.id, "name": profile_a.name, "basis": basis(&a.accepted), "build_stale": a.build_stale},
        "plan_b": {"id": profile_b.id, "name": profile_b.name, "basis": basis(&b.accepted), "build_stale": b.build_stale},
        "base": {"a": base_a, "b": base_b, "same": base_a == base_b},
        "addons": {"only_a": addons_a.difference(&addons_b).collect::<Vec<_>>(),
            "only_b": addons_b.difference(&addons_a).collect::<Vec<_>>(),
            "shared": addons_a.intersection(&addons_b).collect::<Vec<_>>()},
        "refs": refs, "kit_selections": selection_diff(&kit(a)?["selections"], &kit(b)?["selections"]),
        "recent_decisions": {"a": decisions(a), "b": decisions(b)},
        "parts": {"only_a": parts(&a.accepted, &b.accepted), "only_b": parts(&b.accepted, &a.accepted)},
    }))
}
