//! On-demand Moonraker reads for the Rust-only runtime's printer-host routes
//! (GRE-360): connection test, print-file browse and camera list. Each call
//! makes the read-only requests its route needs, once, through one shared
//! `HttpGuard`. Nothing here uploads, starts a print, retries or touches storage.
use anyhow::Result;
use pp_drivers::moonraker_driver::Driver;
use pp_net::outbound::{HttpGuard, SystemResolver};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

/// Node's Moonraker connection test allows 8 s; browse and camera reads share it.
pub const READ_DEADLINE: Duration = Duration::from_secs(8);
const MAX_REPLY_BYTES: usize = 2 * 1024 * 1024;

/// One stored Moonraker integration's address and key.
pub struct MoonrakerTarget {
    pub base_url: String,
    pub api_key: Option<String>,
}

#[derive(Clone)]
pub struct MoonrakerReads {
    guard: HttpGuard,
}

impl MoonrakerReads {
    pub fn new() -> Result<Self> {
        Ok(Self::with_guard(HttpGuard::new(
            Arc::new(SystemResolver),
            READ_DEADLINE,
            MAX_REPLY_BYTES,
        )?))
    }

    pub fn with_guard(guard: HttpGuard) -> Self {
        Self { guard }
    }

    fn driver(&self, target: &MoonrakerTarget) -> Driver {
        Driver::new(
            self.guard.clone(),
            &target.base_url,
            target.api_key.as_deref(),
        )
    }

    /// Node's `IntegrationTestResult`: `{ok, message}`, never an error.
    pub async fn test(&self, target: &MoonrakerTarget) -> Value {
        serde_json::to_value(self.driver(target).test_connection().await).unwrap_or(Value::Null)
    }

    /// One directory of the `gcodes` root as Node's `StorageListing`.
    pub async fn browse(&self, target: &MoonrakerTarget, path: &str) -> Result<Value> {
        Ok(serde_json::to_value(
            self.driver(target).browse(path).await?,
        )?)
    }

    /// The configured webcams as Node's `PrinterCamera` list.
    pub async fn cameras(&self, target: &MoonrakerTarget) -> Result<Value> {
        Ok(serde_json::to_value(self.driver(target).cameras().await?)?)
    }
}
