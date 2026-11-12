//! MCP `get_farm_status` on Rust storage (GRE-1352 part 1), and the Farm
//! Module gate for the farm tools.
//!
//! The tool reads the farm projection `GET /api/v2/printers` serves, with the
//! caller's API key, and what the printer watchers last observed. It sends no
//! request to a printer. Node asked every printer host on each call; Rust
//! reports what its watchers know. The farm read runs on the storage writer
//! queue, like that route.
//!
//! Filament grams need Spoolman stock (GRE-1102), so `filament_remaining_g`
//! is `null` and a printer needs a filament swap only for an empty slot.
//! The watchers hold no `eta_seconds` or `message`; both are `null`.

use super::{
    checkoff::CheckoffDomain,
    write_result::{Code, Refusal, read_failure},
};
use crate::loopback::PresentedCredential;
use pp_services::mcp_domain::{FarmAccess, ToolInvocation};
use pp_storage::{
    features::modules::{Module, ModuleOffReason, ModuleStatus},
    read_model::farm::{FarmPrinter, HostState, Observation, PrinterReason, TooLarge, iso_time},
};
use serde::Serialize;
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::atomic::AtomicBool,
    time::{Duration, SystemTime},
};

pub(super) const TOOL: &str = "get_farm_status";

const FARM_OFF: Refusal = Refusal::new(
    Code::FarmModuleOff,
    "The Farm Module is off. Turn it on in Settings to use the farm tools.",
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

/// A printer's state as the farm tools name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FarmState {
    Idle,
    Printing,
    Paused,
    Complete,
    Error,
    Offline,
    NotConnected,
}

/// Why a printer is `not_connected`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum NotConnectedReason {
    /// The printer has no printer integration.
    NotSetUp,
    /// It names an integration that is gone or is not a printer.
    IntegrationMissing,
    /// Its integration is disabled.
    MonitoringDisabled,
    /// This runtime cannot watch its integration.
    MonitoringNotSupported,
    /// The watcher has no status yet, or is stopped.
    AwaitingStatus,
    /// The printer host answered without a known state.
    HostStateUnknown,
}

#[derive(Serialize)]
struct FarmStatus {
    printer_count: usize,
    printers: Vec<PrinterStatus>,
    state_counts: StateCounts,
    needs_filament_swap: Vec<SwapNeeded>,
}

#[derive(Serialize)]
struct PrinterStatus {
    id: String,
    name: String,
    state: FarmState,
    not_connected_reason: Option<NotConnectedReason>,
    integration_id: Option<String>,
    /// The file the printer reports while printing or paused.
    active_job: Option<String>,
    /// Whole percent while printing or paused.
    progress: Option<u8>,
    /// Not held by the watchers.
    eta_seconds: Option<u64>,
    /// Not held by the watchers.
    message: Option<String>,
    last_heard_at: Option<String>,
    /// When the watcher saw the printer become idle; `null` when it did not.
    idle_since: Option<String>,
    /// How long it has been idle, or `unknown`; `null` unless idle.
    idle_for: Option<String>,
    filament_slots: Vec<SlotStatus>,
    /// Needs Spoolman stock (GRE-1102).
    filament_remaining_g: Option<f64>,
    needs_filament_swap: bool,
    empty_slots: Vec<u8>,
}

#[derive(Serialize)]
struct SlotStatus {
    slot: u8,
    filament_color_id: Option<String>,
    label: String,
    empty: bool,
    /// Needs Spoolman stock (GRE-1102).
    remaining_g: Option<f64>,
}

#[derive(Default, Serialize)]
struct StateCounts {
    idle: usize,
    printing: usize,
    paused: usize,
    complete: usize,
    error: usize,
    offline: usize,
    not_connected: usize,
}

impl StateCounts {
    fn count(&mut self, state: FarmState) {
        *match state {
            FarmState::Idle => &mut self.idle,
            FarmState::Printing => &mut self.printing,
            FarmState::Paused => &mut self.paused,
            FarmState::Complete => &mut self.complete,
            FarmState::Error => &mut self.error,
            FarmState::Offline => &mut self.offline,
            FarmState::NotConnected => &mut self.not_connected,
        } += 1;
    }
}

#[derive(Serialize)]
struct SwapNeeded {
    id: String,
    name: String,
    empty_slots: Vec<u8>,
}

impl CheckoffDomain {
    /// Whether the farm tools are listed and callable: the caller's Farm
    /// Module, read from the reader pool.
    pub(super) fn farm_module(&self) -> FarmAccess {
        let denied = |refusal: Refusal| FarmAccess::Denied {
            code: refusal.code.as_str().into(),
            error: refusal.detail.into(),
        };
        let Ok(PresentedCredential::ApiKey { tenant, .. }) = self.caller() else {
            return denied(INTERNAL);
        };
        // Settings that do not parse come back as `SettingUnreadable`, so a
        // failed read is a reader checkout that timed out or found storage
        // stopping: busy, not broken.
        let Ok(statuses) = self.farm.modules.modules(&tenant, self.wait) else {
            return denied(UNAVAILABLE);
        };
        match statuses
            .into_iter()
            .find(|status| status.module() == Module::Farm)
        {
            Some(ModuleStatus::On { .. }) => FarmAccess::Allowed,
            Some(ModuleStatus::Off {
                reason: ModuleOffReason::SettingUnreadable,
                ..
            }) => denied(MODULES_INVALID),
            Some(ModuleStatus::Off { .. }) => denied(FARM_OFF),
            None => denied(INTERNAL),
        }
    }

    /// `get_farm_status`, called only while the Farm Module is on.
    pub(super) fn farm_status(&self) -> ToolInvocation {
        let content = match self.read_farm_status() {
            Ok(status) => serde_json::to_string(&status).unwrap_or_else(|_| INTERNAL.proposal()),
            Err(refusal) => refusal.proposal(),
        };
        ToolInvocation {
            content,
            proposed_action: None,
        }
    }

    fn read_farm_status(&self) -> Result<FarmStatus, Refusal> {
        let credential = self.caller().map_err(|_| INTERNAL)?.into_credential();
        let farm = self
            .farm
            .reads
            .read(
                credential,
                std::sync::Arc::new(AtomicBool::new(false)),
                self.wait,
            )
            .map_err(|error| match error.downcast_ref::<TooLarge>() {
                Some(_) => INTERNAL,
                None => read_failure(error),
            })?;
        // Keep each observation the projection used, so the state, the file
        // and the idle time below come from the same reading.
        let watchers = &self.farm.observations;
        let seen = RefCell::new(HashMap::new());
        let list = farm.printers(|integration| {
            let observation = watchers.observe(integration);
            let entered = watchers.seen_entering(integration);
            seen.borrow_mut()
                .insert(integration.id.clone(), (observation.clone(), entered));
            observation
        });
        let seen = seen.into_inner();
        let now = SystemTime::now();
        let printers: Vec<PrinterStatus> = list
            .printers
            .into_iter()
            .map(|printer| {
                let reading = printer
                    .integration
                    .as_ref()
                    .and_then(|integration| seen.get(&integration.id));
                let observation = reading.map(|(observation, _)| observation);
                let entered = reading.and_then(|(_, entered)| *entered);
                status(printer, observation, entered, now)
            })
            .collect();
        let mut state_counts = StateCounts::default();
        for printer in &printers {
            state_counts.count(printer.state);
        }
        let needs_filament_swap = printers
            .iter()
            .filter(|printer| printer.needs_filament_swap)
            .map(|printer| SwapNeeded {
                id: printer.id.clone(),
                name: printer.name.clone(),
                empty_slots: printer.empty_slots.clone(),
            })
            .collect();
        Ok(FarmStatus {
            printer_count: printers.len(),
            printers,
            state_counts,
            needs_filament_swap,
        })
    }
}

/// One printer's status from its projected row and the observation the
/// projection used. `entered` is when the watcher saw its host change into
/// its current state; it counts only when it is that observation's
/// `state_since`, so a change between the two reads is not mixed in.
fn status(
    printer: FarmPrinter,
    observation: Option<&Observation>,
    entered: Option<SystemTime>,
    now: SystemTime,
) -> PrinterStatus {
    let (state, not_connected_reason, file) = match observation {
        Some(Observation::Seen {
            state, filename, ..
        }) => match farm_state(*state) {
            Some(state) => (state, None, filename.clone()),
            None => (
                FarmState::NotConnected,
                Some(NotConnectedReason::HostStateUnknown),
                None,
            ),
        },
        _ => (
            FarmState::NotConnected,
            Some(not_connected(printer.reason)),
            None,
        ),
    };
    let active = matches!(state, FarmState::Printing | FarmState::Paused);
    let idle = state == FarmState::Idle;
    let since = match observation {
        Some(Observation::Seen { state_since, .. }) => state_since.as_deref(),
        _ => None,
    };
    let idle_since = entered.filter(|at| idle && since == Some(iso_time(*at).as_str()));
    let filament_slots: Vec<SlotStatus> = printer
        .loaded_filaments
        .into_iter()
        .map(|loaded| {
            let color = loaded
                .filament_color_id
                .map(|id| id.trim().to_owned())
                .filter(|id| !id.is_empty());
            SlotStatus {
                slot: loaded.slot,
                empty: color.is_none(),
                filament_color_id: color,
                label: loaded.label.trim().to_owned(),
                remaining_g: None,
            }
        })
        .collect();
    let empty_slots: Vec<u8> = filament_slots
        .iter()
        .filter(|slot| slot.empty)
        .map(|slot| slot.slot)
        .collect();
    PrinterStatus {
        id: printer.id,
        name: printer.name,
        state,
        not_connected_reason,
        integration_id: printer.integration.map(|integration| integration.id),
        active_job: file.filter(|_| active),
        progress: printer.progress_percent,
        eta_seconds: None,
        message: None,
        last_heard_at: printer.last_heard_at,
        idle_since: idle_since.map(iso_time),
        idle_for: idle.then(|| idle_for(idle_since, now)),
        filament_slots,
        filament_remaining_g: None,
        needs_filament_swap: !empty_slots.is_empty(),
        empty_slots,
    }
}

/// The farm state for a host's answered state; `None` for `unknown`.
fn farm_state(state: HostState) -> Option<FarmState> {
    Some(match state {
        HostState::Idle => FarmState::Idle,
        HostState::Printing => FarmState::Printing,
        HostState::Paused => FarmState::Paused,
        HostState::Complete => FarmState::Complete,
        HostState::Error => FarmState::Error,
        HostState::Offline => FarmState::Offline,
        HostState::Unknown => return None,
    })
}

fn not_connected(reason: Option<PrinterReason>) -> NotConnectedReason {
    match reason {
        Some(PrinterReason::NotSetUp) => NotConnectedReason::NotSetUp,
        Some(PrinterReason::IntegrationMissing) => NotConnectedReason::IntegrationMissing,
        Some(PrinterReason::MonitoringDisabled) => NotConnectedReason::MonitoringDisabled,
        Some(PrinterReason::MonitoringNotSupported) => NotConnectedReason::MonitoringNotSupported,
        Some(PrinterReason::AwaitingStatus | PrinterReason::Unreachable) | None => {
            NotConnectedReason::AwaitingStatus
        }
    }
}

/// `3d 4h`, `2h 5m` or `12m`; `unknown` when the watcher did not see the
/// printer become idle.
fn idle_for(since: Option<SystemTime>, now: SystemTime) -> String {
    let Some(since) = since else {
        return "unknown".into();
    };
    let minutes = now
        .duration_since(since)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        / 60;
    let (days, hours, mins) = (minutes / 1440, minutes % 1440 / 60, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{mins}m"),
        (0, _) => format!("{hours}h {mins}m"),
        _ => format!("{days}d {hours}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printer(reason: Option<PrinterReason>) -> FarmPrinter {
        use pp_storage::printer_display_state::{
            BedClearState, PrinterExecutionState, PrinterStateView,
        };
        FarmPrinter {
            id: "p".into(),
            name: "P".into(),
            model: "P".into(),
            integration: None,
            state: PrinterStateView::new(PrinterExecutionState::Unknown, BedClearState::Unknown),
            reason,
            last_heard_at: None,
            progress_percent: None,
            loaded_filaments: Vec::new(),
        }
    }

    fn seen(state: HostState, since: Option<SystemTime>) -> Observation {
        Observation::Seen {
            state,
            progress_percent: None,
            filename: Some("x.gcode".into()),
            last_heard_at: None,
            state_since: since.map(iso_time),
        }
    }

    #[test]
    fn states_and_reasons_follow_the_reading() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let at = now - Duration::from_secs(600);
        for (host, state, reason) in [
            (HostState::Paused, FarmState::Paused, None),
            (HostState::Complete, FarmState::Complete, None),
            (HostState::Error, FarmState::Error, None),
            (HostState::Offline, FarmState::Offline, None),
            (
                HostState::Unknown,
                FarmState::NotConnected,
                Some(NotConnectedReason::HostStateUnknown),
            ),
        ] {
            let status = status(printer(None), Some(&seen(host, Some(at))), Some(at), now);
            assert_eq!((status.state, status.not_connected_reason), (state, reason));
            assert_eq!(status.idle_since, None, "{state:?} is not idle");
            assert_eq!(status.active_job.is_some(), state == FarmState::Paused);
        }
        let disabled = status(
            printer(Some(PrinterReason::MonitoringDisabled)),
            None,
            None,
            now,
        );
        assert_eq!(
            disabled.not_connected_reason,
            Some(NotConnectedReason::MonitoringDisabled)
        );
        let idle = status(
            printer(None),
            Some(&seen(HostState::Idle, Some(at))),
            Some(at),
            now,
        );
        assert_eq!(idle.idle_for.as_deref(), Some("10m"));
        // An entry time from another reading than the state is not used.
        let moved = status(
            printer(None),
            Some(&seen(HostState::Idle, Some(at))),
            Some(at + Duration::from_secs(5)),
            now,
        );
        assert_eq!(
            (moved.idle_since, moved.idle_for.as_deref()),
            (None, Some("unknown"))
        );
    }

    #[test]
    fn idle_for_labels_minutes_hours_and_days() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 86_400);
        let ago = |secs| Some(now - Duration::from_secs(secs));
        assert_eq!(idle_for(None, now), "unknown");
        assert_eq!(idle_for(ago(59), now), "0m");
        assert_eq!(idle_for(ago(12 * 60), now), "12m");
        assert_eq!(idle_for(ago(2 * 3600 + 5 * 60), now), "2h 5m");
        assert_eq!(idle_for(ago(3 * 86_400 + 4 * 3600 + 59), now), "3d 4h");
        assert_eq!(
            idle_for(Some(now + Duration::from_secs(60)), now),
            "0m",
            "a clock step back is not negative"
        );
    }
}
