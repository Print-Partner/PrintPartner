//! Node's `initializeAcceptedPlates` (`services/accepted-plate-workspace.ts`)
//! and `publishAcceptedPlatesInternal` (`db/accepted-plates.ts`):
//! `POST /plans/{id}/plates/initialize` measures each assigned Required unit's
//! accepted STL, packs every Printer's units by the Production setup's
//! grouping rules, and publishes the layout as one new Plate revision.
//!
//! Node reads, measures and publishes in separate steps; here they run in one
//! immediate transaction, so the STL reads hold the writer.

use super::arrange::{
    TooLarge, group_unit, in_packing_order, new_plate_id, placed, production_rules, refused_layout,
    unit_too_large,
};
use super::artifacts::{self, Failure, LIMITS};
use super::*;
use crate::production::layout::{
    GroupRule, PackResult, PackingUnit, first_too_large, grouping_buckets, pack_units,
};
use std::collections::{BTreeMap, BTreeSet};

/// Width, depth and height in micrometres, by token.
type Sizes = HashMap<String, [u64; 3]>;

pub(super) struct Command {
    profile: i64,
    basis: Basis,
    /// The head the caller saw: `None` before any Plate revision.
    revision: Option<i64>,
    assignments: Vec<(RequiredUnitToken, Option<String>)>,
}

impl Command {
    /// Node's `parseInitializeRequest` and the route's Plan id check.
    pub(super) fn parse(profile: i64, body: &Value) -> Option<Self> {
        let body = body.as_object()?;
        let basis = Basis::parse(body.get("expected")?)?;
        let revision = match body.get("expected_plate_revision_id")? {
            Value::Null => None,
            value => Some(positive(value.as_f64()?)?),
        };
        let assignments = body
            .get("assignments")?
            .as_array()?
            .iter()
            .map(|raw| {
                let raw = raw.as_object()?;
                let token =
                    RequiredUnitToken::parse(raw.get("token")?.as_str()?.to_owned()).ok()?;
                let printer = match raw.get("printer_id")? {
                    Value::Null => None,
                    value => Some(trimmed_text(value.as_str()?)?),
                };
                Some((token, printer))
            })
            .collect::<Option<Vec<_>>>()?;
        (basis.profile_id == profile).then_some(Self {
            profile,
            basis,
            revision,
            assignments,
        })
    }
}

fn tokens_failure(
    status: u16,
    code: &str,
    detail: &str,
    key: &str,
    values: Vec<String>,
) -> Response {
    let mut fields = Map::new();
    fields.insert(key.into(), json!(values));
    failure(status, code, detail, fields)
}

/// Node's `assignmentFailure`, tokens in UTF-8 order.
fn assignment_failure(
    units: &[(&graph::Part, &graph::Unit)],
    assignments: &[(RequiredUnitToken, Option<String>)],
) -> Option<Response> {
    let expected: BTreeSet<&str> = units.iter().map(|(_, unit)| unit.token.as_str()).collect();
    let mut seen = HashSet::new();
    let mut duplicates = BTreeSet::new();
    let mut unknown = BTreeSet::new();
    for (token, _) in assignments {
        let token = token.as_str();
        if !expected.contains(token) {
            unknown.insert(token);
        }
        if !seen.insert(token) {
            duplicates.insert(token);
        }
    }
    let incomplete = |code, tokens: Vec<&str>| {
        let tokens = tokens.into_iter().map(str::to_owned).collect();
        Some(tokens_failure(
            422,
            code,
            "Printer assignments are incomplete",
            "tokens",
            tokens,
        ))
    };
    if !unknown.is_empty() {
        return incomplete("unknown_unit_token", unknown.into_iter().collect());
    }
    if !duplicates.is_empty() {
        return incomplete("duplicate_assignment", duplicates.into_iter().collect());
    }
    if assignments.is_empty() {
        return incomplete("missing_assignment", expected.into_iter().collect());
    }
    let mut unassigned: Vec<&str> = assignments
        .iter()
        .filter(|(_, printer)| printer.is_none())
        .map(|(token, _)| token.as_str())
        .collect();
    unassigned.sort_unstable();
    (!unassigned.is_empty())
        .then(|| incomplete("unassigned_units", unassigned))
        .flatten()
}

fn geometry_failure(failure_kind: Failure) -> Response {
    let token = |token: String| {
        let mut fields = Map::new();
        fields.insert("token".into(), json!(token));
        fields
    };
    match failure_kind {
        Failure::Unavailable {
            token: unit,
            reason,
        } => failure(409, reason, "Accepted artifact is unavailable", token(unit)),
        Failure::InvalidStl(unit) => failure(
            409,
            "invalid_stl",
            "Accepted artifact is not a valid STL",
            token(unit),
        ),
        Failure::Degenerate(unit) => failure(
            422,
            "degenerate_geometry",
            "Accepted artifact geometry cannot be arranged",
            token(unit),
        ),
        Failure::LimitExceeded(limit) => {
            let mut fields = Map::new();
            fields.insert("limit".into(), json!(limit));
            failure(
                413,
                "limit_exceeded",
                "Accepted Plate limit exceeded",
                fields,
            )
        }
    }
}

/// What packing the assigned units produced.
enum Packed {
    /// `outside` when an assigned Printer's bed is past the stored Plate
    /// range and every unit fits it. Node packs that Printer's units onto it
    /// and then refuses the layout as `outside_build_area`, after the
    /// archived check, so its units are left out here and the other Printers
    /// still pack.
    Plates {
        plates: Vec<PlateInput>,
        outside: bool,
    },
    TooLarge(TooLarge),
}

struct Packing<'a> {
    basis: &'a Basis,
    units: &'a [(&'a graph::Part, &'a graph::Unit)],
    assignments: &'a [(RequiredUnitToken, Option<String>)],
    printers: &'a BTreeMap<&'a str, &'a Printer>,
    dimensions: &'a Sizes,
    rules: &'a [GroupRule],
}

/// Node's `packAssignedPlates`: Printers in UTF-8 id order, each Printer's
/// units in Plan order, split into grouping buckets and packed. `None` when a
/// unit has no measured dimensions, which Node treats as a broken invariant.
fn pack_assigned(input: &Packing<'_>) -> Result<Option<Packed>> {
    let printer_of: HashMap<&str, Option<&str>> = input
        .assignments
        .iter()
        .map(|(token, printer)| (token.as_str(), printer.as_deref()))
        .collect();
    let mut plates = Vec::new();
    let mut outside = false;
    for (printer_id, printer) in input.printers {
        let mut members = Vec::new();
        for (part, row) in input.units {
            if printer_of.get(row.token.as_str()).copied().flatten() != Some(*printer_id) {
                continue;
            }
            let Some(size) = input.dimensions.get(&row.token) else {
                return Ok(None);
            };
            let token = RequiredUnitToken::parse(row.token.clone())?;
            let dimension = |value: u64| {
                i64::try_from(value)
                    .ok()
                    .and_then(|v| DimensionUm::parse(v).ok())
            };
            let [Some(width_um), Some(depth_um), Some(height_um)] = size.map(dimension) else {
                return Ok(Some(Packed::TooLarge((token, (*printer_id).to_owned()))));
            };
            let value = PackingUnit {
                token,
                width_um,
                depth_um,
                height_um,
            };
            members.push(group_unit(part, row, value));
        }
        for bucket in grouping_buckets(&members, input.rules) {
            let Some(geometry) = &printer.geometry else {
                if let Some(token) = first_too_large(printer.um, &bucket) {
                    return Ok(Some(Packed::TooLarge((token, (*printer_id).to_owned()))));
                }
                outside = true;
                continue;
            };
            let packed = match pack_units(geometry, &bucket) {
                PackResult::Packed(packed) => packed,
                PackResult::UnitTooLarge(token) => {
                    return Ok(Some(Packed::TooLarge((token, (*printer_id).to_owned()))));
                }
            };
            for members in packed {
                plates.push(PlateInput {
                    plate_id: new_plate_id(input.basis, printer_id, &members)?,
                    printer_id: (*printer_id).to_owned(),
                    printer_name: printer.name.clone(),
                    printer_model: printer.model.clone(),
                    printer: geometry.clone(),
                    units: members
                        .into_iter()
                        .map(|member| placed(member, Placement::Auto, false))
                        .collect(),
                });
            }
        }
    }
    Ok(Some(Packed::Plates { plates, outside }))
}

/// The fleet Printer of each assigned id, as Node's `fleetById` keeps the
/// last row per id, or 422 `printer_not_found` or `missing_printer_geometry`.
fn assigned_printers<'a>(
    fleet: &'a [(String, Option<Printer>)],
    assignments: &'a [(RequiredUnitToken, Option<String>)],
) -> std::result::Result<BTreeMap<&'a str, &'a Printer>, Response> {
    let assigned: BTreeSet<&str> = assignments
        .iter()
        .filter_map(|(_, printer)| printer.as_deref())
        .collect();
    let machine = |id: &str| {
        fleet
            .iter()
            .rev()
            .find(|(raw, _)| raw == id)
            .map(|(_, printer)| printer)
    };
    let unknown: Vec<String> = assigned
        .iter()
        .filter(|id| machine(id).is_none())
        .map(|id| (*id).to_owned())
        .collect();
    if !unknown.is_empty() {
        return Err(tokens_failure(
            422,
            "printer_not_found",
            "Printer was not found",
            "printer_ids",
            unknown,
        ));
    }
    let mut printers = BTreeMap::new();
    let mut missing = Vec::new();
    for id in assigned {
        match machine(id).and_then(Option::as_ref) {
            Some(printer) => {
                printers.insert(id, printer);
            }
            None => missing.push(id.to_owned()),
        }
    }
    if missing.is_empty() {
        Ok(printers)
    } else {
        Err(tokens_failure(
            422,
            "missing_printer_geometry",
            "Printer build geometry is incomplete",
            "printer_ids",
            missing,
        ))
    }
}

pub(super) fn run(
    tx: &Transaction<'_>,
    tenant: &str,
    repos: &Path,
    cmd: &Command,
) -> Result<Response> {
    let snapshot = match accepted_for_write(tx, tenant, cmd.profile, repos)? {
        Ok(snapshot) => snapshot,
        Err(response) => return Ok(response),
    };
    let basis = Basis::of(&snapshot);
    let units = required(&snapshot);
    let stored = stored_layout(tx, tenant, cmd.profile, &basis, &snapshot)?;
    if basis != cmd.basis {
        return Ok(Refusal::Stale.response());
    }
    let rules = production_rules(tx, tenant, cmd.profile)?;
    let (head, current) = match &stored {
        Stored::Setup(head) => (*head, None),
        Stored::Ready(revision, plates) => (Some(revision.id), Some(plates)),
    };
    let matches = head == cmd.revision;
    if !matches && current.is_none() {
        return Ok(Refusal::RevisionChanged.response());
    }
    let fleet = fleet(tx, tenant)?;
    let checked = match assignment_failure(&units, &cmd.assignments) {
        Some(refused) => Err(refused),
        None => assigned_printers(&fleet, &cmd.assignments),
    };
    let printers = match checked {
        Ok(printers) => printers,
        Err(_) if !matches => return Ok(Refusal::RevisionChanged.response()),
        Err(refused) => return Ok(refused),
    };
    let packing = |dimensions: &Sizes| {
        pack_assigned(&Packing {
            basis: &basis,
            units: &units,
            assignments: &cmd.assignments,
            printers: &printers,
            dimensions,
            rules: &rules,
        })
    };
    if let (false, Some(current)) = (matches, current) {
        let dimensions = current
            .iter()
            .flat_map(|plate| &plate.input.units)
            .map(|unit| {
                let size = [unit.width_um, unit.depth_um, unit.height_um]
                    .map(|value| u64::from(value.get()));
                (unit.token.as_str().to_owned(), size)
            })
            .collect();
        let replayed = match packing(&dimensions)? {
            Some(Packed::Plates {
                plates,
                outside: false,
            }) => plates.iter().eq(current.iter().map(|plate| &plate.input)),
            _ => false,
        };
        return if replayed {
            read(tx, tenant, cmd.profile, repos)
        } else {
            Ok(Refusal::RevisionChanged.response())
        };
    }
    let assigned_units = units
        .iter()
        .filter(|(_, unit)| {
            cmd.assignments
                .iter()
                .any(|(token, _)| token.as_str() == unit.token)
        })
        .map(|(part, unit)| (unit.token.as_str(), &part.artifact))
        .collect::<Vec<_>>();
    let dimensions = match artifacts::load(repos, assigned_units.into_iter(), LIMITS) {
        Ok(dimensions) => dimensions,
        Err(refused) => return Ok(geometry_failure(refused)),
    };
    let (plates, outside) = match packing(&dimensions)?
        .ok_or_else(|| anyhow!("Accepted artifact geometry is missing"))?
    {
        Packed::Plates { plates, outside } => (plates, outside),
        Packed::TooLarge(too_large) => return Ok(unit_too_large(too_large)),
    };
    if outside && snapshot.profile.archived_at.is_none() {
        return Ok(Refusal::Geometry("outside_build_area").response());
    }
    publish(tx, tenant, repos, cmd, &stored, plates, &snapshot)
}

/// Node's `publishAcceptedPlatesInternal` after its basis check: refuses an
/// archived Build, validates the layout, and writes it unless it equals the
/// head of the same Accepted Plan. Node answers with the packed layout, which
/// links no arrange to undo.
fn publish(
    tx: &Transaction<'_>,
    tenant: &str,
    repos: &Path,
    cmd: &Command,
    stored: &Stored,
    plates: Vec<PlateInput>,
    snapshot: &Snapshot,
) -> Result<Response> {
    if snapshot.profile.archived_at.is_some() {
        return Ok(Refusal::Archived.response());
    }
    let validated = match validate_plates(plates, &tokens(snapshot)?, false, Spacing::Clearance) {
        Ok(validated) => validated,
        Err(refused) => return Ok(refused_layout(refused)),
    };
    let digest = layout_digest(&validated, LAYOUT_FORMAT);
    let (head, revision) = match stored {
        Stored::Ready(current, _) if current.layout_digest == digest.as_str() => (None, 0),
        Stored::Ready(current, _) => (Some(HeadWrite::Advance(None)), current.id),
        Stored::Setup(Some(id)) => (Some(HeadWrite::Advance(None)), *id),
        Stored::Setup(None) => (Some(HeadWrite::Create), 0),
    };
    if let Some(head) = head {
        let expected = Expected {
            profile: cmd.profile,
            basis: cmd.basis.clone(),
            revision,
        };
        write_revision(tx, tenant, &expected, &validated, &digest, head)?;
    }
    let mut response = read(tx, tenant, cmd.profile, repos)?;
    in_packing_order(&mut response, &validated);
    response.body["arrange_undo_revision_id"] = Value::Null;
    Ok(response)
}
