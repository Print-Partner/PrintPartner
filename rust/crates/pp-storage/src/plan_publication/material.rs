use crate::{
    build_graph::{
        PositiveId, RoleFilamentError, create_with_brief::stored_brief, role_filament::role_key,
    },
    manifest_text::DraftPart,
    read_model::{planning::FilamentInventoryKind, stock::spoolman_ref, views::js_trim},
    required_units::model::role,
};
use anyhow::Result;
use rusqlite::Transaction;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct SavedColors(HashMap<String, (String, String)>);

impl SavedColors {
    pub(super) fn get(&self, part: &DraftPart) -> Option<&str> {
        let id = spoolman_id(part)?;
        let (saved_id, hex) = self.0.get(&role_key(role(part)))?;
        (saved_id == id).then_some(hex.as_str())
    }
}

fn spoolman_id(part: &DraftPart) -> Option<&str> {
    let id = js_trim(part["filamentColorId"].as_str()?);
    spoolman_ref(id, ":filament:")?;
    Some(id)
}

pub(super) fn saved_colors(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    parts: &[DraftPart],
) -> Result<SavedColors> {
    if !parts.iter().any(|part| spoolman_id(part).is_some()) {
        return Ok(SavedColors::default());
    }
    let build = PositiveId::new(u64::try_from(profile)?).map_err(anyhow::Error::msg)?;
    let brief = match stored_brief::<RoleFilamentError>(tx, tenant, build) {
        Ok(brief) => brief,
        Err(RoleFilamentError::NoBrief | RoleFilamentError::BriefInvalid) => {
            return Ok(SavedColors::default());
        }
        Err(error) => return Err(error.into()),
    };
    Ok(SavedColors(
        brief
            .role_filaments
            .into_iter()
            .filter_map(|assignment| {
                if assignment.inventory_kind != FilamentInventoryKind::Spoolman {
                    return None;
                }
                Some((
                    role_key(&assignment.role),
                    (assignment.inventory_id?, assignment.color_hex),
                ))
            })
            .collect(),
    ))
}
