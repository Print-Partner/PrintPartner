//! On-demand printer-host reads for the Rust-only runtime's printer-host
//! routes: connection test, print-file browse and camera list, for Moonraker
//! (GRE-360) and PrusaLink (GRE-1044), through one shared `HttpGuard`.
//! Moonraker reads send each GET once. PrusaLink reads with a password add a
//! Digest probe before each GET and resend a GET once after a 401 with a
//! fresh challenge, as Node's adapter does. These reads never upload, start a
//! print or touch storage.
//!
//! [`HostSends`] is the one upload path: the send queue's worker (GRE-365)
//! calls it once per admitted printer effect, after the user dispatched the
//! item. Each request is sent once and an uncertain result is reported, never
//! resent.
use anyhow::Result;
pub use pp_drivers::moonraker_driver::UploadResult;
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

/// Sends a sliced print file to a printer host. Only the send queue's
/// printer-upload worker uses it.
#[derive(Clone)]
pub struct HostSends {
    reads: HostReads,
}

impl HostSends {
    pub fn new() -> Result<Self> {
        Ok(Self::with_guard(HttpGuard::new(
            Arc::new(SystemResolver),
            READ_DEADLINE,
            MAX_BODY,
        )?))
    }

    pub fn with_guard(guard: HttpGuard) -> Self {
        Self {
            reads: HostReads::with_guard(guard),
        }
    }

    /// Upload `file` as `filename`, then start it when `start` is true and
    /// the upload succeeded. Each request is sent once.
    pub async fn upload(
        &self,
        target: &HostTarget,
        file: Vec<u8>,
        filename: &str,
        start: bool,
    ) -> UploadResult {
        let source = moonraker_driver::UploadSource::Bytes(file);
        match self.reads.driver(target) {
            Driver::Moonraker(driver) => driver.upload_file(source, filename, start).await,
            Driver::PrusaLink(driver) => driver.upload_file(source, filename, start).await,
        }
    }
}
