//! MCP `propose_assign_role_filament` and its `confirm_apply` on Rust storage
//! (GRE-1435).
//!
//! Propose checks the assignment's shape and, for a Spoolman item, that the
//! Build's Spoolman Module is on, then proves the API key and finds the
//! Build's planning brief. It writes nothing and makes no network call.
//! Confirm checks the shape again and verifies the inventory item: a catalog
//! color by id in the bundled catalog, or a Spoolman filament by id through
//! `McpFilaments::spoolman`, with the Module checked before and after that
//! read and no transaction open across it. The color must match, and a
//! `requested_name` or `requested_brand` must be the item's. Then
//! `BuildGraphClient::assign_role_filament` saves the role's default and
//! records the assignment in the brief in one transaction.

use super::{
    checkoff::CheckoffDomain,
    source_roles::{BRIEF_INVALID, INVALID_PLAN, NO_BRIEF, brief_proposal, plan},
    write_result::{Code, Refusal, read_failure, writer_failure},
};
use crate::loopback::PresentedCredential;
use pp_services::mcp_domain::{ProposedAction, ToolInvocation};
use pp_storage::{
    build_graph::{PositiveId, RoleFilamentError},
    filaments::{Outcome, Request},
    read_model::{
        planning::{FilamentInventoryKind, RoleFilamentAssignment},
        views::js_trim,
    },
};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, sync::atomic::AtomicBool};

pub(super) const PROPOSE: &str = "propose_assign_role_filament";

const ASSIGNMENT: &str = "assignment";
const INVALID_ASSIGNMENT: Refusal = Refusal::new(
    Code::InvalidAssignment,
    "assignment must be an object { role, inventory_kind, inventory_id?, color_hex, requested_name?, requested_brand?, substitution_confirmed? }.",
);
const SPOOLMAN_OFF: Refusal = Refusal::new(
    Code::SpoolmanModuleOff,
    "The Spoolman Module is off for this Build. Turn it on in Settings, or assign a catalog or custom filament.",
);
const SPOOLMAN_UNAVAILABLE: Refusal = Refusal::new(
    Code::SpoolmanUnavailable,
    "Spoolman did not answer, or its integration is missing or disabled. Nothing was saved.",
);
const NOT_FOUND: Refusal = Refusal::new(
    Code::InventoryNotFound,
    "inventory_id names no filament of that inventory_kind.",
);
const PLAN_NOT_FOUND: Refusal = Refusal::new(Code::PlanNotFound, "No Build has this plan_id.");

impl CheckoffDomain {
    pub(super) fn role_filament_propose(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolInvocation, String> {
        let Some(build) = plan(args) else {
            return Ok(INVALID_PLAN.invocation());
        };
        let assignment = match assignment(args) {
            Ok(assignment) => assignment,
            Err(refusal) => return Ok(refusal.invocation()),
        };
        let caller = self.caller()?;
        if assignment.inventory_kind == FilamentInventoryKind::Spoolman
            && let Err(refusal) = self.spoolman_integration(&caller, build, None)
        {
            return Ok(refusal.invocation());
        }
        let credential = caller.into_credential();
        if let Err(error) = self.builds.check_role_filament(credential, build, self.wait) {
            return Ok(storage_refusal(&error).invocation());
        }
        brief_proposal(
            PROPOSE,
            build,
            (ASSIGNMENT, wire(&assignment)),
            "Assign role filament on Build",
            "Save the role's filament default, record it in the planning brief and clear its selected draft.",
        )
    }

    pub(super) fn role_filament_apply(&self, action: &ProposedAction) -> Result<Value, String> {
        let Some(build) = plan(&action.params) else {
            return Ok(INVALID_PLAN.confirmation());
        };
        let assignment = match assignment(&action.params) {
            Ok(assignment) => assignment,
            Err(refusal) => return Ok(refusal.confirmation()),
        };
        let caller = self.caller()?;
        if let Err(refusal) = self.verify(&caller, build, &assignment) {
            return Ok(refusal.confirmation());
        }
        let credential = caller.into_credential();
        Ok(
            match self.builds.assign_role_filament(
                credential,
                build,
                assignment,
                &AtomicBool::new(false),
                self.wait,
            ) {
                Ok(saved) => json!({
                    "ok": true,
                    "result": { "role": saved.role, "assignment": wire(&saved) },
                }),
                Err(error) => storage_refusal(&error).confirmation(),
            },
        )
    }

    /// Proves a catalog or Spoolman item names this color, and the requested
    /// name and brand when given. Custom and substitute colors name no item.
    fn verify(
        &self,
        caller: &PresentedCredential,
        build: PositiveId,
        assignment: &RoleFilamentAssignment,
    ) -> Result<(), Refusal> {
        let id = assignment.inventory_id.as_deref().unwrap_or_default();
        let item = match assignment.inventory_kind {
            FilamentInventoryKind::Custom | FilamentInventoryKind::Substitute => return Ok(()),
            FilamentInventoryKind::Catalog => crate::filaments::catalog_color(id).ok_or(NOT_FOUND)?,
            FilamentInventoryKind::Spoolman => self.spoolman_item(caller, build, id)?,
        };
        let same = |key: &str, wanted: &str| {
            item[key]
                .as_str()
                .is_some_and(|value| value.to_lowercase() == wanted.to_lowercase())
        };
        if !same("hex", &assignment.color_hex) {
            return Err(Refusal::new(
                Code::InventoryColorMismatch,
                "color_hex is not the inventory filament's color.",
            ));
        }
        let requested = |field: &Option<String>| {
            field
                .as_deref()
                .map(js_trim)
                .filter(|value| !value.is_empty())
        };
        if let Some(name) = requested(&assignment.requested_name)
            && !same("display_name", name)
        {
            return Err(Refusal::new(
                Code::InventoryNameMismatch,
                "requested_name is not the inventory filament's name. Confirm a substitute instead.",
            ));
        }
        if let Some(brand) = requested(&assignment.requested_brand)
            && !same("product_line", brand)
        {
            return Err(Refusal::new(
                Code::InventoryBrandMismatch,
                "requested_brand is not the inventory filament's product line. Confirm a substitute instead.",
            ));
        }
        Ok(())
    }

    /// The Spoolman filament `id` names (`spoolman:<integration>:filament:<n>`),
    /// read from its integration. The Module is checked before and after the
    /// read, which runs on this blocking thread with no transaction open.
    fn spoolman_item(
        &self,
        caller: &PresentedCredential,
        build: PositiveId,
        id: &str,
    ) -> Result<Value, Refusal> {
        let integration = id
            .strip_prefix("spoolman:")
            .and_then(|rest| rest.rsplit_once(":filament:"))
            .map(|(integration, _)| integration)
            .filter(|integration| !integration.is_empty())
            .ok_or(NOT_FOUND)?;
        let config = self
            .spoolman_integration(caller, build, Some(integration))?
            .ok_or(SPOOLMAN_UNAVAILABLE)?;
        let usable = config["type"] == "spoolman" && config["config"]["enabled"] != false;
        if !usable {
            return Err(SPOOLMAN_UNAVAILABLE);
        }
        let read = self.filaments.spoolman.read(config);
        let colors = tokio::runtime::Handle::try_current()
            .ok()
            .and_then(|runtime| runtime.block_on(read))
            .ok_or(SPOOLMAN_UNAVAILABLE)?;
        self.spoolman_integration(caller, build, Some(integration))?;
        colors
            .into_iter()
            .find(|color| color["id"] == id)
            .ok_or(NOT_FOUND)
    }

    /// The Build's Spoolman integration, `integration` or its default, when
    /// its Spoolman Module is on; refused when the Module is off.
    fn spoolman_integration(
        &self,
        caller: &PresentedCredential,
        build: PositiveId,
        integration: Option<&str>,
    ) -> Result<Option<Value>, Refusal> {
        let mut query = HashMap::from([("profile_id".to_owned(), build.get().to_string())]);
        if let Some(integration) = integration {
            query.insert("spoolman_integration_id".into(), integration.into());
        }
        let credential = caller.clone().into_credential();
        match self
            .filaments
            .client
            .submit(credential, Request::Catalog(query), self.wait)
        {
            Ok(Outcome::Catalog {
                allowed: true,
                integration,
                ..
            }) => Ok(integration),
            Ok(Outcome::Catalog { allowed: false, .. }) => Err(SPOOLMAN_OFF),
            Ok(Outcome::Refused(404, ..)) => Err(PLAN_NOT_FOUND),
            Ok(_) => Err(SPOOLMAN_UNAVAILABLE),
            Err(error) => Err(read_failure(error)),
        }
    }
}

/// The checked assignment, or the first refusal. Optional text fields read
/// as absent unless they are strings; `role` is trimmed.
fn assignment(args: &Map<String, Value>) -> Result<RoleFilamentAssignment, Refusal> {
    let row = args
        .get(ASSIGNMENT)
        .and_then(Value::as_object)
        .ok_or(INVALID_ASSIGNMENT)?;
    let text = |field: &str| row.get(field).and_then(Value::as_str);
    let role = js_trim(text("role").unwrap_or_default());
    if role.is_empty() {
        return Err(Refusal::new(Code::RoleRequired, "assignment.role is required."));
    }
    let inventory_kind = text("inventory_kind")
        .and_then(FilamentInventoryKind::from_wire)
        .ok_or(Refusal::new(
            Code::InvalidInventoryKind,
            "assignment.inventory_kind must be catalog, spoolman, custom or substitute.",
        ))?;
    let color_hex = text("color_hex").unwrap_or_default();
    let digits = color_hex.strip_prefix('#').unwrap_or_default();
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Refusal::new(
            Code::InvalidColor,
            "assignment.color_hex must be a color in the form #rrggbb.",
        ));
    }
    let confirmed = row.get("substitution_confirmed") == Some(&Value::Bool(true));
    let custom = matches!(
        inventory_kind,
        FilamentInventoryKind::Custom | FilamentInventoryKind::Substitute
    );
    if custom && !confirmed {
        return Err(Refusal::new(
            Code::SubstitutionNotConfirmed,
            "A custom or substitute color needs assignment.substitution_confirmed: true.",
        ));
    }
    let owned = |field: &str| text(field).map(str::to_owned);
    Ok(RoleFilamentAssignment {
        role: role.to_owned(),
        requested_brand: owned("requested_brand"),
        requested_name: owned("requested_name"),
        inventory_kind,
        inventory_id: text("inventory_id")
            .map(js_trim)
            .filter(|id| !id.is_empty())
            .map(str::to_owned),
        color_hex: color_hex.to_owned(),
        substitution_confirmed: confirmed,
    })
}

fn wire(assignment: &RoleFilamentAssignment) -> Value {
    json!({
        "role": assignment.role,
        "inventory_kind": assignment.inventory_kind.as_str(),
        "inventory_id": assignment.inventory_id,
        "color_hex": assignment.color_hex,
        "requested_name": assignment.requested_name,
        "requested_brand": assignment.requested_brand,
        "substitution_confirmed": assignment.substitution_confirmed,
    })
}

fn storage_refusal(error: &RoleFilamentError) -> Refusal {
    match error {
        RoleFilamentError::NoBrief => NO_BRIEF,
        RoleFilamentError::BriefInvalid => BRIEF_INVALID,
        RoleFilamentError::InvalidAssignment => INVALID_ASSIGNMENT,
        RoleFilamentError::Writer(failure) => writer_failure(failure),
    }
}
