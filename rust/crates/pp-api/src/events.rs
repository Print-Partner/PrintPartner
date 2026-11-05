//! Domain event stream (GRE-1162): `GET /api/v1/events` serves
//! `pp_storage::domain_events` as Server-Sent Events.
//!
//! Admission is the Accepted Plan reads' (`accepted::Gate`): the rebuild flag,
//! loopback Host/Origin checks and a session or API key. The first read runs
//! before the response starts, so a refused credential is a 401 problem.
//!
//! The resume cursor is the `Last-Event-ID` header, which browsers send on
//! reconnect, or the `cursor` query parameter for a first connect. Without
//! either, the stream opens with a `ready` event carrying the current
//! position; a client loads its state after that event, so any later change
//! arrives as a `domain_event`. A cursor that cannot be replayed gets a
//! `resync_required` event, after which the stream continues from its
//! position.
//!
//! Each stream re-reads after every storage wake and every heartbeat. A read
//! that fails, such as for a revoked session, ends the stream. A client that
//! leaves the bounded frame queue full past the slow-client deadline is
//! disconnected; storage writers never wait on a stream.

use crate::accepted::{AcceptedHttpConfig, Gate, failure, read_error};
use crate::loopback::PresentedCredential;
use axum::{
    Extension, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::Response,
    routing::get,
};
use http_body::Frame;
use pp_storage::domain_events::{Cursor, DomainEventsClient, READ_LIMIT, Read, Waker};
use serde_json::json;
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};

/// Per-router stream bounds.
#[derive(Clone, Copy, Debug)]
pub struct StreamLimits {
    /// Concurrent streams. Another connect answers 503.
    pub streams: usize,
    /// Frames queued for one client. One frame is at most one event, about
    /// 2.5 KiB, so 64 frames bound the queue near 160 KiB.
    pub queued_frames: usize,
    /// How long the queue may stay full before the client is disconnected.
    pub slow_client_deadline: Duration,
    /// Keep-alive comment interval. Each heartbeat also rechecks the
    /// credential.
    pub heartbeat: Duration,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            streams: 8,
            queued_frames: 64,
            slow_client_deadline: Duration::from_secs(10),
            heartbeat: Duration::from_secs(15),
        }
    }
}

#[derive(Clone)]
struct App {
    gate: Gate,
    client: DomainEventsClient,
    limits: StreamLimits,
    streams: Arc<Semaphore>,
}

/// Mounts `GET /api/v1/events` when `PP_DESKTOP_REBUILD` was exactly `1` or
/// `true`; otherwise nothing.
pub fn events_router(
    config: AcceptedHttpConfig,
    client: DomainEventsClient,
    limits: StreamLimits,
) -> Router {
    let Some(gate) = Gate::open(config) else {
        return Router::new();
    };
    gate.guard(Router::new().route("/api/v1/events", get(events)))
        .with_state(App {
            gate,
            client,
            limits,
            streams: Arc::new(Semaphore::new(limits.streams)),
        })
}

/// The resume cursor: `Last-Event-ID` wins over `cursor`. A malformed or
/// repeated value is refused.
fn resume(headers: &HeaderMap, uri: &Uri) -> Result<Option<Cursor>, &'static str> {
    let invalid = "Invalid event cursor";
    let mut values = headers.get_all("last-event-id").iter();
    if let Some(value) = values.next() {
        if values.next().is_some() {
            return Err(invalid);
        }
        return value
            .to_str()
            .ok()
            .and_then(Cursor::parse)
            .map(Some)
            .ok_or(invalid);
    }
    let url = reqwest::Url::parse(&format!("http://localhost/?{}", uri.query().unwrap_or("")))
        .map_err(|_| invalid)?;
    let mut cursor = None;
    for (_, value) in url.query_pairs().filter(|(key, _)| key == "cursor") {
        if cursor.is_some() {
            return Err(invalid);
        }
        cursor = Some(Cursor::parse(&value).ok_or(invalid)?);
    }
    Ok(cursor)
}

async fn events(
    State(app): State<App>,
    Extension(presented): Extension<PresentedCredential>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let after = match resume(&headers, &uri) {
        Ok(after) => after,
        Err(detail) => return failure(StatusCode::BAD_REQUEST, detail),
    };
    let Ok(permit) = app.streams.clone().try_acquire_owned() else {
        return crate::problem::overloaded("Too many event streams");
    };
    let notify = Arc::new(Notify::new());
    let signal = notify.clone();
    let waker: Waker = Arc::new(move || signal.notify_one());
    if app.client.subscribe(&waker).is_err() {
        return crate::problem::overloaded("Too many event streams");
    }
    let first = match read(&app, &presented, after.clone()).await {
        Some(Ok(first)) => first,
        Some(Err(error)) => return read_error(&error),
        None => return crate::accepted::internal(),
    };
    let (sender, receiver) = mpsc::channel(app.limits.queued_frames);
    let overflowed = Arc::new(AtomicBool::new(false));
    let stream = Stream {
        app: app.clone(),
        presented,
        notify,
        _waker: waker,
        _permit: permit,
        sender,
        overflowed: overflowed.clone(),
    };
    tokio::spawn(stream.run(after.is_none(), first));
    let mut response = Response::new(Body::new(EventBody {
        receiver,
        overflowed,
    }));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}

async fn read(
    app: &App,
    presented: &PresentedCredential,
    after: Option<Cursor>,
) -> Option<anyhow::Result<Read>> {
    let client = app.client.clone();
    app.gate
        .read(presented.clone(), move |credential, cancelled, wait| {
            client.read(credential, after, &cancelled, wait)
        })
        .await
}

fn frame(event: &str, id: &impl std::fmt::Display, data: &serde_json::Value) -> Bytes {
    Bytes::from(format!("event: {event}\nid: {id}\ndata: {data}\n\n"))
}

/// One client's producer task. Holds the stream slot and the storage
/// subscription until it returns.
struct Stream {
    app: App,
    presented: PresentedCredential,
    notify: Arc<Notify>,
    _waker: Waker,
    _permit: OwnedSemaphorePermit,
    sender: mpsc::Sender<Bytes>,
    overflowed: Arc<AtomicBool>,
}

impl Stream {
    async fn run(self, fresh: bool, first: Read) {
        let mut next = Some(first);
        let mut position = None;
        if fresh && let Some(Read::Events { position: at, .. }) = &next {
            let data = json!({ "position": at.to_string() });
            if !self.send(frame("ready", at, &data)).await {
                return;
            }
        }
        loop {
            let read = match next.take() {
                Some(read) => read,
                None => match read(&self.app, &self.presented, position.clone()).await {
                    Some(Ok(read)) => read,
                    _ => return,
                },
            };
            let more = match read {
                Read::Events {
                    events,
                    position: at,
                } => {
                    let full = events.len() == READ_LIMIT;
                    for event in &events {
                        let data = serde_json::to_value(event).unwrap_or_default();
                        if !self.send(frame("domain_event", &event.cursor, &data)).await {
                            return;
                        }
                    }
                    position = Some(at);
                    full
                }
                Read::Resync {
                    reason,
                    position: at,
                } => {
                    let data = json!({ "reason": reason, "position": at.to_string() });
                    if !self.send(frame("resync_required", &at, &data)).await {
                        return;
                    }
                    position = Some(at);
                    false
                }
            };
            if more {
                continue;
            }
            tokio::select! {
                () = self.notify.notified() => {}
                () = tokio::time::sleep(self.app.limits.heartbeat) => {
                    if !self.send(Bytes::from_static(b": keep-alive\n\n")).await {
                        return;
                    }
                }
                () = self.sender.closed() => return,
            }
        }
    }

    /// Queues one frame. A client that leaves the queue full past the
    /// deadline is marked overflowed, which ends its body with an error.
    async fn send(&self, bytes: Bytes) -> bool {
        match tokio::time::timeout(
            self.app.limits.slow_client_deadline,
            self.sender.send(bytes),
        )
        .await
        {
            Ok(Ok(())) => true,
            Ok(Err(_)) => false,
            Err(_) => {
                self.overflowed.store(true, Ordering::Release);
                false
            }
        }
    }
}

/// The response body: queued frames in order, then the end of the stream.
/// After an overflow it fails at once, so the server drops the connection
/// without sending the frames still queued.
struct EventBody {
    receiver: mpsc::Receiver<Bytes>,
    overflowed: Arc<AtomicBool>,
}

impl http_body::Body for EventBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        if self.overflowed.load(Ordering::Acquire) {
            return Poll::Ready(Some(Err(std::io::Error::other("slow event stream client"))));
        }
        self.receiver
            .poll_recv(context)
            .map(|frame| frame.map(|bytes| Ok(Frame::data(bytes))))
    }
}

#[cfg(test)]
mod tests;
