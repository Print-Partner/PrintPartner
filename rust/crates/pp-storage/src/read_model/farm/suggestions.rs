//! Match suggestions for unmatched prints (GRE-1091): a proposed Build and
//! Plate per open unmatched print, with a confidence word and the evidence
//! used. A suggestion is a read. Nothing here, and no job or timer anywhere,
//! confirms one; the operator claims a print through the Checkoff operation.
//!
//! - `exact`: every printed object carries a Required-unit token, every token
//!   belongs to one open Build, and together they are exactly one Plate of
//!   that Build's current accepted layout.
//! - `likely`: one Build stands out, from Required-unit tokens that do not
//!   make a whole Plate, from the object list matching that Build's part
//!   files, or from the file name. File name and time window break ties.
//! - `none`: no evidence, or evidence that points at more than one Build.
//!
//! Only `exact` rows start checked.

use super::{CheckoffResult, Farm, OpenPrint, QueuedSend, instant, iso_time, normalized_filename};
use anyhow::Result;
use pp_domain::gcode_objects::{
    group_objects_by_part, interpret_sliced_object_name, match_objects_to_filenames,
};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Sends and results to the same printer this long before a completion count
/// as time-window evidence.
pub const TIME_WINDOW_HOURS: i64 = 72;
const TOKEN_PREFIX: &str = "ppu_";
const TOKEN_HEX: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum MatchConfidence {
    Exact,
    Likely,
    None,
}

/// Evidence that decided a suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum MatchBasis {
    /// Required-unit tokens in the printed object names.
    UnitTokens,
    /// Printed object names matched to the Build's part files.
    ObjectList,
    /// The file name was sent, linked or checked off for the Build before.
    FileName,
    /// The Build was sent or linked to this printer shortly before completion.
    TimeWindow,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct SuggestedBuild {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct SuggestedPlate {
    pub plate_id: String,
    pub ordinal: i64,
}

/// Open Builds whose earlier sends, results or checked-off jobs used the
/// print's file name (trimmed basename, case-insensitive).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct FileNameEvidence {
    pub build_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct ObjectListEvidence {
    pub object_count: u32,
    /// Objects whose name carries a Required-unit token.
    pub unit_token_count: u32,
    /// Open Builds that own at least one of those tokens.
    pub unit_build_ids: Vec<i64>,
    /// Open Builds whose part files match every printed object. Computed only
    /// when no object carries a token.
    pub part_build_ids: Vec<i64>,
}

/// Open Builds sent or linked to the print's printer between `start` and
/// `end` (the completion).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct TimeWindowEvidence {
    pub start: String,
    pub end: String,
    pub build_ids: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct MatchEvidence {
    /// What decided the suggested Build; empty for `none`.
    pub basis: Vec<MatchBasis>,
    pub file_name: FileNameEvidence,
    pub object_list: ObjectListEvidence,
    /// `null` when the completion time does not parse.
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<TimeWindowEvidence>")
    )]
    pub time_window: Option<TimeWindowEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct MatchSuggestion {
    pub print_id: String,
    pub printer_id: String,
    pub filename: String,
    pub completed_at: String,
    pub confidence: MatchConfidence,
    /// True only for `exact`: a review UI starts every other row unchecked.
    pub starts_checked: bool,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<SuggestedBuild>")
    )]
    pub build: Option<SuggestedBuild>,
    #[cfg_attr(
        feature = "codegen",
        schemars(with = "pp_contracts::RequiredNullable<SuggestedPlate>")
    )]
    pub plate: Option<SuggestedPlate>,
    pub evidence: MatchEvidence,
}

/// `GET /api/v2/printer-checkoff/unattributed/suggestions` body: one row per
/// open unmatched print, in the order `/unattributed` lists them.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct MatchSuggestionList {
    pub suggestions: Vec<MatchSuggestion>,
}

#[derive(Debug, Clone, PartialEq)]
struct Plate {
    plate_id: String,
    ordinal: i64,
    tokens: BTreeSet<String>,
    /// Comparison keys of the units' object names without their token.
    stems: Vec<String>,
}

/// The open Builds' facts a suggestion is drawn from, read in the farm read's
/// transaction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MatchLibrary {
    builds: BTreeMap<i64, String>,
    part_files: BTreeMap<i64, Vec<String>>,
    /// Token to owning open Build, only for tokens the open prints carry.
    units: BTreeMap<String, i64>,
    plates: BTreeMap<i64, Vec<Plate>>,
    /// Normalized file name to open Builds whose checked-off jobs used it.
    job_files: BTreeMap<String, BTreeSet<i64>>,
}

impl MatchLibrary {
    pub(super) fn read(tx: &Transaction<'_>, tenant: &str, farm: &Farm) -> Result<Self> {
        let mut library = Self::default();
        let open = "JOIN build_profiles b ON b.id=t.profile_id AND b.tenant_id=t.tenant_id
             WHERE t.tenant_id=?1 AND b.archived_at IS NULL";
        library.builds = id_text(
            tx,
            "SELECT id,name FROM build_profiles WHERE tenant_id=?1 AND archived_at IS NULL ORDER BY id",
            tenant,
        )?
        .into_iter()
        .collect();
        for (build, filename) in id_text(
            tx,
            &format!(
                "SELECT t.profile_id,t.filename FROM parts t {open} AND t.included=1 AND t.filename<>'' ORDER BY t.profile_id,t.id"
            ),
            tenant,
        )? {
            library.part_files.entry(build).or_default().push(filename);
        }
        for (build, filename) in id_text(
            tx,
            &format!(
                "SELECT DISTINCT t.profile_id,t.filename FROM print_jobs t {open} AND t.filename IS NOT NULL"
            ),
            tenant,
        )? {
            library
                .job_files
                .entry(normalized_filename(&filename))
                .or_default()
                .insert(build);
        }
        let mut unit = tx.prepare(&format!(
            "SELECT t.profile_id FROM required_units t {open} AND t.token=?2"
        ))?;
        let tokens: BTreeSet<&str> = farm
            .unmatched
            .iter()
            .flat_map(|open| open.objects.iter().filter_map(|name| unit_token(name)))
            .collect();
        for token in tokens {
            if let Some(build) = unit
                .query_row(params![tenant, token], |row| row.get(0))
                .optional()?
            {
                library.units.insert(token.to_owned(), build);
            }
        }
        library.plates = current_plates(tx, tenant)?;
        Ok(library)
    }
}

/// Rows of an integer id and a text column for `tenant`.
fn id_text(tx: &Transaction<'_>, sql: &str, tenant: &str) -> Result<Vec<(i64, String)>> {
    let mut statement = tx.prepare(sql)?;
    let rows = statement.query_map([tenant], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Each open Build's Plates in its current accepted layout, with their placed
/// units.
fn current_plates(tx: &Transaction<'_>, tenant: &str) -> Result<BTreeMap<i64, Vec<Plate>>> {
    let mut statement = tx.prepare(
        "SELECT h.profile_id,p.plate_id,p.ordinal,u.required_unit_token,r.object_name
         FROM accepted_plate_heads h
         JOIN build_profiles b ON b.id=h.profile_id AND b.tenant_id=h.tenant_id
         JOIN accepted_plates p ON p.tenant_id=h.tenant_id AND p.revision_id=h.current_revision_id
         LEFT JOIN accepted_plate_units u ON u.tenant_id=p.tenant_id AND u.revision_id=p.revision_id
           AND u.plate_id=p.plate_id AND u.placement<>'unplaced'
         LEFT JOIN required_units r ON r.tenant_id=u.tenant_id AND r.token=u.required_unit_token
         WHERE h.tenant_id=?1 AND b.archived_at IS NULL
         ORDER BY h.profile_id,p.ordinal,p.plate_id,u.required_unit_token",
    )?;
    let mut plates: BTreeMap<i64, Vec<Plate>> = BTreeMap::new();
    let mut rows = statement.query([tenant])?;
    while let Some(row) = rows.next()? {
        let plate_id: String = row.get(1)?;
        let list = plates.entry(row.get(0)?).or_default();
        if list.last().is_none_or(|plate| plate.plate_id != plate_id) {
            list.push(Plate {
                plate_id,
                ordinal: row.get(2)?,
                tokens: BTreeSet::new(),
                stems: Vec::new(),
            });
        }
        let (Some(plate), Some(token), Some(object_name)) = (
            list.last_mut(),
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ) else {
            continue;
        };
        let stem = object_name
            .rsplit_once(&format!("__{TOKEN_PREFIX}"))
            .map_or(object_name.as_str(), |(stem, _)| stem);
        plate.stems.push(object_key(stem));
        plate.tokens.insert(token);
    }
    Ok(plates)
}

/// The Required-unit token (`ppu_` and 32 lowercase hex digits, not
/// followed by another hex digit) in an object name, if it carries one.
fn unit_token(name: &str) -> Option<&str> {
    name.match_indices(TOKEN_PREFIX).find_map(|(start, _)| {
        let end = start + TOKEN_PREFIX.len() + TOKEN_HEX;
        let token = name.get(start..end)?;
        let hex = token[TOKEN_PREFIX.len()..].bytes();
        let lower_hex = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
        (hex.into_iter().all(lower_hex)
            && !name.as_bytes().get(end).is_some_and(u8::is_ascii_hexdigit))
        .then_some(token)
    })
}

/// The comparison key of an object or unit name without slicer wrappers or
/// mesh extension.
fn object_key(name: &str) -> String {
    interpret_sliced_object_name(name).basename_key
}

/// The single candidate, or the single one the first decisive refiner leaves,
/// with every basis that agrees with it.
fn narrow(
    candidates: &BTreeSet<i64>,
    primary: MatchBasis,
    refiners: &[(MatchBasis, &BTreeSet<i64>)],
) -> Option<(i64, Vec<MatchBasis>)> {
    let chosen = match candidates.len() {
        0 => return None,
        1 => *candidates.first()?,
        _ => refiners.iter().find_map(|(_, refiner)| {
            let mut left = candidates.intersection(refiner);
            match (left.next(), left.next()) {
                (Some(&build), None) => Some(build),
                _ => None,
            }
        })?,
    };
    let mut basis = vec![primary];
    basis.extend(
        refiners
            .iter()
            .filter(|(_, refiner)| refiner.contains(&chosen))
            .map(|(kind, _)| *kind),
    );
    Some((chosen, basis))
}

/// Where and when a result or send went, and for which Build.
struct Sent<'a> {
    filename: &'a str,
    printer: &'a str,
    created_at: Option<&'a str>,
    build: Option<i64>,
}

/// What one print's evidence points at, before a Build is chosen.
struct Leads<'a> {
    tokens: Vec<&'a str>,
    unit_builds: BTreeSet<i64>,
    part_builds: BTreeSet<i64>,
    has_objects: bool,
    file_builds: BTreeSet<i64>,
    time_window: Option<TimeWindowEvidence>,
}

impl Farm {
    /// One suggestion per open unmatched print, drawn from `library`.
    pub(super) fn suggest(&self, library: &MatchLibrary) -> MatchSuggestionList {
        let sent: Vec<Sent<'_>> = self
            .results
            .iter()
            .map(|result: &CheckoffResult| Sent {
                filename: &result.filename,
                printer: &result.printer_id,
                created_at: Some(&result.created_at),
                build: Some(result.profile_id),
            })
            .chain(self.queue.iter().map(|item: &QueuedSend| Sent {
                filename: &item.filename,
                printer: &item.printer_id,
                created_at: item.created_at.as_deref(),
                build: item.profile_id,
            }))
            .collect();
        MatchSuggestionList {
            suggestions: self
                .unmatched
                .iter()
                .map(|open| suggestion(open, &leads(open, &sent, library), library))
                .collect(),
        }
    }
}

fn leads<'a>(open: &'a OpenPrint, sent: &[Sent<'_>], library: &MatchLibrary) -> Leads<'a> {
    let print = &open.print;
    let open_builds = |ids: &mut dyn Iterator<Item = i64>| -> BTreeSet<i64> {
        ids.filter(|id| library.builds.contains_key(id)).collect()
    };
    let name = normalized_filename(&print.filename);
    let file_builds = open_builds(
        &mut sent
            .iter()
            .filter(|sent| normalized_filename(sent.filename) == name)
            .filter_map(|sent| sent.build)
            .chain(library.job_files.get(&name).into_iter().flatten().copied()),
    );
    let time_window = instant(Some(&print.completed_at)).map(|end| {
        let start = end - i128::from(TIME_WINDOW_HOURS) * 3_600_000_000_000;
        let builds = open_builds(
            &mut sent
                .iter()
                .filter(|sent| {
                    sent.printer == print.printer_id
                        && instant(sent.created_at).is_some_and(|at| at >= start && at <= end)
                })
                .filter_map(|sent| sent.build),
        );
        TimeWindowEvidence {
            start: iso_time(nanos_time(start)),
            end: print.completed_at.clone(),
            build_ids: builds.into_iter().collect(),
        }
    });
    let tokens: Vec<&str> = open
        .objects
        .iter()
        .filter_map(|name| unit_token(name))
        .collect();
    let groups = group_objects_by_part(&open.objects);
    let part_builds = if tokens.is_empty() && !groups.is_empty() {
        library
            .part_files
            .iter()
            .filter(|(_, files)| {
                match_objects_to_filenames(&groups, files)
                    .iter()
                    .all(|(_, matched)| !matched.is_empty())
            })
            .map(|(build, _)| *build)
            .collect()
    } else {
        BTreeSet::new()
    };
    Leads {
        unit_builds: tokens
            .iter()
            .filter_map(|token| library.units.get(*token).copied())
            .collect(),
        tokens,
        part_builds,
        has_objects: !groups.is_empty(),
        file_builds,
        time_window,
    }
}

impl Leads<'_> {
    /// The suggested Build and the bases that support it.
    fn choose(&self) -> Option<(i64, Vec<MatchBasis>)> {
        let window_builds: BTreeSet<i64> = self
            .time_window
            .iter()
            .flat_map(|window| window.build_ids.iter().copied())
            .collect();
        let refiners = [
            (MatchBasis::FileName, &self.file_builds),
            (MatchBasis::TimeWindow, &window_builds),
        ];
        if !self.tokens.is_empty() {
            let (build, mut basis) = narrow(&self.unit_builds, MatchBasis::UnitTokens, &[])
                .filter(|_| self.unit_builds.len() == 1)?;
            basis.extend(
                refiners
                    .iter()
                    .filter(|(_, refiner)| refiner.contains(&build))
                    .map(|(kind, _)| *kind),
            );
            Some((build, basis))
        } else if self.has_objects {
            narrow(&self.part_builds, MatchBasis::ObjectList, &refiners)
        } else {
            narrow(&self.file_builds, MatchBasis::FileName, &refiners[1..])
        }
    }
}

/// The one current Plate of `build` the print fits, and whether it is an
/// exact match: every object tokened, every token this Build's, and the tokens
/// exactly the Plate's units.
fn plate<'a>(
    open: &OpenPrint,
    tokens: &[&str],
    build: i64,
    library: &'a MatchLibrary,
) -> (Option<&'a Plate>, bool) {
    let plates = library.plates.get(&build).map_or(&[][..], Vec::as_slice);
    let only = |mut fits: Box<dyn Iterator<Item = &'a Plate> + '_>| {
        fits.next().filter(|_| fits.next().is_none())
    };
    if tokens.is_empty() {
        let mut stems: Vec<String> = open.objects.iter().map(|name| object_key(name)).collect();
        stems.sort();
        let fit = only(Box::new(plates.iter().filter(|plate| {
            let mut plate_stems = plate.stems.clone();
            plate_stems.sort();
            !stems.is_empty() && plate_stems == stems
        })));
        return (fit, false);
    }
    let printed: BTreeSet<&str> = tokens.iter().copied().collect();
    let fit = only(Box::new(plates.iter().filter(|plate| {
        printed.iter().all(|token| plate.tokens.contains(*token))
    })));
    let exact = fit.is_some_and(|plate| {
        tokens.len() == open.objects.len()
            && printed.len() == tokens.len()
            && plate.tokens.len() == printed.len()
            && printed
                .iter()
                .all(|token| library.units.get(*token) == Some(&build))
    });
    (fit, exact)
}

fn suggestion(open: &OpenPrint, leads: &Leads<'_>, library: &MatchLibrary) -> MatchSuggestion {
    let print = &open.print;
    let (confidence, build, plate, basis) = match leads.choose() {
        None => (MatchConfidence::None, None, None, Vec::new()),
        Some((build, basis)) => {
            let (fit, exact) = plate(open, &leads.tokens, build, library);
            (
                if exact {
                    MatchConfidence::Exact
                } else {
                    MatchConfidence::Likely
                },
                Some(SuggestedBuild {
                    id: build,
                    name: library.builds[&build].clone(),
                }),
                fit.map(|plate| SuggestedPlate {
                    plate_id: plate.plate_id.clone(),
                    ordinal: plate.ordinal,
                }),
                basis,
            )
        }
    };
    let ids = |builds: &BTreeSet<i64>| builds.iter().copied().collect();
    MatchSuggestion {
        print_id: print.id.clone(),
        printer_id: print.printer_id.clone(),
        filename: print.filename.clone(),
        completed_at: print.completed_at.clone(),
        confidence,
        starts_checked: confidence == MatchConfidence::Exact,
        build,
        plate,
        evidence: MatchEvidence {
            basis,
            file_name: FileNameEvidence {
                build_ids: ids(&leads.file_builds),
            },
            object_list: ObjectListEvidence {
                object_count: u32::try_from(open.objects.len()).unwrap_or(u32::MAX),
                unit_token_count: u32::try_from(leads.tokens.len()).unwrap_or(u32::MAX),
                unit_build_ids: ids(&leads.unit_builds),
                part_build_ids: ids(&leads.part_builds),
            },
            time_window: leads.time_window.clone(),
        },
    }
}

fn nanos_time(nanos: i128) -> std::time::SystemTime {
    let duration =
        std::time::Duration::from_nanos(u64::try_from(nanos.unsigned_abs()).unwrap_or(u64::MAX));
    if nanos >= 0 {
        std::time::UNIX_EPOCH + duration
    } else {
        std::time::UNIX_EPOCH - duration
    }
}

#[cfg(test)]
mod tests;
