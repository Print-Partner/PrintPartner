//! Assign the filament one role of a Build prints in (GRE-1435), the
//! operation behind MCP `propose_assign_role_filament`.
//!
//! [`BuildGraphClient::assign_role_filament`] runs in one write transaction
//! on the write seam: prove the credential, read the Build's planning brief,
//! save the role's default in `role_filaments_<build>`, record the
//! assignment in the brief, mark its `color_<role>` requirement satisfied,
//! clear the selected draft and store the brief. The caller verifies a
//! catalog or Spoolman item before calling: no network read happens here.
//! [`BuildGraphClient::check_role_filament`] proves the credential and finds
//! the brief on the read seam, and writes nothing.
//!
//! Errors are [`RoleFilamentError`]. This module holds no status codes and
//! no response text; `pp-api` maps the variants.

use super::{
    BuildGraphClient,
    create_with_brief::{BriefError, save, store_changed_brief, stored_brief},
};
use crate::{
    WriterFailure,
    read_model::{
        Credential,
        planning::{FilamentInventoryKind, RequirementStatus, RoleFilamentAssignment},
    },
    write::authorize,
};
use pp_domain::{PositiveId, build_identity::trim_js};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::atomic::AtomicBool, time::Duration};

/// One role's filament default, as Node stores it in `role_filaments_<build>`.
/// Every field is written, `null` included. A new draft Part of the role
/// takes these values (`working_drafts::preparation`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleFilamentDefault {
    pub filament_color_id: Option<String>,
    pub filament_custom_hex: Option<String>,
    pub spoolman_spool_id: Option<String>,
}

/// The `role_filaments_<build>` document: each role's default, keyed by the
/// role trimmed and lowercased, as the draft reader looks it up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RoleFilaments(BTreeMap<String, RoleFilamentDefault>);

impl RoleFilaments {
    /// Reads the stored document as Node's `loadRoleFilamentDefaults` does:
    /// unreadable text is no defaults, an entry that is not an object is
    /// skipped, and a field that is not a string reads as `null`. Keys are
    /// trimmed and lowercased, the form the draft reader looks up; of two keys
    /// that become one, the later wins.
    pub fn parse(raw: &str) -> Self {
        let Ok(Value::Object(entries)) = serde_json::from_str::<Value>(raw) else {
            return Self::default();
        };
        let text = |row: &Value, key: &str| row[key].as_str().map(str::to_owned);
        Self(
            entries
                .into_iter()
                .filter(|(_, row)| row.is_object())
                .map(|(role, row)| {
                    let default = RoleFilamentDefault {
                        filament_color_id: text(&row, "filament_color_id"),
                        filament_custom_hex: text(&row, "filament_custom_hex"),
                        spoolman_spool_id: text(&row, "spoolman_spool_id"),
                    };
                    (role_key(&role), default)
                })
                .collect(),
        )
    }

    pub fn get(&self, role: &str) -> Option<&RoleFilamentDefault> {
        self.0.get(&role_key(role))
    }

    fn set(&mut self, role: &str, default: RoleFilamentDefault) {
        self.0.insert(role_key(role), default);
    }
}

fn role_key(role: &str) -> String {
    trim_js(role).to_lowercase()
}

#[derive(Debug, thiserror::Error)]
pub enum RoleFilamentError {
    #[error("the Build has no planning brief")]
    NoBrief,
    /// The stored brief does not parse or names another Build.
    #[error("the stored planning brief is invalid")]
    BriefInvalid,
    /// A blank role or a colour that is not `#rrggbb`; `pp-api` checks both
    /// before calling, so this guards the write only.
    #[error("invalid role filament assignment")]
    InvalidAssignment,
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

impl From<rusqlite::Error> for RoleFilamentError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Writer(error.into())
    }
}

impl BriefError for RoleFilamentError {
    fn no_brief() -> Self {
        Self::NoBrief
    }
    fn invalid() -> Self {
        Self::BriefInvalid
    }
}

impl BuildGraphClient {
    /// Proves the credential and finds `build`'s brief in a read
    /// transaction. `Ok` is no promise: assigning reads the brief again.
    pub fn check_role_filament(
        &self,
        credential: Credential,
        build: PositiveId,
        wait: Duration,
    ) -> Result<(), RoleFilamentError> {
        let policy = self.policy;
        let name = "build_graph.check_role_filament";
        self.settings
            .read(name, &AtomicBool::new(false), wait, move |tx| {
                let installation = authorize(tx, &credential, policy)?;
                stored_brief::<RoleFilamentError>(tx, installation.id(), build).map(drop)
            })?
    }

    /// Saves `assignment` as its role's default and records it in `build`'s
    /// brief, in one transaction. A later assignment for the same role
    /// replaces an earlier one. Answers the assignment as stored.
    pub fn assign_role_filament(
        &self,
        credential: Credential,
        build: PositiveId,
        mut assignment: RoleFilamentAssignment,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<RoleFilamentAssignment, RoleFilamentError> {
        assignment.role = trim_js(&assignment.role).to_owned();
        let hex = assignment.color_hex.strip_prefix('#').unwrap_or_default();
        if assignment.role.is_empty()
            || hex.len() != 6
            || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(RoleFilamentError::InvalidAssignment);
        }
        let policy = self.policy;
        let name = "build_graph.assign_role_filament";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let tenant = installation.id();
            let mut brief = stored_brief::<RoleFilamentError>(tx, tenant, build)?;
            let key = format!("role_filaments_{}", build.get());
            let mut defaults = stored_defaults(tx, tenant, &key)?;
            defaults.set(&assignment.role, default_for(&assignment));
            let text = serde_json::to_string(&defaults)
                .map_err(|error| WriterFailure::from(anyhow::Error::from(error)))?;
            save(tx, tenant, &key, &text)?;
            let requirement = requirement_key(&assignment.role);
            for item in &mut brief.requirements {
                if item.key == requirement {
                    item.status = RequirementStatus::Satisfied;
                    item.detail = Some(format!(
                        "Assigned {} filament {}",
                        assignment.inventory_kind.as_str(),
                        assignment
                            .inventory_id
                            .as_deref()
                            .unwrap_or(&assignment.color_hex)
                    ));
                }
            }
            let role = role_key(&assignment.role);
            brief
                .role_filaments
                .retain(|item| role_key(&item.role) != role);
            brief.role_filaments.push(assignment.clone());
            store_changed_brief(tx, tenant, build, brief)?;
            Ok(assignment)
        })?
    }
}

fn stored_defaults(
    tx: &Transaction<'_>,
    tenant: &str,
    key: &str,
) -> rusqlite::Result<RoleFilaments> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, key],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw
        .map(|raw| RoleFilaments::parse(&raw))
        .unwrap_or_default())
}

/// A catalog or Spoolman assignment names its item; a custom colour or a
/// substitute names only its colour. No spool is ever pinned.
fn default_for(assignment: &RoleFilamentAssignment) -> RoleFilamentDefault {
    match assignment.inventory_kind {
        FilamentInventoryKind::Catalog | FilamentInventoryKind::Spoolman => RoleFilamentDefault {
            filament_color_id: assignment.inventory_id.clone(),
            ..RoleFilamentDefault::default()
        },
        FilamentInventoryKind::Custom | FilamentInventoryKind::Substitute => RoleFilamentDefault {
            filament_custom_hex: Some(assignment.color_hex.clone()),
            ..RoleFilamentDefault::default()
        },
    }
}

/// The brief's colour requirement for `role`: `color_` and the role
/// lowercased, each run of other characters made one `_`, as Node keys it.
fn requirement_key(role: &str) -> String {
    let mut key = String::from("color_");
    let mut gap = false;
    for character in role.to_lowercase().chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            key.push(character);
            gap = false;
        } else if !gap {
            key.push('_');
            gap = true;
        }
    }
    key
}

#[cfg(test)]
mod tests;
