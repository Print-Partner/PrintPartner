use super::{Basis, Integrity, accepted};
use crate::{
    WriterFailure,
    production::layout::{Placement, ValidatedPlate},
    read_model::{AcceptedRead, Credential, ReadClient, Snapshot},
    write::authorize,
};
use rusqlite::{Transaction, params};
use std::{collections::HashMap, sync::atomic::AtomicBool, time::Duration};

pub struct ManagedExport {
    pub plan: Snapshot,
    pub plate_revision_id: i64,
    pub plate_revision_number: i64,
    pub layout_digest: String,
    pub plates: Vec<ValidatedPlate>,
    pub materials: HashMap<i64, AcceptedMaterial>,
}

pub struct AcceptedMaterial {
    pub color_id: Option<String>,
    pub custom_hex: Option<String>,
}

#[derive(Debug)]
pub enum ManagedExportFailure {
    Storage(WriterFailure),
    BuildNotFound,
    NoAcceptedPlan,
    PlanUnavailable,
    PlatesNotPublished,
    StalePlan,
    RevisionChanged,
    InvalidLayout,
    UnplacedUnits,
    TooManyObjects,
}

impl ReadClient {
    /// Captures the current Accepted Plan and its expected managed Plate revision
    /// in one authorized reader transaction. No files are opened by this read.
    pub fn read_managed_export(
        &self,
        credential: Credential,
        profile: Option<i64>,
        expected_revision: i64,
        max_objects: usize,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<ManagedExport, ManagedExportFailure> {
        use ManagedExportFailure as Failure;
        self.storage
            .read("plates.read_managed_export", cancelled, wait, |tx| {
                let owner = authorize(tx, &credential, self.policy).map_err(Failure::Storage)?;
                let profile = profile.ok_or(Failure::BuildNotFound)?;
                let repos = &self.repos;
                let plan = accepted(tx, owner.id(), profile, repos)
                    .map_err(|err| Failure::Storage(WriterFailure::from(err)))?
                    .map_err(|state| match state {
                        AcceptedRead::Missing => Failure::BuildNotFound,
                        AcceptedRead::Empty { .. } => Failure::NoAcceptedPlan,
                        _ => Failure::PlanUnavailable,
                    })?;
                let Some(id) = super::head(tx, owner.id(), profile)
                    .map_err(|err| Failure::Storage(WriterFailure::from(err)))?
                else {
                    return Err(Failure::PlatesNotPublished);
                };
                if id != expected_revision {
                    return Err(Failure::RevisionChanged);
                }
                let revision = super::revision(tx, owner.id(), profile, id)
                    .map_err(|err| Failure::Storage(WriterFailure::from(err)))?
                    .ok_or(Failure::InvalidLayout)?;
                if !Basis::of(&plan).stamps(&revision) {
                    return Err(Failure::StalePlan);
                }
                if revision.unit_count > max_objects as i64 {
                    return Err(Failure::TooManyObjects);
                }
                let plates = super::tokens(&plan)
                    .and_then(|tokens| super::checked_layout(tx, owner.id(), &revision, &tokens))
                    .map_err(|err| {
                        if err.is::<Integrity>() {
                            Failure::InvalidLayout
                        } else {
                            Failure::Storage(WriterFailure::from(err))
                        }
                    })?;
                if plates.iter().any(|plate| {
                    plate
                        .input
                        .units
                        .iter()
                        .any(|unit| unit.placement == Placement::Unplaced)
                }) {
                    return Err(Failure::UnplacedUnits);
                }
                let materials = materials(tx, owner.id(), revision.plan_revision_id)
                    .map_err(|error| Failure::Storage(WriterFailure::from(error)))?;
                Ok(ManagedExport {
                    plan,
                    plate_revision_id: id,
                    plate_revision_number: revision.number,
                    layout_digest: revision.layout_digest,
                    plates,
                    materials,
                })
            })
            .map_err(Failure::Storage)?
    }
}

fn materials(
    tx: &Transaction<'_>,
    tenant: &str,
    plan_revision_id: i64,
) -> rusqlite::Result<HashMap<i64, AcceptedMaterial>> {
    tx.prepare(
        "SELECT id,filament_color_id,filament_custom_hex FROM plan_revision_parts
         WHERE tenant_id=?1 AND revision_id=?2 AND included",
    )?
    .query_map(params![tenant, plan_revision_id], |row| {
        Ok((
            row.get(0)?,
            AcceptedMaterial {
                color_id: row.get(1)?,
                custom_hex: row.get(2)?,
            },
        ))
    })?
    .collect()
}
