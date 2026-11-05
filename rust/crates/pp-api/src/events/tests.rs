use super::*;
use pp_storage::{Limits, Setting, SettingCommand, WriterOwner};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, time::Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SECRET: &str = "events-session-secret";

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    owner: Option<WriterOwner>,
    origin: String,
    http: reqwest::Client,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start(limits: StreamLimits) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        let owner = WriterOwner::open(&root, Limits::default()).unwrap().0;
        let db = Connection::open(root.join("print-partner.db")).unwrap();
        let session = hex::encode(Sha256::digest(SECRET));
        db.execute_batch(&format!(
            "INSERT INTO users(id,display_name,is_admin,created_at) VALUES('default','Events',1,'0'); INSERT INTO sessions VALUES('{session}','default','2099-01-01T00:00:00.000Z','email')"
        ))
        .unwrap();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let router = events_router(
            AcceptedHttpConfig::from_startup(&origin, "default", Some("1")).unwrap(),
            owner.domain_events().unwrap(),
            limits,
        );
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        Self {
            _dir: dir,
            root,
            owner: Some(owner),
            origin,
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            task,
        }
    }

    fn database(&self) -> Connection {
        Connection::open(self.root.join("print-partner.db")).unwrap()
    }

    /// Stores events directly, then runs one writer command so the writer
    /// notices the new head and wakes streams.
    fn record(&self, ids: std::ops::Range<i64>, payload_bytes: usize) {
        let mut db = self.database();
        let tx = db.transaction().unwrap();
        let payload = json!({ "pad": "x".repeat(payload_bytes) }).to_string();
        for id in ids {
            tx.execute(
                "INSERT INTO domain_events(tenant,event_type,schema_version,aggregate_type,aggregate_id,aggregate_version,operation_id,payload_json,occurred_at) VALUES('default','checkoff.progress_changed',1,'build',?1,NULL,?2,?3,0)",
                rusqlite::params![id.to_string(), format!("{id:032x}"), payload],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        self.poke();
    }

    /// One settings write; returns how long the writer took to answer.
    fn poke(&self) -> Duration {
        let started = Instant::now();
        let reply = self
            .owner
            .as_ref()
            .unwrap()
            .client()
            .submit(
                SettingCommand::Set(Setting {
                    tenant: "default".into(),
                    key: "events-test-poke".into(),
                    value: format!("{started:?}"),
                }),
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap();
        reply.recv().unwrap().unwrap();
        started.elapsed()
    }

    async fn connect(&self, headers: &[(&str, &str)], query: &str) -> reqwest::Response {
        let mut request = self
            .http
            .get(format!("{}/api/v1/events{query}", self.origin))
            .header("Origin", &self.origin);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.send().await.unwrap()
    }

    async fn open(&self, headers: &[(&str, &str)]) -> Frames {
        let cookie = format!("pp_session={SECRET}");
        let mut all = vec![("Cookie", cookie.as_str())];
        all.extend_from_slice(headers);
        let response = self.connect(&all, "").await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        Frames {
            response,
            buffer: String::new(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(owner) = self.owner.take() {
            owner.shutdown().unwrap();
        }
    }
}

/// Parsed SSE messages, comments skipped.
struct Frames {
    response: reqwest::Response,
    buffer: String,
}

#[derive(Debug)]
struct Message {
    event: String,
    id: String,
    data: serde_json::Value,
}

impl Frames {
    /// The next message, or `None` when the stream ends within 5 s.
    async fn next(&mut self) -> Option<Message> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                if block.starts_with(':') {
                    continue;
                }
                let field = |name: &str| {
                    block
                        .lines()
                        .find_map(|line| line.strip_prefix(&format!("{name}: ")[..]))
                        .unwrap_or_default()
                        .to_owned()
                };
                return Some(Message {
                    event: field("event"),
                    id: field("id"),
                    data: serde_json::from_str(&field("data")).unwrap_or_default(),
                });
            }
            match tokio::time::timeout_at(deadline, self.response.chunk()).await {
                Ok(Ok(Some(bytes))) => self.buffer.push_str(std::str::from_utf8(&bytes).unwrap()),
                Ok(_) => return None,
                Err(_) => panic!("no event within 5 s"),
            }
        }
    }
}

fn quick() -> StreamLimits {
    StreamLimits {
        heartbeat: Duration::from_millis(100),
        ..StreamLimits::default()
    }
}

#[test]
fn domain_events_resume_cursor_prefers_last_event_id_and_refuses_malformed_values() {
    let generation = "0123456789abcdef0123456789abcdef";
    let uri = |query: &str| format!("/api/v1/events{query}").parse::<Uri>().unwrap();
    let mut headers = HeaderMap::new();
    assert_eq!(resume(&headers, &uri("")), Ok(None));
    let cursor = resume(&headers, &uri(&format!("?cursor={generation}.4"))).unwrap();
    assert_eq!(cursor.unwrap().to_string(), format!("{generation}.4"));
    headers.insert("last-event-id", format!("{generation}.9").parse().unwrap());
    let cursor = resume(&headers, &uri(&format!("?cursor={generation}.4"))).unwrap();
    assert_eq!(cursor.unwrap().to_string(), format!("{generation}.9"));
    headers.append("last-event-id", format!("{generation}.9").parse().unwrap());
    assert!(resume(&headers, &uri("")).is_err());
    let empty = HeaderMap::new();
    for query in [
        "?cursor=12",
        &format!("?cursor={generation}.1&cursor={generation}.2"),
    ] {
        assert!(resume(&empty, &uri(query)).is_err(), "{query}");
    }
}

#[tokio::test]
async fn domain_events_unauthenticated_connects_get_401() {
    let fixture = Fixture::start(quick()).await;
    assert_eq!(fixture.connect(&[], "").await.status(), 401);
    let wrong = fixture
        .connect(&[("Cookie", "pp_session=not-a-session")], "")
        .await;
    assert_eq!(wrong.status(), 401);
    assert_eq!(wrong.headers()["content-type"], "application/problem+json");
}

#[tokio::test]
async fn domain_events_stream_opens_ready_then_delivers_live_events() {
    let fixture = Fixture::start(quick()).await;
    let mut frames = fixture.open(&[]).await;
    let ready = frames.next().await.unwrap();
    assert_eq!(ready.event, "ready");
    assert_eq!(ready.data["position"], ready.id.as_str());
    fixture.record(1..3, 10);
    for expected in ["1", "2"] {
        let event = frames.next().await.unwrap();
        assert_eq!(event.event, "domain_event");
        assert_eq!(event.data["aggregate_id"], expected);
        assert_eq!(event.data["cursor"], event.id.as_str());
    }
}

#[tokio::test]
async fn domain_events_reconnect_replays_every_missed_event_in_order() {
    let fixture = Fixture::start(quick()).await;
    let position = fixture.open(&[]).await.next().await.unwrap().id;
    fixture.record(10..15, 10);
    let mut frames = fixture.open(&[("Last-Event-ID", &position)]).await;
    let mut ids = Vec::new();
    for _ in 0..5 {
        let event = frames.next().await.unwrap();
        assert_eq!(event.event, "domain_event");
        ids.push(event.data["aggregate_id"].as_str().unwrap().to_owned());
    }
    assert_eq!(ids, ["10", "11", "12", "13", "14"]);
}

#[tokio::test]
async fn domain_events_unreplayable_cursor_gets_resync_required() {
    let fixture = Fixture::start(quick()).await;
    let position = fixture.open(&[]).await.next().await.unwrap().id;
    let (generation, head) = position.split_once('.').unwrap();
    let head: i64 = head.parse().unwrap();
    let other = format!("{}.0", "f".repeat(32));
    let ahead = format!("{generation}.99");
    for (recorded, (cursor, reason)) in [
        (other.as_str(), "generation_changed"),
        (ahead.as_str(), "ahead_of_log"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut frames = fixture.open(&[("Last-Event-ID", cursor)]).await;
        let marker = frames.next().await.unwrap();
        assert_eq!(marker.event, "resync_required", "{cursor}");
        assert_eq!(marker.data["reason"], reason);
        assert_eq!(
            marker.id,
            format!("{generation}.{}", head + recorded as i64)
        );
        fixture.record(1..2, 10);
        assert_eq!(frames.next().await.unwrap().event, "domain_event");
    }
    let replay_window = pp_storage::domain_events::RETAINED_EVENTS;
    fixture.record(0..replay_window + 1, 10);
    let mut frames = fixture.open(&[("Last-Event-ID", &position)]).await;
    let marker = frames.next().await.unwrap();
    assert_eq!(marker.event, "resync_required");
    assert_eq!(marker.data["reason"], "expired");
    assert_eq!(
        marker.id,
        format!("{generation}.{}", head + 2 + replay_window + 1)
    );
    fixture.record(1..2, 10);
    assert_eq!(frames.next().await.unwrap().event, "domain_event");
}

#[tokio::test]
async fn domain_events_revoked_session_closes_the_stream() {
    let fixture = Fixture::start(quick()).await;
    let mut frames = fixture.open(&[]).await;
    assert_eq!(frames.next().await.unwrap().event, "ready");
    fixture
        .database()
        .execute("DELETE FROM sessions", [])
        .unwrap();
    assert!(frames.next().await.is_none(), "the stream ends");
}

#[tokio::test]
async fn domain_events_stream_count_is_bounded() {
    let fixture = Fixture::start(StreamLimits {
        streams: 1,
        ..quick()
    })
    .await;
    let mut held = fixture.open(&[]).await;
    assert_eq!(held.next().await.unwrap().event, "ready");
    let cookie = format!("pp_session={SECRET}");
    let refused = fixture.connect(&[("Cookie", &cookie)], "").await;
    assert_eq!(refused.status(), 503);
}

#[tokio::test]
async fn domain_events_slow_consumer_is_disconnected_without_stalling_writers() {
    let fixture = Fixture::start(StreamLimits {
        streams: 1,
        queued_frames: 2,
        slow_client_deadline: Duration::from_millis(200),
        heartbeat: Duration::from_secs(60),
    })
    .await;
    let address = fixture.origin.strip_prefix("http://").unwrap();
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    let mut slow = socket.connect(address.parse().unwrap()).await.unwrap();
    slow.write_all(
        format!(
            "GET /api/v1/events HTTP/1.1\r\nHost: {address}\r\nCookie: pp_session={SECRET}\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut head = [0u8; 12];
    slow.read_exact(&mut head).await.unwrap();
    assert_eq!(&head, b"HTTP/1.1 200");
    fixture.record(1..4000, 2000);
    let mut slowest = Duration::ZERO;
    let stalled = Instant::now() + Duration::from_secs(1);
    while Instant::now() < stalled {
        slowest = slowest.max(fixture.poke());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        slowest < Duration::from_secs(1),
        "writers waited {slowest:?}"
    );
    let cookie = format!("pp_session={SECRET}");
    let held = fixture.connect(&[("Cookie", &cookie)], "").await;
    assert_eq!(held.status(), 503, "a stalled connection keeps its slot");
    let mut delivered = Vec::new();
    let drained =
        tokio::time::timeout(Duration::from_secs(5), slow.read_to_end(&mut delivered)).await;
    assert!(drained.is_ok(), "the server kept the slow connection open");
    let events = String::from_utf8_lossy(&delivered)
        .matches("event: domain_event")
        .count();
    assert!(events < 3999, "the slow client got every event");
    let deadline = Instant::now() + Duration::from_secs(2);
    while fixture.connect(&[("Cookie", &cookie)], "").await.status() != 200 {
        assert!(
            Instant::now() < deadline,
            "the closed connection kept its slot"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
