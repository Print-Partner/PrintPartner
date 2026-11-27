//! Authenticated, read-only views of Plan configuration and revision identities.

use super::super::{
    AcceptedRead, Credential, ReadClient, RevisionRef, accepted_read,
    context::{self, FreshnessFacts},
    graph::{self, Budget, one, rows},
    planning::{self, PlanDecision},
};
use crate::{
    WriterFailure,
    build_graph::{PositiveId, ProfileLayer, layers},
    manifest_options::KitManifest,
    required_units::storage as units,
};
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;
use std::{collections::HashMap, sync::atomic::AtomicBool, time::Duration};

pub struct PlanHistory {
    pub accepted: AcceptedRead,
    pub build_stale: bool,
    pub inferred_stack_preset: Option<String>,
    pub layers: Vec<ProfileLayer>,
    pub kit: KitManifest,
    pub revisions: Vec<RevisionRef>,
    pub draft: Option<DraftBasis>,
    pub decisions: Vec<PlanDecision>,
    pub source_refs: Vec<SourceRef>,
}

#[derive(Serialize)]
pub struct DraftBasis {
    pub draft_id: i64,
    pub base_revision_id: Option<i64>,
    pub stale: bool,
}

pub struct SourceRef {
    pub name: String,
    pub branch: Option<String>,
    pub tag: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct PartIdentity {
    pub part_id: Option<i64>,
    pub revision_part_id: i64,
    pub part_key: String,
    pub filename: String,
    pub relative_path: String,
    pub role: String,
    pub included: bool,
    pub required_unit_tokens: Vec<String>,
}

impl From<&super::super::Part> for PartIdentity {
    fn from(part: &super::super::Part) -> Self {
        Self {
            part_id: Some(part.projection_part_id),
            revision_part_id: part.revision_part_id,
            part_key: part.part_key.clone(),
            filename: part.filename.clone(),
            relative_path: part.relative_path.clone(),
            role: part.effective_role.clone(),
            included: part.included,
            required_unit_tokens: part
                .units
                .iter()
                .filter(|unit| unit.required)
                .map(|unit| unit.token.clone())
                .collect(),
        }
    }
}

pub enum RevisionRead {
    MissingBuild,
    MissingRevision,
    Ready {
        revision: RevisionRef,
        current: bool,
        parts: Vec<PartIdentity>,
    },
}

impl ReadClient {
    /// Both Plans in a comparison, including their identity and configuration,
    /// are read in the same snapshot as the credential check.
    pub fn plan_history(
        &self,
        credential: Credential,
        build: PositiveId,
        other: Option<PositiveId>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<(Option<PlanHistory>, Option<PlanHistory>), WriterFailure> {
        let (mut first, second) = self.storage.read(
            "plan_history.read",
            cancelled,
            wait,
            |tx| -> Result<_, WriterFailure> {
                let installation = crate::write::authorize(tx, &credential, self.policy)?;
                let tenant = installation.id();
                let mut budget = Budget::default();
                let mut read = |id: PositiveId| {
                    let id = id.get() as i64;
                    let accepted = accepted_read(tx, tenant, id, &self.repos, &mut budget)?;
                    current(
                        tx,
                        tenant,
                        id,
                        accepted,
                        &mut budget,
                        cancelled,
                        other.is_some(),
                    )
                };
                let first = read(build)?;
                let second = if first.is_some() {
                    other.map(read).transpose()?.flatten()
                } else {
                    None
                };
                Ok((first, second))
            },
        )??;
        if other.is_none()
            && let Some(plan) = &mut first
        {
            let base = plan
                .layers
                .iter()
                .find(|layer| layer.layer_type == "base")
                .and_then(|layer| layer.project_name.as_deref());
            if let (Some(base), Some(data)) = (base, self.repos.parent()) {
                let addons = plan
                    .layers
                    .iter()
                    .filter(|layer| layer.layer_type != "base")
                    .filter_map(|layer| layer.project_name.as_deref())
                    .collect::<Vec<_>>();
                plan.inferred_stack_preset =
                    crate::working_drafts::source_reads::infer_stack_preset(data, base, &addons);
            }
        }
        Ok((first, second))
    }

    pub fn plan_revision(
        &self,
        credential: Credential,
        build: PositiveId,
        revision: PositiveId,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<RevisionRead, WriterFailure> {
        self.storage
            .read("plan_history.revision", cancelled, wait, |tx| {
                let installation = crate::write::authorize(tx, &credential, self.policy)?;
                revision_read(
                    tx,
                    installation.id(),
                    build.get() as i64,
                    revision.get() as i64,
                )
                .map_err(WriterFailure::from)
            })?
    }
}

fn revision_ref(row: &graph::Row) -> Result<RevisionRef> {
    Ok(RevisionRef {
        id: row.n("id")?,
        parent_revision_id: row.on("parent_revision_id")?,
        revision_number: row.n("revision_number")?,
        snapshot_digest: row.s("snapshot_digest")?.into(),
        accepted_at: row.s("accepted_at")?.into(),
    })
}

fn current(
    tx: &Transaction<'_>,
    tenant: &str,
    id: i64,
    accepted: AcceptedRead,
    budget: &mut Budget,
    cancelled: &AtomicBool,
    compare: bool,
) -> Result<Option<PlanHistory>> {
    let Some(facts) = context::capture_build_summary(tx, tenant, id, &accepted, budget)? else {
        return Ok(None);
    };
    let build_stale = match facts.freshness {
        FreshnessFacts::Stale { .. } => true,
        FreshnessFacts::Unavailable { .. } => anyhow::bail!("Plan freshness is unavailable"),
        _ => false,
    };
    let revision_rows = rows(
        tx,
        "plan_revisions",
        "tenant_id=?1 AND profile_id=?2",
        &[&tenant, &id],
        "revision",
        budget,
    )?;
    let mut revisions = revision_rows
        .iter()
        .map(revision_ref)
        .collect::<Result<Vec<_>>>()?;
    revisions.sort_unstable_by_key(|rev| rev.revision_number);
    let draft = tx.query_row(
        "SELECT d.id,d.base_revision_id,
                d.base_revision_id IS NOT b.accepted_plan_revision_id OR d.base_plan_version != b.accepted_plan_version
         FROM plan_drafts d JOIN build_profiles b ON b.id=d.profile_id AND b.tenant_id=d.tenant_id
         WHERE d.tenant_id=?1 AND d.profile_id=?2 AND d.state='open' ORDER BY d.id DESC LIMIT 1",
        params![tenant, id],
        |row| Ok(DraftBasis { draft_id: row.get(0)?, base_revision_id: row.get(1)?, stale: row.get(2)? }),
    ).optional()?;
    let kit = crate::build_graph::manifest_options::read_kit(tx, tenant, id)?;
    let layers = layers(tx, tenant, id, cancelled)?;
    let mut source_refs = Vec::new();
    if compare {
        let mut read_ref =
            tx.prepare("SELECT name,branch,tag FROM projects WHERE tenant_id=? AND id=?")?;
        for source in layers.iter().filter_map(|layer| layer.project_id) {
            if let Some(reference) = read_ref
                .query_row(params![tenant, source], |row| {
                    Ok(SourceRef {
                        name: row.get(0)?,
                        branch: row.get(1)?,
                        tag: row.get(2)?,
                    })
                })
                .optional()?
            {
                source_refs.push(reference);
            }
        }
    }
    Ok(Some(PlanHistory {
        accepted,
        layers,
        kit,
        revisions,
        draft,
        source_refs,
        build_stale,
        inferred_stack_preset: None,
        decisions: if compare {
            planning::decisions(tx, tenant, id, 8)?
        } else {
            Vec::new()
        },
    }))
}

fn revision_read(tx: &Transaction<'_>, tenant: &str, build: i64, id: i64) -> Result<RevisionRead> {
    let budget = &mut Budget::default();
    let Some(profile) = one(
        tx,
        "build_profiles",
        "tenant_id=?1 AND id=?2",
        &[&tenant, &build],
        "pointer",
        budget,
    )?
    else {
        return Ok(RevisionRead::MissingBuild);
    };
    let Some(row) = one(
        tx,
        "plan_revisions",
        "tenant_id=?1 AND profile_id=?2 AND id=?3",
        &[&tenant, &build, &id],
        "revision",
        budget,
    )?
    else {
        return Ok(RevisionRead::MissingRevision);
    };
    let revision = revision_ref(&row)?;
    let parts = graph::verified_revision_parts(tx, tenant, id, &revision.snapshot_digest, budget)?;
    let mut mapped = units::base(tx, tenant, build, Some(id))?
        .map(|(_, parts)| {
            parts
                .into_iter()
                .map(|part| (part.id, part.units))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let parts = parts
        .iter()
        .map(|row| {
            let revision_part_id = row.n("id")?;
            let included = row.n("included")? != 0;
            Ok(PartIdentity {
                part_id: row.on("projection_part_id")?,
                revision_part_id,
                part_key: row.s("part_key")?.into(),
                filename: row.s("filename")?.into(),
                relative_path: row.s("relative_path")?.into(),
                role: row
                    .os("role_override")?
                    .unwrap_or(row.s("role_inferred")?.into()),
                included,
                required_unit_tokens: mapped
                    .remove(&revision_part_id)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|_| included)
                    .map(|unit| unit.token)
                    .collect(),
            })
        })
        .collect::<Result<_>>()?;
    Ok(RevisionRead::Ready {
        revision,
        current: profile.on("accepted_plan_revision_id")? == Some(id),
        parts,
    })
}
