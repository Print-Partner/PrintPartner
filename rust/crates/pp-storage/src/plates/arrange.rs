//! Node's `arrangeAcceptedPlates` (`services/accepted-plate-workspace.ts`):
//! `POST /plans/{id}/plates/arrange` packs the head layout again and publishes
//! it as one new Plate revision. `unplaced` packs each Plate's units that are
//! neither manual nor pinned around the fixed ones; `all` repacks every unit
//! per Printer, split by the Production setup's grouping rules, and records
//! the replaced revision for restore.

use super::*;
use crate::production::{
    layout::{
        GroupRule, GroupUnit, PackResult, PackedUnit, PackingUnit, grouping_buckets,
        initial_plate_id, pack_units, pack_units_around,
    },
    model::AcceptedPlanBasis,
    setup::stored_rules,
};

/// The unit and Printer Node names when a unit cannot fit.
type TooLarge = (RequiredUnitToken, String);

pub(super) fn run(
    tx: &Transaction<'_>,
    tenant: &str,
    repos: &Path,
    cmd: &Expected,
    all: bool,
) -> Result<Response> {
    let empty_plan = || {
        Ok(Response {
            status: 200,
            body: json!({"kind": "empty_plan"}),
        })
    };
    if !visible(tx, tenant, cmd.profile)? {
        return Ok(Refusal::ProfileNotFound.response());
    }
    let snapshot = match accepted(tx, tenant, cmd.profile, repos)? {
        Ok(snapshot) => snapshot,
        Err(AcceptedRead::Empty { .. }) => return empty_plan(),
        Err(AcceptedRead::Missing) => return Ok(Refusal::ProfileNotFound.response()),
        Err(AcceptedRead::CompatibilityDirty | AcceptedRead::Uninitialized) => {
            return Ok(Refusal::StateUnavailable.response());
        }
        Err(AcceptedRead::IntegrityFailure { .. } | AcceptedRead::Ready { .. }) => {
            bail!(Integrity("accepted"))
        }
    };
    let basis = Basis::of(&snapshot);
    let units = required(&snapshot);
    let stored = stored_layout(tx, tenant, cmd.profile, &basis, &snapshot)?;
    if units.is_empty() {
        return empty_plan();
    }
    let Stored::Ready(current, plates) = stored else {
        return Ok(Refusal::RevisionChanged.response());
    };
    if basis != cmd.basis {
        return Ok(Refusal::Stale.response());
    }
    if current.id != cmd.revision {
        return Ok(Refusal::RevisionChanged.response());
    }
    let setup: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id=?1 AND key=?2",
            params![tenant, format!("production_setup:{}", cmd.profile)],
            |row| row.get(0),
        )
        .optional()?;
    let rules = stored_rules(setup.as_deref(), cmd.profile);
    let next = match arranged(all, &basis, &plates, &units, &rules)? {
        Ok(next) => next,
        Err(too_large) => return Ok(unit_too_large(too_large)),
    };
    if snapshot.profile.archived_at.is_some() {
        return Ok(Refusal::Archived.response());
    }
    let validated = match validate_plates(next, &tokens(&snapshot)?, false, Spacing::Clearance) {
        Ok(validated) => validated,
        Err(LayoutFailure::InvalidUnits) => {
            return Ok(failure(
                422,
                "invalid_units",
                "Accepted Plate units are invalid",
                Map::new(),
            ));
        }
        Err(other) => return Ok(Refusal::from(other).response()),
    };
    let digest = layout_digest(&validated, LAYOUT_FORMAT);
    if digest.as_str() != current.layout_digest {
        write_revision(
            tx,
            tenant,
            cmd,
            &validated,
            &digest,
            all.then_some(current.id),
        )?;
    }
    let mut response = read(tx, tenant, cmd.profile, repos)?;
    in_packing_order(&mut response, &validated);
    Ok(response)
}

/// Node answers with the packed layout, so each Plate lists its units in
/// packing order rather than the stored order a read uses.
fn in_packing_order(response: &mut Response, validated: &[ValidatedPlate]) {
    let Some(views) = response.body["plates"].as_array_mut() else {
        return;
    };
    for (view, plate) in views.iter_mut().zip(validated) {
        let order: Vec<&str> = plate
            .input
            .units
            .iter()
            .map(|unit| unit.token.as_str())
            .collect();
        if let Some(units) = view["units"].as_array_mut() {
            units.sort_by_key(|unit| {
                order
                    .iter()
                    .position(|token| unit["token"].as_str() == Some(*token))
            });
        }
    }
}

fn unit_too_large((token, printer): TooLarge) -> Response {
    let mut fields = Map::new();
    fields.insert("token".into(), json!(token.as_str()));
    fields.insert("printer_id".into(), json!(printer));
    failure(
        422,
        "unit_too_large",
        "Required unit does not fit the selected Printer",
        fields,
    )
}

fn packing(unit: &UnitInput) -> PackingUnit {
    PackingUnit {
        token: unit.token.clone(),
        width_um: unit.width_um,
        depth_um: unit.depth_um,
        height_um: unit.height_um,
    }
}

fn placed(packed: PackedUnit, placement: Placement, pinned: bool) -> UnitInput {
    UnitInput {
        token: packed.unit.token,
        x_um: packed.x_um,
        y_um: packed.y_um,
        width_um: packed.unit.width_um,
        depth_um: packed.unit.depth_um,
        height_um: packed.unit.height_um,
        placement,
        pinned,
    }
}

/// Node's `plateId` for a Plate an arrange adds.
fn new_plate_id(basis: &Basis, printer: &str, units: &[PackedUnit]) -> Result<PlateId> {
    let accepted = AcceptedPlanBasis {
        profile_id: basis.profile_id as u64,
        plan_version: basis.plan_version as u64,
        plan_revision_id: basis.revision_id as u64,
        plan_revision_digest: Digest::parse(basis.revision_digest.as_str())?,
        required_unit_mapping_digest: Digest::parse(basis.mapping_digest.as_str())?,
    };
    let tokens: Vec<RequiredUnitToken> = units.iter().map(|unit| unit.unit.token.clone()).collect();
    initial_plate_id(&accepted, printer, &tokens)
}

type Arranged = Result<std::result::Result<Vec<PlateInput>, TooLarge>>;

/// Node's `arrangedPlates`.
fn arranged(
    all: bool,
    basis: &Basis,
    plates: &[ValidatedPlate],
    units: &[(&graph::Part, &graph::Unit)],
    rules: &[GroupRule],
) -> Arranged {
    if all {
        repacked(basis, plates, units, rules)
    } else {
        around_fixed(basis, plates)
    }
}

/// `unplaced`: each Plate's free units pack around its manual and pinned ones.
fn around_fixed(basis: &Basis, plates: &[ValidatedPlate]) -> Arranged {
    let mut next = Vec::new();
    for plate in plates {
        let input = &plate.input;
        let fixed = |unit: &&UnitInput| unit.placement == Placement::Manual || unit.pinned;
        let occupied: Vec<PackedUnit> = input
            .units
            .iter()
            .filter(fixed)
            .map(|unit| PackedUnit {
                unit: packing(unit),
                x_um: unit.x_um,
                y_um: unit.y_um,
            })
            .collect();
        let moving: Vec<PackingUnit> = input
            .units
            .iter()
            .filter(|unit| !fixed(unit))
            .map(packing)
            .collect();
        let packed = match pack_units_around(&input.printer, &occupied, &moving) {
            PackResult::Packed(packed) => packed,
            PackResult::UnitTooLarge(token) => {
                return Ok(Err((token, input.printer_id.clone())));
            }
        };
        for (index, members) in packed.into_iter().enumerate() {
            let plate_id = match index {
                0 => input.plate_id.clone(),
                _ => new_plate_id(basis, &input.printer_id, &members)?,
            };
            let units = members
                .into_iter()
                .map(|member| {
                    let original = input
                        .units
                        .iter()
                        .find(|unit| unit.token == member.unit.token);
                    let manual = original.is_some_and(|unit| unit.placement == Placement::Manual);
                    let pinned = original.is_some_and(|unit| unit.pinned);
                    let placement = if manual {
                        Placement::Manual
                    } else {
                        Placement::Auto
                    };
                    placed(member, placement, pinned)
                })
                .collect();
            next.push(PlateInput {
                plate_id,
                units,
                ..input.clone()
            });
        }
    }
    Ok(Ok(next))
}

/// `all`: every unit of each Printer repacks by grouping bucket.
fn repacked(
    basis: &Basis,
    plates: &[ValidatedPlate],
    units: &[(&graph::Part, &graph::Unit)],
    rules: &[GroupRule],
) -> Arranged {
    let mut next = Vec::new();
    let by_token: HashMap<&str, _> = units
        .iter()
        .map(|(part, unit)| (unit.token.as_str(), (*part, *unit)))
        .collect();
    let mut printers: Vec<(&str, Vec<&PlateInput>)> = Vec::new();
    for plate in plates {
        let input = &plate.input;
        match printers.iter_mut().find(|(id, _)| *id == input.printer_id) {
            Some((_, group)) => group.push(input),
            None => printers.push((input.printer_id.as_str(), vec![input])),
        }
    }
    for (printer, group) in printers {
        let first = group[0];
        let mut members = Vec::new();
        for unit in group.iter().flat_map(|plate| &plate.units) {
            let (part, row) = by_token
                .get(unit.token.as_str())
                .ok_or_else(|| anyhow!(Integrity("layout")))?;
            let setup = setup_unit(part, row);
            let text = |key: &str| setup.get(key).and_then(Value::as_str).map(str::to_owned);
            members.push(GroupUnit {
                value: packing(unit),
                token: unit.token.clone(),
                object_name: text("object_name").unwrap_or_default(),
                filename: text("filename").unwrap_or_default(),
                source_directory: text("source_directory").unwrap_or_default(),
                source_layer: text("source_layer").unwrap_or_default(),
                role: text("role").unwrap_or_default(),
                filament_color_id: text("filament_color_id"),
                filament_custom_hex: text("filament_custom_hex"),
                material_type: None,
            });
        }
        let mut index = 0;
        for bucket in grouping_buckets(&members, rules) {
            let packed = match pack_units(&first.printer, &bucket) {
                PackResult::Packed(packed) => packed,
                PackResult::UnitTooLarge(token) => return Ok(Err((token, printer.to_owned()))),
            };
            for members in packed {
                let plate_id = match group.get(index) {
                    Some(plate) => plate.plate_id.clone(),
                    None => new_plate_id(basis, printer, &members)?,
                };
                index += 1;
                next.push(PlateInput {
                    plate_id,
                    units: members
                        .into_iter()
                        .map(|member| placed(member, Placement::Auto, false))
                        .collect(),
                    ..first.clone()
                });
            }
        }
    }
    Ok(Ok(next))
}
