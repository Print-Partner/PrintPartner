//! GRE-1014: the Rust-only launcher serves `GET /ws/printers` from a recorded
//! Moonraker session: refused without a session, a snapshot of every
//! configured printer first, then every transition in order with a strictly
//! increasing sequence (error then idle both seen), offline with a last-heard
//! time once the printer stops answering, resume by sequence, and heartbeats.
use super::temporary;
use super::ticket_t_4_watcher_proof::{
    SUCCESS, production_rows, proof_data, run_in_child, seed_settings, shut_down, signed_in,
    start_rust_only,
};
use pp_testkit::moonraker::RecordedPrinter;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const ERROR: &[u8] =
    include_bytes!("../../../../../fixtures/drivers/moonraker/moonraker-error.json");
const IDLE: &[u8] = br#"{"result":{"status":{"print_stats":{"state":"standby","filename":""}}}}"#;

#[tokio::test]
async fn printer_stream_rust_only_runtime_replays_a_recorded_session() {
    let data = temporary("printer-stream");
    run_in_child(
        "printer_stream::printer_stream_rust_only_runtime_in_child",
        &data,
    )
    .await;
    let _ = std::fs::remove_dir_all(data);
}

/// Sends the upgrade request and returns the response status line and the
/// socket positioned after the response head.
async fn upgrade(origin: &str, path: &str, cookie: &str) -> (String, TcpStream) {
    let host = origin.trim_start_matches("http://");
    let mut socket = TcpStream::connect(host).await.unwrap();
    let head = [
        format!("GET {path} HTTP/1.1"),
        format!("Host: {host}"),
        format!("Origin: {origin}"),
        "Connection: Upgrade".into(),
        "Upgrade: websocket".into(),
        "Sec-WebSocket-Version: 13".into(),
        "Sec-WebSocket-Key: cHJpbnRlci1zdHJlYW0tMQ==".into(),
        format!("Cookie: {cookie}"),
    ]
    .join("\r\n");
    socket
        .write_all(format!("{head}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = Vec::new();
    while !response.ends_with(b"\r\n\r\n") {
        response.push(socket.read_u8().await.unwrap());
    }
    let response = String::from_utf8(response).unwrap();
    let status = response.lines().next().unwrap().to_owned();
    (status, socket)
}

async fn open(origin: &str, path: &str, cookie: &str) -> TcpStream {
    let (status, socket) = upgrade(origin, path, cookie).await;
    assert!(status.contains(" 101 "), "{status}");
    socket
}

/// The next server text frame as JSON. Server frames are unmasked.
async fn next(socket: &mut TcpStream) -> Value {
    let wait = Duration::from_secs(25);
    tokio::time::timeout(wait, async {
        let opcode = socket.read_u8().await.unwrap() & 0x0f;
        let length = match socket.read_u8().await.unwrap() & 0x7f {
            126 => usize::from(socket.read_u16().await.unwrap()),
            127 => usize::try_from(socket.read_u64().await.unwrap()).unwrap(),
            short => usize::from(short),
        };
        let mut payload = vec![0; length];
        socket.read_exact(&mut payload).await.unwrap();
        assert_eq!(
            opcode,
            1,
            "text frame: {}",
            String::from_utf8_lossy(&payload)
        );
        serde_json::from_slice(&payload).unwrap()
    })
    .await
    .expect("the printer stream went quiet")
}

fn moon(printers: &Value) -> &Value {
    printers
        .as_array()
        .unwrap()
        .iter()
        .find(|printer| printer["id"] == "p-moon")
        .unwrap()
}

/// A recorded Moonraker printer, an unreachable one and one not set up.
fn seed(data: &std::path::Path, url: &str) {
    seed_settings(
        data,
        &[
            (
                "integrations",
                json!([
                    {"id": "moon-1", "type": "moonraker",
                     "config": {"base_url": url, "api_key": "recorded-key"}},
                    {"id": "dead-1", "type": "moonraker",
                     "config": {"base_url": "http://127.0.0.1:9"}}
                ]),
            ),
            (
                "printer.fleet",
                json!([
                    {"id": "p-moon", "name": "Micron", "integration_id": "moon-1"},
                    {"id": "p-dead", "name": "Dead", "integration_id": "dead-1"},
                    {"id": "p-none", "name": "Annex K3"}
                ]),
            ),
        ],
    );
}

/// Frames after the snapshot until the recorded printer is offline and a
/// heartbeat arrived: the events, p-moon's states and the heartbeat.
async fn follow(socket: &mut TcpStream, snapshot: &Value) -> (Vec<Value>, Vec<Value>, Value) {
    let stream = snapshot["stream"].as_str().unwrap();
    let mut states = vec![moon(&snapshot["printers"])["state"].clone()];
    let mut events = Vec::new();
    let mut seq = snapshot["seq"].as_u64().unwrap();
    let mut heartbeat = None;
    while states.last() != Some(&json!("offline")) || heartbeat.is_none() {
        let frame = next(socket).await;
        assert_eq!(frame["stream"], stream);
        if frame["type"] == "heartbeat" {
            assert_eq!(frame["seq"], seq, "a heartbeat names the last sequence");
            heartbeat = Some(frame);
            continue;
        }
        assert_eq!(frame["seq"], seq + 1, "{frame}");
        seq += 1;
        if frame["type"] == "printer" && frame["printer"]["id"] == "p-moon" {
            states.push(frame["printer"]["state"].clone());
        }
        events.push(frame);
    }
    states.retain(|state| state != "unknown");
    states.dedup();
    (events, states, heartbeat.unwrap())
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn printer_stream_rust_only_runtime_in_child() {
    let data = proof_data();
    let replies = vec![
        (200, SUCCESS),
        (200, SUCCESS),
        (200, SUCCESS),
        (200, SUCCESS),
        (200, ERROR),
        (200, IDLE),
        (503, &b"{}"[..]),
    ];
    let printer = RecordedPrinter::start(replies).unwrap();
    seed(&data, printer.url());
    let rows = production_rows(&data);
    let (runtime, origin, desktop) = start_rust_only(&data).await;

    let (refused, _) = upgrade(&origin, "/ws/printers", &desktop).await;
    assert!(refused.contains(" 401 "), "{refused}");
    let cookie = signed_in(&origin, &desktop, 1).await;

    let mut socket = open(&origin, "/ws/printers", &cookie).await;
    let snapshot = next(&mut socket).await;
    assert_eq!(snapshot["type"], "snapshot", "{snapshot}");
    assert_eq!(snapshot["heartbeat_seconds"], 15);
    let ids: Vec<_> = snapshot["printers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|printer| printer["id"].clone())
        .collect();
    assert_eq!(ids, ["p-moon", "p-dead", "p-none"]);
    let stream = snapshot["stream"].as_str().unwrap().to_owned();

    let (events, states, heartbeat) = follow(&mut socket, &snapshot).await;
    assert_eq!(
        states,
        [
            json!("printing"),
            json!("error"),
            json!("idle"),
            json!("offline")
        ],
        "every recorded transition, in order"
    );
    let offline = events
        .iter()
        .rev()
        .find(|frame| frame["printer"]["id"] == "p-moon")
        .unwrap();
    let heard = offline["printer"]["last_heard_at"].as_str().unwrap();
    assert!(heard.ends_with('Z'), "{heard}");
    assert!(heartbeat["at"].as_str().unwrap().ends_with('Z'));

    let error = events
        .iter()
        .find(|frame| frame["printer"]["state"] == "error")
        .unwrap();
    let since = error["seq"].as_u64().unwrap() - 1;
    let resume = format!("/ws/printers?stream={stream}&since={since}");
    let mut resumed = open(&origin, &resume, &cookie).await;
    for expected in events
        .iter()
        .filter(|frame| frame["seq"].as_u64() > Some(since))
    {
        assert_eq!(
            &next(&mut resumed).await,
            expected,
            "resume replays missed events"
        );
    }
    let mut fresh = open(&origin, "/ws/printers?stream=other&since=1", &cookie).await;
    let fresh = next(&mut fresh).await;
    assert_eq!(fresh["type"], "snapshot");
    assert_eq!(moon(&fresh["printers"])["state"], "offline");

    let text = format!("{snapshot}{events:?}");
    assert!(!text.contains("127.0.0.1") && !text.contains("recorded-key"));
    shut_down(runtime).await;
    for line in &printer.request_lines() {
        assert!(
            line.starts_with("GET /printer/objects/query?"),
            "monitoring sent {line}"
        );
    }
    assert_eq!(
        production_rows(&data),
        rows,
        "the stream changed production rows"
    );
    println!("printer_stream: {} events, states {states:?}", events.len());
}
