//! Effective Module state for one tenant, as `GET /health` reports it.
//!
//! Storage owns the settings. This reader returns only fixed module keys,
//! `on` or `off`, and a fixed reason code. It never returns a stored value,
//! a parse error, or any other setting, so the result is safe to serve
//! without authentication.

use super::USER_KEY;
use crate::{SettingSnapshot, SettingsClient};
use anyhow::Result;
use pp_domain::features::{Feature, Source, Toggles, resolve};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Node's `PUT /settings/build-tracking` key. Missing, empty or `"0"` is off.
pub const ASSEMBLY_TRACKING_KEY: &str = "build_tracking_assembly";

/// An optional product area that can be off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Module {
    /// Farm and printers: queue suggestions, printer matching and packing.
    Farm,
    /// Filament stock from Spoolman.
    Spoolman,
    /// Order numbers and customer requests on Plans.
    Orders,
    /// Bambu Lab printers and job hand-off.
    Bambu,
    /// Marking printed units as installed.
    AssemblyTracking,
}

impl Module {
    /// The feature toggle that holds this module's state, or `None` for
    /// assembly tracking, which is the `build_tracking_assembly` setting.
    pub fn feature(self) -> Option<Feature> {
        match self {
            Module::Farm => Some(Feature::Farm),
            Module::Spoolman => Some(Feature::Spoolman),
            Module::Orders => Some(Feature::Orders),
            Module::Bambu => Some(Feature::Bambu),
            Module::AssemblyTracking => None,
        }
    }

    /// Every module, in the order health lists them.
    pub const ALL: [Module; 5] = [
        Module::Farm,
        Module::Spoolman,
        Module::Orders,
        Module::Bambu,
        Module::AssemblyTracking,
    ];
}

/// Every module as `GET /health` reports it when storage cannot be read in
/// time: off with `setting_unreadable`, so no state is claimed that was not read.
pub fn unreadable() -> Vec<ModuleStatus> {
    Module::ALL
        .into_iter()
        .map(|module| off(module, ModuleOffReason::SettingUnreadable))
        .collect()
}

/// Why a module is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ModuleOffReason {
    /// Nobody turned it on; the module is off by default.
    DefaultOff,
    /// The user turned it off.
    TurnedOff,
    /// The stored setting could not be read in time or does not parse, so the
    /// module stays off.
    SettingUnreadable,
}

/// One module's effective state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ModuleStatus {
    On {
        module: Module,
    },
    Off {
        module: Module,
        reason: ModuleOffReason,
    },
}

impl ModuleStatus {
    pub fn module(self) -> Module {
        match self {
            Self::On { module } | Self::Off { module, .. } => module,
        }
    }
}

impl SettingsClient {
    /// Read every module's effective state for `tenant` from the reader pool.
    /// Each call reads current storage, so a change shows on the next call.
    pub fn modules(&self, tenant: &str, wait: Duration) -> Result<Vec<ModuleStatus>> {
        let reader = self.reader(wait)?;
        let toggles = reader.snapshot(tenant, USER_KEY)?;
        let assembly = reader.snapshot(tenant, ASSEMBLY_TRACKING_KEY)?;
        drop(reader);
        Ok(statuses(&toggles, &assembly))
    }
}

/// One module's effective state for `tenant`, read on `connection` so it
/// shares the caller's transaction.
pub(crate) fn status_in(
    connection: &rusqlite::Connection,
    tenant: &str,
    module: Module,
) -> Result<ModuleStatus> {
    statuses_in(connection, tenant)?
        .into_iter()
        .find(|status| status.module() == module)
        .ok_or_else(|| anyhow::anyhow!("Module status missing"))
}

/// Every module's effective state for `tenant`, read on `connection` so it
/// shares the caller's transaction.
pub(crate) fn statuses_in(
    connection: &rusqlite::Connection,
    tenant: &str,
) -> Result<Vec<ModuleStatus>> {
    let toggles = crate::snapshot(connection, tenant, USER_KEY)?;
    let assembly = crate::snapshot(connection, tenant, ASSEMBLY_TRACKING_KEY)?;
    Ok(statuses(&toggles, &assembly))
}

fn statuses(toggles: &SettingSnapshot, assembly: &SettingSnapshot) -> Vec<ModuleStatus> {
    let user: Option<Toggles> = match toggles {
        SettingSnapshot::Missing => Some(Toggles::default()),
        SettingSnapshot::Stored { value } => serde_json::from_str(value).ok(),
    };
    let effective = user.as_ref().map(|user| resolve(user, None));
    Module::ALL
        .into_iter()
        .map(|module| {
            let Some(feature) = module.feature() else {
                return assembly_status(assembly);
            };
            let Some(effective) = &effective else {
                return off(module, ModuleOffReason::SettingUnreadable);
            };
            let Some(resolved) = effective.iter().find(|r| r.feature == feature) else {
                return off(module, ModuleOffReason::SettingUnreadable);
            };
            match (resolved.enabled, resolved.source) {
                (true, _) => ModuleStatus::On { module },
                (false, Source::Default) => off(module, ModuleOffReason::DefaultOff),
                (false, _) => off(module, ModuleOffReason::TurnedOff),
            }
        })
        .collect()
}

fn assembly_status(snapshot: &SettingSnapshot) -> ModuleStatus {
    let module = Module::AssemblyTracking;
    match snapshot {
        SettingSnapshot::Stored { value } if value == "0" => {
            off(module, ModuleOffReason::TurnedOff)
        }
        SettingSnapshot::Stored { value } if !value.is_empty() => ModuleStatus::On { module },
        _ => off(module, ModuleOffReason::DefaultOff),
    }
}

fn off(module: Module, reason: ModuleOffReason) -> ModuleStatus {
    ModuleStatus::Off { module, reason }
}
