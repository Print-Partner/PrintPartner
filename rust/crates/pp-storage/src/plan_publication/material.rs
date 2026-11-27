use crate::{
    build_graph::{
        PositiveId, RoleFilamentError,
        create_with_brief::{save, stored_brief},
        role_filament::role_key,
    },
    manifest_text::DraftPart,
    read_model::{planning::FilamentInventoryKind, stock::spoolman_ref, views::js_trim},
    required_units::model::role,
};
use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Default, Deserialize, Serialize)]
pub(super) struct SavedColors(BTreeMap<String, BTreeMap<String, String>>);

impl SavedColors {
    pub(super) fn get(&self, part: &DraftPart) -> Option<&str> {
        let by_id = self.0.get(&role_key(role(part)))?;
        by_id.get(spoolman_id(part)?).map(String::as_str)
    }

    fn insert(&mut self, role: &str, id: String, hex: String) {
        self.0.entry(role_key(role)).or_default().insert(id, hex);
    }
}

pub(crate) fn experiment_color_key(build: i64) -> String {
    format!("experiment_spoolman_colors:{build}")
}

pub(crate) fn copy_experiment_colors(
    tx: &Transaction<'_>,
    tenant: &str,
    source: i64,
    target: i64,
    parts: &[DraftPart],
) -> Result<()> {
    let colors = saved_colors(tx, tenant, source, parts)?;
    let mut copied = SavedColors::default();
    for part in parts {
        if let (Some(id), Some(hex)) = (spoolman_id(part), colors.get(part)) {
            copied.insert(role(part), id.to_owned(), hex.to_owned());
        }
    }
    if !copied.0.is_empty() {
        save(
            tx,
            tenant,
            &experiment_color_key(target),
            &serde_json::to_string(&copied)?,
        )?;
    }
    Ok(())
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
    let copied: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, experiment_color_key(profile)],
            |row| row.get(0),
        )
        .optional()?;
    let mut colors: SavedColors = copied
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?
        .unwrap_or_default();
    let build = PositiveId::new(u64::try_from(profile)?).map_err(anyhow::Error::msg)?;
    let brief = match stored_brief::<RoleFilamentError>(tx, tenant, build) {
        Ok(brief) => brief,
        Err(RoleFilamentError::NoBrief | RoleFilamentError::BriefInvalid) => {
            return Ok(colors);
        }
        Err(error) => return Err(error.into()),
    };
    for assignment in brief.role_filaments {
        if assignment.inventory_kind == FilamentInventoryKind::Spoolman
            && let Some(id) = assignment.inventory_id
        {
            colors.insert(&assignment.role, id, assignment.color_hex);
        }
    }
    Ok(colors)
}
