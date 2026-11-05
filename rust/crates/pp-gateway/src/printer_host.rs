//! On-demand printer-host reads for the Rust-only runtime's printer-host
//! routes: connection test, print-file browse and camera list, for Moonraker
//! (GRE-360) and PrusaLink (GRE-1044). Each call makes the read-only requests
//! its route needs, once, through one shared `HttpGuard`. Nothing here
//! uploads, starts a print, retries or touches storage.
use anyhow::Result;
use pp_drivers::{moonraker_driver, prusalink};
use pp_net::outbound::{HttpGuard, MAX_BODY, SystemResolver};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

/// Node's Moonraker connection test allows 8 s; Moonraker browse and camera
/// reads share it. The PrusaLink driver sets its own per-request deadlines.
pub const READ_DEADLINE: Duration = Duration::from_secs(8);

/// One stored integration's address and credentials.
pub enum HostTarget {
    Moonraker {
        base_url: String,
        api_key: Option<String>,
    },
    PrusaLink {
        base_url: String,
        username: String,
        password: Option<String>,
        storage: Option<String>,
    },
}

enum Driver {
    Moonraker(moonraker_driver::Driver),
    PrusaLink(prusalink::Driver),
}

#[derive(Clone)]
pub struct HostReads {
    guard: HttpGuard,
}

impl HostReads {
    pub fn new() -> Result<Self> {
        Ok(Self::with_guard(HttpGuard::new(
            Arc::new(SystemResolver),
            READ_DEADLINE,
            MAX_BODY,
        )?))
    }

    pub fn with_guard(guard: HttpGuard) -> Self {
        Self { guard }
    }

    fn driver(&self, target: &HostTarget) -> Driver {
        let guard = self.guard.clone();
        match target {
            HostTarget::Moonraker { base_url, api_key } => Driver::Moonraker(
                moonraker_driver::Driver::new(guard, base_url, api_key.as_deref()),
            ),
            HostTarget::PrusaLink {
                base_url,
                username,
                password,
                storage,
            } => Driver::PrusaLink(prusalink::Driver::new(
                guard,
                base_url,
                username,
                password.as_deref(),
                storage.as_deref(),
            )),
        }
    }

    /// Node's `IntegrationTestResult`: `{ok, message}`, never an error.
    pub async fn test(&self, target: &HostTarget) -> Value {
        let result = match self.driver(target) {
            Driver::Moonraker(driver) => driver.test_connection().await,
            Driver::PrusaLink(driver) => driver.test_connection().await,
        };
        serde_json::to_value(result).unwrap_or(Value::Null)
    }

    /// One directory as Node's `StorageListing`.
    pub async fn browse(&self, target: &HostTarget, path: &str) -> Result<Value> {
        let listing = match self.driver(target) {
            Driver::Moonraker(driver) => driver.browse(path).await?,
            Driver::PrusaLink(driver) => driver.browse(path).await?,
        };
        Ok(serde_json::to_value(listing)?)
    }

    /// The host's cameras as Node's `PrinterCamera` list.
    pub async fn cameras(&self, target: &HostTarget) -> Result<Value> {
        let cameras = match self.driver(target) {
            Driver::Moonraker(driver) => driver.cameras().await?,
            Driver::PrusaLink(driver) => driver.cameras().await?,
        };
        Ok(serde_json::to_value(cameras)?)
    }
}
