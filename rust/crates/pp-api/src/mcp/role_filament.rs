//! MCP `propose_assign_role_filament` and its `confirm_apply` on Rust
//! storage (GRE-1435).
//!
//! Propose checks the assignment's shape and that the Build has a planning
//! brief. A Spoolman assignment also requires Spoolman on for the Build, its
//! Plan override included, as Checkoff reads it. It writes
//! nothing and makes no network call. Confirm checks the assignment again.
//! A catalog assignment is matched against the built-in catalog before the
//! write. A Spoolman assignment, and the spool it may pin (GRE-1898), are
//! matched against one read of that integration's Spoolman inventory, before
//! the write transaction opens. Custom and substitute assignments require
//! `substitution_confirmed`. The write is one transaction in
//! `BuildGraphClient::assign_role_filament`.

use super::{
    checkoff::CheckoffDomain,
    source_roles::{self, brief_proposal, plan},
    write_result::{Code, Refusal, writer_failure},
};
use crate::loopback::PresentedCredential;
use pp_services::mcp_domain::ToolInvocation;
use pp_storage::{
    build_graph::PositiveId,
    build_graph::{RoleFilamentError, SpoolPin},
    read_model::{
        planning::{FilamentInventoryKind, RoleFilamentAssignment},
        stock::{Inventory, spoolman_filament},
        views::{catalog_color, js_trim as trim_js},
    },
};
use serde_json::{Map, Value, json};
use std::sync::atomic::AtomicBool;

pub(super) const PROPOSE: &str = "propose_assign_role_filament";

const INVALID: Refusal = Refusal::new(
    Code::InvalidFilamentAssignment,
    "Invalid filament assignment.",
);
const SUBSTITUTION: Refusal = Refusal::new(
    Code::SubstitutionUnconfirmed,
    "Filament substitution requires explicit confirmation.",
);
const CATALOG_MISSING: Refusal = Refusal::new(
    Code::CatalogFilamentNotFound,
    "Catalog inventory filament not found.",
);
const CATALOG_COLOR: Refusal = Refusal::new(
    Code::CatalogColorMismatch,
    "Catalog inventory filament color does not match.",
);
const CATALOG_NAME: Refusal = Refusal::new(
    Code::CatalogNameMismatch,
    "Requested color is not an exact catalog inventory match; confirm a substitute instead.",
);
const CATALOG_BRAND: Refusal = Refusal::new(
    Code::CatalogBrandMismatch,
    "Requested brand is not an exact catalog inventory match; confirm a substitute instead.",
);
const SPOOLMAN_OFF: Refusal = Refusal::new(Code::SpoolmanModuleOff, "The Spoolman Module is off.");
const SPOOLMAN_UNAVAILABLE: Refusal = Refusal::new(
    Code::SpoolmanUnavailable,
    "Spoolman inventory filament could not be verified.",
);
const SPOOLMAN_FILAMENT_MISSING: Refusal = Refusal::new(
    Code::SpoolmanFilamentNotFound,
    "Spoolman has no such filament.",
);
const SPOOLMAN_COLOR: Refusal = Refusal::new(
    Code::SpoolmanColorMismatch,
    "Spoolman inventory filament color does not match.",
);
const SPOOLMAN_SPOOL_MISSING: Refusal = Refusal::new(
    Code::SpoolmanSpoolNotFound,
    "Spoolman has no unarchived spool of that filament with that id.",
);
const MODULES_INVALID: Refusal = Refusal::new(
    Code::ModuleSettingsInvalid,
    "The stored Module settings are invalid. Repair them in Settings.",
);
const INTERNAL: Refusal = Refusal::new(Code::Internal, "Storage failed. Nothing was written.");
const UNAVAILABLE: Refusal = Refusal::new(
    Code::Unavailable,
    "Storage is busy. Nothing was written; try again.",
);

impl CheckoffDomain {
    pub(super) fn role_filament_propose(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        let Some(build) = plan(args) else {
            return Ok(source_roles::INVALID_PLAN.invocation());
        };
        let (assignment, spool) = match args.get("assignment").map(assignment).transpose() {
            Ok(Some(assignment)) => assignment,
            Ok(None) => return Ok(INVALID.invocation()),
            Err(refusal) => return Ok(refusal.invocation()),
        };
        let spoolman = assignment.inventory_kind == FilamentInventoryKind::Spoolman;
        if let Err(refusal) = self.check(build, spoolman) {
            return Ok(refusal.invocation());
        }
        let mut stored = serde_json::to_value(&assignment).map_err(|error| error.to_string())?;
        if let Some(spool) = &spool {
            stored["spoolman_spool_id"] = spool.as_str().into();
        }
        brief_proposal(
            PROPOSE,
            build,
            ("assignment", stored),
            "Assign role filament on Build",
            "Save the role's filament default and record it on the Build's planning brief.",
        )
    }

    pub(super) fn role_filament_apply(
        &self,
        action: &pp_services::mcp_domain::ProposedAction,
    ) -> Result<Value, String> {
        let Some(build) = plan(&action.params) else {
            return Ok(source_roles::INVALID_PLAN.confirmation());
        };
        let (assignment, spool) = match action.params.get("assignment").map(assignment).transpose()
        {
            Ok(Some(assignment)) => assignment,
            Ok(None) => return Ok(INVALID.confirmation()),
            Err(refusal) => return Ok(refusal.confirmation()),
        };
        if let Err(refusal) = self.ready(build, &assignment, spool.as_ref()) {
            return Ok(refusal.confirmation());
        }
        let credential = self.caller()?.into_credential();
        Ok(
            match self.builds.assign_role_filament(
                credential,
                build,
                assignment,
                spool,
                &AtomicBool::new(false),
                self.wait,
            ) {
                Ok(saved) => json!({ "ok": true, "result": {
                    "role": saved.role,
                    "updated_parts": saved.updated_parts,
                } }),
                Err(error) => storage_refusal(&error).confirmation(),
            },
        )
    }

    /// Catalog match, or the Spoolman check: Spoolman on for the Build,
    /// then the filament and the pinned spool against one inventory read.
    /// The write checks Spoolman for the Build again in its own transaction,
    /// as the read may outlast a change. An integration this process cannot
    /// find or read is `spoolman_unavailable` and nothing is written.
    fn ready(
        &self,
        build: PositiveId,
        assignment: &RoleFilamentAssignment,
        spool: Option<&SpoolPin>,
    ) -> Result<(), Refusal> {
        match assignment.inventory_kind {
            FilamentInventoryKind::Catalog => check_catalog(assignment),
            FilamentInventoryKind::Spoolman => {
                self.check(build, true)?;
                let (integration, filament) = spoolman_item(assignment)?;
                let inventory = self.inventory(integration)?;
                check_spoolman(&inventory, assignment, filament, spool)
            }
            FilamentInventoryKind::Custom | FilamentInventoryKind::Substitute => Ok(()),
        }
    }

    /// The Build's brief, and Spoolman on for it when `spoolman`, read with
    /// the caller's key.
    fn check(&self, build: PositiveId, spoolman: bool) -> Result<(), Refusal> {
        let credential = self.caller().map_err(|_| INTERNAL)?.into_credential();
        self.builds
            .check_role_filament(credential, build, spoolman, self.wait)
            .map_err(|error| storage_refusal(&error))
    }

    /// One read of `integration`'s inventory. A missing or disabled
    /// integration, no reader, or a failed read is `spoolman_unavailable`.
    fn inventory(&self, integration: &str) -> Result<Inventory, Refusal> {
        let tenant = self.tenant()?;
        let Ok(found) = self
            .farm
            .modules
            .spoolman_integration(&tenant, integration, self.wait)
        else {
            return Err(UNAVAILABLE);
        };
        let (Some(integration), Some(reader)) = (
            found.filter(|found| found.enabled),
            self.farm.inventory.clone(),
        ) else {
            return Err(SPOOLMAN_UNAVAILABLE);
        };
        // Confirm runs on a blocking thread of the server's runtime.
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| SPOOLMAN_UNAVAILABLE)?;
        runtime
            .block_on(reader.read(integration))
            .ok_or(SPOOLMAN_UNAVAILABLE)
    }

    fn tenant(&self) -> Result<String, Refusal> {
        match self.caller() {
            Ok(PresentedCredential::ApiKey { tenant, .. }) => Ok(tenant),
            _ => Err(INTERNAL),
        }
    }
}

/// The checked assignment and the spool it pins. A pin must be
/// `spoolman:{integration}:spool:{n}`, on a Spoolman assignment, from the
/// filament's own integration; a blank or null pin is none.
fn assignment(value: &Value) -> Result<(RoleFilamentAssignment, Option<SpoolPin>), Refusal> {
    let assignment = role_assignment(value)?;
    let spool = match value.get("spoolman_spool_id") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if trim_js(text).is_empty() => None,
        Some(Value::String(text)) => Some(SpoolPin::parse(trim_js(text)).ok_or(INVALID)?),
        Some(_) => return Err(INVALID),
    };
    if let Some(pin) = &spool {
        let same = assignment.inventory_kind == FilamentInventoryKind::Spoolman
            && spoolman_item(&assignment)
                .is_ok_and(|(integration, _)| integration == pin.integration());
        if !same {
            return Err(INVALID);
        }
    }
    Ok((assignment, spool))
}

fn role_assignment(value: &Value) -> Result<RoleFilamentAssignment, Refusal> {
    let Some(row) = value.as_object() else {
        return Err(INVALID);
    };
    let role = trim_js(row.get("role").and_then(Value::as_str).unwrap_or(""));
    let kind = FilamentInventoryKind::from_wire(
        row.get("inventory_kind")
            .and_then(Value::as_str)
            .unwrap_or(""),
    );
    let color_hex = row.get("color_hex").and_then(Value::as_str).unwrap_or("");
    let (Some(kind), true) = (kind, !role.is_empty() && hex_color(color_hex)) else {
        return Err(INVALID);
    };
    let substitution_confirmed = row.get("substitution_confirmed") == Some(&Value::Bool(true));
    if matches!(
        kind,
        FilamentInventoryKind::Custom | FilamentInventoryKind::Substitute
    ) && !substitution_confirmed
    {
        return Err(SUBSTITUTION);
    }
    Ok(RoleFilamentAssignment {
        role: role.to_owned(),
        requested_brand: owned_string(row.get("requested_brand")),
        requested_name: owned_string(row.get("requested_name")),
        inventory_kind: kind,
        inventory_id: owned_string(row.get("inventory_id"))
            .map(|id| trim_js(&id).to_owned())
            .filter(|id| !id.is_empty()),
        color_hex: color_hex.to_owned(),
        substitution_confirmed,
    })
}

fn owned_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn hex_color(value: &str) -> bool {
    let Some(digits) = value.strip_prefix('#') else {
        return false;
    };
    digits.len() == 6 && digits.chars().all(|c| c.is_ascii_hexdigit())
}

fn check_catalog(assignment: &RoleFilamentAssignment) -> Result<(), Refusal> {
    let Some(id) = assignment
        .inventory_id
        .as_deref()
        .map(trim_js)
        .filter(|id| !id.is_empty())
    else {
        return Err(CATALOG_MISSING);
    };
    let Some(color) = catalog_color(id) else {
        return Err(CATALOG_MISSING);
    };
    if !color.hex.eq_ignore_ascii_case(&assignment.color_hex) {
        return Err(CATALOG_COLOR);
    }
    if let Some(name) = assignment
        .requested_name
        .as_deref()
        .map(trim_js)
        .filter(|name| !name.is_empty())
        && !color.display_name.eq_ignore_ascii_case(name)
    {
        return Err(CATALOG_NAME);
    }
    if let Some(brand) = assignment
        .requested_brand
        .as_deref()
        .map(trim_js)
        .filter(|brand| !brand.is_empty())
        && !color.product_line.eq_ignore_ascii_case(brand)
    {
        return Err(CATALOG_BRAND);
    }
    Ok(())
}

/// `(integration, filament)` of a Spoolman assignment's
/// `spoolman:{integration}:filament:{n}` id. Any other id names no Spoolman
/// filament.
fn spoolman_item(assignment: &RoleFilamentAssignment) -> Result<(&str, i64), Refusal> {
    assignment
        .inventory_id
        .as_deref()
        .and_then(spoolman_filament)
        .ok_or(SPOOLMAN_FILAMENT_MISSING)
}

/// The filament is in the inventory with `color_hex`, ignoring case, and a
/// pinned spool is one of its unarchived spools. Spoolman's labels join
/// vendor, material and name, so `requested_name` and `requested_brand` are
/// not compared.
fn check_spoolman(
    inventory: &Inventory,
    assignment: &RoleFilamentAssignment,
    filament: i64,
    spool: Option<&SpoolPin>,
) -> Result<(), Refusal> {
    let found = inventory
        .filaments
        .get(&filament)
        .ok_or(SPOOLMAN_FILAMENT_MISSING)?;
    if !found
        .hex
        .as_deref()
        .is_some_and(|hex| hex.eq_ignore_ascii_case(&assignment.color_hex))
    {
        return Err(SPOOLMAN_COLOR);
    }
    if let Some(pin) = spool
        && !inventory
            .spools
            .iter()
            .any(|row| row.id == pin.spool() && row.filament_id == filament)
    {
        return Err(SPOOLMAN_SPOOL_MISSING);
    }
    Ok(())
}

fn storage_refusal(error: &RoleFilamentError) -> Refusal {
    match error {
        RoleFilamentError::NoBrief => source_roles::NO_BRIEF,
        RoleFilamentError::BriefInvalid => source_roles::BRIEF_INVALID,
        RoleFilamentError::SpoolmanOff => SPOOLMAN_OFF,
        RoleFilamentError::FeatureSettingInvalid => MODULES_INVALID,
        RoleFilamentError::Writer(failure) => writer_failure(failure),
    }
}
