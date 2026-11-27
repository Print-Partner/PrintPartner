//! Build history changes guarded by the Accepted head shown at proposal time.

use super::*;
use crate::{
    WriterFailure, plan_publication::snapshot, working_drafts::restore::restorable_revision,
};
pub use pp_contracts::autosave::PlanDraftBasis;
use pp_contracts::autosave::Version;
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    Snapshot { name: Option<BuildName> },
    Restore { snapshot_id: PositiveId },
    Duplicate { name: BuildName },
    Archive,
}

#[derive(Debug, thiserror::Error)]
pub enum Refusal {
    #[error("Plan not found")]
    Missing,
    #[error("The Accepted Plan changed; propose again")]
    Changed,
    #[error("This Plan is archived")]
    Archived,
    #[error("The Plan has no restorable Accepted revision")]
    NotRestorable,
    #[error("Snapshot not found for this Plan")]
    SnapshotMissing,
    #[error("Close the existing Draft before restoring a snapshot through MCP")]
    OpenDraft,
    #[error("A Build with that name already exists")]
    DuplicateName,
    #[error("Build limit reached")]
    BuildLimitReached,
    #[error(
        "The Build's Source layers differ from its Accepted Plan. Reconcile them before duplicating."
    )]
    ExperimentInputsChanged,
    #[error("The Plan cannot be archived")]
    Archive(ArchiveRefusal),
    #[error(transparent)]
    Storage(#[from] WriterFailure),
}

pub enum Applied {
    Snapshot { id: i64, name: String },
    Duplicate(ExperimentCopy),
    Archive,
}

impl BuildGraphClient {
    pub fn preview_history_change(
        &self,
        credential: read_model::Credential,
        plan: PositiveId,
        change: &Change,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> std::result::Result<PlanDraftBasis, Refusal> {
        self.settings
            .read("plan_history.preview", cancelled, wait, |tx| {
                let caller = crate::write::authorize(tx, &credential, self.policy)?;
                let tenant = caller.id();
                let plan = plan.get() as i64;
                let basis = basis(tx, tenant, plan, change)?;
                match change {
                    Change::Restore { snapshot_id } => {
                        restore_basis(tx, tenant, plan, snapshot_id.get() as i64)?;
                    }
                    Change::Archive => {
                        let profile = header::summary(tx, tenant, plan, &self.repos)
                            .map_err(WriterFailure::from)?
                            .ok_or(Refusal::Missing)?;
                        header::archive_allowed(tx, tenant, plan, &profile, &self.repos)
                            .map_err(WriterFailure::from)?
                            .map_err(Refusal::Archive)?;
                    }
                    _ => {}
                }
                Ok(basis)
            })?
    }

    pub fn apply_history_change(
        &self,
        credential: read_model::Credential,
        plan: PositiveId,
        expected: PlanDraftBasis,
        change: Change,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> std::result::Result<Applied, Refusal> {
        let policy = self.policy;
        let repos = self.repos.clone();
        self.shared
            .write("plan_history.apply", cancelled, wait, move |tx, _| {
                let caller = crate::write::authorize(tx, &credential, policy)?;
                let tenant = caller.id();
                let id = plan.get() as i64;
                if basis(tx, tenant, id, &change)? != expected {
                    return Err(Refusal::Changed);
                }
                match change {
                    Change::Snapshot { name } => capture(tx, tenant, id, &expected, name),
                    Change::Duplicate { name } => {
                        match experiments::duplicate(tx, tenant, plan, &name)
                            .map_err(WriterFailure::from)?
                        {
                            BuildOutcome::Duplicated(copy) => Ok(Applied::Duplicate(copy)),
                            BuildOutcome::RenameTaken { .. } => Err(Refusal::DuplicateName),
                            BuildOutcome::BuildLimitReached => Err(Refusal::BuildLimitReached),
                            BuildOutcome::ExperimentInputsChanged => {
                                Err(Refusal::ExperimentInputsChanged)
                            }
                            BuildOutcome::MissingBuild => Err(Refusal::Missing),
                            _ => {
                                Err(WriterFailure::from(anyhow!("Unexpected duplicate outcome"))
                                    .into())
                            }
                        }
                    }
                    Change::Archive => {
                        let patch = BuildPatch {
                            archive: true,
                            ..BuildPatch::default()
                        };
                        match header::update(tx, tenant, plan, &patch, false, &repos)
                            .map_err(WriterFailure::from)?
                        {
                            BuildOutcome::Touched { .. } => Ok(Applied::Archive),
                            BuildOutcome::ArchiveRefused(reason) => Err(Refusal::Archive(reason)),
                            BuildOutcome::MissingBuild => Err(Refusal::Missing),
                            _ => {
                                Err(WriterFailure::from(anyhow!("Unexpected archive outcome"))
                                    .into())
                            }
                        }
                    }
                    // Restore has its own draft writer and idempotency key.
                    Change::Restore { .. } => Err(WriterFailure::from(anyhow!(
                        "Restore requires the Draft writer"
                    ))
                    .into()),
                }
            })?
    }
}

fn basis(
    tx: &Transaction<'_>,
    tenant: &str,
    plan: i64,
    change: &Change,
) -> std::result::Result<PlanDraftBasis, Refusal> {
    let row = tx.query_row(
        "SELECT accepted_plan_revision_id,accepted_plan_version,archived_at IS NOT NULL FROM build_profiles WHERE tenant_id=?1 AND id=?2",
        params![tenant, plan],
        |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?, row.get::<_, bool>(2)?)),
    ).optional().map_err(WriterFailure::from)?;
    let (revision, version, archived) = row.ok_or(Refusal::Missing)?;
    if matches!(change, Change::Snapshot { .. } | Change::Restore { .. }) {
        if archived {
            return Err(Refusal::Archived);
        }
        if revision.is_none() {
            return Err(Refusal::NotRestorable);
        }
    }
    let checked = || {
        let revision = revision.map(|id| PositiveId::new(id as u64)).transpose()?;
        PlanDraftBasis::new(revision, Version::new(version as u64)?)
    };
    checked().map_err(|reason| WriterFailure::from(anyhow!(reason)).into())
}

fn restore_basis(
    tx: &Transaction<'_>,
    tenant: &str,
    plan: i64,
    id: i64,
) -> std::result::Result<(), Refusal> {
    let payload: String = tx.query_row(
        "SELECT payload_json FROM plan_snapshots WHERE tenant_id=?1 AND profile_id=?2 AND id=?3",
        params![tenant, plan, id],
        |row| row.get(0),
    ).optional().map_err(WriterFailure::from)?.ok_or(Refusal::SnapshotMissing)?;
    if restorable_revision(tx, tenant, plan, &payload)
        .map_err(WriterFailure::from)?
        .is_none()
    {
        return Err(Refusal::NotRestorable);
    }
    let open: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM plan_drafts WHERE tenant_id=?1 AND profile_id=?2 AND state='open')",
        params![tenant, plan],
        |row| row.get(0),
    ).map_err(WriterFailure::from)?;
    if open {
        return Err(Refusal::OpenDraft);
    }
    Ok(())
}

fn capture(
    tx: &Transaction<'_>,
    tenant: &str,
    plan: i64,
    basis: &PlanDraftBasis,
    name: Option<BuildName>,
) -> std::result::Result<Applied, Refusal> {
    let at = auth::catalog_timestamp();
    let name = name.map_or_else(
        || format!("Plan snapshot {at}"),
        |name| name.as_str().to_owned(),
    );
    let revision = basis.revision_id().map(|id| id.get() as i64);
    let layers = snapshot::replaced_layers(tx, tenant, plan, revision)
        .map_err(WriterFailure::from)?
        .ok_or(Refusal::NotRestorable)?;
    let id = snapshot::save(
        tx,
        tenant,
        plan,
        (revision, basis.plan_version().get() as i64),
        layers,
        (snapshot::Source::Assistant, &name, &at),
    )
    .map_err(WriterFailure::from)?;
    Ok(Applied::Snapshot { id, name })
}
