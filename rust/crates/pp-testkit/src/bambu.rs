//! A loopback Bambu LAN MQTT broker for driver tests: TLS with a throwaway
//! self-signed certificate generated per test (no key material in the repo),
//! MQTT 3.1.1 CONNECT, SUBSCRIBE and PUBLISH, and a log of what the client
//! sent. The access code itself is never logged, only whether it matched.
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
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
    pub fingerprint: [u8; 32],
}

impl Identity {
    pub fn generate() -> Result<Self, rcgen::Error> {
        let key = rcgen::KeyPair::generate()?;
        let certificate = rcgen::CertificateParams::new(vec!["printer.test".into()])?
            .self_signed(&key)?
            .der()
            .clone();
        Ok(Self {
            fingerprint: Sha256::digest(certificate.as_ref()).into(),
            certificate,
            key: Arc::new(key.serialize_der()),
        })
    }

    fn server(&self) -> Result<Arc<rustls::ServerConfig>, rustls::Error> {
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.as_ref().clone()));
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        Ok(Arc::new(
            rustls::ServerConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()?
                .with_no_client_auth()
                .with_single_cert(vec![self.certificate.clone()], key)?,
        ))
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
            replies: vec![Reply::Publish {
                topic: format!("device/{serial}/report"),
                payload: report.to_vec(),
            }],
        }
    }
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
                frame(0x90, &[body[0], body[1], script.suback])
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
