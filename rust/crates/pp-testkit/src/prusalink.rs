//! A loopback PrusaLink for send tests. It answers the Digest probe
//! `GET /api/v1/status` with 200 and no challenge, answers one upload target
//! with a set status, and keeps every request's head and body, so a test can
//! compare the bytes the printer received with the staged file. It runs on
//! its own OS thread and only ever answers.
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

/// The probe a PrusaLink driver sends before a request when it has a password.
pub const PROBE: &str = "/api/v1/status";

/// One request as the printer received it.
#[derive(Clone, Debug)]
pub struct Request {
    /// The request line and headers, up to and including the blank line.
    pub head: String,
    /// The body, `Content-Length` bytes long.
    pub body: Vec<u8>,
}

impl Request {
    pub fn line(&self) -> &str {
        self.head.lines().next().unwrap_or_default()
    }
}

pub struct UploadPrinter {
    url: String,
    seen: Arc<Mutex<Vec<Request>>>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl UploadPrinter {
    /// `upload` is the request target the upload answers at, with `status`
    /// and `reply`. Status 0 writes a malformed status line, which leaves the
    /// sender's outcome unknown. Any other target answers 404.
    pub fn start(upload: &str, status: u16, reply: &[u8]) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://localhost:{}", listener.local_addr()?.port());
        let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
        let stopped = Arc::new(AtomicBool::new(false));
        let (logged, stop) = (Arc::clone(&seen), Arc::clone(&stopped));
        let (upload, reply) = (upload.to_owned(), reply.to_vec());
        let thread = std::thread::spawn(move || {
            for socket in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut socket) = socket else { continue };
                let Some(request) = read_request(&mut socket) else {
                    continue;
                };
                let target = request.line().split(' ').nth(1).unwrap_or_default();
                let (status, body) = if target == PROBE {
                    (200, &b"{}"[..])
                } else if target == upload {
                    (status, &reply[..])
                } else {
                    (404, &b"{}"[..])
                };
                logged
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(request);
                let head = format!(
                    "HTTP/1.1 {status} Recorded\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes());
                let _ = socket.write_all(body);
            }
        });
        Ok(Self {
            url,
            seen,
            stopped,
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Every request so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Every request but a GET: the requests that send something. The probe
    /// and status-watcher reads are GETs.
    pub fn sends(&self) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|request| !request.line().starts_with("GET "))
            .collect()
    }

    /// The request line of every request so far, in order.
    pub fn request_lines(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|request| request.line().to_owned())
            .collect()
    }
}

impl Drop for UploadPrinter {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let port = self.url.rsplit(':').next().unwrap_or_default();
        let _ = TcpStream::connect(format!("127.0.0.1:{port}"));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One request: its head, then `Content-Length` bytes of body. `None` when
/// the client closed before the head was complete.
fn read_request(socket: &mut TcpStream) -> Option<Request> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    let end = loop {
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
        }
    };
    let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = bytes.split_off(end);
    while body.len() < length {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => body.extend_from_slice(&chunk[..read]),
        }
    }
    Some(Request { head, body })
}
