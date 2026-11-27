//! Save a role's filament default and record it on the Build's planning
//! brief (GRE-1435), the operation behind MCP `propose_assign_role_filament`.
//!
//! [`BuildGraphClient::assign_role_filament`] runs in one write transaction:
//! prove the credential, replace that role's entry on the brief, mark its
//! `color_<role>` requirement satisfied when the brief has one, clear the
//! selected draft, merge `role_filaments_<build>`, and give the Build's
//! included accepted Parts of that role the same filament and spool pin
//! (GRE-1898). An accepted Part's filament is live: it changes without a new
//! revision, and Accept publishes it. A Spoolman assignment also needs
//! Spoolman on for the Build, with its Plan override, as Checkoff reads it;
//! both the check and the write test that in their own transaction. The catalog and Spoolman checks happen
//! in `pp-api` before this write, so the transaction never waits on a
//! network call.
//!
//! Errors are [`RoleFilamentError`]. This module holds no status codes and
//! no response text.

use super::{
    BuildGraphClient,
    create_with_brief::{BriefError, authorized_brief, save, store_changed_brief, stored_brief},
};
use crate::{
    WriterFailure,
    read_model::{
        Credential,
        planning::{
            BuildPlanningBrief, FilamentInventoryKind, RequirementStatus, RoleFilamentAssignment,
        },
        stock::{SpoolmanIntegration, spoolman_integration},
    },
    write::authorize,
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;
use std::{sync::atomic::AtomicBool, time::Duration};

/// The role whose default was saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssignedRole {
    pub role: String,
    /// Included accepted Parts of the role that now carry the assignment.
    pub updated_parts: usize,
}

/// A Spoolman spool pin, `spoolman:{integration}:spool:{n}`: one integration
/// segment without `:` and a positive spool number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpoolPin {
    text: String,
    integration: String,
    spool: i64,
}

impl SpoolPin {
    pub fn parse(text: &str) -> Option<Self> {
        let (integration, spool) = crate::read_model::stock::spoolman_ref(text, ":spool:")?;
        Some(Self {
            text: text.to_owned(),
            integration: integration.to_owned(),
            spool,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn integration(&self) -> &str {
        &self.integration
    }

    pub fn spool(&self) -> i64 {
        self.spool
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RoleFilamentError {
    #[error("the Build has no planning brief")]
    NoBrief,
    /// The stored brief does not parse or names another Build.
    #[error("the stored planning brief is invalid")]
    BriefInvalid,
    /// A Spoolman assignment, and Spoolman is off for the Build.
    #[error("Spoolman is off for the Build")]
    SpoolmanOff,
    /// A Spoolman assignment, and a stored feature layer does not parse.
    #[error("a stored feature setting is invalid")]
    FeatureSettingInvalid,
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
    /// Proves the credential, checks Spoolman is on for `build` when
    /// `spoolman`, and finds `build`'s brief, in one read transaction. `Ok` is
    /// no promise: the write checks both again.
    pub fn check_role_filament(
        &self,
        credential: Credential,
        build: pp_domain::PositiveId,
        spoolman: bool,
        wait: Duration,
    ) -> Result<(), RoleFilamentError> {
        let policy = self.policy;
        let name = "build_graph.check_role_filament";
        self.settings
            .read(name, &AtomicBool::new(false), wait, move |tx| {
                let installation = authorize(tx, &credential, policy)?;
                check_assignment(tx, installation.id(), build, spoolman)
            })?
    }

    /// Resolves a Spoolman integration for this Build in the caller's installation.
    pub fn role_filament_integration(
        &self,
        credential: Credential,
        build: pp_domain::PositiveId,
        integration_id: String,
        wait: Duration,
    ) -> Result<Option<SpoolmanIntegration>, RoleFilamentError> {
        let policy = self.policy;
        let name = "build_graph.role_filament_integration";
        self.settings
            .read(name, &AtomicBool::new(false), wait, move |tx| {
                let installation = authorize(tx, &credential, policy)?;
                let tenant = installation.id();
                check_assignment(tx, tenant, build, true)?;
                spoolman_integration(tx, tenant, &integration_id)
                    .map_err(|error| RoleFilamentError::Writer(WriterFailure::from(error)))
            })?
    }

    /// Replaces `assignment`'s role on the brief and in
    /// `role_filaments_<build>`, and gives the role's included accepted Parts
    /// the assignment and `spool`, in one transaction.
    pub fn assign_role_filament(
        &self,
        credential: Credential,
        build: pp_domain::PositiveId,
        assignment: RoleFilamentAssignment,
        spool: Option<SpoolPin>,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<AssignedRole, RoleFilamentError> {
        let policy = self.policy;
        let name = "build_graph.assign_role_filament";
        self.shared.write(name, cancelled, wait, move |tx, _| {
            let (tenant, mut brief) =
                authorized_brief::<RoleFilamentError>(tx, &credential, policy, build)?;
            if assignment.inventory_kind == FilamentInventoryKind::Spoolman {
                spoolman_on(tx, &tenant, build)?;
            }
            let role = assignment.role.clone();
            apply(&mut brief, &assignment);
            store_changed_brief(tx, &tenant, build, brief)?;
            let columns = Columns::of(&assignment, spool.as_ref());
            save_default(tx, &tenant, build, &role_key(&role), &columns)?;
            let updated_parts = assign_accepted_parts(tx, &tenant, build, &role, &columns)?;
            Ok(AssignedRole {
                role,
                updated_parts,
            })
        })?
    }
}

/// Spoolman's effective setting for `build`: the user's choice under the
/// Build's Plan override, as Checkoff's deduction reads it.
fn check_assignment(
    tx: &Transaction<'_>,
    tenant: &str,
    build: pp_domain::PositiveId,
    spoolman: bool,
) -> Result<(), RoleFilamentError> {
    if spoolman {
        spoolman_on(tx, tenant, build)?;
    }
    stored_brief::<RoleFilamentError>(tx, tenant, build).map(drop)
}

fn spoolman_on(
    tx: &rusqlite::Transaction<'_>,
    tenant: &str,
    build: pp_domain::PositiveId,
) -> Result<(), RoleFilamentError> {
    match crate::features::resolve_for_tenant(tx, tenant, Some(build)) {
        Ok(view)
            if view
                .effective()
                .enabled(pp_domain::features::Feature::Spoolman) =>
        {
            Ok(())
        }
        Ok(_) => Err(RoleFilamentError::SpoolmanOff),
        Err(error)
            if error
                .downcast_ref::<crate::features::InvalidLayer>()
                .is_some() =>
        {
            Err(RoleFilamentError::FeatureSettingInvalid)
        }
        Err(error) => Err(WriterFailure::from(error).into()),
    }
}

/// Replaces every assignment with the role's key, whatever its case or
/// spacing, and marks `color_<role>` satisfied when that requirement is
/// already on the brief. Other requirements stay.
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
    let role = role_key(&assignment.role);
    brief
        .role_filaments
        .retain(|item| role_key(&item.role) != role);
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

/// The key draft preparation looks a role's default up by: the role
/// JS-trimmed and lowercased, `primary` when blank, as Node's
/// `normalizePartRole` gives it.
fn role_key(role: &str) -> String {
    let key = pp_domain::build_identity::trim_js(role).to_lowercase();
    if key.is_empty() {
        "primary".into()
    } else {
        key
    }
}

/// The three filament columns a role default and an accepted Part store.
/// A catalog or Spoolman item keeps its inventory id; a custom or substitute
/// color keeps its hex. Only a Spoolman item may pin a spool.
struct Columns {
    color_id: Option<String>,
    custom_hex: Option<String>,
    spool: Option<String>,
}

impl Columns {
    fn of(assignment: &RoleFilamentAssignment, spool: Option<&SpoolPin>) -> Self {
        let inventory = matches!(
            assignment.inventory_kind,
            FilamentInventoryKind::Catalog | FilamentInventoryKind::Spoolman
        );
        Self {
            color_id: inventory.then(|| assignment.inventory_id.clone()).flatten(),
            custom_hex: (!inventory).then(|| assignment.color_hex.clone()),
            spool: (assignment.inventory_kind == FilamentInventoryKind::Spoolman)
                .then(|| spool.map(|pin| pin.as_str().to_owned()))
                .flatten(),
        }
    }
}

fn save_default(
    tx: &rusqlite::Transaction<'_>,
    tenant: &str,
    build: pp_domain::PositiveId,
    role: &str,
    columns: &Columns,
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
    // An older writer kept the role as typed; drop those aliases.
    defaults.retain(|stored, _| role_key(stored) != role);
    defaults.insert(
        role.to_owned(),
        serde_json::json!({
            "filament_color_id": columns.color_id,
            "filament_custom_hex": columns.custom_hex,
            "spoolman_spool_id": columns.spool,
        }),
    );
    let text =
        serde_json::to_string(&defaults).map_err(|error| WriterFailure::Internal(error.into()))?;
    save(tx, tenant, &key, &text)?;
    Ok(())
}

/// Writes `columns` on the Build's included accepted Parts whose role has
/// `role`'s key, as Node's `PUT /plans/:id/role-filament` does. A Build with
/// no accepted Plan has no Parts, so only its default changes.
fn assign_accepted_parts(
    tx: &rusqlite::Transaction<'_>,
    tenant: &str,
    build: pp_domain::PositiveId,
    role: &str,
    columns: &Columns,
) -> Result<usize, RoleFilamentError> {
    let key = role_key(role);
    let build =
        i64::try_from(build.get()).map_err(|error| WriterFailure::Internal(error.into()))?;
    let mut statement =
        tx.prepare("SELECT id, role FROM parts WHERE tenant_id=?1 AND profile_id=?2 AND included")?;
    let parts = statement
        .query_map(params![tenant, build], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut updated = 0;
    for (id, _) in parts
        .iter()
        .filter(|(_, part_role)| role_key(part_role) == key)
    {
        updated += tx.execute(
            "UPDATE parts SET filament_color_id=?1, filament_custom_hex=?2, spoolman_spool_id=?3
             WHERE tenant_id=?4 AND id=?5",
            params![
                columns.color_id,
                columns.custom_hex,
                columns.spool,
                tenant,
                id
            ],
        )?;
    }
    Ok(updated)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::{
        SettingSnapshot,
        build_graph::create_with_brief::tests::{
            Fixture as Created, IDLE, read_stored_brief, seeded_voron,
        },
        write::test_support::{WAIT, seed_setting},
    };
    use serde_json::json;

    /// `(filename, filament_color_id, filament_custom_hex, spoolman_spool_id)`.
    type PartFilament = (String, Option<String>, Option<String>, Option<String>);

    struct Fixture {
        created: Created,
        build: pp_domain::PositiveId,
    }

    impl Fixture {
        fn new() -> Self {
            let created = Created::new();
            let build = seeded_voron(&created);
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

        /// The user's Spoolman choice, and the Build's Plan override when
        /// `plan` is `Some`.
        fn spoolman(&self, user: bool, plan: Option<&str>) {
            let owner = &self.created.owner;
            seed_setting(
                owner,
                "feature_toggles",
                &json!({ "orders": true, "spoolman": user }).to_string(),
            );
            if let Some(plan) = plan {
                seed_setting(
                    owner,
                    &format!("feature_toggles:plan:{}", self.build.get()),
                    plan,
                );
            }
        }

        fn check(&self, spoolman: bool) -> Result<(), RoleFilamentError> {
            self.created.client.check_role_filament(
                self.created.session(),
                self.build,
                spoolman,
                WAIT,
            )
        }

        fn brief(&self) -> Value {
            read_stored_brief(&self.created.owner, &self.key())
        }

        fn store_brief(&self, brief: &Value) {
            seed_setting(&self.created.owner, &self.key(), &brief.to_string());
        }

        fn assign(
            &self,
            assignment: RoleFilamentAssignment,
        ) -> Result<AssignedRole, RoleFilamentError> {
            self.assign_pinned(assignment, None)
        }

        fn assign_pinned(
            &self,
            assignment: RoleFilamentAssignment,
            spool: Option<&str>,
        ) -> Result<AssignedRole, RoleFilamentError> {
            self.created.client.assign_role_filament(
                self.created.session(),
                self.build,
                assignment,
                spool.map(|text| SpoolPin::parse(text).unwrap()),
                &IDLE,
                WAIT,
            )
        }

        /// Accepted Parts of the Build: `(filename, role, included)`.
        fn seed_parts(&self, parts: &[(&str, &str, bool)]) {
            let database = self.created.database();
            for (filename, role, included) in parts {
                database
                    .execute(
                        "INSERT INTO parts (tenant_id, profile_id, match_key, filename, role, included)
                         VALUES ('default', ?1, ?2, ?2, ?3, ?4)",
                        params![self.build.get() as i64, filename, role, included],
                    )
                    .unwrap();
            }
        }

        /// Each Part's `(filename, filament_color_id, filament_custom_hex,
        /// spoolman_spool_id)`, by id.
        fn parts(&self) -> Vec<PartFilament> {
            let database = self.created.database();
            let mut statement = database
                .prepare(
                    "SELECT filename, filament_color_id, filament_custom_hex, spoolman_spool_id
                     FROM parts WHERE profile_id=?1 ORDER BY id",
                )
                .unwrap();
            statement
                .query_map([self.build.get() as i64], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
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
        assert_eq!(saved.updated_parts, 0);
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
        // Stored under the key draft preparation reads: the role lowercased.
        assert!(defaults.get("Accent").is_none());
        assert_eq!(
            defaults["accent"]["filament_color_id"],
            "abs-matte::black-pantone-7c"
        );
        assert!(defaults["accent"]["filament_custom_hex"].is_null());
        assert!(defaults["accent"]["spoolman_spool_id"].is_null());

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
            fixture.defaults()["accent"]["filament_color_id"],
            "abs-matte::hot-pink"
        );
    }

    fn spoolman(role: &str, id: &str) -> RoleFilamentAssignment {
        RoleFilamentAssignment {
            role: role.into(),
            requested_brand: None,
            requested_name: None,
            inventory_kind: FilamentInventoryKind::Spoolman,
            inventory_id: Some(id.into()),
            color_hex: "#112233".into(),
            substitution_confirmed: false,
        }
    }

    #[test]
    fn a_spoolman_assignment_pins_the_spool_on_the_role_default_and_its_included_parts() {
        let fixture = Fixture::new();
        fixture.spoolman(true, None);
        fixture.seed_parts(&[
            ("bracket.stl", "accent", true),
            ("gear.stl", " Accent", true),
            ("excluded.stl", "accent", false),
            ("panel.stl", "primary", true),
        ]);
        let saved = fixture
            .assign_pinned(
                spoolman("Accent", "spoolman:sm-1:filament:3"),
                Some("spoolman:sm-1:spool:7"),
            )
            .unwrap();
        assert_eq!(saved.updated_parts, 2);
        let defaults = fixture.defaults();
        assert_eq!(
            defaults["accent"],
            json!({
                "filament_color_id": "spoolman:sm-1:filament:3",
                "filament_custom_hex": null,
                "spoolman_spool_id": "spoolman:sm-1:spool:7",
            })
        );
        let pinned = |name: &str| {
            (
                name.to_owned(),
                Some("spoolman:sm-1:filament:3".to_owned()),
                None,
                Some("spoolman:sm-1:spool:7".to_owned()),
            )
        };
        let untouched = |name: &str| (name.to_owned(), None, None, None);
        assert_eq!(
            fixture.parts(),
            [
                pinned("bracket.stl"),
                pinned("gear.stl"),
                untouched("excluded.stl"),
                untouched("panel.stl"),
            ]
        );

        // A custom color clears the filament id and the pin, on both.
        fixture
            .assign_pinned(
                RoleFilamentAssignment {
                    inventory_kind: FilamentInventoryKind::Custom,
                    inventory_id: None,
                    substitution_confirmed: true,
                    ..spoolman("accent", "")
                },
                Some("spoolman:sm-1:spool:7"),
            )
            .unwrap();
        assert!(fixture.defaults()["accent"]["spoolman_spool_id"].is_null());
        assert_eq!(
            fixture.parts()[0],
            ("bracket.stl".into(), None, Some("#112233".into()), None)
        );
    }

    #[test]
    fn a_spoolman_assignment_needs_spoolman_on_for_the_build_and_writes_nothing_otherwise() {
        let fixture = Fixture::new();
        fixture.seed_parts(&[("bracket.stl", "accent", true)]);
        let assign = || {
            fixture.assign_pinned(
                spoolman("Accent", "spoolman:sm-1:filament:3"),
                Some("spoolman:sm-1:spool:7"),
            )
        };
        let before = (fixture.brief(), fixture.defaults(), fixture.parts());
        // On for the user, off for this Build: the check and the write refuse.
        fixture.spoolman(true, Some(r#"{"spoolman":false}"#));
        assert!(matches!(
            fixture.check(true),
            Err(RoleFilamentError::SpoolmanOff)
        ));
        assert!(matches!(assign(), Err(RoleFilamentError::SpoolmanOff)));
        // Other kinds never read the Spoolman setting.
        fixture.check(false).unwrap();
        // Off for the user with no Build override, or an unreadable layer.
        fixture.spoolman(false, Some("{}"));
        assert!(matches!(assign(), Err(RoleFilamentError::SpoolmanOff)));
        fixture.spoolman(true, Some(r#"{"spoolman":"yes"}"#));
        assert!(matches!(
            fixture.check(true),
            Err(RoleFilamentError::FeatureSettingInvalid)
        ));
        assert!(matches!(
            assign(),
            Err(RoleFilamentError::FeatureSettingInvalid)
        ));
        assert_eq!(
            before,
            (fixture.brief(), fixture.defaults(), fixture.parts())
        );
        // Off for the user, on for this Build: the Build's override wins.
        fixture.spoolman(false, Some(r#"{"spoolman":true}"#));
        fixture.check(true).unwrap();
        assert_eq!(assign().unwrap().updated_parts, 1);
    }

    #[test]
    fn reassigning_a_role_replaces_its_entries_whatever_their_case_or_spacing() {
        let fixture = Fixture::new();
        fixture.spoolman(true, None);
        let mut brief = fixture.brief();
        brief["role_filaments"] = json!([{
            "role": "primary", "requested_brand": null, "requested_name": null,
            "inventory_kind": "custom", "inventory_id": null, "color_hex": "#aabbcc",
            "substitution_confirmed": true,
        }, {
            "role": "accent", "requested_brand": null, "requested_name": null,
            "inventory_kind": "custom", "inventory_id": null, "color_hex": "#ddeeff",
            "substitution_confirmed": true,
        }]);
        fixture.store_brief(&brief);
        seed_setting(
            &fixture.created.owner,
            &format!("role_filaments_{}", fixture.build.get()),
            r#"{" Primary ":{"filament_color_id":"spoolman:sm-1:filament:2","filament_custom_hex":null,"spoolman_spool_id":"spoolman:sm-1:spool:5"},"PRIMARY":{"filament_color_id":"old","filament_custom_hex":null,"spoolman_spool_id":null},"accent":{"filament_color_id":"keep","filament_custom_hex":null,"spoolman_spool_id":null}}"#,
        );
        fixture
            .assign_pinned(
                spoolman("Primary", "spoolman:sm-1:filament:3"),
                Some("spoolman:sm-1:spool:7"),
            )
            .unwrap();
        let roles: Vec<(String, String)> = fixture.brief()["role_filaments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| {
                (
                    item["role"].as_str().unwrap().to_owned(),
                    item["inventory_kind"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(
            roles,
            [
                ("accent".to_owned(), "custom".to_owned()),
                ("Primary".to_owned(), "spoolman".to_owned()),
            ]
        );
        assert_eq!(
            fixture.defaults(),
            json!({
                "accent": {"filament_color_id": "keep", "filament_custom_hex": null, "spoolman_spool_id": null},
                "primary": {
                    "filament_color_id": "spoolman:sm-1:filament:3",
                    "filament_custom_hex": null,
                    "spoolman_spool_id": "spoolman:sm-1:spool:7",
                },
            })
        );
    }

    #[test]
    fn a_spool_pin_names_one_integration_and_a_positive_spool() {
        let pin = SpoolPin::parse("spoolman:sm-1:spool:7").unwrap();
        assert_eq!((pin.integration(), pin.spool()), ("sm-1", 7));
        assert_eq!(pin.as_str(), "spoolman:sm-1:spool:7");
        for bad in [
            "7",
            "spoolman:sm-1:filament:7",
            "spoolman:sm-1:spool:0",
            "spoolman::spool:7",
            "spoolman:a:b:spool:7",
        ] {
            assert!(SpoolPin::parse(bad).is_none(), "{bad}");
        }
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
