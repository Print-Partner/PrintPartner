//! On-demand printer-host reads for the Rust-only runtime's printer-host
//! routes: connection test, print-file browse and camera list, for Moonraker
//! (GRE-360) and PrusaLink (GRE-1044), and the streamed print-file download
//! and camera view (GRE-1174), through one shared `HttpGuard`.
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
use axum::body::{Body, Bytes};
use http_body::Frame;
pub use pp_drivers::moonraker_driver::UploadResult;
use pp_drivers::{
    bambu,
    moonraker::PrinterHostState,
    moonraker_driver::{self, StorageEntry},
    prusalink,
};
use pp_net::outbound::{HttpGuard, MAX_BODY, ReplyBody, StreamReply, SystemResolver};
use serde_json::Value;
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

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

/// A listed print file and the printer's reply for it.
pub struct OpenedFile {
    pub name: String,
    /// Node's `printerStoredFileIdentity` of the listing entry.
    pub identity: Value,
    pub opened: Opened,
}

/// Chunks a pump task reads from the printer, a few at a time, so the
/// printer is read only as fast as the client takes the body. `stopped`
/// fires when the pump gives up on a client that stopped reading.
struct Pumped {
    chunks: mpsc::Receiver<std::io::Result<Bytes>>,
    /// Taken once it resolves: a completed oneshot must not be polled again.
    stopped: Option<oneshot::Receiver<std::io::Error>>,
}

impl http_body::Body for Pumped {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if let Some(stopped) = self.stopped.as_mut()
            && let Poll::Ready(stopped) = Pin::new(stopped).poll(cx)
        {
            self.stopped = None;
            if let Ok(error) = stopped {
                return Poll::Ready(Some(Err(error)));
            }
        }
        self.chunks
            .poll_recv(cx)
            .map(|next| next.map(|chunk| chunk.map(Frame::data)))
    }
}

/// Streams `body` until it ends, fails or the client goes away. A failure
/// ends the client's body with an error instead of a clean end. Waiting for a
/// slow client counts against the body's deadline, and a dropped client body
/// stops a pending printer read at once; either way the printer body is
/// dropped, which closes its connection.
fn pump(mut body: ReplyBody) -> Body {
    let (sender, chunks) = mpsc::channel(4);
    let (stop, stopped) = oneshot::channel();
    tokio::spawn(async move {
        loop {
            let chunk = tokio::select! {
                biased;
                () = sender.closed() => return,
                chunk = body.chunk() => chunk,
            };
            let chunk = match chunk {
                Ok(Some(chunk)) => Ok(chunk),
                Ok(None) => return,
                Err(error) => Err(std::io::Error::other(error.to_string())),
            };
            let failed = chunk.is_err();
            match body.within(sender.send(chunk)).await {
                Ok(Ok(())) if !failed => {}
                Ok(_) => return,
                Err(error) => {
                    let _ = stop.send(std::io::Error::other(error.to_string()));
                    return;
                }
            }
        }
    });
    Body::new(Pumped {
        chunks,
        stopped: Some(stopped),
    })
}

/// Only a 2xx body is streamed. Any other reply's body is dropped unread,
/// which closes the printer connection, as Node's `sendUpstreamResponse`
/// cancels it.
fn opened(reply: StreamReply) -> Opened {
    let body = if (200..300).contains(&reply.status) {
        pump(reply.body)
    } else {
        Body::empty()
    };
    Opened {
        status: reply.status,
        content_type: reply.content_type,
        content_length: reply.content_length,
        body,
    }
}

/// A Bambu certificate pin: the printer address it was taken at and the
/// SHA-256 of the certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BambuPin {
    pub host: String,
    pub port: u16,
    pub sha256: [u8; 32],
}

/// A Bambu connection test's `{ok, message}`, and the pin to store when the
/// test ran without one and reached the printer's status.
#[derive(Debug)]
pub struct BambuTest {
    pub result: Value,
    pub pin: Option<BambuPin>,
}

pub use bambu::CERTIFICATE_CHANGED as BAMBU_CERTIFICATE_CHANGED;
pub const BAMBU_ADDRESS_CHANGED: &str =
    "Printer address changed since its certificate was pinned. Reset its certificate in Settings.";

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

    /// Node's Bambu `testConnection` over pinned TLS (`pp_drivers::bambu`):
    /// with `pin`, only that certificate at that address; without one, the
    /// certificate the printer presents, which the result then carries for
    /// the caller to store. Only the user's Test connection calls this.
    pub async fn bambu_test(&self, config: &Value, pin: Option<&BambuPin>) -> BambuTest {
        let failed = |message: &str| BambuTest {
            result: serde_json::json!({ "ok": false, "message": message }),
            pin: None,
        };
        let config = match bambu::Config::from_stored(config) {
            Ok(config) => config,
            Err(message) => return failed(&message),
        };
        let trust = match pin {
            None => bambu::Trust::FirstUse,
            Some(pin) if pin.host == config.host && pin.port == config.port => {
                bambu::Trust::Pinned(pin.sha256)
            }
            Some(_) => return failed(BAMBU_ADDRESS_CHANGED),
        };
        let (host, port) = (config.host.clone(), config.port);
        let read = bambu::Driver::new(self.guard.clone(), config)
            .status(trust)
            .await;
        let status = read.status;
        if status.state == PrinterHostState::Offline {
            return failed(status.message.as_deref().unwrap_or("Bambu MQTT offline"));
        }
        let state = serde_json::to_value(&status.state).unwrap_or(Value::Null);
        let shown = status
            .message
            .clone()
            .unwrap_or_else(|| state.as_str().unwrap_or_default().to_owned());
        BambuTest {
            result: serde_json::json!({ "ok": true, "message": format!("Connected (LAN MQTT · {shown})") }),
            pin: match (trust, read.certificate) {
                (bambu::Trust::FirstUse, Some(sha256)) => Some(BambuPin { host, port, sha256 }),
                _ => None,
            },
        }
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
    pub async fn open_file(&self, target: &HostTarget, path: &str) -> Result<Option<OpenedFile>> {
        let directory = path.rsplit_once('/').map_or("", |(directory, _)| directory);
        let driver = self.driver(target);
        let listing = match &driver {
            Driver::Moonraker(driver) => driver.browse(directory).await?,
            Driver::PrusaLink(driver) => driver.browse(directory).await?,
        };
        let Some((name, identity)) = listing.entries.into_iter().find_map(|entry| match entry {
            StorageEntry::File {
                path: listed,
                name,
                size_bytes,
                modified_at,
            } if listed == path => {
                let mut identity = serde_json::Map::new();
                if let Some(size) = size_bytes {
                    identity.insert("size_bytes".into(), size.into());
                }
                if let Some(modified) = modified_at {
                    identity.insert("modified_at".into(), modified.into());
                }
                Some((name, Value::Object(identity)))
            }
            _ => None,
        }) else {
            return Ok(None);
        };
        let reply = match driver {
            Driver::Moonraker(driver) => driver.open_file(path).await?,
            Driver::PrusaLink(driver) => driver.open_file(path).await?,
        };
        Ok(Some(OpenedFile {
            name,
            identity,
            opened: opened(reply),
        }))
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

#[cfg(test)]
mod tests {
    use super::*;
    use pp_net::outbound::BodyDeadline;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    /// A loopback printer that answers one request with a streamed head and
    /// `body`, then hands the socket back.
    async fn printer(body: &'static [u8]) -> (String, tokio::task::JoinHandle<TcpStream>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://127.0.0.1:{}/stream",
            listener.local_addr().unwrap().port()
        );
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = [0; 2048];
            let _ = socket.read(&mut head).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            socket.write_all(body).await.unwrap();
            socket
        });
        (url, task)
    }

    async fn open(url: &str, deadline: Duration) -> Body {
        let guard =
            HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(5), MAX_BODY).unwrap();
        let reply = guard
            .stream(url, &[], deadline, BodyDeadline::Total)
            .await
            .unwrap();
        pump(reply.body)
    }

    /// Whether the printer sees its connection closed within `limit`.
    async fn closed_within(socket: &mut TcpStream, limit: Duration) -> bool {
        let chunk = vec![b'x'; 64 * 1024];
        let frame = format!("{:x}\r\n", chunk.len());
        let (mut reader, mut writer) = socket.split();
        tokio::time::timeout(limit, async {
            loop {
                let mut read = [0; 1];
                tokio::select! {
                    read = reader.read(&mut read) => if matches!(read, Ok(0) | Err(_)) { return },
                    written = async {
                        writer.write_all(frame.as_bytes()).await?;
                        writer.write_all(&chunk).await?;
                        writer.write_all(b"\r\n").await
                    } => if written.is_err() { return },
                }
            }
        })
        .await
        .is_ok()
    }

    #[tokio::test]
    async fn ticket_t_21_a_finished_body_reads_to_its_end() {
        let body: &[u8] = b"5\r\nframe\r\n5\r\nframe\r\n5\r\nframe\r\n5\r\nframe\r\n5\r\nframe\r\n5\r\nframe\r\n0\r\n\r\n";
        let (url, printer) = printer(body).await;
        let body = open(&url, Duration::from_secs(5)).await;
        let _socket = printer.await.unwrap();
        // Let the pump read every chunk and finish before the client polls.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let read = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(read, "frame".repeat(6));
    }

    #[tokio::test]
    async fn ticket_t_21_a_refused_reply_is_dropped_unread() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://127.0.0.1:{}/missing",
            listener.local_addr().unwrap().port()
        );
        let printer = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = [0; 2048];
            let _ = socket.read(&mut head).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            socket
        });
        let guard =
            HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(5), MAX_BODY).unwrap();
        let deadline = Duration::from_secs(30);
        let reply = guard
            .stream(&url, &[], deadline, BodyDeadline::Total)
            .await
            .unwrap();
        let opened = opened(reply);
        assert_eq!(opened.status, 404);
        let mut socket = printer.await.unwrap();
        // The printer never sends a body; the connection still closes at once.
        let mut read = [0; 1];
        let closed = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut read)).await;
        assert!(
            matches!(closed, Ok(Ok(0) | Err(_))),
            "the printer connection stayed open"
        );
        assert!(
            axum::body::to_bytes(opened.body, 16)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn ticket_t_21_dropped_client_body_closes_a_stalled_printer_read() {
        let (url, printer) = printer(b"").await;
        let body = open(&url, Duration::from_secs(60)).await;
        let mut socket = printer.await.unwrap();
        drop(body);
        let mut read = [0; 1];
        let closed = tokio::time::timeout(Duration::from_secs(2), socket.read(&mut read)).await;
        assert!(
            matches!(closed, Ok(Ok(0) | Err(_))),
            "the printer connection stayed open"
        );
    }

    #[tokio::test]
    async fn ticket_t_21_a_client_that_stops_reading_still_hits_the_total_deadline() {
        let (url, printer) = printer(b"").await;
        let body = open(&url, Duration::from_millis(500)).await;
        let mut socket = printer.await.unwrap();
        // The client holds the body but never reads it, so the pump fills its
        // channel and waits on the client.
        assert!(
            closed_within(&mut socket, Duration::from_secs(5)).await,
            "the printer connection outlived the stream deadline"
        );
        let error = axum::body::to_bytes(body, usize::MAX).await.unwrap_err();
        assert!(error.to_string().contains("deadline"), "{error}");
    }

    #[tokio::test]
    async fn ticket_t_23_a_pin_counts_only_for_its_host_and_port() {
        let (broker, printer) = pp_testkit::bambu::idle_printer("synthetic-code", "FIXTURE01")
            .await
            .unwrap();
        let port = broker.addr.port();
        let config = serde_json::json!({
            "host": "127.0.0.1", "port": port, "serial": "FIXTURE01", "access_code": "synthetic-code",
        });
        let reads = HostReads::new().unwrap();
        let pin = |host: &str, port: u16| BambuPin {
            host: host.into(),
            port,
            sha256: printer.fingerprint,
        };
        for moved in [
            pin("127.0.0.2", port),
            pin("127.0.0.1", port.wrapping_add(1)),
        ] {
            let test = reads.bambu_test(&config, Some(&moved)).await;
            assert_eq!(
                (test.result, test.pin.is_none()),
                (
                    serde_json::json!({"ok": false, "message": BAMBU_ADDRESS_CHANGED}),
                    true
                )
            );
        }
        assert!(broker.log().is_empty(), "{:?}", broker.log());

        let test = reads
            .bambu_test(&config, Some(&pin("127.0.0.1", port)))
            .await;
        assert_eq!(
            test.result["ok"],
            serde_json::json!(true),
            "{}",
            test.result
        );
        assert!(test.pin.is_none(), "a matching pin is never taken again");
    }
}
