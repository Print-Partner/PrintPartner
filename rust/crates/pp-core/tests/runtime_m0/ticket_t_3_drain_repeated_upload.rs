use std::{
    io::{Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
};

use sha2::{Digest, Sha256};

const UPLOAD_BYTES: usize = 256 * 1024;
const UPLOAD_PREFIX: usize = 16 * 1024;
const INTERRUPTED_UPLOAD: &str = "Interrupted by process restart. Printer outcome is unknown, so this send was not marked successful. The staged file remains for an explicit retry. This process will not retry a print on its own.";

#[derive(Clone, Debug)]
struct HeldUpload {
    target: String,
    content_length: usize,
    bytes_read: usize,
    saw_name: bool,
    saw_marker: bool,
}

struct UploadPeer {
    origin: String,
    uploads: Arc<Mutex<Vec<HeldUpload>>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl UploadPeer {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let accepted = Arc::clone(&uploads);
        let stopping = Arc::clone(&stop);
        let accept = std::thread::spawn(move || serve(listener, accepted, stopping));
        Self {
            origin,
            uploads,
            stop,
            accept: Some(accept),
        }
    }

    fn uploads(&self) -> Vec<HeldUpload> {
        self.uploads.lock().unwrap().clone()
    }
}

impl Drop for UploadPeer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

fn serve(
    listener: std::net::TcpListener,
    uploads: Arc<Mutex<Vec<HeldUpload>>>,
    stop: Arc<AtomicBool>,
) {
    listener.set_nonblocking(true).unwrap();
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let uploads = Arc::clone(&uploads);
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    if let Err(error) = handle(stream, &uploads, &stop) {
                        eprintln!("interrupted-upload peer: {error}");
                    }
                });
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                if !stop.load(Ordering::SeqCst) {
                    eprintln!("interrupted-upload peer accept: {error}");
                }
                break;
            }
        }
    }
}

fn handle(
    mut stream: std::net::TcpStream,
    uploads: &Mutex<Vec<HeldUpload>>,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    let _ = socket2::SockRef::from(&stream).set_recv_buffer_size(4096);
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    let (start, headers, mut body) = read_head(&mut stream)?;
    let target = start.split_whitespace().nth(1).unwrap_or("").to_owned();
    if start.starts_with("GET ") {
        let payload = br#"{"result":{"status":{"print_stats":{"state":"standby"},"virtual_sdcard":{"progress":0}}}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        stream.write_all(response.as_bytes())?;
        stream.write_all(payload)?;
        return Ok(());
    }
    if !start.starts_with("POST ") || target != "/server/files/upload" {
        let payload = b"{}";
        let response = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            payload.len()
        );
        stream.write_all(response.as_bytes())?;
        stream.write_all(payload)?;
        return Ok(());
    }
    let content_length = header_value(&headers, "content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    while body.len() < UPLOAD_PREFIX && (content_length == 0 || body.len() < content_length) {
        let mut chunk = [0u8; 4096];
        let want = (UPLOAD_PREFIX - body.len()).min(chunk.len());
        let read = stream.read(&mut chunk[..want])?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    uploads.lock().unwrap().push(HeldUpload {
        target,
        content_length,
        bytes_read: body.len(),
        saw_name: contains(&body, b"filename=\"interrupted.gcode\""),
        saw_marker: contains(&body, b"; GRE-448 upload proof"),
    });
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn read_head(stream: &mut std::net::TcpStream) -> std::io::Result<(String, String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    let split = loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "printer peer closed before headers",
            ));
        }
        buf.extend_from_slice(&chunk[..read]);
        if let Some(position) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        if buf.len() > 65_536 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "printer peer headers exceeded 64KiB",
            ));
        }
    };
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let start = head.lines().next().unwrap_or("").to_owned();
    let rest = buf.split_off(split);
    Ok((start, head, rest))
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn gcode() -> Vec<u8> {
    let line = b"; GRE-448 upload proof\nG1 X1 Y1 F1200\n";
    let mut content = Vec::with_capacity(UPLOAD_BYTES);
    while content.len() < UPLOAD_BYTES {
        let take = line.len().min(UPLOAD_BYTES - content.len());
        content.extend_from_slice(&line[..take]);
    }
    content
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn event_count(data: &Path, event: &str) -> usize {
    let text = fs::read_to_string(data.join("logs/desktop.jsonl")).unwrap_or_default();
    text.matches(&format!("\"event\":\"{event}\"")).count()
}

fn persisted_send(data: &Path) -> serde_json::Value {
    let connection = rusqlite::Connection::open_with_flags(
        data.join("print-partner.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("shutdown must leave the desktop database readable");
    connection.busy_timeout(Duration::from_secs(5)).unwrap();
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let value: String = connection
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id = 'default' AND key = 'printer.send_queue'",
            [],
            |row| row.get(0),
        )
        .expect("printer send queue was not persisted");
    let items: Vec<serde_json::Value> = serde_json::from_str(&value).unwrap();
    assert_eq!(items.len(), 1, "persisted queue lost or duplicated an item");
    items.into_iter().next().unwrap()
}

fn predecessors_are_dead(pids: &[u32]) {
    assert!(
        pids.iter().all(|pid| pp_core::process_group_absent(*pid)),
        "repeated start/stop left a duplicate child: {pids:?}"
    );
}

fn json_body(bytes: &[u8]) -> serde_json::Value {
    serde_json::from_slice(bytes).unwrap_or_else(|error| {
        panic!("{error}: {}", String::from_utf8_lossy(bytes));
    })
}

async fn call(
    origin: &str,
    method: &str,
    path: &str,
    cookie: &str,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let (status, _, bytes) = request(origin, path, method, Some(cookie), None, None, body).await;
    let parsed = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        json_body(&bytes)
    };
    (status, parsed)
}

async fn post_bytes(
    origin: &str,
    path: &str,
    cookie: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, Vec<u8>) {
    use axum::body::Body;
    use http_body_util::BodyExt;
    use hyper_util::rt::TokioIo;

    let authority = origin.strip_prefix("http://").unwrap();
    let socket = tokio::net::TcpStream::connect(authority).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let response = sender
        .send_request(
            axum::http::Request::builder()
                .uri(path)
                .method("POST")
                .header("host", authority)
                .header("origin", origin)
                .header("cookie", cookie)
                .header("content-type", content_type)
                .header("content-length", body.len())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let (head, incoming) = response.into_parts();
    let bytes = incoming.collect().await.unwrap().to_bytes().to_vec();
    (head.status, bytes)
}

fn multipart(fields: &[(&str, &str)], file: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!("gre448{}", hex::encode(rand::random::<[u8; 12]>()));
    let mut body = Vec::new();
    for (name, value) in fields {
        body.extend(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .into_bytes(),
        );
    }
    body.extend(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"interrupted.gcode\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .into_bytes(),
    );
    body.extend_from_slice(file);
    body.extend(format!("\r\n--{boundary}--\r\n").into_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn wait_for_upload(peer: &UploadPeer) -> HeldUpload {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(upload) = peer.uploads().into_iter().next() {
                return upload;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("printer peer did not see the interrupted upload")
}

#[tokio::test]
async fn ticket_t_3_drain_runtime_repeated_start_stop_eof_upload_no_duplicate_or_resend() {
    let Some(data) = isolated_case_for(
        "ticket_t_3_drain_runtime_repeated_start_stop_eof_upload_no_duplicate_or_resend",
        false,
        Duration::from_secs(420),
    )
    .await
    else {
        return;
    };
    let peer = UploadPeer::start();
    let file = gcode();
    let file_sha256 = digest(&file);
    let mut pids = Vec::new();

    predecessors_are_dead(&pids);
    let runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let first = pid(&runtime);
    pids.push(first);
    let first_receipt = runtime.shutdown().await;
    assert_clean(&first_receipt, first);
    assert_eq!(event_count(&data, "compat_starting"), 1);

    predecessors_are_dead(&pids);
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let second = pid(&runtime);
    assert_ne!(second, first);
    pids.push(second);
    let cookie = bootstrap(&mut runtime).await;
    let origin = runtime.origin().to_owned();
    let (status, created) = call(
        &origin,
        "POST",
        "/api/v1/integrations",
        &cookie,
        Some(json!({
            "name": "GRE-448 local peer",
            "type": "moonraker",
            "config": {"base_url": peer.origin}
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let integration_id = created["id"].as_str().unwrap();
    let (status, printers) = call(
        &origin,
        "PUT",
        "/api/v1/printers",
        &cookie,
        Some(json!({
            "printers": [{
                "id": "gre448-printer",
                "name": "GRE-448 printer",
                "integration_id": integration_id
            }]
        })),
    )
    .await;
    assert!(status.is_success(), "{status} {printers}");
    let (status, plan) = call(
        &origin,
        "POST",
        "/api/v1/plans",
        &cookie,
        Some(json!({"name": "GRE-448 interrupted upload"})),
    )
    .await;
    assert!(status.is_success(), "{status} {plan}");
    let profile_id = plan["id"].as_u64().expect("plan id");
    let (content_type, body) = multipart(
        &[
            ("printer_id", "gre448-printer"),
            ("profile_id", &profile_id.to_string()),
            ("start", "true"),
            ("wait_for_idle", "true"),
        ],
        &file,
    );
    let (status, queued_bytes) = post_bytes(
        &origin,
        "/api/v1/printer-send-queue",
        &cookie,
        &content_type,
        body,
    )
    .await;
    let queued = json_body(&queued_bytes);
    assert!(status.is_success(), "{status} {queued}");
    let item = &queued["item"];
    assert_eq!(item["state"], "queued");
    let item_id = item["id"].as_str().unwrap().to_owned();
    let artifact = PathBuf::from(item["artifact_path"].as_str().unwrap());
    let staged = fs::read(&artifact).unwrap();
    assert_eq!(staged.len(), UPLOAD_BYTES);
    assert_eq!(digest(&staged), file_sha256);
    let (status, drained) = call(
        &origin,
        "POST",
        "/api/v1/printer-send-queue/drain",
        &cookie,
        Some(json!({})),
    )
    .await;
    assert!(status.is_success(), "{status} {drained}");
    let results = drained["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{drained}");
    assert_eq!(results[0]["item_id"], item_id);
    let job_id = results[0]["job_id"].as_str().unwrap().to_owned();
    let upload = wait_for_upload(&peer).await;
    assert_eq!(upload.target, "/server/files/upload");
    assert!(upload.content_length > UPLOAD_PREFIX, "{upload:?}");
    assert_eq!(upload.bytes_read, UPLOAD_PREFIX, "{upload:?}");
    assert!(upload.saw_name && upload.saw_marker, "{upload:?}");
    let (status, running) = call(
        &origin,
        "GET",
        &format!("/api/v1/jobs/{job_id}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{running}");
    assert_eq!(running["status"], "running", "{running}");
    let sending = persisted_send(&data);
    assert_eq!(sending["state"], "sending");
    assert_eq!(sending["upload_job_id"], job_id);
    assert_eq!(peer.uploads().len(), 1);
    let interrupted_receipt = runtime.shutdown().await;
    assert_clean(&interrupted_receipt, second);
    let interrupted = persisted_send(&data);
    assert_eq!(
        interrupted["state"], "error",
        "EOF shutdown left the send in progress: {interrupted}"
    );
    assert_eq!(interrupted["error"], INTERRUPTED_UPLOAD);
    assert_eq!(interrupted["id"], item_id);
    assert_eq!(interrupted["upload_job_id"], job_id);
    assert_eq!(
        peer.uploads().len(),
        1,
        "shutdown resent the interrupted upload"
    );

    predecessors_are_dead(&pids);
    let mut runtime = CoreRuntime::start(launch(data.clone())).await.unwrap();
    let third = pid(&runtime);
    assert!(!pids.contains(&third));
    pids.push(third);
    let recovered = persisted_send(&data);
    assert_eq!(recovered, interrupted);
    let cookie = bootstrap(&mut runtime).await;
    let origin = runtime.origin().to_owned();
    let (status, queue) = call(&origin, "GET", "/api/v1/printer-send-queue", &cookie, None).await;
    assert_eq!(status, StatusCode::OK, "{queue}");
    assert_eq!(queue["items"].as_array().unwrap(), &vec![recovered.clone()]);
    let (status, missing) = call(
        &origin,
        "GET",
        &format!("/api/v1/jobs/{job_id}"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{missing}");
    let mut drains = Vec::new();
    for _ in 0..4 {
        let (status, drain) = call(
            &origin,
            "POST",
            "/api/v1/printer-send-queue/drain",
            &cookie,
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{drain}");
        drains.push(drain);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (status, jobs) = call(&origin, "GET", "/api/v1/jobs", &cookie, None).await;
    assert_eq!(status, StatusCode::OK, "{jobs}");
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 0, "{jobs}");
    assert_eq!(
        drains,
        vec![json!({"results": []}); 4],
        "restart drain admitted another upload"
    );
    assert_eq!(
        peer.uploads().len(),
        1,
        "interrupted upload was resent automatically"
    );
    let after_drains = persisted_send(&data);
    assert_eq!(after_drains, recovered);
    let retained = fs::read(&artifact).unwrap();
    assert_eq!(retained.len(), UPLOAD_BYTES);
    assert_eq!(digest(&retained), file_sha256);
    let restart_receipt = runtime.shutdown().await;
    assert_clean(&restart_receipt, third);
    predecessors_are_dead(&pids);
    assert_eq!(pids.len(), 3);
    assert_eq!(
        event_count(&data, "compat_starting"),
        3,
        "repeated start/stop spawned a duplicate child"
    );
    assert_eq!(event_count(&data, "compat_reaped"), 3);
    assert_eq!(event_count(&data, "compat_failure"), 0);
    println!(
        "{}",
        json!({
            "case": "repeated_start_stop_eof_upload",
            "pids": pids,
            "old_groups_absent": true,
            "spawn_count": 3,
            "uploads": 1,
            "job_id": job_id,
            "queue_state": "error",
            "artifact_sha256": file_sha256,
            "shutdown": [first_receipt, interrupted_receipt, restart_receipt],
        })
    );
}
