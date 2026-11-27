use super::{AcceptedRead, Credential, ReadClient, Snapshot, planning::FilamentInventoryKind};
use crate::{
    WriterFailure,
    build_graph::{
        PositiveId, RoleFilamentError, create_with_brief::stored_brief, role_filament::role_key,
    },
};
use rusqlite::Transaction;
use std::{collections::HashMap, sync::atomic::AtomicBool, time::Duration};

pub struct DirectExportPlan {
    pub accepted: AcceptedRead,
    pub spoolman_colors: HashMap<i64, String>,
}

impl ReadClient {
    pub fn read_direct_export(
        &self,
        credential: Credential,
        profile_id: Option<i64>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<DirectExportPlan, WriterFailure> {
        self.with_plan(
            "read_model.direct_export",
            credential,
            profile_id,
            cancelled,
            wait,
            |tx, tenant, accepted| {
                let spoolman_colors = match &accepted {
                    AcceptedRead::Ready { snapshot } => spoolman_colors(tx, tenant, snapshot)?,
                    _ => HashMap::new(),
                };
                Ok(DirectExportPlan {
                    accepted,
                    spoolman_colors,
                })
            },
        )
    }
}

fn spoolman_colors(
    tx: &Transaction<'_>,
    tenant: &str,
    plan: &Snapshot,
) -> Result<HashMap<i64, String>, WriterFailure> {
    let parts = plan.parts.iter().filter_map(|part| {
        let id = super::views::js_trim(part.filament_color_id.as_deref()?);
        super::stock::spoolman_ref(id, ":filament:")?;
        Some((part, id))
    });
    let mut parts = parts.peekable();
    if parts.peek().is_none() {
        return Ok(HashMap::new());
    }
    let id =
        u64::try_from(plan.profile.id).map_err(|error| WriterFailure::Internal(error.into()))?;
    let build =
        PositiveId::new(id).map_err(|error| WriterFailure::Internal(anyhow::Error::msg(error)))?;
    let brief = match stored_brief::<RoleFilamentError>(tx, tenant, build) {
        Ok(brief) => brief,
        Err(RoleFilamentError::Writer(error)) => return Err(error),
        Err(_) => return Ok(HashMap::new()),
    };
    let colors: HashMap<_, _> = brief
        .role_filaments
        .into_iter()
        .filter_map(|assignment| {
            if assignment.inventory_kind != FilamentInventoryKind::Spoolman {
                return None;
            }
            Some((
                (role_key(&assignment.role), assignment.inventory_id?),
                assignment.color_hex,
            ))
        })
        .collect();
    Ok(parts
        .filter_map(|(part, id)| {
            let color = colors.get(&(role_key(&part.effective_role), id.to_owned()))?;
            Some((part.projection_part_id, color.clone()))
        })
        .collect())
}
