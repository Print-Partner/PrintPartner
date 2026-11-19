//! A loopback PrusaLink for send tests. It answers the Digest probe
//! `GET /api/v1/status` with 200 and no challenge, answers one upload target
//! with a set status, and keeps every request's head and body, so a test can
//! compare the bytes the printer received with the staged file. A request
//! whose body is shorter than its `Content-Length` is recorded as incomplete
//! and gets no answer. It runs on its own OS thread and only ever answers.
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

/// How long one read on an accepted connection may block, so a client that
/// stops writing mid-request cannot hold the server thread or its `Drop`.
const READ_DEADLINE: Duration = Duration::from_secs(5);

/// The probe a PrusaLink driver sends before a request when it has a password.
pub const PROBE: &str = "/api/v1/status";

/// One request as the printer received it.
#[derive(Clone, Debug)]
pub struct Request {
    /// The request line and headers, up to and including the blank line.
    pub head: String,
    /// The body bytes that arrived, never more than `Content-Length`.
    pub body: Vec<u8>,
    /// Whether every body byte the `Content-Length` declared arrived. A
    /// request without one has no body, so it is complete.
    pub complete: bool,
}

impl Request {
    pub fn line(&self) -> &str {
        self.head.lines().next().unwrap_or_default()
    }

    /// The trimmed value of the first header named `name`, ignoring case.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.head
            .lines()
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.trim().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
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
                let complete = request.complete;
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
                if !complete {
                    continue;
                }
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

/// One request: its head, then up to `Content-Length` bytes of body. `None`
/// when the client closed or stalled before the head was complete.
fn read_request(socket: &mut TcpStream) -> Option<Request> {
    socket.set_read_timeout(Some(READ_DEADLINE)).ok()?;
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
    let mut body = bytes.split_off(end);
    let mut request = Request {
        head,
        body: Vec::new(),
        complete: false,
    };
    let wanted = request
        .header("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while body.len() < wanted {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => body.extend_from_slice(&chunk[..read]),
        }
    }
    request.complete = body.len() >= wanted;
    body.truncate(wanted);
    request.body = body;
    Some(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Writes `request`, closes the write side, and returns the whole answer.
    fn exchange(printer: &UploadPrinter, request: &[u8]) -> Vec<u8> {
        let port = printer.url().rsplit(':').next().unwrap();
        let mut socket = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        socket.write_all(request).unwrap();
        socket.shutdown(std::net::Shutdown::Write).unwrap();
        let mut answer = Vec::new();
        let _ = socket.read_to_end(&mut answer);
        answer
    }

    #[test]
    fn a_whole_put_is_answered_and_recorded_with_its_exact_headers() {
        let printer = UploadPrinter::start("/u", 201, b"{}").unwrap();
        let answer = exchange(
            &printer,
            b"PUT /u HTTP/1.1\r\nPrint-After-Upload: ?10\r\nContent-Length: 3\r\n\r\nabcEXTRA",
        );
        assert!(answer.starts_with(b"HTTP/1.1 201 "));
        let [put] = &printer.requests()[..] else {
            panic!("one request")
        };
        assert!(put.complete);
        assert_eq!(put.body, b"abc", "never more than Content-Length");
        assert_eq!(put.header("print-after-upload"), Some("?10"));
        assert_ne!(put.header("print-after-upload"), Some("?1"));
        assert_eq!(put.header("missing"), None);
    }

    #[test]
    fn a_short_body_is_recorded_incomplete_and_gets_no_answer() {
        let printer = UploadPrinter::start("/u", 201, b"{}").unwrap();
        let answer = exchange(
            &printer,
            b"PUT /u HTTP/1.1\r\nContent-Length: 12\r\n\r\nG28\nG1 X10\n",
        );
        assert!(answer.is_empty(), "{}", String::from_utf8_lossy(&answer));
        let [put] = &printer.requests()[..] else {
            panic!("one request")
        };
        assert!(!put.complete);
        assert_eq!(put.body, b"G28\nG1 X10\n");
    }

    #[test]
    fn a_stalled_client_cannot_hold_drop_past_the_read_deadline() {
        let printer = UploadPrinter::start("/u", 201, b"{}").unwrap();
        let port = printer.url().rsplit(':').next().unwrap().to_owned();
        let mut stalled = TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stalled
            .write_all(b"PUT /u HTTP/1.1\r\nContent-Length: 100\r\n\r\npartial")
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        drop(printer);
        assert!(
            started.elapsed() < READ_DEADLINE + Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        drop(stalled);
    }
}
