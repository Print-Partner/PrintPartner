//! A send through the Rust-only runtime: Add to queue, then Send now, against
//! loopback Moonraker simulators that record every request. One accepts the
//! upload, one closes the connection without answering, and one refuses it.
//! The typed answers and the queue read carry `error`, `upload_job_id` and
//! `outcome_unknown`; an unknown outcome is never resent, across a restart too.
use super::watcher_proof::{
    proof_data, run_in_child, seed_settings, shut_down, signed_in, start_rust_only,
};
use super::{request, temporary};
use axum::{body::Body, http::StatusCode};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use pp_testkit::moonraker::RoutedPrinter;
use serde_json::{Value, json};
use std::{
    io::Read,
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const QUEUE: &str = "/api/v2/printer-send-queue";
const UPLOAD: &str = "/server/files/upload";
const API_KEY: &str = "simulator-api-key-0d5f";

#[tokio::test]
async fn send_queue_rust_only_runtime_sends_once_and_never_resends_an_unknown_outcome() {
    let data = temporary("send-queue");
    run_in_child("send_queue::send_queue_rust_only_runtime_in_child", &data).await;
    let _ = std::fs::remove_dir_all(data);
}

/// A loopback host that records each request line, reads what follows until
/// the client stops writing, and closes without answering.
struct SilentPrinter {
    url: String,
    lines: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl SilentPrinter {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (logged, stopped) = (Arc::clone(&lines), Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                };
                let logged = Arc::clone(&logged);
                std::thread::spawn(move || {
                    socket.set_nonblocking(false).unwrap();
                    let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
                    let mut head = Vec::new();
                    let mut bytes = [0; 8192];
                    let mut logged_line = false;
                    while let Ok(read @ 1..) = socket.read(&mut bytes) {
                        head.extend_from_slice(&bytes[..read]);
                        // Record the request as soon as its line is in.
                        if !logged_line
                            && let Some(end) = head.windows(2).position(|w| w == b"\r\n")
                        {
                            let line = String::from_utf8_lossy(&head[..end]).into_owned();
                            logged.lock().unwrap().push(line);
                            logged_line = true;
                        }
                    }
                });
            }
        });
        Self { url, lines, stop }
    }
}

impl Drop for SilentPrinter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// The request lines that would send something: every request but status
/// polling.
fn sends(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .filter(|line| !line.starts_with("GET "))
        .collect()
}

async fn call(origin: &str, cookie: &str, method: &str, path: &str) -> (StatusCode, Value) {
    let (status, _, body) = request(origin, path, method, Some(cookie), None, None, None).await;
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// Add to queue for `printer`: an upload-only send of a small G-code file
/// bound to Build `build`.
async fn add(origin: &str, cookie: &str, printer: &str, build: i64) -> Value {
    let boundary = "send-queue-proof";
    let mut body = String::new();
    for (name, value) in [
        ("printer_id", printer.to_owned()),
        ("profile_id", build.to_string()),
        ("start", "false".into()),
        ("wait_for_idle", "false".into()),
    ] {
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        ));
    }
    body.push_str(&format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"plate.gcode\"\r\nContent-Type: application/octet-stream\r\n\r\nG28\nG1 X10\n\r\n--{boundary}--\r\n"
    ));
    let authority = origin.strip_prefix("http://").unwrap();
    let socket = tokio::net::TcpStream::connect(authority).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    tokio::spawn(connection);
    let response = sender
        .send_request(
            axum::http::Request::post(QUEUE)
                .header("host", authority)
                .header("origin", origin)
                .header("cookie", cookie)
                .header(
                    "content-type",
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    body["item"].clone()
}

/// Send now; the answer's `job_id`.
async fn send_now(origin: &str, cookie: &str, id: &str) -> String {
    let (status, body) = call(origin, cookie, "POST", &format!("{QUEUE}/{id}/dispatch")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["item"]["state"], "sending", "{body}");
    assert_eq!(body["item"]["upload_job_id"], body["job_id"], "{body}");
    body["job_id"].as_str().unwrap().to_owned()
}

/// The queue read's item `id` once it has left `sending`.
async fn settled(origin: &str, cookie: &str, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (status, queue) = call(origin, cookie, "GET", QUEUE).await;
            assert_eq!(status, StatusCode::OK, "{queue}");
            let item = queue["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["id"] == id)
                .cloned()
                .unwrap();
            if item["state"] != "sending" {
                return item;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the send never settled")
}

/// Send now on an item whose outcome is unknown is refused.
async fn refused_as_unknown(origin: &str, cookie: &str, id: &str) {
    let (status, body) = call(origin, cookie, "POST", &format!("{QUEUE}/{id}/dispatch")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "send_outcome_unknown", "{body}");
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn send_queue_rust_only_runtime_in_child() {
    let data = proof_data();
    let accepting = RoutedPrinter::start(vec![(UPLOAD.into(), 201, b"{}".to_vec())]).unwrap();
    let refusing = RoutedPrinter::start(vec![(
        UPLOAD.into(),
        500,
        br#"{"error":{"message":"Disk full"}}"#.to_vec(),
    )])
    .unwrap();
    let silent = SilentPrinter::start();
    let host = |id: &str, url: &str| {
        json!({"id": id, "name": id, "type": "moonraker",
            "config": {"base_url": url, "api_key": API_KEY}})
    };
    seed_settings(
        &data,
        &[
            ("feature_toggles", json!({"farm": true})),
            (
                "integrations",
                json!([
                    host("accepting", accepting.url()),
                    host("silent", &silent.url),
                    host("refusing", refusing.url()),
                ]),
            ),
            (
                "printer.fleet",
                json!([
                    {"id": "p-accepting", "name": "Accepting", "integration_id": "accepting"},
                    {"id": "p-silent", "name": "Silent", "integration_id": "silent"},
                    {"id": "p-refusing", "name": "Refusing", "integration_id": "refusing"}
                ]),
            ),
        ],
    );
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookie = signed_in(&origin, &desktop, 1).await;
    let (status, _, build) = request(
        &origin,
        "/api/v2/plans",
        "POST",
        Some(&cookie),
        None,
        None,
        Some(json!({"name": "Send proof"})),
    )
    .await;
    let build: Value = serde_json::from_slice(&build).unwrap();
    assert!(status.is_success(), "{build}");
    let build = build["id"].as_i64().unwrap();

    // Add sends nothing.
    let (_, queue) = call(&origin, &cookie, "GET", QUEUE).await;
    assert_eq!(queue, json!({"items": []}));
    let item = add(&origin, &cookie, "p-accepting", build).await;
    assert_eq!(item["state"], "queued");
    assert_eq!(
        (
            &item["error"],
            &item["upload_job_id"],
            &item["outcome_unknown"]
        ),
        (&Value::Null, &Value::Null, &json!(false))
    );
    let (_, queue) = call(&origin, &cookie, "GET", QUEUE).await;
    assert_eq!(queue["items"], json!([item]));
    assert_eq!(sends(accepting.request_lines()), Vec::<String>::new());

    // Send now uploads once and never starts the print.
    let id = item["id"].as_str().unwrap();
    let job = send_now(&origin, &cookie, id).await;
    let done = settled(&origin, &cookie, id).await;
    assert_eq!(done["state"], "done", "{done}");
    assert_eq!(
        (
            &done["error"],
            &done["upload_job_id"],
            &done["outcome_unknown"]
        ),
        (&Value::Null, &json!(job), &json!(false))
    );
    let lines = sends(accepting.request_lines());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with(&format!("POST {UPLOAD}")), "{lines:?}");

    // A connection closed without an answer leaves the outcome unknown.
    let lost = add(&origin, &cookie, "p-silent", build).await;
    let lost_id = lost["id"].as_str().unwrap().to_owned();
    let lost_job = send_now(&origin, &cookie, &lost_id).await;
    let unknown = settled(&origin, &cookie, &lost_id).await;
    assert_eq!(
        (
            &unknown["state"],
            &unknown["outcome_unknown"],
            &unknown["upload_job_id"]
        ),
        (&json!("error"), &json!(true), &json!(lost_job)),
        "{unknown}"
    );
    refused_as_unknown(&origin, &cookie, &lost_id).await;
    assert_eq!(sends(silent.lines.lock().unwrap().clone()).len(), 1);

    // A refused upload is a known failure with the printer's reason.
    let refused = add(&origin, &cookie, "p-refusing", build).await;
    let refused_id = refused["id"].as_str().unwrap().to_owned();
    send_now(&origin, &cookie, &refused_id).await;
    let failed = settled(&origin, &cookie, &refused_id).await;
    assert_eq!(
        (&failed["state"], &failed["outcome_unknown"]),
        (&json!("error"), &json!(false)),
        "{failed}"
    );
    let reason = failed["error"].as_str().unwrap();
    assert!(reason.starts_with("Upload failed (HTTP 500)"), "{reason}");
    assert!(reason.contains("Disk full"), "{reason}");
    assert!(!reason.contains(API_KEY), "{reason}");
    assert_eq!(sends(refusing.request_lines()).len(), 1);
    shut_down(runtime).await;

    // After a restart the unknown outcome stays, and nothing is resent.
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookie = signed_in(&origin, &desktop, 2).await;
    let (_, queue) = call(&origin, &cookie, "GET", QUEUE).await;
    let after = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == lost_id.as_str())
        .cloned()
        .unwrap();
    assert_eq!(
        (
            &after["state"],
            &after["outcome_unknown"],
            &after["upload_job_id"]
        ),
        (&json!("error"), &json!(true), &json!(lost_job)),
        "{after}"
    );
    refused_as_unknown(&origin, &cookie, &lost_id).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    shut_down(runtime).await;
    assert_eq!(
        sends(silent.lines.lock().unwrap().clone()).len(),
        1,
        "the unknown send was resent"
    );
    assert_eq!(sends(accepting.request_lines()).len(), 1);
    assert_eq!(sends(refusing.request_lines()).len(), 1);
    let text = queue.to_string();
    assert!(
        !text.contains(API_KEY) && !text.contains("printer-uploads"),
        "{text}"
    );
}
