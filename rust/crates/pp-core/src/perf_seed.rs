//! Simulated Moonraker printers for T-53 captures (GRE-388).
//!
//! The capture launches `pp-server` once to seed its data directory with N
//! printers pointing at loopback simulators, then measures later launches of
//! the same command while the simulators keep answering. The simulators run as
//! threads of the harness, outside the measured process tree.
//!
//! Only this module knows `pp-server`'s login: the `--test-credential-file`
//! flag, the single-use bootstrap link and `POST /api/v1/integrations`.
//! `perf_launch::capture_run` stays a generic launcher.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::perf_launch::{CaptureError, with_ready_app};

/// The recorded `GET /printer/objects/query` body every simulator answers.
const STATUS_BODY: &str =
    include_str!("../../../../fixtures/drivers/moonraker/moonraker-success.json");
/// Names the seed gives its printers, so the read-back counts only them.
const NAME_PREFIX: &str = "perf-sim-";
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Loopback Moonraker simulators. Each listener is bound before `start`
/// returns, so the simulators are ready before any app is launched. Dropping
/// the value stops them.
pub struct SimulatedPrinters {
    addresses: Vec<SocketAddr>,
    requests: Arc<AtomicUsize>,
    stopping: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl SimulatedPrinters {
    pub fn start(count: usize) -> std::io::Result<Self> {
        let requests = Arc::new(AtomicUsize::new(0));
        let stopping = Arc::new(AtomicBool::new(false));
        let mut addresses = Vec::with_capacity(count);
        let mut threads = Vec::with_capacity(count);
        for _ in 0..count {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            addresses.push(listener.local_addr()?);
            let (requests, stopping) = (requests.clone(), stopping.clone());
            threads.push(std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        requests.fetch_add(1, Ordering::Relaxed);
                        let _ = answer(stream);
                    }
                }
            }));
        }
        Ok(Self {
            addresses,
            requests,
            stopping,
            threads,
        })
    }

    pub fn base_urls(&self) -> Vec<String> {
        self.addresses
            .iter()
            .map(|address| format!("http://{address}"))
            .collect()
    }

    /// Requests answered so far, across every simulator.
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::Relaxed)
    }
}

impl Drop for SimulatedPrinters {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        // Each accept loop checks the flag on its next connection.
        for address in &self.addresses {
            let _ = TcpStream::connect_timeout(address, Duration::from_secs(1));
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// Moonraker's status query gets the recorded body; anything else is 404.
fn answer(mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    let head = read_head(&mut stream)?;
    // Read any body before replying: closing with unread input resets the
    // connection, and the client would lose the reply.
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    std::io::copy(
        &mut (&mut stream).take(length.min(1 << 20)),
        &mut std::io::sink(),
    )?;
    let request_line = head.lines().next().unwrap_or_default();
    let mut parts = request_line.split(' ');
    let (method, path) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    let (status, body) = if method == "GET"
        && (path == "/printer/objects/query" || path.starts_with("/printer/objects/query?"))
    {
        ("200 OK", STATUS_BODY)
    } else {
        ("404 Not Found", r#"{"error":"not found"}"#)
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

/// Request line and headers, up to the blank line (at most 64 KiB).
fn read_head(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte)? == 0 || head.len() > 65_536 {
            break;
        }
        head.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

#[derive(Debug)]
pub(crate) struct HttpReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpReply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// One HTTP/1.1 request to a loopback `origin` such as `http://127.0.0.1:4100`,
/// with the Host and Origin headers the gateway checks. The connection closes
/// after the reply.
pub(crate) fn http(
    origin: &str,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    json: Option<&str>,
) -> Result<HttpReply, String> {
    let authority = origin
        .strip_prefix("http://")
        .ok_or_else(|| format!("{origin} is not an http:// origin"))?;
    let mut stream = TcpStream::connect(authority).map_err(|e| format!("{origin}: {e}"))?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {authority}\r\nOrigin: {origin}\r\nConnection: close\r\n"
    );
    if let Some(cookie) = cookie {
        request.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    let body = json.unwrap_or_default();
    if json.is_some() {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("{method} {path}: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("{method} {path}: {e}"))?;
    parse_reply(&raw).map_err(|e| format!("{method} {path}: {e}"))
}

pub(crate) fn parse_reply(raw: &[u8]) -> Result<HttpReply, String> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("reply has no header end")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or("reply has no status code")?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_owned(), value.trim().to_owned()))
        .collect();
    let mut reply = HttpReply {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    };
    if reply
        .header("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        reply.body = dechunk(&reply.body)?;
    }
    Ok(reply)
}

fn dechunk(mut raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    loop {
        let line_end = raw
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or("chunk size line is not terminated")?;
        let size_text = String::from_utf8_lossy(&raw[..line_end]);
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| format!("chunk size {size_text:?} is not hex"))?;
        raw = &raw[line_end + 2..];
        if size == 0 {
            return Ok(body);
        }
        if raw.len() < size + 2 {
            return Err("chunk is shorter than its size".into());
        }
        body.extend_from_slice(&raw[..size]);
        raw = &raw[size + 2..];
    }
}

/// Logs in through the single-use `bootstrap_url`, creates one Moonraker
/// printer per simulator, then reads the list back. Returns how many
/// `perf-sim-*` printers the app reports, which must equal the simulator
/// count. Any refused request fails the whole seed: the bootstrap link cannot
/// be reused, so there is no retry.
pub(crate) fn seed_session(
    origin: &str,
    bootstrap_url: &str,
    printers: &SimulatedPrinters,
) -> Result<usize, String> {
    let path = bootstrap_url
        .strip_prefix(origin)
        .ok_or_else(|| "bootstrap link is not on the app's origin".to_owned())?;
    let login = http(origin, "GET", path, None, None)?;
    let cookie = match (login.status, login.header("set-cookie")) {
        (303, Some(value)) => value.split(';').next().unwrap_or_default().to_owned(),
        (status, _) => return Err(format!("bootstrap login answered {status}")),
    };
    for (index, base_url) in printers.base_urls().iter().enumerate() {
        // A fixed fake key: nothing credential-shaped reaches the data dir.
        let body = serde_json::json!({
            "name": format!("{NAME_PREFIX}{}", index + 1),
            "type": "moonraker",
            "config": {"base_url": base_url, "api_key": "perf-simulator"},
        });
        let created = http(
            origin,
            "POST",
            "/api/v1/integrations",
            Some(&cookie),
            Some(&body.to_string()),
        )?;
        if created.status != 201 {
            return Err(format!(
                "creating printer {} answered {}: {}",
                index + 1,
                created.status,
                String::from_utf8_lossy(&created.body)
            ));
        }
    }
    let listed = http(origin, "GET", "/api/v1/integrations", Some(&cookie), None)?;
    if listed.status != 200 {
        return Err(format!("listing printers answered {}", listed.status));
    }
    let list: serde_json::Value =
        serde_json::from_slice(&listed.body).map_err(|e| format!("printer list: {e}"))?;
    let seeded = list["integrations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            item["type"] == "moonraker"
                && item["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with(NAME_PREFIX))
        })
        .count();
    if seeded != printers.addresses.len() {
        return Err(format!(
            "the app lists {seeded} simulated printers, expected {}",
            printers.addresses.len()
        ));
    }
    Ok(seeded)
}

/// One setup launch of `command` with `--test-credential-file`, which
/// `pp-server` uses to hand over its bootstrap link. Seeds one printer per
/// simulator into the data directory the command names, verifies the count
/// and stops the app before returning.
pub fn seed_printers(
    command: &[OsString],
    printers: &SimulatedPrinters,
    ready_timeout: Duration,
    stop_timeout: Duration,
) -> Result<usize, CaptureError> {
    let scratch = Scratch::new().map_err(|e| CaptureError::Seed(e.to_string()))?;
    let credential = scratch.0.join("credential.json");
    let mut setup = command.to_vec();
    setup.push("--test-credential-file".into());
    setup.push(credential.clone().into_os_string());
    with_ready_app(&setup, ready_timeout, stop_timeout, |app| {
        let receipt: serde_json::Value = std::fs::read(&credential)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(|| CaptureError::Seed("no credential file from the app".into()))?;
        let bootstrap = receipt["bootstrap_url"]
            .as_str()
            .ok_or_else(|| CaptureError::Seed("credential file has no bootstrap_url".into()))?;
        seed_session(app.ready_line.trim(), bootstrap, printers).map_err(CaptureError::Seed)
    })
}

/// A private scratch directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path =
            std::env::temp_dir().join(format!("pp-perf-seed-{}-{nanos}", std::process::id()));
        pp_os::create_private_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn ticket_t_53_simulators_answer_the_status_query_and_count_requests() {
        let printers = SimulatedPrinters::start(3).unwrap();
        let urls = printers.base_urls();
        assert_eq!(urls.len(), 3);
        for url in &urls {
            let reply = http(url, "GET", "/printer/objects/query?print_stats", None, None).unwrap();
            assert_eq!(reply.status, 200);
            assert_eq!(reply.body, STATUS_BODY.as_bytes());
        }
        let missing = http(&urls[0], "GET", "/machine/proc_stats", None, None).unwrap();
        assert_eq!(missing.status, 404);
        let posted = http(&urls[1], "POST", "/printer/objects/query", None, Some("{}")).unwrap();
        assert_eq!(posted.status, 404);
        assert_eq!(printers.requests(), 5);
        drop(printers);
        for url in &urls {
            assert!(http(url, "GET", "/printer/objects/query", None, None).is_err());
        }
    }

    /// What the fake gateway saw and what it refuses.
    #[derive(Default)]
    struct Gateway {
        created: Vec<serde_json::Value>,
        reject_login: bool,
        reject_create: Option<usize>,
        hide_listed: usize,
    }

    /// A fake `pp-server` gateway: the single-use bootstrap link, then the
    /// integration routes behind its cookie and same-origin check.
    fn fake_gateway(gateway: Gateway) -> (String, Arc<Mutex<Gateway>>) {
        use axum::{
            Router,
            extract::State,
            http::{HeaderMap, StatusCode, header},
            response::IntoResponse,
            routing::get,
        };
        type Shared = Arc<Mutex<Gateway>>;
        fn allowed(headers: &HeaderMap) -> bool {
            headers
                .get(header::COOKIE)
                .is_some_and(|c| c == "pp_session=s3cret")
                && headers
                    .get(header::ORIGIN)
                    .zip(headers.get(header::HOST))
                    .is_some_and(|(origin, host)| {
                        origin.to_str().unwrap() == format!("http://{}", host.to_str().unwrap())
                    })
        }
        async fn login(State(state): State<Shared>) -> axum::response::Response {
            if state.lock().unwrap().reject_login {
                return StatusCode::FORBIDDEN.into_response();
            }
            (
                StatusCode::SEE_OTHER,
                [(header::SET_COOKIE, "pp_session=s3cret; HttpOnly; Path=/")],
            )
                .into_response()
        }
        async fn create(
            State(state): State<Shared>,
            headers: HeaderMap,
            body: String,
        ) -> axum::response::Response {
            let mut state = state.lock().unwrap();
            if !allowed(&headers) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            if state.reject_create == Some(state.created.len() + 1) {
                return (StatusCode::BAD_REQUEST, "Invalid integration type").into_response();
            }
            state.created.push(serde_json::from_str(&body).unwrap());
            StatusCode::CREATED.into_response()
        }
        async fn list(State(state): State<Shared>, headers: HeaderMap) -> axum::response::Response {
            let state = state.lock().unwrap();
            if !allowed(&headers) {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            let mut items: Vec<_> = state.created[state.hide_listed..].to_vec();
            items.push(serde_json::json!({"name": "Shop printer", "type": "moonraker"}));
            axum::Json(serde_json::json!({ "integrations": items })).into_response()
        }
        let shared = Arc::new(Mutex::new(gateway));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let app = Router::new()
            .route("/__desktop/bootstrap", get(login))
            .route("/api/v1/integrations", get(list).post(create))
            .with_state(shared.clone());
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        (origin, shared)
    }

    #[test]
    fn ticket_t_53_seed_creates_one_printer_per_simulator_and_verifies_the_list() {
        let printers = SimulatedPrinters::start(3).unwrap();
        let (origin, gateway) = fake_gateway(Gateway::default());
        let bootstrap = format!("{origin}/__desktop/bootstrap?token=once");
        assert_eq!(seed_session(&origin, &bootstrap, &printers), Ok(3));
        let created = gateway.lock().unwrap().created.clone();
        let urls = printers.base_urls();
        for (index, item) in created.iter().enumerate() {
            assert_eq!(item["name"], format!("perf-sim-{}", index + 1));
            assert_eq!(item["type"], "moonraker");
            assert_eq!(item["config"]["base_url"], urls[index]);
            assert_eq!(item["config"]["api_key"], "perf-simulator");
        }
        assert_eq!(created.len(), 3);
    }

    #[test]
    fn ticket_t_53_seed_fails_loudly_instead_of_retrying() {
        let printers = SimulatedPrinters::start(3).unwrap();
        let seed = |gateway| {
            let (origin, _) = fake_gateway(gateway);
            seed_session(
                &origin,
                &format!("{origin}/__desktop/bootstrap?token=once"),
                &printers,
            )
        };
        assert_eq!(
            seed(Gateway {
                reject_login: true,
                ..Gateway::default()
            }),
            Err("bootstrap login answered 403".into())
        );
        assert_eq!(
            seed(Gateway {
                reject_create: Some(2),
                ..Gateway::default()
            }),
            Err("creating printer 2 answered 400: Invalid integration type".into())
        );
        assert_eq!(
            seed(Gateway {
                hide_listed: 1,
                ..Gateway::default()
            }),
            Err("the app lists 2 simulated printers, expected 3".into())
        );
        let (origin, _) = fake_gateway(Gateway::default());
        assert_eq!(
            seed_session(&origin, "http://elsewhere/__desktop/bootstrap", &printers),
            Err("bootstrap link is not on the app's origin".into())
        );
    }

    #[test]
    fn ticket_t_53_reply_parser_reads_length_and_chunked_bodies() {
        let plain = parse_reply(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\n{}").unwrap();
        assert_eq!((plain.status, plain.body.as_slice()), (201, &b"{}"[..]));
        let chunked = parse_reply(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nSet-Cookie: a=b; Path=/\r\n\r\n4\r\n{\"a\"\r\n3;x=y\r\n:1}\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!(chunked.body, b"{\"a\":1}");
        assert_eq!(chunked.header("SET-COOKIE"), Some("a=b; Path=/"));
        assert!(parse_reply(b"HTTP/1.1 200 OK\r\n").is_err());
        assert!(
            parse_reply(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err()
        );
    }
}
