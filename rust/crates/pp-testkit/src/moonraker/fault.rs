//! A loopback effect fixture with an explicit boundary between receiving a
//! complete request, accepting its effect and returning (or losing) its reply.
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_IO_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(10);

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
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct FaultLoop {
    listener: TcpListener,
    requests: Sender<Request>,
    responses: Receiver<Option<Reply>>,
    acknowledge: Sender<()>,
    effects: Arc<AtomicUsize>,
    calls: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
}

impl FaultLoop {
    fn run(self) {
        while !self.stopped.load(Ordering::SeqCst) {
            match self.listener.accept() {
                Ok((mut socket, _)) => {
                    if socket.set_nonblocking(false).is_err() {
                        continue;
                    }
                    let Some(request) = read_request(&mut socket, &self.stopped) else {
                        continue;
                    };
                    if self.stopped.load(Ordering::SeqCst) {
                        break;
                    }
                    self.calls.fetch_add(1, Ordering::SeqCst);
                    if self.requests.send(request).is_err() {
                        break;
                    }
                    if !self.gate_replies(&mut socket) {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL_INTERVAL);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    }

    fn gate_replies(&self, socket: &mut TcpStream) -> bool {
        let mut applied = false;
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return false;
            }
            let Ok(Some(reply)) = self.responses.recv() else {
                return false;
            };
            if self.stopped.load(Ordering::SeqCst) {
                return false;
            }
            if !applied {
                self.effects.fetch_add(1, Ordering::SeqCst);
                applied = true;
            }
            if self.acknowledge.send(()).is_err() {
                return false;
            }
            match reply {
                Reply::Accepted => {
                    let _ = socket.write_all(
                        b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    );
                    return true;
                }
                Reply::Lost => return true,
                Reply::Hold => {}
            }
        }
    }
}

impl FaultPrinter {
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (requests, request_rx) = mpsc::channel();
        let (replies, responses) = mpsc::channel();
        let (acknowledge, acknowledgements) = mpsc::channel();
        let accepted = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let server = FaultLoop {
            listener,
            requests,
            responses,
            acknowledge,
            effects: accepted.clone(),
            calls: received.clone(),
            stopped: stopped.clone(),
        };
        let thread = thread::spawn(move || server.run());
        Ok(Self {
            address,
            requests: request_rx,
            replies,
            acknowledgements,
            accepted,
            received,
            stopped,
            thread: Some(thread),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub fn request(&self) -> io::Result<Request> {
        self.requests
            .recv_timeout(WAIT_TIMEOUT)
            .map_err(io::Error::other)
    }

    pub fn accept(&self, reply: Reply) -> io::Result<()> {
        self.replies
            .send(Some(reply))
            .map_err(|_| io::Error::other("fake stopped"))?;
        self.acknowledgements
            .recv_timeout(WAIT_TIMEOUT)
            .map_err(io::Error::other)
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
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.replies.send(None);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read_request(socket: &mut TcpStream, stopped: &AtomicBool) -> Option<Request> {
    let deadline = Instant::now() + REQUEST_IO_TIMEOUT;
    socket.set_write_timeout(Some(REQUEST_IO_TIMEOUT)).ok()?;
    let mut bytes = Vec::new();
    let end = loop {
        if let Some(end) = bytes.windows(4).position(|value| value == b"\r\n\r\n") {
            break end + 4;
        }
        if bytes.len() > 16 * 1024 {
            return None;
        }
        read_more(socket, &mut bytes, deadline, stopped)?;
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
        read_more(socket, &mut bytes, deadline, stopped)?;
    }
    Some(Request {
        line,
        body: bytes[end..end + length].to_vec(),
    })
}

fn read_more(
    socket: &mut TcpStream,
    bytes: &mut Vec<u8>,
    deadline: Instant,
    stopped: &AtomicBool,
) -> Option<()> {
    let mut buffer = [0; 4096];
    loop {
        if stopped.load(Ordering::SeqCst) {
            return None;
        }
        let remaining = deadline.checked_duration_since(Instant::now())?;
        if remaining.is_zero() {
            return None;
        }
        let slice = remaining.min(POLL_INTERVAL).max(Duration::from_millis(1));
        socket.set_read_timeout(Some(slice)).ok()?;
        match socket.read(&mut buffer) {
            Ok(0) => return None,
            Ok(size) => {
                bytes.extend_from_slice(&buffer[..size]);
                return Some(());
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_idle_fault_printer_returns_promptly() {
        let printer = FaultPrinter::start().unwrap();
        assert_eq!(printer.received(), 0);
        assert_eq!(printer.accepted(), 0);
        let started = Instant::now();
        drop(printer);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn early_close_and_incomplete_request_do_not_stop_accept_loop() {
        let printer = FaultPrinter::start().unwrap();
        drop(TcpStream::connect(printer.address).unwrap());
        let mut partial = TcpStream::connect(printer.address).unwrap();
        partial.write_all(b"POST /incomplete HTTP/1.1\r\n").unwrap();
        drop(partial);

        let mut client = TcpStream::connect(printer.address).unwrap();
        client
            .write_all(b"POST /ok HTTP/1.1\r\nContent-Length: 2\r\n\r\nhi")
            .unwrap();
        let request = printer.request().unwrap();
        assert_eq!(request.line, "POST /ok HTTP/1.1");
        assert_eq!(request.body, b"hi");
        assert_eq!(printer.received(), 1);
        assert_eq!(printer.accepted(), 0);

        printer.accept(Reply::Accepted).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 201 Created\r\n"));
        assert_eq!(printer.accepted(), 1);
    }

    #[test]
    fn dropping_fault_printer_interrupts_stalled_read_and_held_reply() {
        let printer = FaultPrinter::start().unwrap();
        let mut stalled = TcpStream::connect(printer.address).unwrap();
        stalled.write_all(b"POST /stalled HTTP/1.1\r\n").unwrap();
        let started = Instant::now();
        drop(printer);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(stalled);

        let printer = FaultPrinter::start().unwrap();
        let mut held = TcpStream::connect(printer.address).unwrap();
        held.write_all(b"POST /held HTTP/1.1\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        assert_eq!(printer.request().unwrap().line, "POST /held HTTP/1.1");
        printer.accept(Reply::Hold).unwrap();
        let started = Instant::now();
        drop(printer);
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(held);
    }
}
