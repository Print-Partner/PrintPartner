//! A loopback effect fixture with an explicit boundary between receiving a
//! complete request, accepting its effect and returning (or losing) its reply.
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::JoinHandle,
    time::Duration,
};

#[derive(Debug)]
pub struct Request {
    pub line: String,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy)]
pub enum Reply {
    Accepted,
    Lost,
    /// Apply the fake effect but wait for another instruction before replying.
    Hold,
}

/// Never contacts a printer. Each received request waits for `accept`, allowing
/// the test to inspect committed storage before releasing a fake remote effect.
pub struct FaultPrinter {
    address: SocketAddr,
    requests: Receiver<Request>,
    replies: Sender<Option<Reply>>,
    acknowledgements: Receiver<()>,
    accepted: Arc<AtomicUsize>,
    received: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

impl FaultPrinter {
    pub fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let (requests, request_rx) = mpsc::channel();
        let (replies, responses) = mpsc::channel();
        let (acknowledge, acknowledgements) = mpsc::channel();
        let accepted = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(AtomicUsize::new(0));
        let effects = accepted.clone();
        let calls = received.clone();
        let thread = std::thread::spawn(move || {
            for socket in listener.incoming() {
                let Ok(mut socket) = socket else { break };
                let Some(request) = read_request(&mut socket) else {
                    break;
                };
                calls.fetch_add(1, Ordering::SeqCst);
                if requests.send(request).is_err() {
                    break;
                }
                let mut applied = false;
                loop {
                    let Ok(Some(reply)) = responses.recv_timeout(Duration::from_secs(5)) else {
                        return;
                    };
                    if !applied {
                        effects.fetch_add(1, Ordering::SeqCst);
                        applied = true;
                    }
                    if acknowledge.send(()).is_err() {
                        return;
                    }
                    match reply {
                        Reply::Accepted => {
                            let _ = socket.write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
                            break;
                        }
                        Reply::Lost => break,
                        Reply::Hold => {}
                    }
                }
            }
        });
        Ok(Self {
            address,
            requests: request_rx,
            replies,
            acknowledgements,
            accepted,
            received,
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub fn request(&self) -> Request {
        self.requests
            .recv_timeout(Duration::from_secs(5))
            .expect("complete fake request")
    }

    pub fn accept(&self, reply: Reply) {
        self.replies.send(Some(reply)).expect("fake still running");
        self.acknowledgements
            .recv_timeout(Duration::from_secs(5))
            .expect("fake effect accepted");
    }

    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    pub fn received(&self) -> usize {
        self.received.load(Ordering::SeqCst)
    }
}

impl Drop for FaultPrinter {
    fn drop(&mut self) {
        let _ = self.replies.send(None);
        // Wake accept; EOF ends read_request without recording another effect.
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("fake thread completed");
        }
    }
}

fn read_request(socket: &mut TcpStream) -> Option<Request> {
    socket.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    let mut bytes = Vec::new();
    let end = loop {
        if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            break end + 4;
        }
        if bytes.len() > 16 * 1024 {
            return None;
        }
        let mut buffer = [0; 4096];
        let size = socket.read(&mut buffer).ok()?;
        if size == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..size]);
    };
    let head = std::str::from_utf8(&bytes[..end]).ok()?;
    let line = head.lines().next()?.to_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    if length > 1024 * 1024 {
        return None;
    }
    while bytes.len() < end + length {
        let mut buffer = [0; 4096];
        let size = socket.read(&mut buffer).ok()?;
        if size == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..size]);
    }
    Some(Request {
        line,
        body: bytes[end..end + length].to_vec(),
    })
}
