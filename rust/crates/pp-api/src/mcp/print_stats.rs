//! MCP `get_print_stats { hours? }` on Rust storage (GRE-1352 part 2): what
//! was printed in the last `hours` (8 by default, at most 2160).
//!
//! - Plates sent, completed and failed, and filament used, from
//!   `pp_storage::print_activity`: every `print_jobs` row in the window,
//!   counted per printer in one query. Node counted only the newest 100.
//! - Prints seen on printers, from the farm read `GET /api/v2/printer-checkoff`
//!   and `…/unattributed` serve: Checkoff results whose host reported an
//!   outcome (attached to a Plan) and open unmatched prints (not attached).
//!   Claimed and dismissed prints are not open, so they are not counted.
//! - Active plans: every Build that is not archived, with its Accepted Plan
//!   progress as `list_plans` reads it.

use super::{
    checkoff::CheckoffDomain,
    farm_status::INTERNAL,
    write_result::{Code, Refusal, read_failure, writer_failure},
};
use pp_services::mcp_domain::ToolInvocation;
use pp_storage::{
    build_graph::{
        AcceptedProgress, AcceptedProgressUnavailable, BuildCommand, BuildOutcome, Failure,
        ProfileSummary,
    },
    print_activity::{PrintActivityError, PrinterActivity},
    read_model::farm::{CheckoffResult, HostOutcome, Observation, UnmatchedPrint, iso_time},
};
use serde::Serialize;
use serde_json::{Map, Number, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, SystemTime},
};

pub(super) const TOOL: &str = "get_print_stats";
const DEFAULT_HOURS: f64 = 8.0;
/// 90 days.
const MAX_HOURS: f64 = 2160.0;

const INVALID_HOURS: Refusal = Refusal::new(
    Code::InvalidHours,
    "hours must be a number greater than 0 and at most 2160 (90 days).",
);

/// The window to count, from `hours`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Window {
    hours: f64,
}

impl Window {
    /// `hours` absent or `null` is 8; otherwise a JSON number above 0 and at
    /// most 2160. A numeric string is refused rather than read as a number.
    pub(super) fn parse(args: &Map<String, Value>) -> Result<Self, Refusal> {
        let hours = match args.get("hours") {
            None | Some(Value::Null) => DEFAULT_HOURS,
            Some(Value::Number(hours)) => hours.as_f64().ok_or(INVALID_HOURS)?,
            Some(_) => return Err(INVALID_HOURS),
        };
        if !(hours.is_finite() && hours > 0.0 && hours <= MAX_HOURS) {
            return Err(INVALID_HOURS);
        }
        Ok(Self { hours })
    }

    /// The window's start, `hours` before `now`.
    pub(super) fn since(self, now: SystemTime) -> SystemTime {
        now - Duration::from_secs_f64(self.hours * 3600.0)
    }
}

#[derive(Debug, Serialize)]
pub(super) struct PrintStats {
    /// The window as asked, a whole number when it is one.
    window_hours: Number,
    since: String,
    /// No plate was sent and no print was seen in the window.
    empty_window: bool,
    plates_sent: u64,
    plates_completed: u64,
    plates_failed: u64,
    /// Completed over completed plus failed, to 3 places; `null` when none
    /// finished. Plates still printing do not count against it.
    completion_rate: Option<f64>,
    /// Grams every job in the window recorded; `120`, not `120.0`, when whole.
    #[serde(serialize_with = "grams")]
    filament_consumed_g: f64,
    by_printer: Vec<PrinterStats>,
    prints_seen_on_printers: PrintsSeen,
    active_plans: ActivePlans,
}

#[derive(Debug, Serialize)]
struct PrinterStats {
    /// `null` for jobs that name no printer.
    printer_id: Option<String>,
    printer_name: Option<String>,
    plates_sent: u64,
    plates_completed: u64,
    plates_failed: u64,
    completion_rate: Option<f64>,
    #[serde(serialize_with = "grams")]
    filament_consumed_g: f64,
}

#[derive(Debug, Default, Serialize)]
struct PrintsSeen {
    total: u64,
    attached_to_plan: u64,
    not_attached_to_plan: u64,
    by_printer: Vec<SeenOnPrinter>,
}

#[derive(Debug, Serialize)]
struct SeenOnPrinter {
    printer_id: String,
    printer_name: Option<String>,
    seen: u64,
    completed: u64,
    failed: u64,
    cancelled: u64,
}

/// The active Builds, or why they could not be listed. The plate and print
/// counts are answered either way, as Node did.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ActivePlans {
    Listed { plans: Vec<ActivePlan> },
    Unavailable { reason: PlansUnavailable },
}

/// Why the Build list could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PlansUnavailable {
    /// More than 64 Builds, archived ones included: the Build list refuses.
    TooManyBuilds,
    /// Busy storage, or a Build whose summary could not be read.
    ReadFailed,
}

#[derive(Debug, Serialize)]
struct ActivePlan {
    plan_id: i64,
    plan_name: String,
    part_count: i64,
    accepted_progress: Progress,
}

/// A Build's Accepted Plan progress, as `get_remaining` counts it.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Progress {
    Ready {
        total_units: i64,
        remaining_units: i64,
        printed_units: i64,
    },
    /// The Build has no Accepted Plan.
    Empty,
    /// The Accepted Plan cannot be counted now.
    Unavailable { reason: Unavailable },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum Unavailable {
    CompatibilityDirty,
    Uninitialized,
    Integrity,
}

impl CheckoffDomain {
    /// `get_print_stats`, called only while the Farm Module is on.
    pub(super) fn print_stats(&self, args: &Map<String, Value>) -> ToolInvocation {
        let content = match self.read_print_stats(args) {
            Ok(stats) => serde_json::to_string(&stats).unwrap_or_else(|_| INTERNAL.proposal()),
            Err(refusal) => refusal.proposal(),
        };
        ToolInvocation {
            content,
            proposed_action: None,
        }
    }

    fn read_print_stats(&self, args: &Map<String, Value>) -> Result<PrintStats, Refusal> {
        let window = Window::parse(args)?;
        let now = SystemTime::now();
        let caller = self.caller().map_err(|_| INTERNAL)?;
        let since = iso_time(window.since(now));
        let jobs = self
            .farm
            .activity
            .since(
                caller.clone().into_credential(),
                &since,
                &AtomicBool::new(false),
                self.wait,
            )
            .map_err(|PrintActivityError::Reader(failure)| writer_failure(&failure))?;
        let farm = self.read_farm()?;
        let fleet = Fleet::new(
            farm.printers(|_| Observation::NotWatched)
                .printers
                .into_iter()
                .map(|printer| (printer.id, printer.name, printer.integration.map(|i| i.id))),
        );
        let listed = self.builds.execute(
            caller.into_credential(),
            BuildCommand::List,
            Arc::new(AtomicBool::new(false)),
            self.wait,
        );
        let builds = match listed {
            Ok(BuildOutcome::Listed { profiles }) => Ok(profiles),
            Ok(_) => Err(PlansUnavailable::ReadFailed),
            Err(error) if matches!(error.downcast_ref(), Some(Failure::TooManyBuilds)) => {
                Err(PlansUnavailable::TooManyBuilds)
            }
            // A refused key refuses the whole call; anything else costs only
            // the plan list.
            Err(error) => match read_failure(error) {
                refusal if refusal.code == Code::Unauthorized => return Err(refusal),
                _ => Err(PlansUnavailable::ReadFailed),
            },
        };
        let (checkoff, unmatched) = (farm.checkoff(), farm.unmatched());
        Ok(stats(
            window,
            now,
            Inputs {
                jobs,
                results: &checkoff.results,
                unmatched: &unmatched.prints,
                fleet,
                builds,
            },
        ))
    }
}

/// The fleet printers, to name the printer a job or a print reports.
#[derive(Default)]
pub(super) struct Fleet {
    names: HashMap<String, String>,
    /// Each integration's printer, or `None` when several share it.
    by_integration: HashMap<String, Option<String>>,
}

impl Fleet {
    /// From `(id, name, integration id)` for each fleet printer.
    pub(super) fn new(
        printers: impl IntoIterator<Item = (String, String, Option<String>)>,
    ) -> Self {
        let mut fleet = Self::default();
        for (id, name, integration) in printers {
            if let Some(integration) = integration {
                fleet
                    .by_integration
                    .entry(integration.trim().to_owned())
                    .and_modify(|sole| *sole = None)
                    .or_insert_with(|| Some(id.clone()));
            }
            fleet.names.insert(id, name);
        }
        fleet
    }

    fn name(&self, id: &str) -> Option<String> {
        self.names.get(id).cloned()
    }

    /// The printer a report names, and its name: the reported id when it is
    /// a fleet printer, else the only fleet printer on the reporting
    /// integration, as Node resolves the `default` placeholder a host poll
    /// stores. Otherwise the reported id, unnamed.
    fn resolve(&self, printer_id: &str, integration_id: &str) -> (String, Option<String>) {
        if let Some(name) = self.names.get(printer_id) {
            return (printer_id.to_owned(), Some(name.clone()));
        }
        match self.by_integration.get(integration_id.trim()) {
            Some(Some(id)) => (id.clone(), self.name(id)),
            _ => (printer_id.to_owned(), None),
        }
    }
}

/// What the tool reads, each from its own storage read.
pub(super) struct Inputs<'a> {
    pub(super) jobs: Vec<PrinterActivity>,
    pub(super) results: &'a [CheckoffResult],
    pub(super) unmatched: &'a [UnmatchedPrint],
    pub(super) fleet: Fleet,
    pub(super) builds: Result<Vec<ProfileSummary>, PlansUnavailable>,
}

/// The stats for `window`, ending at `now`.
pub(super) fn stats(window: Window, now: SystemTime, inputs: Inputs<'_>) -> PrintStats {
    let since = window.since(now);
    let fleet = &inputs.fleet;
    let by_printer: Vec<PrinterStats> = inputs
        .jobs
        .into_iter()
        .map(|row| PrinterStats {
            printer_name: row.printer_id.as_deref().and_then(|id| fleet.name(id)),
            completion_rate: completion_rate(row.completed.into(), row.failed.into()),
            printer_id: row.printer_id,
            plates_sent: row.sent.into(),
            plates_completed: row.completed.into(),
            plates_failed: row.failed.into(),
            filament_consumed_g: row.filament_consumed_g,
        })
        .collect();
    let sum = |field: fn(&PrinterStats) -> u64| by_printer.iter().map(field).sum::<u64>();
    let (sent, completed, failed) = (
        sum(|row| row.plates_sent),
        sum(|row| row.plates_completed),
        sum(|row| row.plates_failed),
    );
    let seen = prints_seen(inputs.results, inputs.unmatched, since, fleet);
    PrintStats {
        window_hours: whole(window.hours),
        since: iso_time(since),
        empty_window: sent == 0 && seen.total == 0,
        plates_sent: sent,
        plates_completed: completed,
        plates_failed: failed,
        completion_rate: completion_rate(completed, failed),
        filament_consumed_g: by_printer.iter().map(|row| row.filament_consumed_g).sum(),
        by_printer,
        prints_seen_on_printers: seen,
        active_plans: match &inputs.builds {
            Ok(builds) => ActivePlans::Listed {
                plans: builds
                    .iter()
                    .filter(|build| build.archived_at.is_none())
                    .map(|build| ActivePlan {
                        plan_id: build.id,
                        plan_name: build.name.clone(),
                        part_count: build.part_count,
                        accepted_progress: progress(&build.accepted_progress),
                    })
                    .collect(),
            },
            Err(reason) => ActivePlans::Unavailable { reason: *reason },
        },
    }
}

/// How each print the printers reported finished.
enum Outcome {
    Completed,
    Failed,
    Cancelled,
}

fn prints_seen(
    results: &[CheckoffResult],
    unmatched: &[UnmatchedPrint],
    since: SystemTime,
    fleet: &Fleet,
) -> PrintsSeen {
    let after = |at: &str| at_or_after(at, since);
    let attached = results.iter().filter_map(|result| {
        let outcome = match result.host_outcome? {
            HostOutcome::Success => Outcome::Completed,
            HostOutcome::Failed => Outcome::Failed,
            HostOutcome::Cancelled => Outcome::Cancelled,
            HostOutcome::Unknown => return None,
        };
        let at = result.completed_at.as_deref().unwrap_or(&result.created_at);
        let printer = fleet.resolve(&result.printer_id, &result.integration_id);
        after(at).then_some((printer, outcome, true))
    });
    let open = unmatched
        .iter()
        .filter(|print| after(&print.completed_at))
        .map(|print| {
            let printer = fleet.resolve(&print.printer_id, &print.integration_id);
            (printer, Outcome::Completed, false)
        });
    let mut seen = PrintsSeen::default();
    let mut rows: BTreeMap<String, SeenOnPrinter> = BTreeMap::new();
    for ((printer, name), outcome, on_plan) in attached.chain(open) {
        seen.total += 1;
        if on_plan {
            seen.attached_to_plan += 1;
        } else {
            seen.not_attached_to_plan += 1;
        }
        let row = rows
            .entry(printer.clone())
            .or_insert_with(|| SeenOnPrinter {
                printer_id: printer,
                printer_name: name,
                seen: 0,
                completed: 0,
                failed: 0,
                cancelled: 0,
            });
        row.seen += 1;
        *match outcome {
            Outcome::Completed => &mut row.completed,
            Outcome::Failed => &mut row.failed,
            Outcome::Cancelled => &mut row.cancelled,
        } += 1;
    }
    seen.by_printer = rows.into_values().collect();
    seen
}

/// Whether the RFC 3339 time `at` is at or after `since`. A time that does
/// not parse is left out.
fn at_or_after(at: &str, since: SystemTime) -> bool {
    use time::{OffsetDateTime, format_description::well_known::Rfc3339};
    OffsetDateTime::parse(at, &Rfc3339).is_ok_and(|at| SystemTime::from(at) >= since)
}

fn completion_rate(completed: u64, failed: u64) -> Option<f64> {
    let finished = completed + failed;
    (finished > 0).then(|| (completed as f64 / finished as f64 * 1000.0).round() / 1000.0)
}

fn progress(progress: &AcceptedProgress) -> Progress {
    match progress {
        AcceptedProgress::Ready {
            total_units,
            remaining_units,
        } => Progress::Ready {
            total_units: *total_units,
            remaining_units: *remaining_units,
            printed_units: total_units - remaining_units,
        },
        AcceptedProgress::Empty => Progress::Empty,
        AcceptedProgress::Unavailable(reason) => Progress::Unavailable {
            reason: match reason {
                AcceptedProgressUnavailable::CompatibilityDirty => Unavailable::CompatibilityDirty,
                AcceptedProgressUnavailable::Uninitialized => Unavailable::Uninitialized,
                AcceptedProgressUnavailable::Integrity => Unavailable::Integrity,
            },
        },
    }
}

/// A non-negative amount as JSON: `8`, not `8.0`, when it is whole.
fn whole(amount: f64) -> Number {
    if amount.fract() == 0.0 && amount >= 0.0 && amount <= u64::MAX as f64 {
        Number::from(amount as u64)
    } else {
        Number::from_f64(amount).unwrap_or_else(|| Number::from(0))
    }
}

fn grams<S: serde::Serializer>(grams: &f64, out: S) -> Result<S::Ok, S::Error> {
    whole(*grams).serialize(out)
}

#[cfg(test)]
#[path = "print_stats/tests.rs"]
mod tests;
