//! A loopback Bambu LAN MQTT broker for driver tests: TLS with a throwaway
//! self-signed certificate generated per test (no key material in the repo),
//! MQTT 3.1.1 CONNECT, SUBSCRIBE and PUBLISH, and a log of what the client
//! sent. The access code itself is never logged, only whether it matched.
use rcgen::{PublicKeyData, SigningKey};
use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};
use sha2::{Digest, Sha256};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

/// A self-signed printer identity and the SHA-256 of its certificate DER.
#[derive(Clone)]
pub struct Identity {
    certificate: CertificateDer<'static>,
    key: Arc<Vec<u8>>,
    tls12_only: bool,
    pub fingerprint: [u8; 32],
}

/// A DER element.
fn tlv(tag: u8, contents: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    match contents.len() {
        length @ 0..0x80 => out.push(length as u8),
        length @ 0x80..0x100 => out.extend([0x81, length as u8]),
        length => out.extend([0x82, (length >> 8) as u8, length as u8]),
    }
    out.extend(contents);
    out
}

fn identity(certificate: Vec<u8>, key: Vec<u8>) -> Identity {
    Identity {
        fingerprint: Sha256::digest(&certificate).into(),
        certificate: CertificateDer::from(certificate),
        key: Arc::new(key),
        tls12_only: false,
    }
}

impl Identity {
    pub fn generate() -> Result<Self, rcgen::Error> {
        let key = rcgen::KeyPair::generate()?;
        let certificate = rcgen::CertificateParams::new(vec!["printer.test".into()])?
            .self_signed(&key)?
            .der()
            .clone();
        Ok(identity(certificate.to_vec(), key.serialize_der()))
    }

    /// A self-signed X.509 v1 certificate (no version field, no extensions),
    /// the kind Node's adapter says Bambu printers present. ECDSA P-256.
    pub fn generate_v1() -> Result<Self, rcgen::Error> {
        let key = rcgen::KeyPair::generate()?;
        let ecdsa_sha256 = tlv(0x30, &[6, 8, 0x2a, 0x86, 0x48, 0xce, 0x3d, 4, 3, 2]);
        let name = tlv(
            0x30,
            &tlv(
                0x31,
                &tlv(
                    0x30,
                    &[&[6, 3, 0x55, 4, 3][..], &tlv(0x0c, b"printer.test")].concat(),
                ),
            ),
        );
        let validity = tlv(
            0x30,
            &[tlv(0x17, b"250101000000Z"), tlv(0x17, b"491231235959Z")].concat(),
        );
        let fields = [
            tlv(2, &[1]),
            ecdsa_sha256.clone(),
            name.clone(),
            validity,
            name,
            key.subject_public_key_info(),
        ];
        let tbs = tlv(0x30, &fields.concat());
        let signature = tlv(3, &[&[0][..], &key.sign(&tbs)?].concat());
        let certificate = tlv(0x30, &[tbs, ecdsa_sha256, signature].concat());
        Ok(identity(certificate, key.serialize_der()))
    }

    /// This certificate served with `other`'s private key: the pin matches,
    /// but the server cannot prove it holds the certificate's key.
    pub fn with_key_of(&self, other: &Identity) -> Self {
        Self {
            key: other.key.clone(),
            ..self.clone()
        }
    }

    /// Serve this identity over TLS 1.2 only.
    pub fn tls12_only(self) -> Self {
        Self {
            tls12_only: true,
            ..self
        }
    }

    /// Without rustls's certificate parsing or key check, so v1 and
    /// mismatched identities are served as they are.
    fn server(&self) -> Result<Arc<rustls::ServerConfig>, rustls::Error> {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.as_ref().clone()));
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let key = provider.key_provider.load_private_key(key)?;
        let certified = Arc::new(CertifiedKey::new(vec![self.certificate.clone()], key));
        let versions: &[&rustls::SupportedProtocolVersion] = if self.tls12_only {
            &[&rustls::version::TLS12]
        } else {
            rustls::DEFAULT_VERSIONS
        };
        Ok(Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_protocol_versions(versions)?
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(Single(certified))),
        ))
    }
}

#[derive(Debug)]
struct Single(Arc<CertifiedKey>);

impl ResolvesServerCert for Single {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// What the broker does after it reads the client's pushall PUBLISH.
#[derive(Clone)]
pub enum Reply {
    /// One QoS 0 PUBLISH.
    Publish { topic: String, payload: Vec<u8> },
    /// Raw bytes, for malformed or oversized packets.
    Raw(Vec<u8>),
    /// Close the connection.
    Close,
}

/// How the broker answers one connection.
#[derive(Clone)]
pub struct Script {
    pub access_code: String,
    pub connack: u8,
    pub suback: u8,
    /// Published on the report topic between SUBSCRIBE and SUBACK, as a
    /// broker may send a retained message.
    pub retained: Option<Vec<u8>>,
    pub replies: Vec<Reply>,
}

impl Script {
    /// Accepts `access_code` and answers pushall with `report` on
    /// `device/<serial>/report`.
    pub fn reporting(access_code: &str, serial: &str, report: &[u8]) -> Self {
        Self {
            access_code: access_code.into(),
            connack: 0,
            suback: 0,
            retained: None,
            replies: vec![Reply::Publish {
                topic: format!("device/{serial}/report"),
                payload: report.to_vec(),
            }],
        }
    }
}

/// A broker for one synthetic printer that answers pushall with the recorded
/// idle report (`fixtures/drivers/bambu/pushall-idle.json`), and that
/// printer's identity.
pub async fn idle_printer(access_code: &str, serial: &str) -> std::io::Result<(Broker, Identity)> {
    let report = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../fixtures/drivers/bambu/pushall-idle.json"
    ))?;
    let printer = Identity::generate().map_err(std::io::Error::other)?;
    let broker = Broker::start(
        printer.clone(),
        Script::reporting(access_code, serial, &report),
    )
    .await?;
    Ok((broker, printer))
}

pub struct Broker {
    pub addr: SocketAddr,
    log: Arc<Mutex<Vec<String>>>,
    identity: Arc<Mutex<Identity>>,
    script: Arc<Mutex<Script>>,
    task: JoinHandle<()>,
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Broker {
    pub async fn start(identity: Identity, script: Script) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let log = Arc::new(Mutex::new(Vec::new()));
        let identity = Arc::new(Mutex::new(identity));
        let script = Arc::new(Mutex::new(script));
        let (shared_log, shared_identity, shared_script) =
            (log.clone(), identity.clone(), script.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let server = lock(&shared_identity).server();
                let script = lock(&shared_script).clone();
                let log = shared_log.clone();
                tokio::spawn(async move {
                    lock(&log).push("TCP".into());
                    let Ok(server) = server else {
                        lock(&log).push("TLS refused".into());
                        return;
                    };
                    let Ok(stream) = tokio_rustls::TlsAcceptor::from(server).accept(socket).await
                    else {
                        lock(&log).push("TLS refused".into());
                        return;
                    };
                    lock(&log).push("TLS".into());
                    serve(stream, &script, &log).await;
                });
            }
        });
        Ok(Self {
            addr,
            log,
            identity,
            script,
            task,
        })
    }

    /// Everything the client did, in order, one line per event.
    pub fn log(&self) -> Vec<String> {
        lock(&self.log).clone()
    }

    /// Presents `identity` to every later connection, as a printer whose
    /// certificate changed would.
    pub fn present(&self, identity: Identity) {
        *lock(&self.identity) = identity;
    }

    pub fn script(&self, script: Script) {
        *lock(&self.script) = script;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn packet(reader: &mut (impl AsyncRead + Unpin)) -> Option<(u8, Vec<u8>)> {
    let header = reader.read_u8().await.ok()?;
    let mut length = 0usize;
    let mut shift = 0;
    loop {
        let byte = reader.read_u8().await.ok()?;
        length |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.ok()?;
    Some((header, body))
}

fn field(body: &[u8], at: usize) -> (String, usize) {
    let length = usize::from(u16::from_be_bytes([body[at], body[at + 1]]));
    let end = at + 2 + length;
    (
        String::from_utf8_lossy(&body[at + 2..end]).into_owned(),
        end,
    )
}

fn frame(header: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![header];
    let mut length = body.len();
    loop {
        let byte = (length % 128) as u8;
        length /= 128;
        out.push(if length > 0 { byte | 0x80 } else { byte });
        if length == 0 {
            break;
        }
    }
    out.extend(body);
    out
}

/// A QoS 0 PUBLISH packet.
pub fn publish(topic: &str, payload: &[u8]) -> Vec<u8> {
    let mut body = (topic.len() as u16).to_be_bytes().to_vec();
    body.extend(topic.as_bytes());
    body.extend(payload);
    frame(0x30, &body)
}

async fn serve<S>(mut stream: S, script: &Script, log: &Mutex<Vec<String>>)
where
    S: AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some((header, body)) = packet(&mut stream).await {
        let write = match header >> 4 {
            1 => {
                // CONNECT: protocol name, level, flags, keep alive, client id.
                let (_, at) = field(&body, 0);
                let (_, at) = field(&body, at + 4);
                let (username, at) = field(&body, at);
                let (password, _) = field(&body, at);
                let matched = if password == script.access_code {
                    "access code"
                } else {
                    "wrong access code"
                };
                lock(log).push(format!("CONNECT {username} {matched}"));
                frame(0x20, &[0, script.connack])
            }
            8 => {
                let (topic, _) = field(&body, 2);
                lock(log).push(format!("SUBSCRIBE {topic}"));
                let mut out = script
                    .retained
                    .as_ref()
                    .map_or_else(Vec::new, |payload| publish(&topic, payload));
                out.extend(frame(0x90, &[body[0], body[1], script.suback]));
                out
            }
            3 => {
                let (topic, at) = field(&body, 0);
                let payload = String::from_utf8_lossy(&body[at..]);
                lock(log).push(format!("PUBLISH {topic} {payload}"));
                let mut out = Vec::new();
                for reply in &script.replies {
                    match reply {
                        Reply::Publish { topic, payload } => out.extend(publish(topic, payload)),
                        Reply::Raw(bytes) => out.extend(bytes),
                        Reply::Close => {
                            let _ = stream.write_all(&out).await;
                            let _ = stream.shutdown().await;
                            return;
                        }
                    }
                }
                out
            }
            14 => {
                lock(log).push("DISCONNECT".into());
                return;
            }
            12 => {
                lock(log).push("PINGREQ".into());
                frame(0xd0, &[])
            }
            other => {
                lock(log).push(format!("packet {other}"));
                Vec::new()
            }
        };
        if stream.write_all(&write).await.is_err() || stream.flush().await.is_err() {
            return;
        }
    }
}
