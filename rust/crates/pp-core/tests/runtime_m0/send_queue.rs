//! A send through the Rust-only runtime: Add to queue, then Send now, against
//! loopback Moonraker simulators that record every request. One accepts the
//! upload, one closes the connection without answering, and one refuses it.
//! The typed answers and the queue read carry `error`, `upload_job_id` and
//! `outcome_unknown`; an unknown outcome is never resent, across a restart too.
use super::watcher_proof::{
    proof_data, run_in_child, seed_settings, shut_down, signed_in, start_rust_only,
};
use axum::http::StatusCode;
use pp_testkit::{
    moonraker::RoutedPrinter,
    prusalink::{Request, UploadPrinter},
};
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
/// The file Add to queue stages.
const GCODE: &str = "G28\nG1 X10\n";

#[tokio::test]
async fn send_queue_rust_only_runtime_sends_once_and_never_resends_an_unknown_outcome() {
    let data = super::temporary("send-queue");
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
    let (status, _, body) =
        super::request(origin, path, method, Some(cookie), None, None, None).await;
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

/// Add to queue for `printer`: a send of a small G-code file bound to Build
/// `build`, upload-only unless `start`.
async fn add(origin: &str, cookie: &str, printer: &str, build: i64, start: bool) -> Value {
    let boundary = "send-queue-proof";
    let mut body = String::new();
    for (name, value) in [
        ("printer_id", printer.to_owned()),
        ("profile_id", build.to_string()),
        ("start", start.to_string()),
        ("wait_for_idle", "false".into()),
    ] {
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        ));
    }
    body.push_str(&format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"plate.gcode\"\r\nContent-Type: application/octet-stream\r\n\r\n{GCODE}\r\n--{boundary}--\r\n"
    ));
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("{origin}{QUEUE}"))
        .header("origin", origin)
        .header("cookie", cookie)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
    let body: Value = response.json().await.unwrap();
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

/// The simulators: one accepts the upload, one closes without answering and
/// one refuses it.
struct Printers {
    accepting: RoutedPrinter,
    silent: SilentPrinter,
    refusing: RoutedPrinter,
}

impl Printers {
    fn start() -> Self {
        let refusal = br#"{"error":{"message":"Disk full"}}"#.to_vec();
        Self {
            accepting: RoutedPrinter::start(vec![(UPLOAD.into(), 201, b"{}".to_vec())]).unwrap(),
            silent: SilentPrinter::start(),
            refusing: RoutedPrinter::start(vec![(UPLOAD.into(), 500, refusal)]).unwrap(),
        }
    }

    /// Farm on, and one saved Moonraker printer per simulator.
    fn seed(&self, data: &std::path::Path) {
        let host = |id: &str, url: &str| {
            json!({"id": id, "name": id, "type": "moonraker",
                "config": {"base_url": url, "api_key": API_KEY}})
        };
        let printer = |id: &str| json!({"id": format!("p-{id}"), "name": id, "integration_id": id});
        seed_settings(
            data,
            &[
                ("feature_toggles", json!({"farm": true})),
                (
                    "integrations",
                    json!([
                        host("accepting", self.accepting.url()),
                        host("silent", &self.silent.url),
                        host("refusing", self.refusing.url()),
                    ]),
                ),
                (
                    "printer.fleet",
                    json!([printer("accepting"), printer("silent"), printer("refusing")]),
                ),
            ],
        );
    }

    /// The send requests each simulator received, in the order above.
    fn sends(&self) -> [usize; 3] {
        [
            sends(self.accepting.request_lines()).len(),
            sends(self.silent.lines.lock().unwrap().clone()).len(),
            sends(self.refusing.request_lines()).len(),
        ]
    }
}

/// The item's `(state, error, upload_job_id, outcome_unknown)`.
fn outcome(item: &Value) -> [&Value; 4] {
    [
        &item["state"],
        &item["error"],
        &item["upload_job_id"],
        &item["outcome_unknown"],
    ]
}

async fn create_build(origin: &str, cookie: &str) -> i64 {
    let name = json!({"name": "Send proof"});
    let (status, _, build) = super::request(
        origin,
        "/api/v2/plans",
        "POST",
        Some(cookie),
        None,
        None,
        Some(name),
    )
    .await;
    let build: Value = serde_json::from_slice(&build).unwrap();
    assert!(status.is_success(), "{build}");
    build["id"].as_i64().unwrap()
}

/// Add sends nothing; Send now uploads once and never starts the print.
async fn upload_only_sends_once(origin: &str, cookie: &str, build: i64, printers: &Printers) {
    let (_, queue) = call(origin, cookie, "GET", QUEUE).await;
    assert_eq!(queue, json!({"items": []}));
    let item = add(origin, cookie, "p-accepting", build, false).await;
    let none = Value::Null;
    assert_eq!(
        outcome(&item),
        [&json!("queued"), &none, &none, &json!(false)]
    );
    let (_, queue) = call(origin, cookie, "GET", QUEUE).await;
    assert_eq!(queue["items"], json!([item]));
    assert_eq!(printers.sends(), [0, 0, 0]);

    let id = item["id"].as_str().unwrap();
    let job = json!(send_now(origin, cookie, id).await);
    let done = settled(origin, cookie, id).await;
    assert_eq!(outcome(&done), [&json!("done"), &none, &job, &json!(false)]);
    let lines = sends(printers.accepting.request_lines());
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].starts_with(&format!("POST {UPLOAD}")), "{lines:?}");
}

/// A connection closed without an answer leaves the outcome unknown; the
/// item's id and upload job.
async fn unknown_outcome_is_refused(origin: &str, cookie: &str, build: i64) -> (String, Value) {
    let lost = add(origin, cookie, "p-silent", build, false).await;
    let id = lost["id"].as_str().unwrap().to_owned();
    let job = json!(send_now(origin, cookie, &id).await);
    let unknown = settled(origin, cookie, &id).await;
    let [state, _, upload, flag] = outcome(&unknown);
    assert_eq!(
        [state, upload, flag],
        [&json!("error"), &job, &json!(true)],
        "{unknown}"
    );
    refused_as_unknown(origin, cookie, &id).await;
    (id, job)
}

/// A refused upload is a known failure with the printer's reason.
async fn refused_upload_keeps_the_reason(origin: &str, cookie: &str, build: i64) {
    let refused = add(origin, cookie, "p-refusing", build, false).await;
    let id = refused["id"].as_str().unwrap().to_owned();
    send_now(origin, cookie, &id).await;
    let failed = settled(origin, cookie, &id).await;
    let [state, _, _, flag] = outcome(&failed);
    assert_eq!([state, flag], [&json!("error"), &json!(false)], "{failed}");
    let reason = failed["error"].as_str().unwrap();
    assert!(reason.starts_with("Upload failed (HTTP 500)"), "{reason}");
    assert!(reason.contains("Disk full"), "{reason}");
    assert!(!reason.contains(API_KEY), "{reason}");
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn send_queue_rust_only_runtime_in_child() {
    let data = proof_data();
    let printers = Printers::start();
    printers.seed(&data);
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookie = signed_in(&origin, &desktop, 1).await;
    let build = create_build(&origin, &cookie).await;
    upload_only_sends_once(&origin, &cookie, build, &printers).await;
    let (lost, job) = unknown_outcome_is_refused(&origin, &cookie, build).await;
    refused_upload_keeps_the_reason(&origin, &cookie, build).await;
    assert_eq!(printers.sends(), [1, 1, 1]);
    shut_down(runtime).await;

    // After a restart the unknown outcome stays, and nothing is resent.
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookie = signed_in(&origin, &desktop, 2).await;
    let (_, queue) = call(&origin, &cookie, "GET", QUEUE).await;
    let items = queue["items"].as_array().unwrap();
    let after = items
        .iter()
        .find(|item| item["id"] == lost.as_str())
        .unwrap();
    let [state, _, upload, flag] = outcome(after);
    assert_eq!(
        [state, upload, flag],
        [&json!("error"), &job, &json!(true)],
        "{after}"
    );
    refused_as_unknown(&origin, &cookie, &lost).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    shut_down(runtime).await;
    assert_eq!(printers.sends(), [1, 1, 1], "a send was repeated");
    let text = queue.to_string();
    assert!(
        !text.contains(API_KEY) && !text.contains("printer-uploads"),
        "{text}"
    );
}

const PRUSA_PUT: &str = "/api/v1/files/usb/plate.gcode";
const PRUSA_PASSWORD: &str = "simulator-digest-password-7c1e";

/// GRE-1240: the same user actions against loopback PrusaLink simulators. A
/// confirmed send with Start is one `PUT` carrying `print-after-upload: ?1`;
/// an unknown outcome and a refusal are handled as for Moonraker.
#[tokio::test]
async fn send_queue_rust_only_runtime_owns_prusalink_uploads_end_to_end() {
    let data = super::temporary("send-queue-prusalink");
    let stdout = run_in_child(
        "send_queue::send_queue_rust_only_runtime_prusalink_in_child",
        &data,
    )
    .await;
    let _ = std::fs::remove_dir_all(data);
    let receipt = stdout
        .lines()
        .find(|line| line.starts_with("GRE_1240_RUNTIME_RECEIPT "))
        .expect("the child printed its receipt");
    println!("{receipt}");
}

/// PrusaLink simulators whose Digest probe answers 200: one accepts the
/// upload, one answers it with a malformed status line (outcome unknown) and
/// one refuses it.
struct PrusaPrinters {
    accepting: UploadPrinter,
    lost: UploadPrinter,
    refusing: UploadPrinter,
}

impl PrusaPrinters {
    fn start() -> Self {
        let printer =
            |status: u16, body: &[u8]| UploadPrinter::start(PRUSA_PUT, status, body).unwrap();
        Self {
            accepting: printer(201, b"{}"),
            lost: printer(0, b"{}"),
            refusing: printer(500, br#"{"title":"Disk full"}"#),
        }
    }

    /// Farm on, and one saved PrusaLink printer per simulator.
    fn seed(&self, data: &std::path::Path) {
        let host = |id: &str, url: &str| {
            json!({"id": id, "name": id, "type": "prusalink", "config": {
                "base_url": url, "username": "maker", "password": PRUSA_PASSWORD}})
        };
        let printer = |id: &str| json!({"id": format!("p-{id}"), "name": id, "integration_id": id});
        seed_settings(
            data,
            &[
                ("feature_toggles", json!({"farm": true})),
                (
                    "integrations",
                    json!([
                        host("accepting", self.accepting.url()),
                        host("lost", self.lost.url()),
                        host("refusing", self.refusing.url()),
                    ]),
                ),
                (
                    "printer.fleet",
                    json!([printer("accepting"), printer("lost"), printer("refusing")]),
                ),
            ],
        );
    }

    /// The send requests each simulator received, in the order above. Each
    /// must be a whole `PUT` of the file Add staged, with exact header values:
    /// only `accepting` gets a send with Start.
    fn sends(&self) -> [Vec<Request>; 3] {
        [
            (&self.accepting, "?1"),
            (&self.lost, "?0"),
            (&self.refusing, "?0"),
        ]
        .map(|(printer, start)| {
            let sends = printer.sends();
            for send in &sends {
                assert!(
                    send.line().starts_with(&format!("PUT {PRUSA_PUT} ")),
                    "{}",
                    send.head
                );
                assert!(send.complete, "{}", send.head);
                assert_eq!(send.body, GCODE.as_bytes(), "{}", send.head);
                assert_eq!(
                    send.header("print-after-upload"),
                    Some(start),
                    "{}",
                    send.head
                );
                assert_eq!(send.header("overwrite"), Some("?1"), "{}", send.head);
            }
            sends
        })
    }

    fn counts(&self) -> [usize; 3] {
        self.sends().map(|sends| sends.len())
    }
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn send_queue_rust_only_runtime_prusalink_in_child() {
    let data = proof_data();
    let printers = PrusaPrinters::start();
    printers.seed(&data);
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookie = signed_in(&origin, &desktop, 1).await;
    let build = create_build(&origin, &cookie).await;
    let none = Value::Null;

    // Add sends nothing; Send now with Start is one PUT that starts the print.
    let item = add(&origin, &cookie, "p-accepting", build, true).await;
    assert_eq!(
        outcome(&item),
        [&json!("queued"), &none, &none, &json!(false)]
    );
    assert_eq!(printers.counts(), [0, 0, 0]);
    let id = item["id"].as_str().unwrap();
    let job = json!(send_now(&origin, &cookie, id).await);
    let done = settled(&origin, &cookie, id).await;
    assert_eq!(outcome(&done), [&json!("done"), &none, &job, &json!(false)]);
    // `sends` checks the PUT's bytes and its exact `print-after-upload: ?1`.
    assert_eq!(printers.counts(), [1, 0, 0], "one PUT, no start request");

    // A malformed answer to the PUT leaves the outcome unknown.
    let lost = add(&origin, &cookie, "p-lost", build, false).await;
    let lost_id = lost["id"].as_str().unwrap().to_owned();
    let lost_job = json!(send_now(&origin, &cookie, &lost_id).await);
    let unknown = settled(&origin, &cookie, &lost_id).await;
    let [state, _, upload, flag] = outcome(&unknown);
    assert_eq!(
        [state, upload, flag],
        [&json!("error"), &lost_job, &json!(true)],
        "{unknown}"
    );
    refused_as_unknown(&origin, &cookie, &lost_id).await;

    // A refused PUT is a known failure with the printer's reason.
    let refused = add(&origin, &cookie, "p-refusing", build, false).await;
    let refused_id = refused["id"].as_str().unwrap().to_owned();
    send_now(&origin, &cookie, &refused_id).await;
    let failed = settled(&origin, &cookie, &refused_id).await;
    let [state, _, _, flag] = outcome(&failed);
    assert_eq!([state, flag], [&json!("error"), &json!(false)], "{failed}");
    let reason = failed["error"].as_str().unwrap();
    assert!(reason.starts_with("Upload failed (HTTP 500)"), "{reason}");
    assert!(reason.contains("Disk full"), "{reason}");
    assert_eq!(printers.counts(), [1, 1, 1]);
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
        .unwrap();
    let [state, _, upload, flag] = outcome(after);
    assert_eq!(
        [state, upload, flag],
        [&json!("error"), &lost_job, &json!(true)],
        "{after}"
    );
    refused_as_unknown(&origin, &cookie, &lost_id).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    shut_down(runtime).await;
    assert_eq!(printers.counts(), [1, 1, 1], "a send was repeated");
    let text = queue.to_string();
    assert!(
        !text.contains(PRUSA_PASSWORD) && !text.contains("printer-uploads"),
        "{text}"
    );
    println!(
        "GRE_1240_RUNTIME_RECEIPT {}",
        json!({"sends": printers.sends().map(|sends| sends.iter().map(|send| json!({
            "head": send.head, "bytes": send.body.len()})).collect::<Vec<_>>()), "done": done["state"],
        "unknown_after_restart": after["outcome_unknown"], "refused": reason})
    );
}
