//! A loopback Moonraker that replays recorded responses, for status-watcher
//! tests in several crates. It runs on its own OS thread, so it needs no async
//! runtime, and it only ever answers: it never initiates a connection.
pub mod fault;

use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

/// The request line of one HTTP request head, read up to its blank line.
/// `None` when the client closed or failed before the head was complete: a
/// client cancelled after it connected but before it wrote has sent no
/// request, so the printers neither record nor answer it.
fn request_line(socket: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut bytes = [0; 4096];
    while !head.windows(4).any(|end| end == b"\r\n\r\n") {
        match socket.read(&mut bytes) {
            Ok(0) | Err(_) => return None,
            Ok(len) => head.extend_from_slice(&bytes[..len]),
        }
    }
    String::from_utf8_lossy(&head)
        .lines()
        .next()
        .map(str::to_owned)
}

/// Replays `replies` in request order and repeats the last one. A 3xx reply
/// carries `Location: /moonraker/`.
pub struct RecordedPrinter {
    url: String,
    calls: Arc<AtomicUsize>,
    lines: Arc<Mutex<Vec<String>>>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RecordedPrinter {
    pub fn start(replies: Vec<(u16, &'static [u8])>) -> std::io::Result<Self> {
        assert!(!replies.is_empty(), "a recorded printer needs a reply");
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://{}", listener.local_addr()?);
        let calls = Arc::new(AtomicUsize::new(0));
        let lines = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let (seen, logged, stop) = (calls.clone(), lines.clone(), stopped.clone());
        let thread = std::thread::spawn(move || {
            for socket in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut socket) = socket else { continue };
                let Some(line) = request_line(&mut socket) else {
                    continue;
                };
                logged.lock().unwrap_or_else(|e| e.into_inner()).push(line);
                let call = seen.fetch_add(1, Ordering::SeqCst);
                let (status, body) = replies[call.min(replies.len() - 1)];
                let head = format!(
                    "HTTP/1.1 {status} Recorded\r\nLocation: /moonraker/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(head.as_bytes());
                let _ = socket.write_all(body);
            }
        });
        Ok(Self {
            url,
            calls,
            lines,
            stopped,
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Requests answered so far.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// The request line of every request so far, in order.
    pub fn request_lines(&self) -> Vec<String> {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for RecordedPrinter {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        // Wake the blocking accept so the thread sees the stop flag.
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A loopback Moonraker that answers by request target (path and query):
/// each listed target gets its recorded status and body, anything else 404.
/// `{ORIGIN}` in a body becomes this printer's origin. Like
/// [`RecordedPrinter`] it only answers and never initiates a connection.
pub struct RoutedPrinter {
    url: String,
    lines: Arc<Mutex<Vec<String>>>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RoutedPrinter {
    /// `routes` maps a request target to its status and body.
    pub fn start(routes: Vec<(String, u16, Vec<u8>)>) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let url = format!("http://localhost:{}", listener.local_addr()?.port());
        let routes: Vec<_> = routes
            .into_iter()
            .map(|(target, status, body)| {
                let body = String::from_utf8_lossy(&body).replace("{ORIGIN}", &url);
                (target, status, body.into_bytes())
            })
            .collect();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let (logged, stop) = (lines.clone(), stopped.clone());
        let thread = std::thread::spawn(move || {
            for socket in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut socket) = socket else { continue };
                let Some(line) = request_line(&mut socket) else {
                    continue;
                };
                let target = line.split(' ').nth(1).unwrap_or_default();
                let (status, body) = routes
                    .iter()
                    .find(|(listed, _, _)| listed == target)
                    .map_or((404, &b"{}"[..]), |(_, status, body)| (*status, &body[..]));
                logged.lock().unwrap_or_else(|e| e.into_inner()).push(line);
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
            lines,
            stopped,
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The request line of every request so far, in order.
    pub fn request_lines(&self) -> Vec<String> {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl Drop for RoutedPrinter {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let port = self.url.rsplit(':').next().unwrap_or_default();
        let _ = TcpStream::connect(format!("127.0.0.1:{port}"));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
