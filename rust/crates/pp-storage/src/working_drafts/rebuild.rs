//! Rebuild a Build's Working Plan from the Source roles in its planning brief
//! (GRE-1518), the operation behind MCP `propose_rebuild_plan`. One write
//! transaction sets the base and addon layers from the roles, prepares and
//! stores a draft as the recompute route does (reading the Source files inside
//! the transaction), and records the draft in the brief, so a refusal changes
//! nothing. Path choices and detaching earlier layers come in GRE-1434 PR 2.
//! Errors are [`RebuildError`]; `pp-api` maps them.

use super::{
    Origin, RecomputeOutcome, WorkingDraftClient, abandon_open, baseline_required,
    carry_open_edits, idempotency_key, insert_snapshot, observation, preparation, snapshot, winner,
};
use crate::{
    WriterFailure, auth,
    build_graph::{
        self, BuildCommand, BuildOutcome,
        create_with_brief::{brief_key, save},
    },
    read_model::{
        Credential,
        planning::{BuildPlanningBrief, ContributionStatus, SourceRole},
    },
    required_units::{
        model,
        storage::{draft, one},
    },
    write::authorize,
};
use pp_domain::PositiveId;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeSet, sync::atomic::AtomicBool, time::Duration};

/// The caller's idempotency key, checked. Confirm passes the proposal's key,
/// or else its action id.
#[derive(Clone, Debug)]
pub struct RebuildRequest {
    key: String,
}

impl RebuildRequest {
    /// Room for the `:<inputs>` suffix inside the draft key's 160 units.
    const MAX_KEY: usize = 143;

    pub fn new(key: &str) -> Result<Self, RebuildError> {
        let key = key.trim();
        if key.is_empty() || key.chars().count() > Self::MAX_KEY {
            return Err(RebuildError::InvalidIdempotencyKey);
        }
        Ok(Self {
            key: key.to_owned(),
        })
    }
}

/// A Source the rebuild used, and the revision its draft was prepared from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebuildSource {
    pub source_id: i64,
    pub role: SourceRole,
    /// The Source's last synced commit, when it has one.
    pub revision: Option<String>,
}

/// The selected draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rebuilt {
    pub draft_id: PositiveId,
    pub snapshot_digest: String,
    /// `false` when the same key and inputs found the draft an earlier
    /// rebuild created.
    pub created: bool,
    pub sources: Vec<RebuildSource>,
}

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("the idempotency key is empty or too long")]
    InvalidIdempotencyKey,
    #[error("the Build has no planning brief")]
    NoBrief,
    /// The stored brief does not parse or names another Build.
    #[error("the stored planning brief is invalid")]
    BriefInvalid,
    #[error("no Source has the structural_base role")]
    NoStructuralBase,
    #[error("more than one Source has the structural_base role")]
    MultipleStructuralBases,
    /// The brief resolves a difference or scopes a contribution to paths.
    #[error("the brief holds path choices, which the rebuild does not apply yet")]
    SourceChoicesUnsupported,
    #[error("a Source the brief names does not exist")]
    SourceNotFound(i64),
    /// A role Source is attached in a layer the rebuild cannot reuse.
    #[error("a Source is already attached in another layer")]
    LayerConflict(i64),
    #[error("the Build needs an Accepted Plan first")]
    AcceptedBaselineRequired,
    #[error("the Sources have no printable files")]
    NoStls,
    #[error("the rebuild would remove every Part")]
    WouldWipe,
    /// The key made a draft that was not a rebuild of these inputs.
    #[error("the idempotency key belongs to another draft")]
    IdempotencyConflict,
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

impl From<rusqlite::Error> for RebuildError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Writer(error.into())
    }
}

/// An `anyhow` error from an older helper, sorted by type.
fn internal(error: anyhow::Error) -> RebuildError {
    RebuildError::Writer(WriterFailure::from(error))
}

impl WorkingDraftClient {
    pub fn rebuild_from_brief(
        &self,
        credential: Credential,
        build: PositiveId,
        request: RebuildRequest,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Rebuilt, RebuildError> {
        let policy = self.policy;
        let observations = self
            .observations
            .clone()
            .ok_or_else(|| internal(anyhow::anyhow!("draft observations are not configured")))?;
        let name = "working_drafts.rebuild_from_brief";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let tenant = installation.id();
            let (_, actor) =
                auth::authority::queries::reconciliation_actor_ref(tx, &credential, policy)
                    .map_err(internal)?;
            let profile = i64::try_from(build.get()).map_err(|_| RebuildError::BriefInvalid)?;
            let mut brief = stored_brief(tx, tenant, build)?;
            let sources = role_sources(tx, tenant, &brief)?;
            configure_layers(tx, tenant, build, &sources, &observations.repos)?;
            let key = format!("{}:{}", request.key, inputs_digest(&sources));
            let key = idempotency_key(&key).map_err(|_| RebuildError::InvalidIdempotencyKey)?;
            let (document, created) =
                match winner(tx, tenant, &actor, profile, &key).map_err(internal)? {
                    Some(RecomputeOutcome::Existing { draft }) => (draft, false),
                    Some(_) => return Err(RebuildError::IdempotencyConflict),
                    None => {
                        let flag = AtomicBool::new(false);
                        let id = prepare_and_insert(
                            tx,
                            tenant,
                            &actor,
                            profile,
                            &key,
                            &observations,
                            &flag,
                        )?;
                        let saved = draft(tx, tenant, profile, id)
                            .map_err(internal)?
                            .ok_or_else(|| internal(anyhow::anyhow!("created draft is missing")))?;
                        (snapshot(&saved).map_err(internal)?, true)
                    }
                };
            let identity = document.identity();
            brief.draft_id = Some(
                i64::try_from(identity.draft_id().get()).map_err(|_| RebuildError::BriefInvalid)?,
            );
            brief.managed_source_ids = Some(sources.iter().map(|s| s.source_id).collect());
            brief.draft_source_revisions = Some(
                sources
                    .iter()
                    .filter_map(|s| Some((s.source_id.to_string(), s.revision.clone()?)))
                    .collect(),
            );
            brief.updated_at = auth::catalog_timestamp();
            let text = brief.stored_text().map_err(WriterFailure::from)?;
            save(tx, tenant, &brief_key(build), &text)?;
            Ok(Rebuilt {
                draft_id: identity.draft_id(),
                snapshot_digest: identity.snapshot_digest().as_str().to_owned(),
                created,
                sources,
            })
        })?
    }
}

fn stored_brief(
    tx: &Transaction<'_>,
    tenant: &str,
    build: PositiveId,
) -> Result<BuildPlanningBrief, RebuildError> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, brief_key(build)],
            |row| row.get(0),
        )
        .optional()?;
    let raw = raw.ok_or(RebuildError::NoBrief)?;
    let id = i64::try_from(build.get()).map_err(|_| RebuildError::BriefInvalid)?;
    BuildPlanningBrief::parse(&raw, id).map_err(|_| RebuildError::BriefInvalid)
}

/// The brief's role Sources with their synced revisions: the structural base
/// first, then overlay and addon Sources in brief order.
fn role_sources(
    tx: &Transaction<'_>,
    tenant: &str,
    brief: &BuildPlanningBrief,
) -> Result<Vec<RebuildSource>, RebuildError> {
    let path_scoped = brief.contributions.iter().any(|contribution| {
        contribution.status == ContributionStatus::Confirmed && !contribution.path_scopes.is_empty()
    });
    if !brief.resolutions.is_empty() || path_scoped {
        return Err(RebuildError::SourceChoicesUnsupported);
    }
    let mut bases = Vec::new();
    let mut layered = Vec::new();
    let mut seen = BTreeSet::new();
    for item in &brief.evidence {
        let (Some(id), Some(role)) = (item.source_id, item.source_role) else {
            continue;
        };
        match role {
            SourceRole::StructuralBase => bases.push(id),
            SourceRole::Overlay | SourceRole::Addon if seen.insert(id) => layered.push((id, role)),
            _ => {}
        }
    }
    let base = match bases.as_slice() {
        [] => return Err(RebuildError::NoStructuralBase),
        [base] => *base,
        _ => return Err(RebuildError::MultipleStructuralBases),
    };
    std::iter::once((base, SourceRole::StructuralBase))
        .chain(layered.into_iter().filter(|(id, _)| *id != base))
        .map(|(source_id, role)| {
            let row = one(
                tx,
                "SELECT last_synced_at,last_commit_sha FROM projects WHERE tenant_id=? AND id=?",
                &[&tenant, &source_id],
            )
            .map_err(internal)?
            .ok_or(RebuildError::SourceNotFound(source_id))?;
            let synced = row["lastSyncedAt"]
                .as_str()
                .is_some_and(|at| !at.is_empty());
            let revision = synced
                .then(|| row["lastCommitSha"].as_str().filter(|sha| !sha.is_empty()))
                .flatten()
                .map(str::to_owned);
            Ok(RebuildSource {
                source_id,
                role,
                revision,
            })
        })
        .collect()
}

/// Makes the structural base the base layer and attaches the other role
/// Sources, leaving layers that already hold them as they are.
fn configure_layers(
    tx: &Transaction<'_>,
    tenant: &str,
    build: PositiveId,
    sources: &[RebuildSource],
    repos: &std::path::Path,
) -> Result<(), RebuildError> {
    let flag = AtomicBool::new(false);
    for source in sources {
        let attached: Option<String> = tx
            .query_row(
                "SELECT layer_type FROM profile_layers WHERE tenant_id=?1 AND profile_id=?2 AND project_id=?3",
                params![tenant, i64::try_from(build.get()).unwrap_or(i64::MAX), source.source_id],
                |row| row.get(0),
            )
            .optional()?;
        let base = source.role == SourceRole::StructuralBase;
        match (attached.as_deref(), base) {
            (Some("base"), true) | (Some(_), false) => continue,
            (Some(_), true) => return Err(RebuildError::LayerConflict(source.source_id)),
            (None, _) => {}
        }
        let id = PositiveId::new(u64::try_from(source.source_id).unwrap_or(0))
            .map_err(|_| RebuildError::SourceNotFound(source.source_id))?;
        let command = if base {
            BuildCommand::SetBase { build, source: id }
        } else {
            BuildCommand::AttachAddon { build, source: id }
        };
        match build_graph::mutation(tx, tenant, &command, repos, &flag).map_err(internal)? {
            BuildOutcome::Layers { .. } => {}
            BuildOutcome::MissingSource => {
                return Err(RebuildError::SourceNotFound(source.source_id));
            }
            BuildOutcome::DuplicateAttachment { .. } => {
                return Err(RebuildError::LayerConflict(source.source_id));
            }
            _ => return Err(internal(anyhow::anyhow!("unexpected layer outcome"))),
        }
    }
    Ok(())
}

/// The role Sources and their revisions. A rebuild with the same key and
/// these inputs answers the draft it made before.
fn inputs_digest(sources: &[RebuildSource]) -> String {
    let inputs: Vec<_> = sources
        .iter()
        .map(|s| json!([s.source_id, s.role, s.revision]))
        .collect();
    hex::encode(Sha256::digest(json!(inputs).to_string().as_bytes()))[..16].to_owned()
}

/// Prepares a draft from the Build's layers inside `tx` and stores it, as
/// the recompute route does. Answers the new draft's id.
fn prepare_and_insert(
    tx: &Transaction<'_>,
    tenant: &str,
    actor: &str,
    profile: i64,
    key: &str,
    observations: &super::Observations,
    flag: &AtomicBool,
) -> Result<i64, RebuildError> {
    let profile_row = one(
        tx,
        "SELECT * FROM build_profiles WHERE tenant_id=? AND id=?",
        &[&tenant, &profile],
    )
    .map_err(internal)?
    .ok_or_else(|| internal(anyhow::anyhow!("Build is missing")))?;
    if baseline_required(tx, tenant, &profile_row).map_err(internal)? {
        return Err(RebuildError::AcceptedBaselineRequired);
    }
    let base = json!({
        "baseRevisionId": profile_row["acceptedPlanRevisionId"],
        "basePlanVersion": profile_row["acceptedPlanVersion"],
    });
    let mut budget = observation::PreparationBudget::new(flag, observations.limits)
        .map_err(|failure| internal(anyhow::Error::new(failure)))?;
    let options = preparation::PreparationOptions::default();
    let prepared = preparation::prepare(
        &mut &*tx,
        tenant,
        profile,
        base,
        preparation::PreparationContext {
            options: &options,
            repos: &observations.repos,
        },
        observations.reads.as_ref(),
        &mut budget,
    );
    let mut prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return Err(match error.downcast_ref() {
                Some(preparation::Failure::NoStls) => RebuildError::NoStls,
                Some(preparation::Failure::WouldWipe) => RebuildError::WouldWipe,
                // `NoLayers` cannot follow: the base layer was just set.
                _ => internal(error),
            });
        }
    };
    if carry_open_edits(tx, tenant, profile, &mut prepared.parts, false, &mut budget)
        .map_err(internal)?
    {
        prepared.digest =
            model::planning(&prepared.base, &prepared.capture.inputs, &prepared.parts)
                .map_err(internal)?;
    }
    let open = abandon_open(tx, tenant, profile).map_err(internal)?;
    let origin = open.last().copied().map_or(Origin::New, Origin::Draft);
    insert_snapshot(tx, tenant, actor, profile, key, prepared.rows(), origin).map_err(internal)
}
