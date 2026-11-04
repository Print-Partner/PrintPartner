//! Spoolman shortfalls for accepted Build work.
//!
//! Grams needed come from past prints: each completed print job's
//! `filament_consumed_g` divided by the units that job printed, averaged over
//! the Part's most recent [`MEASURED_JOBS`] jobs and multiplied by the Part's
//! remaining units. A Part never printed has unknown need. Grams available
//! come from Spoolman, which alone owns stock counts. Unreadable inventory is
//! unknown, never zero, and an unknown on either side never yields a shortfall.
//!
//! Everything here reads. Nothing reserves or deducts stock.

use super::{AcceptedRead, Snapshot};
use crate::features::modules::{self, Module, ModuleOffReason, ModuleStatus};
use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Completed jobs averaged per Part, most recent first.
pub const MEASURED_JOBS: usize = 5;
/// Node's `PUT /integrations` setting key: a JSON array of integrations.
const INTEGRATIONS_KEY: &str = "integrations";

/// A configured Spoolman integration. `Debug` hides the API key.
#[derive(Clone, PartialEq, Eq)]
pub struct SpoolmanIntegration {
    pub id: String,
    pub base_url: String,
    pub api_key: Option<String>,
}

impl std::fmt::Debug for SpoolmanIntegration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpoolmanIntegration")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "****"))
            .finish()
    }
}

/// Measured grams for one projection Part.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
    pub grams_per_unit: f64,
    pub job_ids: Vec<String>,
}

/// Stock inputs read in the same transaction as the accepted snapshot.
#[derive(Clone, Debug, PartialEq)]
pub enum StockInputs {
    Off(ModuleOffReason),
    On {
        integrations: Vec<SpoolmanIntegration>,
        /// Keyed by projection Part ID.
        measured: HashMap<i64, Measured>,
    },
}

/// One Spoolman inventory, as read through a checked connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Inventory {
    pub filaments: HashMap<i64, InventoryFilament>,
    /// Unarchived spools only.
    pub spools: Vec<InventorySpool>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InventoryFilament {
    pub label: String,
    pub hex: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InventorySpool {
    pub id: i64,
    pub filament_id: i64,
    /// `None` when Spoolman reports no remaining weight.
    pub remaining_g: Option<f64>,
}

/// `GET /plans/{id}/filament-shortfalls`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FilamentShortfalls {
    /// The Spoolman Module is off; nothing was read from Spoolman.
    ModuleOff {
        module: Module,
        reason: ModuleOffReason,
    },
    /// The Build has no accepted Plan yet.
    NoAcceptedPlan { profile_id: i64 },
    Ready {
        profile_id: i64,
        revision_id: i64,
        plan_version: i64,
        /// One row per filament with remaining units, sorted by filament ID.
        filaments: Vec<FilamentNeed>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct FilamentNeed {
    /// The Parts' trimmed filament ID, exactly as accepted; empty when none.
    pub filament_color_id: String,
    /// Spoolman's label, else the catalog label, else empty.
    pub label: String,
    pub grams_needed: GramsNeeded,
    pub grams_available: GramsAvailable,
    pub status: Sufficiency,
    /// Sorted by Part ID.
    pub parts: Vec<PartNeed>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct PartNeed {
    /// Projection Part ID.
    pub part_id: i64,
    pub filename: String,
    pub remaining_units: u64,
    pub grams_needed: GramsNeeded,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GramsNeeded {
    /// From past prints, rounded up. `job_count` is the distinct jobs used.
    Measured { grams: u64, job_count: u64 },
    /// At least one Part has no completed print with measured grams.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GramsAvailable {
    /// Spoolman's remaining weight over the filament's unarchived spools,
    /// rounded down. No spools reads as zero grams.
    Spoolman {
        grams: u64,
        spool_ids: Vec<i64>,
    },
    Unknown {
        reason: InventoryUnknown,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InventoryUnknown {
    /// A catalog or custom colour, or no filament: Spoolman does not track it.
    NotSpoolman,
    /// No configured Spoolman integration has the ID the filament names.
    IntegrationMissing,
    /// The Spoolman read failed.
    Unreachable,
    /// Spoolman has no filament with this ID.
    FilamentMissing,
    /// A spool of this filament reports no remaining weight.
    SpoolWeightMissing,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Sufficiency {
    Sufficient,
    Short {
        short_grams: u64,
    },
    /// Need or stock is unknown, so no shortfall is claimed.
    Unknown,
}

/// The Spoolman Module state, configured integrations and measured grams for
/// `profile_id`, read on the accepted-read transaction.
pub(super) fn read_inputs(
    connection: &Connection,
    tenant: &str,
    profile_id: i64,
) -> Result<StockInputs> {
    if let ModuleStatus::Off { reason, .. } =
        modules::status_in(connection, tenant, Module::Spoolman)?
    {
        return Ok(StockInputs::Off(reason));
    }
    let integrations = match crate::snapshot(connection, tenant, INTEGRATIONS_KEY)? {
        crate::SettingSnapshot::Stored { value } => parse_integrations(&value),
        crate::SettingSnapshot::Missing => Vec::new(),
    };
    Ok(StockInputs::On {
        integrations,
        measured: read_measured(connection, tenant, profile_id)?,
    })
}

/// Spoolman entries of Node's integration list. Corrupt entries are skipped,
/// as Node's `loadRaw` skips them.
fn parse_integrations(value: &str) -> Vec<SpoolmanIntegration> {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(value) else {
        return Vec::new();
    };
    let text = |config: &Value, keys: [&str; 2]| {
        keys.iter()
            .find_map(|key| config.get(*key)?.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    items
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("spoolman"))
        .filter_map(|item| {
            let config = item.get("config")?;
            Some(SpoolmanIntegration {
                id: item.get("id")?.as_str()?.to_owned(),
                base_url: text(config, ["base_url", "baseUrl"])?,
                api_key: text(config, ["api_key", "apiKey"]),
            })
        })
        .collect()
}

fn read_measured(
    connection: &Connection,
    tenant: &str,
    profile_id: i64,
) -> Result<HashMap<i64, Measured>> {
    let mut statement = connection.prepare(
        "SELECT p.part_id, j.id, j.filament_consumed_g,
                (SELECT count(*) FROM print_job_parts u
                  WHERE u.tenant_id=j.tenant_id AND u.job_id=j.id)
           FROM print_jobs j
           JOIN print_job_parts p ON p.tenant_id=j.tenant_id AND p.job_id=j.id
          WHERE j.tenant_id=?1 AND j.profile_id=?2 AND j.status='completed'
            AND typeof(j.filament_consumed_g)='integer' AND j.filament_consumed_g>0
          GROUP BY p.part_id, j.id
          ORDER BY p.part_id, COALESCE(j.completed_at, j.at) DESC, j.id DESC
          LIMIT 50000",
    )?;
    let mut rows = statement.query(rusqlite::params![tenant, profile_id])?;
    let mut per_unit = BTreeMap::<i64, Vec<(String, f64)>>::new();
    while let Some(row) = rows.next()? {
        let (part, job, grams, units): (i64, String, i64, i64) =
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        let jobs = per_unit.entry(part).or_default();
        if units > 0 && jobs.len() < MEASURED_JOBS {
            jobs.push((job, grams as f64 / units as f64));
        }
    }
    Ok(per_unit
        .into_iter()
        .filter(|(_, jobs)| !jobs.is_empty())
        .map(|(part, jobs)| {
            let grams_per_unit = jobs.iter().map(|(_, g)| g).sum::<f64>() / jobs.len() as f64;
            let job_ids = jobs.into_iter().map(|(job, _)| job).collect();
            (
                part,
                Measured {
                    grams_per_unit,
                    job_ids,
                },
            )
        })
        .collect())
}

/// `(integration ID, filament ID)` when `id` names a Spoolman filament.
fn spoolman_filament(id: &str) -> Option<(&str, i64)> {
    let rest = id.strip_prefix("spoolman:")?;
    let (integration, number) = rest.split_once(":filament:")?;
    let number = number.parse::<i64>().ok().filter(|n| *n > 0)?;
    (!integration.is_empty() && !integration.contains(':')).then_some((integration, number))
}

/// Included Parts with remaining units, grouped by trimmed filament ID.
fn remaining_work(snapshot: &Snapshot) -> BTreeMap<String, Vec<(&super::Part, u64)>> {
    let mut groups = BTreeMap::<String, Vec<_>>::new();
    for part in snapshot.parts.iter().filter(|part| part.included) {
        let remaining = part.units.iter().filter(|unit| !unit.completed).count() as u64;
        if remaining == 0 {
            continue;
        }
        let id = super::views::js_trim(part.filament_color_id.as_deref().unwrap_or(""));
        groups
            .entry(id.to_owned())
            .or_default()
            .push((part, remaining));
    }
    groups
}

/// The configured integrations the remaining work names, in ID order. Only
/// these are read from Spoolman.
pub fn integrations_to_read(snapshot: &Snapshot, inputs: &StockInputs) -> Vec<SpoolmanIntegration> {
    let StockInputs::On { integrations, .. } = inputs else {
        return Vec::new();
    };
    let work = remaining_work(snapshot);
    let named: BTreeSet<&str> = work
        .keys()
        .filter_map(|id| spoolman_filament(id).map(|(integration, _)| integration))
        .collect();
    named
        .into_iter()
        .filter_map(|id| integrations.iter().find(|known| known.id == id).cloned())
        .collect()
}

/// Shortfalls for an accepted read. `inventories` maps an integration ID to
/// its inventory, or `None` when the read failed. Returns `None` for reads
/// that have no Build to answer for (missing, unavailable, corrupt).
pub fn shortfalls(
    profile_id: i64,
    accepted: &AcceptedRead,
    inputs: &StockInputs,
    inventories: &HashMap<String, Option<Inventory>>,
) -> Option<FilamentShortfalls> {
    let snapshot = match accepted {
        AcceptedRead::Ready { snapshot } => snapshot,
        AcceptedRead::Empty { .. } => {
            return Some(match inputs {
                StockInputs::Off(reason) => module_off(*reason),
                StockInputs::On { .. } => FilamentShortfalls::NoAcceptedPlan { profile_id },
            });
        }
        _ => return None,
    };
    let (integrations, measured) = match inputs {
        StockInputs::Off(reason) => return Some(module_off(*reason)),
        StockInputs::On {
            integrations,
            measured,
        } => (integrations, measured),
    };
    let filaments = remaining_work(snapshot)
        .into_iter()
        .map(|(id, parts)| {
            let parts: Vec<(PartNeed, Vec<&str>)> = parts
                .into_iter()
                .map(|(part, remaining)| {
                    let measured = measured.get(&part.projection_part_id);
                    let need = PartNeed {
                        part_id: part.projection_part_id,
                        filename: part.filename.clone(),
                        remaining_units: remaining,
                        grams_needed: measured.map_or(GramsNeeded::Unknown, |m| {
                            GramsNeeded::Measured {
                                grams: (m.grams_per_unit * remaining as f64).ceil() as u64,
                                job_count: m.job_ids.len() as u64,
                            }
                        }),
                    };
                    let jobs =
                        measured.map_or(vec![], |m| m.job_ids.iter().map(String::as_str).collect());
                    (need, jobs)
                })
                .collect();
            let needed = group_need(&parts);
            let (label, available) = available(&id, integrations, inventories);
            let status = match (&needed, &available) {
                (
                    GramsNeeded::Measured { grams: need, .. },
                    GramsAvailable::Spoolman { grams: have, .. },
                ) if need > have => Sufficiency::Short {
                    short_grams: need - have,
                },
                (GramsNeeded::Measured { .. }, GramsAvailable::Spoolman { .. }) => {
                    Sufficiency::Sufficient
                }
                _ => Sufficiency::Unknown,
            };
            let mut parts: Vec<PartNeed> = parts.into_iter().map(|(need, _)| need).collect();
            parts.sort_by_key(|part| part.part_id);
            FilamentNeed {
                filament_color_id: id,
                label,
                grams_needed: needed,
                grams_available: available,
                status,
                parts,
            }
        })
        .collect();
    Some(FilamentShortfalls::Ready {
        profile_id,
        revision_id: snapshot.revision_id,
        plan_version: snapshot.plan_version,
        filaments,
    })
}

fn module_off(reason: ModuleOffReason) -> FilamentShortfalls {
    FilamentShortfalls::ModuleOff {
        module: Module::Spoolman,
        reason,
    }
}

fn group_need(parts: &[(PartNeed, Vec<&str>)]) -> GramsNeeded {
    let mut grams = 0u64;
    let mut jobs = BTreeSet::new();
    for (part, part_jobs) in parts {
        let GramsNeeded::Measured { grams: g, .. } = part.grams_needed else {
            return GramsNeeded::Unknown;
        };
        grams = grams.saturating_add(g);
        jobs.extend(part_jobs.iter().copied());
    }
    GramsNeeded::Measured {
        grams,
        job_count: jobs.len() as u64,
    }
}

fn available(
    id: &str,
    integrations: &[SpoolmanIntegration],
    inventories: &HashMap<String, Option<Inventory>>,
) -> (String, GramsAvailable) {
    let catalog = || {
        super::views::part_catalog_color(Some(id))
            .map(|color| color.display_name.clone())
            .unwrap_or_default()
    };
    let unknown = |reason| GramsAvailable::Unknown { reason };
    let Some((integration, filament_id)) = spoolman_filament(id) else {
        return (catalog(), unknown(InventoryUnknown::NotSpoolman));
    };
    if !integrations.iter().any(|known| known.id == integration) {
        return (String::new(), unknown(InventoryUnknown::IntegrationMissing));
    }
    let Some(Some(inventory)) = inventories.get(integration) else {
        return (String::new(), unknown(InventoryUnknown::Unreachable));
    };
    let Some(filament) = inventory.filaments.get(&filament_id) else {
        return (String::new(), unknown(InventoryUnknown::FilamentMissing));
    };
    let spools: Vec<&InventorySpool> = inventory
        .spools
        .iter()
        .filter(|spool| spool.filament_id == filament_id)
        .collect();
    let mut total = 0.0;
    for spool in &spools {
        match spool.remaining_g {
            Some(grams) if grams.is_finite() => total += grams.max(0.0),
            _ => {
                return (
                    filament.label.clone(),
                    unknown(InventoryUnknown::SpoolWeightMissing),
                );
            }
        }
    }
    (
        filament.label.clone(),
        GramsAvailable::Spoolman {
            grams: total.floor() as u64,
            spool_ids: spools.iter().map(|spool| spool.id).collect(),
        },
    )
}

#[cfg(test)]
#[path = "stock_tests.rs"]
mod tests;
