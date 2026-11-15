//! MCP `propose_assign_role_filament` and its `confirm_apply` on Rust
//! storage (GRE-1435).
//!
//! Propose checks the assignment's shape and that the Build has a planning
//! brief. A Spoolman assignment also requires the Spoolman Module. It writes
//! nothing and makes no network call. Confirm checks the assignment again.
//! A catalog assignment is matched against the built-in catalog before the
//! write. A Spoolman assignment is refused when the module is off, and when
//! this process cannot verify the item: the MCP domain has no Spoolman
//! inventory reader, so confirm does not write. Custom and substitute
//! assignments require `substitution_confirmed`. The write is one
//! transaction in `BuildGraphClient::assign_role_filament`.

use super::{
    checkoff::CheckoffDomain,
    source_roles::{self, brief_proposal, plan},
    write_result::{Code, Refusal, writer_failure},
};
use crate::loopback::PresentedCredential;
use pp_services::mcp_domain::ToolInvocation;
use pp_storage::{
    build_graph::RoleFilamentError,
    features::modules::{Module, ModuleOffReason, ModuleStatus},
    read_model::{
        planning::{FilamentInventoryKind, RoleFilamentAssignment},
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
        let assignment = match args.get("assignment").map(assignment).transpose() {
            Ok(Some(assignment)) => assignment,
            Ok(None) => return Ok(INVALID.invocation()),
            Err(refusal) => return Ok(refusal.invocation()),
        };
        if assignment.inventory_kind == FilamentInventoryKind::Spoolman
            && let Err(refusal) = self.spoolman_gate()
        {
            return Ok(refusal.invocation());
        }
        let credential = self.caller()?.into_credential();
        if let Err(error) = self
            .builds
            .check_role_filament(credential, build, self.wait)
        {
            return Ok(storage_refusal(&error).invocation());
        }
        brief_proposal(
            PROPOSE,
            build,
            (
                "assignment",
                serde_json::to_value(&assignment).map_err(|error| error.to_string())?,
            ),
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
        let assignment = match action.params.get("assignment").map(assignment).transpose() {
            Ok(Some(assignment)) => assignment,
            Ok(None) => return Ok(INVALID.confirmation()),
            Err(refusal) => return Ok(refusal.confirmation()),
        };
        if let Err(refusal) = self.ready(&assignment) {
            return Ok(refusal.confirmation());
        }
        let credential = self.caller()?.into_credential();
        Ok(
            match self.builds.assign_role_filament(
                credential,
                build,
                assignment,
                &AtomicBool::new(false),
                self.wait,
            ) {
                Ok(saved) => json!({ "ok": true, "result": { "role": saved.role } }),
                Err(error) => storage_refusal(&error).confirmation(),
            },
        )
    }

    /// Catalog match, or the Spoolman module check. A Spoolman item this
    /// process cannot read is `spoolman_unavailable` and is not written.
    fn ready(&self, assignment: &RoleFilamentAssignment) -> Result<(), Refusal> {
        match assignment.inventory_kind {
            FilamentInventoryKind::Catalog => check_catalog(assignment),
            FilamentInventoryKind::Spoolman => {
                self.spoolman_gate()?;
                Err(SPOOLMAN_UNAVAILABLE)
            }
            FilamentInventoryKind::Custom | FilamentInventoryKind::Substitute => Ok(()),
        }
    }

    fn spoolman_gate(&self) -> Result<(), Refusal> {
        let Ok(PresentedCredential::ApiKey { tenant, .. }) = self.caller() else {
            return Err(INTERNAL);
        };
        let Ok(statuses) = self.farm.modules.modules(&tenant, self.wait) else {
            return Err(UNAVAILABLE);
        };
        match statuses
            .into_iter()
            .find(|status| status.module() == Module::Spoolman)
        {
            Some(ModuleStatus::On { .. }) => Ok(()),
            Some(ModuleStatus::Off {
                reason: ModuleOffReason::SettingUnreadable,
                ..
            }) => Err(MODULES_INVALID),
            Some(ModuleStatus::Off { .. }) => Err(SPOOLMAN_OFF),
            None => Err(INTERNAL),
        }
    }
}

fn assignment(value: &Value) -> Result<RoleFilamentAssignment, Refusal> {
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

fn storage_refusal(error: &RoleFilamentError) -> Refusal {
    match error {
        RoleFilamentError::NoBrief => source_roles::NO_BRIEF,
        RoleFilamentError::BriefInvalid => source_roles::BRIEF_INVALID,
        RoleFilamentError::Writer(failure) => writer_failure(failure),
    }
}
