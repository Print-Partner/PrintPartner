//! The status file `pp-server --status-file` keeps, and the `pp-server
//! healthcheck` command that reads it.
//!
//! The check is local, bounded and effect-free. It reads one small file, checks
//! that the recorded process is alive, and, once the server is ready, sends one
//! `GET /health` without a session to the recorded local origin. The gateway
//! refuses that request with 401 before admitting it, so the probe creates no
//! session, job or printer contact. It only proves the gateway answered.

use anyhow::{Context, Result, bail, ensure};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Largest status file the check reads.
const STATUS_LIMIT: u64 = 4096;
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(2000);
const MAX_TIMEOUT_MS: u64 = 10_000;

pub(crate) const USAGE: &str = "\
Usage: pp-server healthcheck <liveness|readiness> --status-file PATH [--timeout-ms N]

Reads the status file a running `pp-server --status-file PATH` keeps.
  liveness   the server process is alive; once ready, its gateway also answers
  readiness  the server finished startup (storage open, migrations applied)
             and its gateway answers; false while starting or stopping
--timeout-ms bounds the HTTP probe (1 to 10000, default 2000).
Exits 0 when the probe passes and 1 when it fails. Sends no credentials and
changes nothing.";

/// What the server records in the status file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// Storage is opening: lease, ownership marker, schema migration, release check.
    Starting,
    /// The gateway is serving.
    Ready,
    /// A stop signal or logout arrived; the runtime is draining.
    Stopping,
}

impl State {
    fn word(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Stopping => "stopping",
        }
    }
}

/// The server side: writes the status file atomically and removes it on drop.
/// A file another live process recorded is never overwritten or removed.
pub(crate) struct StatusFile {
    path: PathBuf,
}

impl StatusFile {
    pub(crate) fn create(path: PathBuf) -> Result<Self> {
        if let Ok(recorded) = read_status(&path) {
            ensure!(
                recorded.pid == std::process::id() || !process_alive(recorded.pid),
                "Status file belongs to a running server"
            );
        }
        let file = Self { path };
        file.publish(State::Starting, None)?;
        Ok(file)
    }

    pub(crate) fn publish(&self, state: State, origin: Option<&str>) -> Result<()> {
        let mut body = serde_json::json!({"pid": std::process::id(), "state": state.word()});
        if let Some(origin) = origin {
            body["origin"] = origin.into();
        }
        let name = self
            .path
            .file_name()
            .context("Status file needs a file name")?;
        let mut temporary = name.to_owned();
        temporary.push(format!(".{}.tmp", std::process::id()));
        let temporary = self.path.with_file_name(temporary);
        // A leftover or planted entry, including a symlink, is removed, and the
        // new file is created exclusively, so the write never follows a link.
        let _ = std::fs::remove_file(&temporary);
        let mut options = std::fs::File::options();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o644);
        let written = options
            .open(&temporary)
            .and_then(|mut handle| handle.write_all(body.to_string().as_bytes()));
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temporary);
            return Err(error).context("Status file write failed");
        }
        std::fs::rename(&temporary, &self.path).context("Status file write failed")
    }
}

impl Drop for StatusFile {
    fn drop(&mut self) {
        if read_status(&self.path).is_ok_and(|recorded| recorded.pid == std::process::id()) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Probe {
    Liveness,
    Readiness,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Command {
    Help,
    Check {
        probe: Probe,
        status_file: PathBuf,
        timeout: Duration,
    },
}

pub(crate) fn parse(mut args: impl Iterator<Item = String>) -> Result<Command> {
    let mut probe = None;
    let mut status_file = None;
    let mut timeout = DEFAULT_TIMEOUT;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => return Ok(Command::Help),
            "liveness" if probe.is_none() => probe = Some(Probe::Liveness),
            "readiness" if probe.is_none() => probe = Some(Probe::Readiness),
            "--status-file" => {
                status_file = Some(PathBuf::from(
                    args.next().context("Argument value missing")?,
                ));
            }
            "--timeout-ms" => {
                let value: u64 = args
                    .next()
                    .context("Argument value missing")?
                    .parse()
                    .context("Invalid --timeout-ms")?;
                ensure!(
                    (1..=MAX_TIMEOUT_MS).contains(&value),
                    "Invalid --timeout-ms"
                );
                timeout = Duration::from_millis(value);
            }
            _ => bail!("Unknown argument"),
        }
    }
    Ok(Command::Check {
        probe: probe.context("Name a probe: liveness or readiness")?,
        status_file: status_file.context("--status-file is required")?,
        timeout,
    })
}

#[derive(Debug)]
struct Recorded {
    pid: u32,
    state: String,
    origin: Option<String>,
}

fn read_status(path: &Path) -> Result<Recorded> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .context("Status file missing")?
        .take(STATUS_LIMIT + 1)
        .read_to_end(&mut bytes)
        .context("Status file unreadable")?;
    ensure!(bytes.len() as u64 <= STATUS_LIMIT, "Status file too large");
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).context("Status file malformed")?;
    let pid = value["pid"]
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .context("Status file malformed")?;
    let state = value["state"]
        .as_str()
        .context("Status file malformed")?
        .to_owned();
    let origin = value["origin"].as_str().map(str::to_owned);
    Ok(Recorded { pid, state, origin })
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the pid exists; nothing is delivered.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Windows has no signal-0 check here; the HTTP probe still runs once ready.
#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

/// One `GET /health` with no cookie to a local origin. Passes on any HTTP
/// status below 500: the gateway answers 401 before admitting the request.
fn probe_gateway(origin: &str, timeout: Duration) -> Result<()> {
    let authority = origin
        .strip_prefix("http://")
        .filter(|rest| !rest.contains('/'))
        .context("Recorded origin is not an http origin")?;
    let address: SocketAddr = authority
        .parse()
        .context("Recorded origin is not an address")?;
    let ip = address.ip().to_canonical();
    ensure!(!ip.is_unspecified(), "Recorded origin is unspecified");
    ensure!(
        ip.is_loopback() || std::net::UdpSocket::bind((address.ip(), 0)).is_ok(),
        "Recorded origin is not loopback or a local interface"
    );
    let deadline = Instant::now() + timeout;
    let remaining = || {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .context("Gateway did not answer in time")
    };
    let mut stream =
        TcpStream::connect_timeout(&address, remaining()?).context("Gateway unreachable")?;
    stream.set_write_timeout(Some(remaining()?))?;
    stream
        .write_all(
            format!("GET /health HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .context("Gateway unreachable")?;
    let mut head = Vec::with_capacity(32);
    let mut buffer = [0u8; 32];
    while head.len() < 12 {
        stream.set_read_timeout(Some(remaining()?))?;
        let count = stream
            .read(&mut buffer)
            .context("Gateway did not answer in time")?;
        ensure!(count > 0, "Gateway closed the connection");
        head.extend_from_slice(&buffer[..count]);
    }
    let status: u16 = head
        .strip_prefix(b"HTTP/1.1 ")
        .and_then(|rest| std::str::from_utf8(&rest[..3]).ok())
        .and_then(|code| code.parse().ok())
        .context("Gateway answer is not HTTP")?;
    ensure!(status < 500, "Gateway answered {status}");
    Ok(())
}

/// Runs one probe. `Ok` carries the line to print on success.
pub(crate) fn check(probe: Probe, status_file: &Path, timeout: Duration) -> Result<String> {
    let recorded = read_status(status_file)?;
    ensure!(process_alive(recorded.pid), "Server process is not running");
    let state = recorded.state.as_str();
    let probe_ready = || {
        probe_gateway(
            recorded
                .origin
                .as_deref()
                .context("Status file malformed")?,
            timeout,
        )
    };
    match (probe, state) {
        (Probe::Liveness, "starting" | "stopping") => {}
        (_, "ready") => probe_ready()?,
        (Probe::Readiness, "starting" | "stopping") => bail!("Server is {state}"),
        _ => bail!("Status file malformed"),
    }
    Ok(format!(
        "{} ok ({state})",
        match probe {
            Probe::Liveness => "liveness",
            Probe::Readiness => "readiness",
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Above `pid_t::MAX`, so `process_alive` reports it dead.
    #[cfg(unix)]
    const DEAD_PID: u32 = 4_000_000_000;

    fn parse_args(args: &[&str]) -> Result<Command> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "pp-health-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn record(path: &Path, body: serde_json::Value) {
        std::fs::write(path, body.to_string()).unwrap();
    }

    /// A loopback listener that answers each connection with `reply`, or never
    /// answers when `reply` is `None`.
    fn gateway(reply: Option<&'static str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut request = [0u8; 256];
                let _ = stream.read(&mut request);
                match reply {
                    Some(reply) => {
                        let _ = stream.write_all(reply.as_bytes());
                    }
                    None => held.push(stream),
                }
            }
        });
        origin
    }

    #[test]
    fn parses_probes_and_options() {
        assert_eq!(parse_args(&["--help"]).unwrap(), Command::Help);
        assert_eq!(
            parse_args(&[
                "readiness",
                "--status-file",
                "s.json",
                "--timeout-ms",
                "500"
            ])
            .unwrap(),
            Command::Check {
                probe: Probe::Readiness,
                status_file: "s.json".into(),
                timeout: Duration::from_millis(500),
            }
        );
        assert_eq!(
            parse_args(&["--status-file", "s.json", "liveness"]).unwrap(),
            Command::Check {
                probe: Probe::Liveness,
                status_file: "s.json".into(),
                timeout: DEFAULT_TIMEOUT,
            }
        );
        let error = |args: &[&str]| parse_args(args).unwrap_err().to_string();
        assert_eq!(
            error(&["--status-file", "s"]),
            "Name a probe: liveness or readiness"
        );
        assert_eq!(error(&["liveness"]), "--status-file is required");
        assert_eq!(error(&["liveness", "readiness"]), "Unknown argument");
        assert_eq!(
            error(&["liveness", "--timeout-ms", "0"]),
            "Invalid --timeout-ms"
        );
        assert_eq!(
            error(&["liveness", "--timeout-ms", "10001"]),
            "Invalid --timeout-ms"
        );
        assert_eq!(
            error(&["liveness", "--status-file"]),
            "Argument value missing"
        );
    }

    #[test]
    fn starting_is_live_but_not_ready() {
        let root = scratch();
        let path = root.join("status.json");
        let status = StatusFile::create(path.clone()).unwrap();
        let timeout = Duration::from_millis(500);
        assert_eq!(
            check(Probe::Liveness, &path, timeout).unwrap(),
            "liveness ok (starting)"
        );
        assert_eq!(
            check(Probe::Readiness, &path, timeout)
                .unwrap_err()
                .to_string(),
            "Server is starting"
        );
        status
            .publish(
                State::Ready,
                Some(&gateway(Some("HTTP/1.1 401 Unauthorized\r\n\r\n"))),
            )
            .unwrap();
        assert_eq!(
            check(Probe::Readiness, &path, timeout).unwrap(),
            "readiness ok (ready)"
        );
        assert_eq!(
            check(Probe::Liveness, &path, timeout).unwrap(),
            "liveness ok (ready)"
        );
        status.publish(State::Stopping, None).unwrap();
        assert!(check(Probe::Liveness, &path, timeout).is_ok());
        assert_eq!(
            check(Probe::Readiness, &path, timeout)
                .unwrap_err()
                .to_string(),
            "Server is stopping"
        );
        drop(status);
        assert!(!path.exists(), "the status file leaves with the server");
        assert_eq!(
            check(Probe::Liveness, &path, timeout)
                .unwrap_err()
                .to_string(),
            "Status file missing"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_live_servers_status_file_is_left_alone() {
        let root = scratch();
        let path = root.join("status.json");
        // pid 1 is always alive and is never this test.
        let foreign =
            serde_json::json!({"pid": 1, "state": "ready", "origin": "http://127.0.0.1:1"});
        record(&path, foreign.clone());
        let error = StatusFile::create(path.clone()).err().unwrap().to_string();
        assert_eq!(error, "Status file belongs to a running server");
        let kept: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(kept, foreign);
        // A pid past every platform's limit is never alive.
        record(
            &path,
            serde_json::json!({"pid": DEAD_PID, "state": "ready"}),
        );
        // A dead process's file is replaced, and a planted temporary symlink
        // is removed rather than followed.
        let target = root.join("target.txt");
        std::fs::write(&target, "keep").unwrap();
        let temporary = root.join(format!("status.json.{}.tmp", std::process::id()));
        std::os::unix::fs::symlink(&target, &temporary).unwrap();
        let status = StatusFile::create(path.clone()).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        assert!(!temporary.exists());
        // Another process's file is not removed on drop.
        record(&path, foreign);
        drop(status);
        assert!(path.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn gateway_failures_fail_both_probes_within_the_timeout() {
        let root = scratch();
        let path = root.join("status.json");
        let pid = std::process::id();
        let timeout = Duration::from_millis(300);
        for (origin, message) in [
            (
                gateway(Some("HTTP/1.1 503 Service Unavailable\r\n\r\n")),
                "Gateway answered 503",
            ),
            (gateway(None), "Gateway did not answer in time"),
            (
                "http://0.0.0.0:80".to_owned(),
                "Recorded origin is unspecified",
            ),
            (
                "http://[::]:80".to_owned(),
                "Recorded origin is unspecified",
            ),
            (
                "http://[::ffff:0.0.0.0]:80".to_owned(),
                "Recorded origin is unspecified",
            ),
            (
                "http://192.0.2.1:80".to_owned(),
                "Recorded origin is not loopback or a local interface",
            ),
        ] {
            record(
                &path,
                serde_json::json!({"pid": pid, "state": "ready", "origin": origin}),
            );
            let started = Instant::now();
            for probe in [Probe::Liveness, Probe::Readiness] {
                let error = check(probe, &path, timeout).unwrap_err().to_string();
                assert_eq!(error, message);
            }
            assert!(started.elapsed() < Duration::from_secs(2), "bounded probe");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_or_stale_status_fails() {
        let root = scratch();
        let path = root.join("status.json");
        let timeout = Duration::from_millis(300);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(
            check(Probe::Liveness, &path, timeout)
                .unwrap_err()
                .to_string(),
            "Status file malformed"
        );
        std::fs::write(&path, vec![b' '; STATUS_LIMIT as usize + 1]).unwrap();
        assert_eq!(
            check(Probe::Liveness, &path, timeout)
                .unwrap_err()
                .to_string(),
            "Status file too large"
        );
        record(
            &path,
            serde_json::json!({"pid": std::process::id(), "state": "unknown"}),
        );
        assert_eq!(
            check(Probe::Readiness, &path, timeout)
                .unwrap_err()
                .to_string(),
            "Status file malformed"
        );
        #[cfg(unix)]
        {
            record(
                &path,
                serde_json::json!({"pid": DEAD_PID, "state": "starting"}),
            );
            assert_eq!(
                check(Probe::Liveness, &path, timeout)
                    .unwrap_err()
                    .to_string(),
                "Server process is not running"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
