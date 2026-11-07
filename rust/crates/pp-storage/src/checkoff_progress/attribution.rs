//! Which Required units a print's object names attribute to. One port of
//! Node's `resolveAcceptedPrinterAttribution`, `confirmAcceptedPrinterUnits`
//! and `applyConfirmedObjectMappings` (`db/accepted-printer-attribution.ts`),
//! shared by the unattributed-print claim and the print-file assignment.
//!
//! The serialized `Attribution` is the `attribution` object Node's routes
//! return, with its camelCase keys.

use super::{Refusal, Response};
use crate::read_model::{AcceptedRead, Part, Snapshot, Unit as RequiredUnit};
use anyhow::Result;
use pp_domain::gcode_objects::{
    interpret_sliced_object_name, match_object_name, match_object_name_exact,
};
use rusqlite::Transaction;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// A Required-unit coordinate, as the browser round-trips it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unit {
    pub part_id: i64,
    pub unit_index: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_name: Option<String>,
}

impl Unit {
    pub fn at(part_id: i64, unit_index: i64) -> Self {
        Self {
            part_id,
            unit_index,
            object_name: None,
        }
    }

    fn key(&self) -> (i64, i64) {
        (self.part_id, self.unit_index)
    }
}

/// What one observed object name came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NameOutcome {
    pub input_index: usize,
    pub raw_name: String,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<Unit>,
}

/// `acceptedPlanBasis`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Basis {
    pub profile_id: i64,
    pub plan_version: i64,
    pub revision_id: i64,
    pub revision_digest: String,
    pub required_unit_mapping_digest: String,
}

/// `AcceptedPrinterAttribution`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attribution {
    pub expected: Basis,
    pub units: Vec<Unit>,
    pub outcomes: Vec<NameOutcome>,
    pub unmatched_object_names: Vec<String>,
    pub fallback: &'static str,
}

/// The Build's Accepted Plan to attribute against, or Node's
/// `materializeProblem` wording for why there is none. Node throws for a
/// Build that does not exist, so that is a 500 like an integrity failure.
pub(super) fn accepted(
    tx: &Transaction<'_>,
    tenant: &str,
    profile: i64,
    repos: &Path,
) -> Result<std::result::Result<Snapshot, std::result::Result<Response, Refusal>>> {
    let detail = match super::read(tx, tenant, profile, repos)? {
        AcceptedRead::Ready { snapshot } => return Ok(Ok(*snapshot)),
        AcceptedRead::Empty { .. } => "Accepted Plan has no Required units",
        AcceptedRead::CompatibilityDirty => "Accepted Plan requires compatibility repair",
        AcceptedRead::Uninitialized => "Accepted Plan operational state is not initialized",
        AcceptedRead::Missing | AcceptedRead::IntegrityFailure { .. } => {
            return Ok(Err(Err(Refusal::Internal)));
        }
    };
    Ok(Err(Ok(Response::failure(409, detail, None))))
}

fn part_path(part: &Part) -> &str {
    if part.relative_path.is_empty() {
        &part.filename
    } else {
        &part.relative_path
    }
}

/// `interpretSlicedObjectName(raw).unwrappedName` with a trailing `_stl`
/// read as `.stl`, in lower case.
fn parsed_object_name(raw: &str) -> String {
    let name = interpret_sliced_object_name(raw).unwrapped_name;
    let cut = name.len().saturating_sub(4);
    match name.get(cut..) {
        Some(tail) if name.len() >= 4 && tail.eq_ignore_ascii_case("_stl") => {
            format!("{}.stl", &name[..cut])
        }
        _ => name,
    }
    .to_lowercase()
}

fn unmatched_names(outcomes: &[NameOutcome]) -> Vec<String> {
    outcomes
        .iter()
        .filter(|outcome| outcome.unit.is_none())
        .map(|outcome| outcome.raw_name.clone())
        .collect()
}

/// The included Parts with Required units, and their Required units in order.
struct Slots<'a> {
    parts: Vec<&'a Part>,
    units: Vec<(&'a Part, &'a RequiredUnit)>,
    positive_only: bool,
}

impl<'a> Slots<'a> {
    fn new(snapshot: &'a Snapshot, positive_only: bool) -> Self {
        let parts: Vec<&Part> = snapshot
            .parts
            .iter()
            .filter(|part| part.included && part.units.iter().any(|unit| unit.required))
            .collect();
        let units = parts
            .iter()
            .flat_map(|part| {
                part.units
                    .iter()
                    .filter(|unit| unit.required)
                    .map(move |unit| (*part, unit))
            })
            .collect();
        Self {
            parts,
            units,
            positive_only,
        }
    }

    /// `matchingAcceptedParts` for one name.
    fn matching(&self, raw: &str) -> Vec<&'a Part> {
        let paths: Vec<&str> = self.parts.iter().map(|part| part_path(part)).collect();
        let found = if self.positive_only {
            match_object_name_exact(raw, &paths)
        } else {
            match_object_name(raw, &paths)
        };
        let wanted: HashSet<String> = found.iter().map(|path| path.to_lowercase()).collect();
        self.parts
            .iter()
            .filter(|part| wanted.contains(&part_path(part).to_lowercase()))
            .copied()
            .collect()
    }

    /// The Part's first incomplete Required unit not in `used`.
    fn free(&self, part: &Part, used: &HashSet<(i64, i64)>) -> Option<Unit> {
        self.units
            .iter()
            .find(|(slot_part, unit)| {
                slot_part.projection_part_id == part.projection_part_id
                    && !unit.completed
                    && !used.contains(&(part.projection_part_id, unit.unit_index))
            })
            .map(|(_, unit)| Unit::at(part.projection_part_id, unit.unit_index))
    }
}

/// `resolveAcceptedPrinterAttribution`. An object name that is a Required
/// unit's name binds that unit. Any other name that matches exactly one Part
/// by file name takes that Part's first free incomplete unit. With nothing
/// bound and no Required-unit name seen, the fallback file name may bind one
/// unit. `positive_only` drops fuzzy file-name matches and, when object
/// names were observed, the fallback.
pub fn resolve(
    snapshot: &Snapshot,
    names: &[String],
    fallback: Option<&str>,
    positive_only: bool,
) -> Attribution {
    let slots = Slots::new(snapshot, positive_only);
    let canonical: HashMap<String, (&Part, &RequiredUnit)> = slots
        .units
        .iter()
        .map(|slot| (slot.1.object_name.to_lowercase(), *slot))
        .collect();
    let outcome = |index: usize, kind, unit| NameOutcome {
        input_index: index,
        raw_name: names[index].clone(),
        kind,
        unit,
    };
    let mut used = HashSet::new();
    let mut outcomes: Vec<Option<NameOutcome>> = vec![None; names.len()];
    for (index, raw) in names.iter().enumerate() {
        let Some((part, unit)) = canonical.get(&parsed_object_name(raw)) else {
            continue;
        };
        let coordinate = Unit::at(part.projection_part_id, unit.unit_index);
        outcomes[index] = Some(if unit.completed {
            outcome(index, "already_completed", None)
        } else if !used.insert(coordinate.key()) {
            outcome(index, "duplicate_observation", None)
        } else {
            outcome(index, "required_object_name", Some(coordinate))
        });
    }
    let recognized = outcomes.iter().any(Option::is_some);
    for (index, raw) in names.iter().enumerate() {
        if outcomes[index].is_some() {
            continue;
        }
        let candidates = slots.matching(raw);
        outcomes[index] = Some(if candidates.len() > 1 {
            outcome(index, "ambiguous_filename", None)
        } else if let Some(unit) = candidates.first().and_then(|part| slots.free(part, &used)) {
            used.insert(unit.key());
            outcome(index, "legacy_filename", Some(unit))
        } else {
            outcome(index, "unmatched", None)
        });
    }
    let outcomes: Vec<NameOutcome> = outcomes.into_iter().flatten().collect();
    let mut units: Vec<Unit> = outcomes.iter().filter_map(|o| o.unit.clone()).collect();
    let mut fallback_kind = if recognized {
        "recognized_observation"
    } else {
        "unused"
    };
    if let Some(name) = fallback.filter(|name| !super::links::js_trim(name).is_empty())
        && units.is_empty()
        && !recognized
        && (!positive_only || names.is_empty())
        && let [part] = slots.matching(name).as_slice()
        && let Some(unit) = slots.free(part, &HashSet::new())
    {
        units.push(unit);
        fallback_kind = "used";
    }
    Attribution {
        expected: Basis {
            profile_id: snapshot.profile.id,
            plan_version: snapshot.plan_version,
            revision_id: snapshot.revision_id,
            revision_digest: snapshot.revision_digest.clone(),
            required_unit_mapping_digest: snapshot.required_unit_mapping_digest.clone(),
        },
        unmatched_object_names: unmatched_names(&outcomes),
        units,
        outcomes,
        fallback: fallback_kind,
    }
}

/// `confirmAcceptedPrinterUnits`: each pick must be a Required unit of an
/// included Part that is still incomplete, picked once. A confirmed unit
/// carries its Required-unit object name. `None` is Node's `rejected`.
pub fn confirm(snapshot: &Snapshot, picks: &[Unit]) -> Option<Vec<Unit>> {
    let slots: HashMap<(i64, i64), &RequiredUnit> = snapshot
        .parts
        .iter()
        .filter(|part| part.included)
        .flat_map(|part| {
            part.units
                .iter()
                .filter(|unit| unit.required)
                .map(move |unit| ((part.projection_part_id, unit.unit_index), unit))
        })
        .collect();
    let mut taken = HashSet::new();
    picks
        .iter()
        .map(|pick| {
            let slot = slots.get(&pick.key())?;
            if slot.completed || !taken.insert(pick.key()) {
                return None;
            }
            Some(Unit {
                object_name: (!slot.object_name.is_empty()).then(|| slot.object_name.clone()),
                ..Unit::at(pick.part_id, pick.unit_index)
            })
        })
        .collect()
}

/// One operator choice: object `object_index` is unit `unit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    pub object_index: usize,
    pub unit: Unit,
}

/// `applyConfirmedObjectMappings`: each mapping must point an unmatched or
/// ambiguous object at a distinct confirmed unit. A matched object whose unit
/// was not confirmed, or was taken by a mapping, becomes unmatched.
pub fn apply_mappings(
    suggestion: &Attribution,
    confirmed: &[Unit],
    mappings: &[Mapping],
) -> Option<Attribution> {
    let by_key: HashMap<(i64, i64), &Unit> = confirmed.iter().map(|u| (u.key(), u)).collect();
    let mut indexed: HashMap<usize, Unit> = HashMap::new();
    let mut used = HashSet::new();
    for mapping in mappings {
        let outcome = suggestion.outcomes.get(mapping.object_index)?;
        let unit = by_key.get(&mapping.unit.key())?;
        if !matches!(outcome.kind, "unmatched" | "ambiguous_filename")
            || indexed.contains_key(&mapping.object_index)
            || !used.insert(unit.key())
        {
            return None;
        }
        indexed.insert(mapping.object_index, (*unit).clone());
    }
    let outcomes: Vec<NameOutcome> = suggestion
        .outcomes
        .iter()
        .map(|outcome| {
            let replaced = |kind, unit| NameOutcome {
                input_index: outcome.input_index,
                raw_name: outcome.raw_name.clone(),
                kind,
                unit,
            };
            if let Some(unit) = indexed.get(&outcome.input_index) {
                return replaced("user_confirmed", Some(unit.clone()));
            }
            match &outcome.unit {
                Some(unit) if !by_key.contains_key(&unit.key()) || used.contains(&unit.key()) => {
                    replaced("unmatched", None)
                }
                _ => outcome.clone(),
            }
        })
        .collect();
    Some(Attribution {
        expected: suggestion.expected.clone(),
        units: confirmed.to_vec(),
        unmatched_object_names: unmatched_names(&outcomes),
        outcomes,
        fallback: "unused",
    })
}
