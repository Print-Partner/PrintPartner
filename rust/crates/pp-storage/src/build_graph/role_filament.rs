//! Save a role's filament default and record it on the Build's planning
//! brief (GRE-1435), the operation behind MCP `propose_assign_role_filament`.
//!
//! [`BuildGraphClient::assign_role_filament`] runs in one write transaction:
//! prove the credential, replace that role's entry on the brief, mark its
//! `color_<role>` requirement satisfied when the brief has one, clear the
//! selected draft, and merge `role_filaments_<build>`. The catalog and
//! Spoolman checks happen in `pp-api` before this write, so the transaction
//! never waits on a network call.
//!
//! Errors are [`RoleFilamentError`]. This module holds no status codes and
//! no response text.

use super::{
    BuildGraphClient,
    create_with_brief::{BriefError, save, store_changed_brief, stored_brief},
};
use crate::{
    WriterFailure,
    read_model::{
        Credential,
        planning::{
            BuildPlanningBrief, FilamentInventoryKind, RequirementStatus, RoleFilamentAssignment,
        },
    },
    write::authorize,
};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use std::{sync::atomic::AtomicBool, time::Duration};

/// The role whose default was saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssignedRole {
    pub role: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RoleFilamentError {
    #[error("the Build has no planning brief")]
    NoBrief,
    /// The stored brief does not parse or names another Build.
    #[error("the stored planning brief is invalid")]
    BriefInvalid,
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
    /// transaction. `Ok` is no promise: confirm reads the brief again.
    pub fn check_role_filament(
        &self,
        credential: Credential,
        build: pp_domain::PositiveId,
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

    /// Replaces `assignment`'s role on the brief and in
    /// `role_filaments_<build>`, in one transaction.
    pub fn assign_role_filament(
        &self,
        credential: Credential,
        build: pp_domain::PositiveId,
        assignment: RoleFilamentAssignment,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<AssignedRole, RoleFilamentError> {
        let policy = self.policy;
        let name = "build_graph.assign_role_filament";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let installation = authorize(tx, &credential, policy)?;
            let tenant = installation.id();
            let mut brief = stored_brief::<RoleFilamentError>(tx, tenant, build)?;
            let role = assignment.role.clone();
            apply(&mut brief, &assignment);
            store_changed_brief(tx, tenant, build, brief)?;
            save_default(tx, tenant, build, &assignment)?;
            Ok(AssignedRole { role })
        })?
    }
}

/// Replaces the role's assignment and marks `color_<role>` satisfied when
/// that requirement is already on the brief. Other requirements stay.
fn apply(brief: &mut BuildPlanningBrief, assignment: &RoleFilamentAssignment) {
    let key = color_requirement_key(&assignment.role);
    let detail = format!(
        "Assigned {} filament {}",
        assignment.inventory_kind.as_str(),
        assignment
            .inventory_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .unwrap_or(&assignment.color_hex)
    );
    for requirement in &mut brief.requirements {
        if requirement.key == key {
            requirement.status = RequirementStatus::Satisfied;
            requirement.detail = Some(detail.clone());
        }
    }
    brief
        .role_filaments
        .retain(|item| item.role != assignment.role);
    brief.role_filaments.push(assignment.clone());
}

/// `color_` plus Node's `role.toLowerCase().replace(/[^a-z0-9]+/g, "_")`.
fn color_requirement_key(role: &str) -> String {
    format!("color_{}", js_non_alnum_slug(&role.to_lowercase()))
}

fn js_non_alnum_slug(text: &str) -> String {
    let mut slug = String::new();
    let mut gap = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if gap {
                slug.push('_');
                gap = false;
            }
            slug.push(c);
        } else {
            gap = true;
        }
    }
    if gap {
        slug.push('_');
    }
    slug
}

fn save_default(
    tx: &rusqlite::Transaction<'_>,
    tenant: &str,
    build: pp_domain::PositiveId,
    assignment: &RoleFilamentAssignment,
) -> Result<(), RoleFilamentError> {
    let key = format!("role_filaments_{}", build.get());
    let stored: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, key],
            |row| row.get(0),
        )
        .optional()?;
    let mut defaults = stored
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    defaults.insert(assignment.role.clone(), default_row(assignment));
    let text =
        serde_json::to_string(&defaults).map_err(|error| WriterFailure::Internal(error.into()))?;
    save(tx, tenant, &key, &text)?;
    Ok(())
}

/// Node's `saveRoleFilamentDefault` writes these three fields. A catalog or
/// Spoolman item keeps its inventory id; a custom or substitute color keeps
/// its hex. `spoolman_spool_id` stays null: this operation pins no spool.
fn default_row(assignment: &RoleFilamentAssignment) -> Value {
    let color_id = match assignment.inventory_kind {
        FilamentInventoryKind::Catalog | FilamentInventoryKind::Spoolman => {
            assignment.inventory_id.clone()
        }
        FilamentInventoryKind::Custom | FilamentInventoryKind::Substitute => None,
    };
    let custom_hex = match assignment.inventory_kind {
        FilamentInventoryKind::Custom | FilamentInventoryKind::Substitute => {
            Some(assignment.color_hex.clone())
        }
        FilamentInventoryKind::Catalog | FilamentInventoryKind::Spoolman => None,
    };
    serde_json::json!({
        "filament_color_id": color_id,
        "filament_custom_hex": custom_hex,
        "spoolman_spool_id": Value::Null,
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::{
        SettingSnapshot,
        build_graph::{
            NewBuild,
            create_with_brief::tests::{Fixture as Created, IDLE},
        },
        write::test_support::{WAIT, seed_setting},
    };
    use serde_json::json;

    struct Fixture {
        created: Created,
        build: pp_domain::PositiveId,
    }

    impl Fixture {
        fn new() -> Self {
            let created = Created::new();
            let new = NewBuild::parse("Voron", "A Voron 2.4", None).unwrap();
            let build = created.create(new).unwrap().id;
            let fixture = Self { created, build };
            let mut brief = fixture.brief();
            brief["requirements"] = json!([
                requirement("color_accent", "unverified"),
                requirement("color_primary", "unverified"),
            ]);
            brief["draft_id"] = json!(5);
            brief["updated_at"] = json!("2020-01-01T00:00:00.000Z");
            fixture.store_brief(&brief);
            seed_setting(
                &fixture.created.owner,
                &format!("role_filaments_{}", build.get()),
                r#"{"primary":{"filament_color_id":"keep","filament_custom_hex":null,"spoolman_spool_id":null}}"#,
            );
            fixture
        }

        fn key(&self) -> String {
            format!("build_planning.v1.{}", self.build.get())
        }

        fn brief(&self) -> Value {
            let reader = self.created.owner.client().reader(WAIT).unwrap();
            match reader.snapshot("default", &self.key()).unwrap() {
                SettingSnapshot::Stored { value } => serde_json::from_str(&value).unwrap(),
                SettingSnapshot::Missing => panic!("no brief"),
            }
        }

        fn store_brief(&self, brief: &Value) {
            seed_setting(&self.created.owner, &self.key(), &brief.to_string());
        }

        fn assign(
            &self,
            assignment: RoleFilamentAssignment,
        ) -> Result<AssignedRole, RoleFilamentError> {
            self.created.client.assign_role_filament(
                self.created.session(),
                self.build,
                assignment,
                &IDLE,
                WAIT,
            )
        }

        fn defaults(&self) -> Value {
            let reader = self.created.owner.client().reader(WAIT).unwrap();
            let key = format!("role_filaments_{}", self.build.get());
            match reader.snapshot("default", &key).unwrap() {
                SettingSnapshot::Stored { value } => serde_json::from_str(&value).unwrap(),
                SettingSnapshot::Missing => panic!("no role defaults"),
            }
        }
    }

    fn requirement(key: &str, status: &str) -> Value {
        json!({ "key": key, "value": "black", "status": status, "detail": null })
    }

    fn catalog(role: &str, id: &str) -> RoleFilamentAssignment {
        RoleFilamentAssignment {
            role: role.into(),
            requested_brand: Some("ABS Matte".into()),
            requested_name: Some("Black (Pantone 7C)".into()),
            inventory_kind: FilamentInventoryKind::Catalog,
            inventory_id: Some(id.into()),
            color_hex: "#c0afa1".into(),
            substitution_confirmed: false,
        }
    }

    #[test]
    fn assign_saves_the_role_default_and_satisfies_its_requirement() {
        let fixture = Fixture::new();
        let saved = fixture
            .assign(catalog("Accent", "abs-matte::black-pantone-7c"))
            .unwrap();
        assert_eq!(saved.role, "Accent");
        let brief = fixture.brief();
        assert!(brief["draft_id"].is_null());
        assert_eq!(brief["requirements"][0]["status"], "satisfied");
        assert_eq!(
            brief["requirements"][0]["detail"],
            "Assigned catalog filament abs-matte::black-pantone-7c"
        );
        assert_eq!(brief["requirements"][1]["status"], "unverified");
        assert_eq!(brief["role_filaments"][0]["role"], "Accent");
        let defaults = fixture.defaults();
        assert_eq!(defaults["primary"]["filament_color_id"], "keep");
        assert_eq!(
            defaults["Accent"]["filament_color_id"],
            "abs-matte::black-pantone-7c"
        );
        assert!(defaults["Accent"]["filament_custom_hex"].is_null());
        assert!(defaults["Accent"]["spoolman_spool_id"].is_null());

        let mut replacement = catalog("Accent", "abs-matte::hot-pink");
        replacement.color_hex = "#ff3eb5".into();
        fixture.assign(replacement).unwrap();
        let brief = fixture.brief();
        assert_eq!(brief["role_filaments"].as_array().unwrap().len(), 1);
        assert_eq!(
            brief["role_filaments"][0]["inventory_id"],
            "abs-matte::hot-pink"
        );
        assert_eq!(
            fixture.defaults()["Accent"]["filament_color_id"],
            "abs-matte::hot-pink"
        );
    }

    #[test]
    fn slug_matches_nodes_requirement_key() {
        assert_eq!(color_requirement_key("Accent"), "color_accent");
        assert_eq!(
            color_requirement_key("Accent Color!"),
            "color_accent_color_"
        );
        assert_eq!(color_requirement_key(" accent"), "color__accent");
    }
}
