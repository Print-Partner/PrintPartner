//! Bambu Lab LAN driver, status only (GRE-362). It reads a printer's status
//! the way Node's `integrations/adapters/bambu.ts` does: MQTT over TLS to
//! port 8883 as user `bblp` with the LAN access code, subscribe to
//! `device/<serial>/report`, publish one pushall, and map the first report
//! that carries `print.gcode_state`. It never uploads or controls a print:
//! [`mqtt`] can build no PUBLISH other than pushall.
//!
//! Unlike Node, which skips TLS verification for private addresses, every
//! connection is pinned. The host goes through `HttpGuard::connect` (checked
//! DNS, permitted addresses only, no proxy). The TLS verifier then accepts
//! exactly one end-entity certificate: the one whose SHA-256 is the stored pin,
//! with handshake signatures still verified against its public key (v1
//! certificates included). [`Trust::FirstUse`] is for an
//! explicit user action only; it accepts the presented certificate and reports
//! its fingerprint so the caller can store it. A changed certificate fails the
//! TLS handshake, so MQTT CONNECT and the access code never reach that peer.
mod mqtt;

use crate::moonraker_driver::js_trim;
use pp_api_types::printer_status::{PrinterHostState as State, PrinterHostStatus as Status};
use pp_net::outbound::HttpGuard;
use rustls::{
    CertificateError, DigitallySignedStruct, PeerMisbehaved, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, SubjectPublicKeyInfoDer, UnixTime},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncWriteExt, BufStream};

pub const DEFAULT_PORT: u16 = 8883;
const USERNAME: &str = "bblp";
/// Node's `CONNECT_TIMEOUT_MS`: TCP, TLS and CONNACK.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// Node's `STATUS_TIMEOUT_MS`, from the start of the read to the first report.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const KEEP_ALIVE_SECONDS: u16 = 30;
/// Stored values longer than this are not read.
const MAX_FIELD_BYTES: usize = 4096;

pub const TIMED_OUT: &str =
    "Timed out waiting for Bambu MQTT status (check LAN mode, IP, serial, access code)";
pub const CLOSED: &str = "Bambu MQTT connection closed (LAN mode off or connection refused?)";
pub const CERTIFICATE_CHANGED: &str = "Printer certificate changed. If you replaced or reset the printer, reset its certificate in Settings.";
const CONNECTION_ERROR: &str = "Bambu MQTT connection error";
const TLS_FAILED: &str = "Bambu MQTT TLS handshake failed";
const SUBSCRIBE_FAILED: &str = "Failed to subscribe to Bambu report topic";

/// SHA-256 of a certificate's DER encoding.
pub type Fingerprint = [u8; 32];

/// Which certificate a read accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Only an explicit user action (Settings → Test connection) may use this:
    /// it accepts whatever certificate the printer presents.
    FirstUse,
    /// Only the certificate with this fingerprint.
    Pinned(Fingerprint),
}

/// A status read and the fingerprint of the certificate the printer presented,
/// when the TLS handshake got that far.
#[derive(Debug)]
pub struct Read {
    pub status: Status,
    pub certificate: Option<Fingerprint>,
}

/// The printer settings Node's `resolveConnection` reads, before DNS.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub serial: String,
    access_code: String,
}

/// Node's `asNonEmptyString`.
fn nonblank(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(js_trim)
        .filter(|value| !value.is_empty())
}

fn text<'a>(config: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| nonblank(config.get(*name)).filter(|value| value.len() <= MAX_FIELD_BYTES))
}

impl Config {
    /// Reads an integration's `config` object as Node does. The error is
    /// Node's message for the first missing or invalid field.
    pub fn from_stored(config: &Value) -> Result<Self, String> {
        let host =
            text(config, &["host", "hostname", "ip"]).ok_or("host (printer LAN IP) is required")?;
        let access_code = text(config, &["access_code", "accessCode"])
            .filter(|code| *code != "****")
            .ok_or("access_code is required")?;
        let serial = text(config, &["serial", "device_id", "deviceId"])
            .ok_or("serial (device id) is required")?;
        let valid = (6..=64).contains(&serial.len())
            && serial
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
        if !valid {
            return Err(
                "serial must be 6–64 letters/digits (no MQTT wildcards or path characters)".into(),
            );
        }
        Ok(Self {
            host: host.to_owned(),
            port: port(config.get("port")),
            serial: serial.to_owned(),
            access_code: access_code.to_owned(),
        })
    }
}

/// Node's `readPort`: a finite number or numeric string in 1..65536, floored.
fn port(raw: Option<&Value>) -> u16 {
    let number = match raw {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(text)) if !js_trim(text).is_empty() => js_number(text),
        _ => None,
    };
    number
        .filter(|number| number.is_finite() && *number > 0.0 && *number < 65536.0)
        .map_or(DEFAULT_PORT, |number| number.floor() as u16)
}

/// JavaScript's `Number(text)`: blank is 0; decimal, `Infinity`, and
/// unsigned `0x`/`0o`/`0b` literals are numbers; anything else is `None` (NaN).
fn js_number(text: &str) -> Option<f64> {
    let text = js_trim(text);
    if text.is_empty() {
        return Some(0.0);
    }
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        let digits = text
            .get(..2)
            .filter(|head| head.eq_ignore_ascii_case(prefix))
            .map(|_| &text[2..]);
        if let Some(digits) = digits {
            let valid =
                !digits.is_empty() && digits.chars().all(|c| c.is_ascii() && c.is_digit(radix));
            return valid.then(|| {
                digits.chars().fold(0.0, |total, c| {
                    total * f64::from(radix) + f64::from(c.to_digit(radix).unwrap_or(0))
                })
            });
        }
    }
    let unsigned = text.trim_start_matches(['+', '-']);
    if unsigned == "Infinity" && text.len() - unsigned.len() <= 1 {
        return Some(if text.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    let decimal = text.len() - unsigned.len() <= 1
        && unsigned
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'e' | b'E' | b'+' | b'-'));
    decimal.then(|| text.parse().ok()).flatten()
}

/// Node's `finiteNumber`: a number, or a string `Number()` reads as finite.
fn finite(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => js_number(text),
        _ => None,
    }
    .filter(|number| number.is_finite())
}

/// Node's `mapBambuGcodeState`.
fn state(raw: &str) -> State {
    match js_trim(raw).to_uppercase().as_str() {
        "IDLE" => State::Idle,
        "RUNNING" | "PREPARE" | "SLICING" | "INIT" => State::Printing,
        "PAUSE" => State::Paused,
        "FINISH" => State::Complete,
        "FAILED" => State::Error,
        "OFFLINE" => State::Offline,
        _ => State::Unknown,
    }
}

/// Node's `statusFromBambuPrint` for one report's `print` object.
pub fn status_from_print(print: &Value) -> Status {
    let raw = print
        .get("gcode_state")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let state = state(raw);
    let show_progress = matches!(state, State::Printing | State::Paused);
    let progress = finite(print.get("mc_percent")).map(|percent| percent.clamp(0.0, 100.0).round());
    let eta_seconds = finite(print.get("mc_remaining_time"))
        .filter(|minutes| *minutes > 0.0)
        .map(|minutes| (minutes * 60.0).round());
    let filename = nonblank(print.get("subtask_name"))
        .or_else(|| nonblank(print.get("gcode_file")))
        .map(str::to_owned);
    let message = match (&state, &filename) {
        (State::Printing, Some(name)) => Some(format!("Printing {name}")),
        (State::Complete, Some(name)) => Some(format!("Complete · {name}")),
        (State::Complete, None) => Some("Complete".into()),
        (State::Idle, _) => Some("Idle".into()),
        (State::Paused, _) => Some("Paused".into()),
        (State::Error, _) => Some("Error".into()),
        _ if !raw.is_empty() => Some(format!("gcode_state: {raw}")),
        _ => None,
    };
    Status {
        progress: progress.filter(|_| show_progress),
        eta_seconds: eta_seconds.filter(|_| show_progress),
        filename,
        message,
        nozzle_temperature_c: finite(print.get("nozzle_temper")),
        nozzle_target_c: finite(print.get("nozzle_target_temper")),
        bed_temperature_c: finite(print.get("bed_temper")),
        bed_target_c: finite(print.get("bed_target_temper")),
        ip_address: None,
        uptime_seconds: None,
        state,
    }
}

/// The `print` object of a report Node would use: one whose
/// `print.gcode_state` is a string that is not blank.
fn usable(payload: &[u8]) -> Option<Value> {
    let report: Value = serde_json::from_slice(payload).ok()?;
    let print = report.get("print")?;
    let state = print.get("gcode_state")?.as_str()?;
    (!js_trim(state).is_empty()).then(|| print.clone())
}

/// One DER element at the start of `input`: its tag, contents and the rest.
fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, mut rest) = rest.split_first()?;
    let length = if first < 0x80 {
        usize::from(first)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 || rest.len() < count {
            return None;
        }
        let (bytes, tail) = rest.split_at(count);
        rest = tail;
        bytes
            .iter()
            .fold(0, |length, byte| length << 8 | usize::from(*byte))
    };
    (rest.len() >= length).then(|| (tag, &rest[..length], &rest[length..]))
}

/// The DER SubjectPublicKeyInfo of an X.509 certificate of any version.
/// rustls's own signature helpers parse the certificate with webpki, which
/// accepts only v3, and Node's adapter notes that Bambu printers present
/// self-signed v1 certificates.
fn spki(certificate: &[u8]) -> Option<&[u8]> {
    let (0x30, certificate, _) = der(certificate)? else {
        return None;
    };
    let (0x30, mut fields, _) = der(certificate)? else {
        return None;
    };
    if let (0xa0, _, rest) = der(fields)? {
        fields = rest;
    }
    for _ in ["serialNumber", "signature", "issuer", "validity", "subject"] {
        fields = der(fields)?.2;
    }
    let (0x30, _, rest) = der(fields)? else {
        return None;
    };
    Some(&fields[..fields.len() - rest.len()])
}

/// The certificate's public-key algorithm identifier (without its SEQUENCE
/// header, as `AlgorithmIdentifier` holds it) and the public key bits.
fn public_key(certificate: &[u8]) -> Option<(&[u8], &[u8])> {
    let (_, info, _) = der(spki(certificate)?)?;
    let (0x30, algorithm, rest) = der(info)? else {
        return None;
    };
    let (0x03, key, _) = der(rest)? else {
        return None;
    };
    Some((algorithm, key.strip_prefix(&[0])?))
}

/// Accepts one end-entity certificate by fingerprint and records the
/// fingerprint it saw. Handshake signatures are verified as usual.
#[derive(Debug)]
struct Verifier {
    trust: Trust,
    seen: Mutex<Option<Fingerprint>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fingerprint: Fingerprint = Sha256::digest(end_entity.as_ref()).into();
        *self
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(fingerprint);
        match self.trust {
            Trust::Pinned(pin) if pin != fingerprint => {
                Err(rustls::Error::General(CERTIFICATE_CHANGED.into()))
            }
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let bad = || rustls::Error::InvalidCertificate(CertificateError::BadSignature);
        let (algorithm, key) = public_key(cert).ok_or_else(bad)?;
        let candidates = self
            .provider
            .signature_verification_algorithms
            .mapping
            .iter()
            .filter(|(scheme, _)| *scheme == dss.scheme)
            .flat_map(|(_, algorithms)| algorithms.iter());
        for candidate in candidates {
            if candidate.public_key_alg_id().as_ref() == algorithm {
                return candidate
                    .verify_signature(key, message, dss.signature())
                    .map(|()| HandshakeSignatureValid::assertion())
                    .map_err(|_| bad());
            }
        }
        Err(rustls::Error::PeerMisbehaved(
            PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme,
        ))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let spki = spki(cert).ok_or(rustls::Error::InvalidCertificate(
            CertificateError::BadEncoding,
        ))?;
        rustls::crypto::verify_tls13_signature_with_raw_key(
            message,
            &SubjectPublicKeyInfoDer::from(spki),
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Why a read produced no status; each maps to Node's offline message.
enum Failure {
    /// The host check refused the address before any connection.
    Refused(String),
    Offline(String),
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Failure::Offline(match error.kind() {
            std::io::ErrorKind::UnexpectedEof => CLOSED.into(),
            _ => CONNECTION_ERROR.into(),
        })
    }
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        match error.downcast::<std::io::Error>() {
            Ok(error) => error.into(),
            Err(error) => Failure::Offline(error.to_string()),
        }
    }
}

pub struct Driver {
    guard: HttpGuard,
    config: Config,
    connect_timeout: Duration,
    status_timeout: Duration,
}

impl Driver {
    pub fn new(guard: HttpGuard, config: Config) -> Self {
        Self {
            guard,
            config,
            connect_timeout: CONNECT_TIMEOUT,
            status_timeout: STATUS_TIMEOUT,
        }
    }

    /// Shorter deadlines for tests; the connect deadline stays within the
    /// status deadline.
    pub fn with_timeouts(mut self, connect: Duration, status: Duration) -> Self {
        self.connect_timeout = connect.min(status);
        self.status_timeout = status;
        self
    }

    /// One status read, as Node's `getStatus`: connect, subscribe, pushall,
    /// the first usable report, disconnect. Every failure is an `offline`
    /// status, with Node's message where Node has one; the access code never
    /// appears in one.
    pub async fn status(&self, trust: Trust) -> Read {
        let verifier = Arc::new(Verifier {
            trust,
            seen: Mutex::new(None),
            provider: Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        });
        let read = tokio::time::timeout(self.status_timeout, self.read(verifier.clone())).await;
        let certificate = *verifier
            .seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut status = match read {
            Ok(Ok(status)) => status,
            Ok(Err(Failure::Refused(message))) => {
                return Read {
                    status: Status::offline(&message),
                    certificate,
                };
            }
            Ok(Err(Failure::Offline(message))) => Status::offline(&message),
            Err(_) => Status::offline(TIMED_OUT),
        };
        status.ip_address = Some(self.config.host.clone());
        Read {
            status,
            certificate,
        }
    }

    async fn read(&self, verifier: Arc<Verifier>) -> Result<Status, Failure> {
        let report = format!("device/{}/report", self.config.serial);
        let mut stream = self.open(&verifier).await?;
        // Like Node's message listener, a usable report that arrives before
        // SUBACK (a retained one, say) answers the read.
        let print = match subscribe(&mut stream, &report).await? {
            Some(print) => print,
            None => {
                stream
                    .write_all(&mqtt::pushall(&self.config.serial))
                    .await?;
                stream.flush().await?;
                loop {
                    if let mqtt::Packet::Publish { topic, payload } =
                        mqtt::read(&mut stream).await?
                        && topic == report
                        && let Some(print) = usable(&payload)
                    {
                        break print;
                    }
                }
            }
        };
        let _ = stream.write_all(&mqtt::disconnect()).await;
        let _ = stream.flush().await;
        let _ = stream.shutdown().await;
        Ok(status_from_print(&print))
    }

    /// TCP through the guard, pinned TLS and an accepted CONNECT, within the
    /// connect deadline.
    async fn open(&self, verifier: &Arc<Verifier>) -> Result<Stream, Failure> {
        let config = &self.config;
        let host = config.host.trim_start_matches('[').trim_end_matches(']');
        tokio::time::timeout(self.connect_timeout, async {
            let tcp = self
                .guard
                .connect(host, config.port)
                .await
                .map_err(|error| match error.to_string() {
                    message if message == "Host unavailable" => Failure::Offline(message),
                    message => Failure::Refused(message),
                })?;
            let peer = tcp.peer_addr()?.ip();
            let tls = rustls::ClientConfig::builder_with_provider(verifier.provider.clone())
                .with_safe_default_protocol_versions()
                .map_err(|_| Failure::Offline(TLS_FAILED.into()))?
                .dangerous()
                .with_custom_certificate_verifier(verifier.clone())
                .with_no_client_auth();
            let tls = tokio_rustls::TlsConnector::from(Arc::new(tls))
                .connect(ServerName::IpAddress(peer.into()), tcp)
                .await
                .map_err(|error| {
                    let changed = matches!(verifier.trust, Trust::Pinned(pin) if verifier.seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_some_and(|seen| seen != pin));
                    Failure::Offline(if changed { CERTIFICATE_CHANGED.into() } else if error.kind() == std::io::ErrorKind::UnexpectedEof { CLOSED.into() } else { TLS_FAILED.into() })
                })?;
            let mut stream = BufStream::new(tls);
            let client_id = format!("pp_{:08x}", rand::random::<u32>());
            let connect = mqtt::connect(&client_id, USERNAME, &config.access_code, KEEP_ALIVE_SECONDS);
            stream.write_all(&connect).await?;
            stream.flush().await?;
            match mqtt::read(&mut stream).await? {
                mqtt::Packet::ConnAck(0) => Ok(stream),
                mqtt::Packet::ConnAck(code) => Err(Failure::Offline(refused(code))),
                _ => Err(Failure::Offline(CONNECTION_ERROR.into())),
            }
        })
        .await
        .map_err(|_| Failure::Offline(CLOSED.into()))?
    }

    /// One long-lived session for the status watcher (GRE-1176), pinned to
    /// `pin` only: it never pins. It subscribes, sends pushall unless
    /// `session` sent one less than [`PUSHALL_SPACING`] ago, and calls
    /// `report` with the merged status after every report, until the
    /// connection fails. The answer is the offline status that ended it.
    /// A reconnect inside the spacing drops the held report, reports
    /// `Awaiting status` with `false` (nothing was heard from the printer),
    /// and sends pushall in the session once the spacing allows.
    pub async fn watch(
        &self,
        pin: Fingerprint,
        session: &mut Session,
        mut report: impl FnMut(Status, bool),
    ) -> Status {
        let verifier = Arc::new(Verifier {
            trust: Trust::Pinned(pin),
            seen: Mutex::new(None),
            provider: Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
        });
        let message = match self.session(&verifier, session, &mut report).await {
            Err(Failure::Refused(message) | Failure::Offline(message)) => message,
        };
        let mut status = Status::offline(&message);
        status.ip_address = Some(self.config.host.clone());
        status
    }

    async fn session(
        &self,
        verifier: &Arc<Verifier>,
        session: &mut Session,
        report: &mut impl FnMut(Status, bool),
    ) -> Result<std::convert::Infallible, Failure> {
        let topic = format!("device/{}/report", self.config.serial);
        let mut stream = self.open(verifier).await?;
        let early = tokio::time::timeout(self.status_timeout, subscribe(&mut stream, &topic))
            .await
            .map_err(|_| Failure::Offline(TIMED_OUT.into()))??;
        let deferred = session
            .pushed
            .filter(|pushed| pushed.elapsed() < PUSHALL_SPACING)
            .map(|pushed| pushed + PUSHALL_SPACING);
        if let Some(print) = early {
            report(status_from_print(&print), true);
            session.print = Some(print);
        } else if deferred.is_some() {
            // Reconnected within the spacing: changes made while the session
            // was down never arrive as changes, so the held report is stale.
            session.print = None;
            let mut awaiting = Status::unknown("Awaiting status");
            awaiting.ip_address = Some(self.config.host.clone());
            report(awaiting, false);
        }
        if deferred.is_none() {
            stream
                .write_all(&mqtt::pushall(&self.config.serial))
                .await?;
            stream.flush().await?;
            session.pushed = Some(tokio::time::Instant::now());
        }
        let pushed_late = std::sync::atomic::AtomicBool::new(false);
        let (mut reader, mut writer) = tokio::io::split(stream);
        let idle = Duration::from_secs(KEEP_ALIVE_SECONDS.into());
        let reading = async {
            loop {
                let packet = tokio::time::timeout(idle, mqtt::read(&mut reader))
                    .await
                    .map_err(|_| Failure::Offline(TIMED_OUT.into()))??;
                if let mqtt::Packet::Publish {
                    topic: from,
                    payload,
                } = packet
                    && from == topic
                    && let Some(status) = session.merge(&payload)
                {
                    report(status, true);
                }
            }
        };
        let writing = async {
            let mut tick =
                tokio::time::interval_at(tokio::time::Instant::now() + idle / 2, idle / 2);
            let mut due = deferred;
            loop {
                let packet = tokio::select! {
                    _ = tick.tick() => mqtt::pingreq(),
                    () = tokio::time::sleep_until(due.unwrap_or_else(tokio::time::Instant::now)), if due.is_some() => {
                        due = None;
                        pushed_late.store(true, std::sync::atomic::Ordering::Relaxed);
                        mqtt::pushall(&self.config.serial)
                    }
                };
                writer.write_all(&packet).await?;
                writer.flush().await?;
            }
        };
        let ended = tokio::select! {
            ended = reading => ended,
            ended = writing => ended,
        };
        if pushed_late.into_inner() {
            session.pushed = deferred;
        }
        ended
    }
}

type Stream = BufStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

/// SUBSCRIBE to `topic` and wait for SUBACK. A usable report that arrives
/// first is returned.
async fn subscribe(stream: &mut Stream, topic: &str) -> Result<Option<Value>, Failure> {
    stream.write_all(&mqtt::subscribe(topic)).await?;
    stream.flush().await?;
    loop {
        match mqtt::read(stream).await? {
            mqtt::Packet::SubAck(codes) if codes.iter().all(|code| *code < 0x80) => {
                return Ok(None);
            }
            mqtt::Packet::SubAck(_) => return Err(Failure::Offline(SUBSCRIBE_FAILED.into())),
            mqtt::Packet::Publish {
                topic: from,
                payload,
            } if from == topic => {
                if let Some(print) = usable(&payload) {
                    return Ok(Some(print));
                }
            }
            _ => {}
        }
    }
}

/// A Bambu printer answers pushall with a full report and then publishes only
/// the fields that changed. The watcher sends pushall at most this often,
/// across reconnects, and merges the changes into the last full report.
pub const PUSHALL_SPACING: Duration = Duration::from_secs(300);

/// What one watched host's sessions carry from one connection to the next.
#[derive(Default)]
pub struct Session {
    /// The last full `print` object, with every later change merged in.
    print: Option<Value>,
    pushed: Option<tokio::time::Instant>,
}

impl Session {
    /// Merges one report's `print` object into the last full one. Until a
    /// full report (one with a non-blank `gcode_state`) arrives, changes have
    /// nothing to merge into and give no status.
    fn merge(&mut self, payload: &[u8]) -> Option<Status> {
        match &mut self.print {
            None => self.print = Some(usable(payload)?),
            Some(Value::Object(full)) => {
                let report: Value = serde_json::from_slice(payload).ok()?;
                let Value::Object(changed) = report.get("print")?.clone() else {
                    return None;
                };
                full.extend(changed);
            }
            Some(_) => return None,
        }
        self.print.as_ref().map(status_from_print)
    }
}

/// mqtt.js's CONNACK refusal messages.
fn refused(code: u8) -> String {
    let reason = match code {
        1 => "Unacceptable protocol version",
        2 => "Identifier rejected",
        3 => "Server unavailable",
        4 => "Bad username or password",
        5 => "Not authorized",
        _ => "Unknown error",
    };
    format!("Connection refused: {reason}")
}
