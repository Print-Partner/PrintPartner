//! `GET /ws/printers` (GRE-1014): the read-only live printer stream for an
//! authenticated session or API key.
//!
//! On connect the server sends one `snapshot` of every configured printer,
//! then one `printer` or `removed` event per change with a strictly
//! increasing `seq`, and a `heartbeat` every [`HEARTBEAT`]. Each change is its
//! own event, so a passed-through state such as `error` before `idle` is never
//! merged away. `?stream=<id>&since=<seq>` resumes: the events after `since`
//! when this stream still holds them, else a fresh snapshot. Clients never send
//! commands; anything they send is ignored. The credential is checked again
//! before each heartbeat.

use crate::{
    accepted::{AcceptedHttpConfig, Gate, internal, read_error},
    loopback::PresentedCredential,
};
use axum::{
    Extension, Router,
    extract::{
        State,
        ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade},
    },
    http::{StatusCode, Uri},
    response::Response,
    routing::get,
};
use pp_storage::read_model::farm::{FarmClient, StreamPrinter, iso_time};
use serde_json::{Map, Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, SystemTime},
};
use tokio::sync::{RwLock, broadcast, watch};

/// How often a connected client hears from the stream when nothing changes.
pub const HEARTBEAT: Duration = Duration::from_secs(15);
/// Events kept for resuming clients and for clients that fall behind.
pub const RETAINED: usize = 1024;

#[derive(Clone)]
struct Frame {
    seq: u64,
    text: Utf8Bytes,
}

struct Hub {
    stream: String,
    seq: u64,
    printers: Vec<StreamPrinter>,
    retained: VecDeque<Frame>,
    /// Set by the first publish: before it the fleet is not known yet.
    ready: bool,
}

impl Hub {
    fn frame(&self, kind: &str, fields: Value) -> Utf8Bytes {
        let mut body = Map::new();
        body.insert("type".into(), kind.into());
        body.insert("stream".into(), self.stream.clone().into());
        body.insert("seq".into(), self.seq.into());
        if let Value::Object(fields) = fields {
            body.extend(fields);
        }
        Utf8Bytes::from(Value::Object(body).to_string())
    }

    fn push(&mut self, kind: &str, fields: Value, live: &broadcast::Sender<Frame>) {
        self.seq += 1;
        let frame = Frame {
            seq: self.seq,
            text: self.frame(kind, fields),
        };
        if self.retained.len() == RETAINED {
            self.retained.pop_front();
        }
        self.retained.push_back(frame.clone());
        let _ = live.send(frame);
    }
}

/// The stream's printers and event history, fed by `pp-core`'s producer.
#[derive(Clone)]
pub struct PrinterStream {
    hub: Arc<Mutex<Hub>>,
    live: broadcast::Sender<Frame>,
    heartbeat: Duration,
    stopped: watch::Sender<bool>,
    /// Each socket holds a read guard; [`PrinterStream::stop`] takes the write
    /// lock, so it returns once every socket has closed.
    sockets: Arc<RwLock<()>>,
}

impl PrinterStream {
    pub fn new(heartbeat: Duration) -> Self {
        let mut id = [0u8; 16];
        getrandom::fill(&mut id).unwrap_or_default();
        let stream = id.iter().map(|byte| format!("{byte:02x}")).collect();
        Self {
            hub: Arc::new(Mutex::new(Hub {
                stream,
                seq: 0,
                printers: Vec::new(),
                retained: VecDeque::new(),
                ready: false,
            })),
            live: broadcast::channel(RETAINED).0,
            heartbeat,
            stopped: watch::channel(false).0,
            sockets: Arc::default(),
        }
    }

    fn hub(&self) -> std::sync::MutexGuard<'_, Hub> {
        self.hub.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Replaces the printer list. Each printer whose state changed (ignoring
    /// `last_heard_at`) is one `printer` event, in list order, then each
    /// printer no longer configured is one `removed` event.
    pub fn publish(&self, printers: Vec<StreamPrinter>) {
        let mut hub = self.hub();
        for printer in &printers {
            let before = hub.printers.iter().find(|known| known.id == printer.id);
            if before.is_none_or(|known| !known.same_state(printer)) {
                hub.push("printer", json!({"printer": printer}), &self.live);
            }
        }
        let removed: Vec<_> = hub
            .printers
            .iter()
            .filter(|known| !printers.iter().any(|printer| printer.id == known.id))
            .map(|known| known.id.clone())
            .collect();
        for id in removed {
            hub.push("removed", json!({"id": id}), &self.live);
        }
        hub.printers = printers;
        hub.ready = true;
    }

    /// Closes every socket with 1001 and refuses new ones; returns once each
    /// socket's task has finished.
    pub async fn stop(&self) {
        self.stopped.send_replace(true);
        drop(self.sockets.write().await);
    }

    fn open_for_clients(&self) -> bool {
        self.hub().ready && !*self.stopped.borrow()
    }

    /// The first frames for a client, the live feed after them, and the last
    /// `seq` those frames cover. Subscribing under the hub lock means no event
    /// is missed or repeated between the two.
    fn open(
        &self,
        resume: Option<(String, u64)>,
    ) -> (Vec<Utf8Bytes>, broadcast::Receiver<Frame>, u64) {
        let hub = self.hub();
        let live = self.live.subscribe();
        let held = |since: u64| {
            since == hub.seq
                || hub
                    .retained
                    .front()
                    .is_some_and(|first| first.seq <= since + 1)
        };
        let frames = match resume {
            Some((stream, since)) if stream == hub.stream && since <= hub.seq && held(since) => hub
                .retained
                .iter()
                .filter(|frame| frame.seq > since)
                .map(|frame| frame.text.clone())
                .collect(),
            _ => vec![hub.frame(
                "snapshot",
                json!({"heartbeat_seconds": self.heartbeat.as_secs(), "printers": hub.printers}),
            )],
        };
        (frames, live, hub.seq)
    }

    fn heartbeat(&self, seq: u64) -> Utf8Bytes {
        let hub = self.hub();
        let text = json!({"type": "heartbeat", "stream": hub.stream, "seq": seq,
            "at": iso_time(SystemTime::now())});
        Utf8Bytes::from(text.to_string())
    }
}

#[derive(Clone)]
struct App {
    gate: Gate,
    client: FarmClient,
    stream: PrinterStream,
}

/// Mounts `GET /ws/printers` when `PP_DESKTOP_REBUILD` was exactly `1` or
/// `true`; otherwise nothing. Admission is `accepted::Gate`, then a storage
/// read proves the credential before the upgrade.
pub fn printer_stream_router(
    config: AcceptedHttpConfig,
    client: FarmClient,
    stream: PrinterStream,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    gate.guard(Router::new().route("/ws/printers", get(connect)))
        .with_state(App {
            gate,
            client,
            stream,
        })
}

/// `stream` and `since` from the query, URL-decoded. Both or neither; other
/// parameters are ignored.
fn resume(query: Option<&str>) -> Result<Option<(String, u64)>, &'static str> {
    const INVALID: &str = "Invalid printer stream resume";
    let url = reqwest::Url::parse(&format!("http://localhost/?{}", query.unwrap_or_default()))
        .map_err(|_| INVALID)?;
    let (mut stream, mut since) = (None, None);
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "stream" if stream.is_none() => stream = Some(value.into_owned()),
            "since" if since.is_none() => since = Some(value.parse::<u64>().map_err(|_| INVALID)?),
            "stream" | "since" => return Err(INVALID),
            _ => {}
        }
    }
    match (stream, since) {
        (Some(stream), Some(since)) => Ok(Some((stream, since))),
        (None, None) => Ok(None),
        _ => Err(INVALID),
    }
}

async fn connect(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    uri: Uri,
    ws: WebSocketUpgrade,
) -> Response {
    if let Some(refused) = authenticate(&app, presented.clone()).await {
        return refused;
    }
    let resume = match resume(uri.query()) {
        Ok(resume) => resume,
        Err(detail) => return crate::accepted::failure(StatusCode::BAD_REQUEST, detail),
    };
    if !app.stream.open_for_clients() {
        return crate::accepted::failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Printer stream is not running",
        );
    }
    ws.on_upgrade(move |socket| run(socket, app, presented, resume))
}

/// Serves one socket until it ends or the stream stops. Stopping drops the
/// serving future, including a blocked send, then sends a bounded 1001 close.
async fn run(
    mut socket: WebSocket,
    app: App,
    presented: PresentedCredential,
    resume: Option<(String, u64)>,
) {
    let _socket = app.stream.sockets.clone().read_owned().await;
    let mut stopped = app.stream.stopped.subscribe();
    if *stopped.borrow_and_update() {
        return;
    }
    tokio::select! {
        () = pump(&mut socket, &app, presented, resume) => {}
        () = async { drop(stopped.wait_for(|stopped| *stopped).await) } => {
            let closing = close(&mut socket, 1001, "Printer stream stopped");
            let _ = tokio::time::timeout(Duration::from_secs(1), closing).await;
        }
    }
}

/// `None` when the credential still reads, else the refusal to send.
async fn authenticate(app: &App, presented: PresentedCredential) -> Option<Response> {
    let client = app.client.clone();
    let read = app
        .gate
        .read(presented, move |credential, cancelled, wait| {
            client.read(credential, cancelled, wait).map(drop)
        })
        .await;
    match read {
        Some(Ok(())) => None,
        Some(Err(error)) => Some(read_error(&error)),
        None => Some(internal()),
    }
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let frame = CloseFrame {
        code,
        reason: Utf8Bytes::from_static(reason),
    };
    let _ = socket.send(Message::Close(Some(frame))).await;
}

async fn pump(
    socket: &mut WebSocket,
    app: &App,
    presented: PresentedCredential,
    resume: Option<(String, u64)>,
) {
    let (first, mut live, mut last) = app.stream.open(resume);
    for text in first {
        if socket.send(Message::Text(text)).await.is_err() {
            return;
        }
    }
    let period = app.stream.heartbeat;
    let mut beat = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    loop {
        let text = tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_)) | Err(_)) | None => return,
                Some(Ok(_)) => continue,
            },
            frame = live.recv() => match frame {
                Ok(frame) if frame.seq > last => {
                    last = frame.seq;
                    frame.text
                }
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    return close(socket, 1013, "Printer stream fell behind; resume with since").await;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return close(socket, 1001, "Printer stream stopped").await;
                }
            },
            _ = beat.tick() => {
                if authenticate(app, presented.clone()).await.is_some() {
                    return close(socket, 1008, "Authentication required").await;
                }
                app.stream.heartbeat(last)
            }
        };
        if socket.send(Message::Text(text)).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests;
