//! On-demand printer-host reads for the Rust-only runtime's printer-host
//! routes: connection test, print-file browse and camera list, for Moonraker
//! (GRE-360) and PrusaLink (GRE-1044), and the streamed print-file download
//! and camera view (GRE-1174), through one shared `HttpGuard`.
//! Moonraker reads send each GET once. PrusaLink reads with a password add a
//! Digest probe before each GET and resend a GET once after a 401 with a
//! fresh challenge, as Node's adapter does. Nothing here uploads, starts a
//! print or touches storage.
use anyhow::Result;
use axum::body::{Body, Bytes};
use http_body::Frame;
use pp_drivers::{
    moonraker_driver::{self, StorageEntry},
    prusalink,
};
use pp_net::outbound::{HttpGuard, MAX_BODY, ReplyBody, StreamReply, SystemResolver};
use serde_json::Value;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::mpsc;

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

/// A printer reply head whose body streams to the client as it reads it.
pub struct Opened {
    pub status: u16,
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    pub body: Body,
}

/// Chunks a pump task reads from the printer, a few at a time, so the
/// printer is read only as fast as the client takes the body.
struct Pumped(mpsc::Receiver<std::io::Result<Bytes>>);

impl http_body::Body for Pumped {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        self.0
            .poll_recv(cx)
            .map(|next| next.map(|chunk| chunk.map(Frame::data)))
    }
}

/// Streams `body` until it ends, fails or the client goes away. A failure
/// ends the client's body with an error instead of a clean end, and dropping
/// the printer body closes its connection.
fn pump(mut body: ReplyBody) -> Body {
    let (sender, receiver) = mpsc::channel(4);
    tokio::spawn(async move {
        loop {
            let chunk = match body.chunk().await {
                Ok(Some(chunk)) => Ok(chunk),
                Ok(None) => break,
                Err(error) => Err(std::io::Error::other(error.to_string())),
            };
            let failed = chunk.is_err();
            if sender.send(chunk).await.is_err() || failed {
                break;
            }
        }
    });
    Body::new(Pumped(receiver))
}

fn opened(reply: StreamReply) -> Opened {
    Opened {
        status: reply.status,
        content_type: reply.content_type,
        content_length: reply.content_length,
        body: pump(reply.body),
    }
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

    /// Opens `path` for download when the host lists it as a file in its
    /// directory, so a crafted path cannot reach a file outside the browsable
    /// storage tree; `None` when it is not listed.
    pub async fn open_file(&self, target: &HostTarget, path: &str) -> Result<Option<Opened>> {
        let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
        let driver = self.driver(target);
        let listing = match &driver {
            Driver::Moonraker(driver) => driver.browse(directory).await?,
            Driver::PrusaLink(driver) => driver.browse(directory).await?,
        };
        let listed = listing.entries.iter().any(
            |entry| matches!(entry, StorageEntry::File { path: listed, .. } if listed == path),
        );
        if !listed {
            return Ok(None);
        }
        let reply = match driver {
            Driver::Moonraker(driver) => driver.open_file(path).await?,
            Driver::PrusaLink(driver) => driver.open_file(path).await?,
        };
        Ok(Some(opened(reply)))
    }

    /// Opens a camera's view when the host lists a camera with that id;
    /// `None` when it does not.
    pub async fn open_camera(&self, target: &HostTarget, id: &str) -> Result<Option<Opened>> {
        let driver = self.driver(target);
        let cameras = match &driver {
            Driver::Moonraker(driver) => driver.cameras().await?,
            Driver::PrusaLink(driver) => driver.cameras().await?,
        };
        if !cameras.iter().any(|camera| camera.id == id) {
            return Ok(None);
        }
        let reply = match driver {
            Driver::Moonraker(driver) => driver.open_camera(id).await?,
            Driver::PrusaLink(driver) => driver.open_camera(id).await?,
        };
        Ok(Some(opened(reply)))
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
