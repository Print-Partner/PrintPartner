//! Printer HTTP: resolve once, check every answer, connect only to checked IPs.
//! LAN/loopback printers are intentional; metadata/link-local, unspecified,
//! multicast and reserved targets are forbidden. No proxy, redirects or TLS downgrade.
//! `post` sends one attempt and never retries.
use anyhow::{Context, Result, ensure};
use axum::http::{Request, Uri};
use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper_util::rt::TokioIo;
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context as TaskContext, Poll, ready},
    time::{Duration, SystemTime},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    net::TcpStream,
};

/// Uploads take longer than status reads; this caps `HttpGuard::post`.
pub const MAX_POST_DEADLINE: Duration = Duration::from_secs(120);
/// The largest buffered reply a guard built by `HttpGuard::new` accepts.
pub const MAX_BODY: usize = 2 * 1024 * 1024;
/// The largest buffered reply `HttpGuard::with_body_limit` allows: Node's
/// PrusaLink directory listing cap.
pub const MAX_LISTING_BODY: usize = 8 * 1024 * 1024;

pub type Resolution = Pin<Box<dyn Future<Output = Result<Vec<IpAddr>>> + Send>>;
pub trait Resolver: Send + Sync {
    fn resolve(&self, host: String) -> Resolution;
}
pub struct SystemResolver;
impl Resolver for SystemResolver {
    fn resolve(&self, host: String) -> Resolution {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .map(|a| a.ip())
                .collect())
        })
    }
}

// IANA IPv4 Special-Purpose Address Registry, plus multicast/reserved space.
// https://www.iana.org/assignments/iana-ipv4-special-registry/
// RFC 1918 private addresses and 127/8 are explicit printer-network exceptions.
const SPECIAL_IPV4: &[([u8; 4], u32)] = &[
    ([0, 0, 0, 0], 8),
    ([100, 64, 0, 0], 10),
    ([169, 254, 0, 0], 16),
    ([192, 0, 0, 0], 24),
    ([192, 0, 2, 0], 24),
    ([192, 31, 196, 0], 24),
    ([192, 52, 193, 0], 24),
    ([192, 88, 99, 0], 24),
    ([192, 175, 48, 0], 24),
    ([198, 18, 0, 0], 15),
    ([198, 51, 100, 0], 24),
    ([203, 0, 113, 0], 24),
    ([224, 0, 0, 0], 3),
];

// Special-purpose prefixes within global unicast; other special-purpose space is
// outside 2000::/3. ULA and loopback remain explicit printer-network exceptions.
// https://www.iana.org/assignments/iana-ipv6-special-registry/
const SPECIAL_IPV6: &[([u16; 8], u32)] = &[
    ([0x2001, 0, 0, 0, 0, 0, 0, 0], 23),
    ([0x2001, 0x0db8, 0, 0, 0, 0, 0, 0], 32),
    ([0x2002, 0, 0, 0, 0, 0, 0, 0], 16),
    ([0x2620, 0x004f, 0x8000, 0, 0, 0, 0, 0], 48),
    ([0x3fff, 0, 0, 0, 0, 0, 0, 0], 20),
];

pub fn permitted(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            if ip.is_private() || ip.is_loopback() {
                return true;
            }
            let address = u32::from(ip);
            SPECIAL_IPV4.iter().all(|(base, prefix)| {
                let mask = u32::MAX << (32 - *prefix);
                address & mask != u32::from_be_bytes(*base)
            })
        }
        IpAddr::V6(ip) => {
            if let Some(ip) = ip.to_ipv4_mapped() {
                return permitted(IpAddr::V4(ip));
            }
            let first = ip.segments()[0];
            if ip.is_loopback() || first & 0xfe00 == 0xfc00 {
                return ip != std::net::Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254);
            }
            let address = u128::from(ip);
            first & 0xe000 == 0x2000
                && SPECIAL_IPV6.iter().all(|(base, prefix)| {
                    let mask = u128::MAX << (128 - *prefix);
                    address & mask != u128::from(std::net::Ipv6Addr::from(*base))
                })
        }
    }
}

trait WallClock: Send + Sync {
    fn now(&self) -> SystemTime;
}
struct SystemClock;
impl WallClock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}
#[derive(Clone)]
pub struct HttpGuard {
    resolver: Arc<dyn Resolver>,
    deadline: Duration,
    max_body: usize,
    clock: Arc<dyn WallClock>,
}
#[derive(Debug)]
pub struct HttpReply {
    pub status: u16,
    pub body: Vec<u8>,
    /// The `WWW-Authenticate` values joined with ", ", for Digest challenges.
    /// Other response headers are not kept.
    pub www_authenticate: Option<String>,
}

/// A POST body of exactly `length` bytes, sent with that `Content-Length`.
pub struct PostBody {
    pub content_type: String,
    pub reader: Pin<Box<dyn AsyncRead + Send>>,
    pub length: u64,
}

/// Whether a failed POST may have reached the printer.
#[derive(Debug)]
pub enum PostError {
    /// Failed before any request byte was written.
    Unsent(anyhow::Error),
    /// The request was at least partly written; the printer may have acted on it.
    Unknown(anyhow::Error),
    /// The body source failed or ended before its declared length, so the
    /// printer never received a complete request.
    SourceFailed(anyhow::Error),
}

impl std::fmt::Display for PostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsent(error) | Self::Unknown(error) | Self::SourceFailed(error) => {
                write!(f, "{error:#}")
            }
        }
    }
}

impl std::error::Error for PostError {}

struct ReaderBody {
    reader: Pin<Box<dyn AsyncRead + Send>>,
    remaining: u64,
    buf: Box<[u8]>,
    failed: Arc<AtomicBool>,
}

impl Body for ReaderBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, std::io::Error>>> {
        let this = self.get_mut();
        if this.remaining == 0 {
            return Poll::Ready(None);
        }
        let limit = this
            .buf
            .len()
            .min(usize::try_from(this.remaining).unwrap_or(usize::MAX));
        let mut read = ReadBuf::new(&mut this.buf[..limit]);
        if let Err(error) = ready!(this.reader.as_mut().poll_read(cx, &mut read)) {
            this.failed.store(true, Ordering::SeqCst);
            return Poll::Ready(Some(Err(error)));
        }
        let filled = read.filled();
        if filled.is_empty() {
            this.failed.store(true, Ordering::SeqCst);
            return Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "upload source ended before its declared length",
            ))));
        }
        this.remaining -= filled.len() as u64;
        Poll::Ready(Some(Ok(Frame::data(Bytes::copy_from_slice(filled)))))
    }

    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}
impl HttpGuard {
    pub fn new(resolver: Arc<dyn Resolver>, deadline: Duration, max_body: usize) -> Result<Self> {
        ensure!(
            !deadline.is_zero() && deadline <= Duration::from_secs(30),
            "Invalid deadline"
        );
        ensure!(max_body > 0 && max_body <= MAX_BODY, "Invalid body limit");
        Ok(Self {
            resolver,
            deadline,
            max_body,
            clock: Arc::new(SystemClock),
        })
    }
    /// The same guard with a buffered reply limit up to `MAX_LISTING_BODY`.
    pub fn with_body_limit(&self, max_body: usize) -> Result<Self> {
        ensure!(
            max_body > 0 && max_body <= MAX_LISTING_BODY,
            "Invalid body limit"
        );
        Ok(Self {
            max_body,
            ..self.clone()
        })
    }

    /// One GET whose reply body is not buffered: the reply head must arrive
    /// within `deadline`, then the body reads follow `body`.
    /// Extra headers follow `HttpGuard::request`'s rules; no API key is sent.
    /// The connection closes when the returned body is dropped.
    pub async fn stream(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        deadline: Duration,
        body: BodyDeadline,
    ) -> Result<StreamReply> {
        ensure!(
            !deadline.is_zero()
                && deadline <= MAX_POST_DEADLINE
                && body != BodyDeadline::Idle(Duration::ZERO),
            "Invalid deadline"
        );
        let limit = match body {
            BodyDeadline::Total => Limit::Until(tokio::time::Instant::now() + deadline),
            BodyDeadline::Idle(idle) => Limit::Idle(idle),
        };
        let owned = ["host", "connection", "content-length", "content-type"];
        ensure!(
            !headers
                .iter()
                .any(|(name, _)| owned.iter().any(|o| o.eq_ignore_ascii_case(name))),
            "Reserved request header"
        );
        let written = AtomicBool::new(false);
        let empty = Empty::<Bytes>::new();
        let (response, connection) = self
            .bounded(
                deadline,
                self.open("GET", url, headers, None, empty, &written),
            )
            .await?;
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        Ok(StreamReply {
            status: response.status().as_u16(),
            content_type: header("content-type"),
            content_length: header("content-length").and_then(|value| value.parse().ok()),
            www_authenticate: challenges(&response),
            body: ReplyBody {
                body: response.into_body(),
                limit,
                _connection: connection,
            },
        })
    }

    pub async fn get(&self, url: &str, api_key: Option<&str>) -> Result<HttpReply> {
        let written = AtomicBool::new(false);
        let key = key_header(api_key);
        let headers = with_key(&key, &[]);
        let empty = Empty::<Bytes>::new();
        let request = async {
            self.reply(
                self.open("GET", url, &headers, None, empty, &written)
                    .await?,
            )
            .await
        };
        self.bounded(self.deadline, request).await
    }

    /// One GET or PUT attempt with extra request headers and no API key.
    /// `body: None` sends no body. Errors say whether the printer may have
    /// acted on the request, as for [`HttpGuard::post`].
    pub async fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<PostBody>,
        deadline: Duration,
    ) -> Result<HttpReply, PostError> {
        if !matches!(method, "GET" | "PUT") {
            return Err(PostError::Unsent(anyhow::anyhow!("Unsupported method")));
        }
        if method == "GET" && body.is_some() {
            return Err(PostError::Unsent(anyhow::anyhow!("GET with a body")));
        }
        // The guard owns framing and the pinned host.
        let owned = ["host", "connection", "content-length", "content-type"];
        if headers
            .iter()
            .any(|(name, _)| owned.iter().any(|o| o.eq_ignore_ascii_case(name)))
        {
            return Err(PostError::Unsent(anyhow::anyhow!(
                "Reserved request header"
            )));
        }
        self.write(method, url, None, headers, body, deadline).await
    }

    /// One POST attempt. `body: None` sends an empty body.
    pub async fn post(
        &self,
        url: &str,
        api_key: Option<&str>,
        body: Option<PostBody>,
        deadline: Duration,
    ) -> Result<HttpReply, PostError> {
        self.write("POST", url, api_key, &[], body, deadline).await
    }

    async fn write(
        &self,
        method: &str,
        url: &str,
        api_key: Option<&str>,
        headers: &[(&str, &str)],
        body: Option<PostBody>,
        deadline: Duration,
    ) -> Result<HttpReply, PostError> {
        if deadline.is_zero() || deadline > MAX_POST_DEADLINE {
            return Err(PostError::Unsent(anyhow::anyhow!("Invalid deadline")));
        }
        let written = AtomicBool::new(false);
        let failed = Arc::new(AtomicBool::new(false));
        let key = key_header(api_key);
        let headers = with_key(&key, headers);
        let result = match body {
            Some(body) => {
                let reader = ReaderBody {
                    reader: body.reader,
                    remaining: body.length,
                    buf: vec![0; 64 * 1024].into_boxed_slice(),
                    failed: failed.clone(),
                };
                let header = Some((body.content_type, body.length));
                let request = async {
                    self.reply(
                        self.open(method, url, &headers, header, reader, &written)
                            .await?,
                    )
                    .await
                };
                self.bounded(deadline, request).await
            }
            None => {
                let empty = Empty::<Bytes>::new();
                let request = async {
                    self.reply(
                        self.open(method, url, &headers, None, empty, &written)
                            .await?,
                    )
                    .await
                };
                self.bounded(deadline, request).await
            }
        };
        result.map_err(|error| {
            if failed.load(Ordering::SeqCst) {
                PostError::SourceFailed(error)
            } else if written.load(Ordering::SeqCst) {
                PostError::Unknown(error)
            } else {
                PostError::Unsent(error)
            }
        })
    }

    async fn bounded<T>(
        &self,
        timeout: Duration,
        request: impl Future<Output = Result<T>>,
    ) -> Result<T> {
        let deadline = self.clock.now() + timeout;
        // Tokio's monotonic clock may pause during system sleep. Also bound wall
        // time, so resumed requests are dropped instead of publishing stale data.
        let wall_timeout = async {
            loop {
                if self.clock.now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50).min(timeout)).await;
            }
        };
        let result = tokio::time::timeout(timeout, async {
            tokio::select! { biased;
                _=wall_timeout=>anyhow::bail!("HTTP wall deadline"),
                result=request=>result,
            }
        })
        .await
        .context("HTTP deadline")?;
        ensure!(self.clock.now() < deadline, "HTTP wall deadline");
        result
    }

    /// Buffers an opened reply under `max_body`; the connection lives until
    /// the body is read.
    async fn reply(
        &self,
        (response, _connection): (axum::http::Response<Incoming>, Connection),
    ) -> Result<HttpReply> {
        let status = response.status().as_u16();
        let www_authenticate = challenges(&response);
        if response.status().is_redirection() {
            return Ok(HttpReply {
                status,
                body: Vec::new(),
                www_authenticate,
            });
        }
        let body = Limited::new(response.into_body(), self.max_body)
            .collect()
            .await
            .map_err(|_| anyhow::anyhow!("HTTP body unavailable or oversized"))?
            .to_bytes()
            .to_vec();
        Ok(HttpReply {
            status,
            body,
            www_authenticate,
        })
    }

    /// Sends one request and returns the reply head with its unread body.
    async fn open<B>(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        content: Option<(String, u64)>,
        body: B,
        written: &AtomicBool,
    ) -> Result<(axum::http::Response<Incoming>, Connection)>
    where
        B: Body<Data = Bytes> + Send + 'static,
        B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    {
        let uri: Uri = url.parse()?;
        ensure!(
            uri.scheme_str() == Some("http"),
            "Only explicit HTTP status URLs supported"
        );
        let authority = uri.authority().context("Missing host")?;
        ensure!(
            !authority.as_str().contains('@'),
            "Credentials in URL denied"
        );
        let host = uri
            .host()
            .context("Missing host")?
            .trim_start_matches('[')
            .trim_end_matches(']');
        ensure!(
            uri.port().is_none() || uri.port_u16().is_some(),
            "Invalid port"
        );
        let port = uri.port_u16().unwrap_or(80);
        ensure!(port != 0, "Invalid port");
        let addresses = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![ip]
        } else {
            self.resolver.resolve(host.into()).await?
        };
        ensure!(
            !addresses.is_empty()
                && addresses.len() <= 32
                && addresses.iter().all(|ip| permitted(*ip)),
            "Prohibited DNS answer"
        );
        // Race only the checked, pinned addresses. The request deadline bounds
        // the whole race, and dropping the JoinSet cancels every losing socket.
        let stream = connect_addresses(
            addresses
                .into_iter()
                .map(|ip| SocketAddr::new(ip, port))
                .collect(),
            TcpStream::connect,
        )
        .await?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake::<_, B>(TokioIo::new(stream)).await?;
        let connection = Connection(tokio::spawn(connection));
        let mut builder = Request::builder()
            .method(method)
            .uri(uri.path_and_query().map_or("/", |p| p.as_str()))
            .header("host", authority.as_str())
            .header("connection", "close");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        if let Some((content_type, length)) = content {
            builder = builder
                .header("content-type", content_type)
                .header("content-length", length);
        } else if method != "GET" {
            builder = builder.header("content-length", 0);
        }
        let request = builder.body(body)?;
        written.store(true, Ordering::SeqCst);
        let response = sender.send_request(request).await?;
        Ok((response, connection))
    }
}

/// The connection task behind one request. Dropping it aborts the task, so a
/// timeout, cancellation, error or finished read releases the socket.
struct Connection(tokio::task::JoinHandle<Result<(), hyper::Error>>);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Repeated values are joined with ", ", as fetch's `Headers.get` does.
fn challenges<T>(response: &axum::http::Response<T>) -> Option<String> {
    let values: Vec<_> = response
        .headers()
        .get_all("www-authenticate")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    (!values.is_empty()).then(|| values.join(", "))
}

/// A reply head from `HttpGuard::stream`, with its body still unread.
pub struct StreamReply {
    pub status: u16,
    pub content_type: Option<String>,
    pub content_length: Option<u64>,
    /// As in `HttpReply`.
    pub www_authenticate: Option<String>,
    pub body: ReplyBody,
}

/// The idle deadline of a streamed reply passed with no new body bytes.
#[derive(Debug)]
pub struct IdleTimeout;

impl std::fmt::Display for IdleTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HTTP body idle deadline")
    }
}

impl std::error::Error for IdleTimeout {}

/// How long a streamed reply body may take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyDeadline {
    /// The head deadline also covers the whole body, as fetch with
    /// `AbortSignal.timeout` does.
    Total,
    /// Each chunk must arrive within this long of the last; the error is then
    /// `IdleTimeout`.
    Idle(Duration),
}

enum Limit {
    Until(tokio::time::Instant),
    Idle(Duration),
}

/// A streamed reply body, read under its `BodyDeadline`. Dropping it closes
/// the connection.
pub struct ReplyBody {
    body: Incoming,
    limit: Limit,
    _connection: Connection,
}

impl ReplyBody {
    /// The next non-empty data chunk, or `None` at the end of the body.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        loop {
            let frame = match self.limit {
                Limit::Until(at) => tokio::time::timeout_at(at, self.body.frame())
                    .await
                    .map_err(|_| anyhow::anyhow!("HTTP body deadline"))?,
                Limit::Idle(idle) => tokio::time::timeout(idle, self.body.frame())
                    .await
                    .map_err(|_| IdleTimeout)?,
            };
            let Some(frame) = frame else {
                return Ok(None);
            };
            if let Ok(data) = frame?.into_data()
                && !data.is_empty()
            {
                return Ok(Some(data));
            }
        }
    }

    /// At most the first `limit` bytes; the rest is never read.
    pub async fn prefix(mut self, limit: usize) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        while bytes.len() < limit {
            let Some(chunk) = self.chunk().await? else {
                break;
            };
            let take = chunk.len().min(limit - bytes.len());
            bytes.extend_from_slice(&chunk[..take]);
        }
        Ok(bytes)
    }
}

/// The header an API key travels in: a JWT as a bearer token, else `x-api-key`.
pub fn key_header(api_key: Option<&str>) -> Option<(&'static str, String)> {
    let key = api_key.filter(|key| !key.is_empty() && *key != "****")?;
    let jwt = key.split('.').count() == 3
        && key.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        });
    Some(if jwt {
        ("authorization", format!("Bearer {key}"))
    } else {
        ("x-api-key", key.to_owned())
    })
}

fn with_key<'a>(
    key: &'a Option<(&'static str, String)>,
    headers: &[(&'a str, &'a str)],
) -> Vec<(&'a str, &'a str)> {
    let mut all = headers.to_vec();
    if let Some((name, value)) = key {
        all.push((name, value));
    }
    all
}

async fn connect_addresses<T, F>(
    addresses: Vec<SocketAddr>,
    connect: impl Fn(SocketAddr) -> F,
) -> Result<T>
where
    T: Send + 'static,
    F: Future<Output = std::io::Result<T>> + Send + 'static,
{
    let mut attempts = tokio::task::JoinSet::new();
    for address in addresses {
        // SocketAddr cannot trigger another DNS lookup, including retries.
        attempts.spawn(connect(address));
    }
    while let Some(attempt) = attempts.join_next().await {
        if let Ok(Ok(stream)) = attempt {
            return Ok(stream);
        }
    }
    anyhow::bail!("Host unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    struct Fake {
        calls: Arc<AtomicUsize>,
        answers: Vec<IpAddr>,
    }
    impl Resolver for Fake {
        fn resolve(&self, _: String) -> Resolution {
            let count = self.calls.fetch_add(1, Ordering::SeqCst);
            let answers = if count == 0 {
                self.answers.clone()
            } else {
                vec!["169.254.169.254".parse().unwrap()]
            };
            Box::pin(async move { Ok(answers) })
        }
    }
    #[tokio::test]
    async fn ticket_t_4_stalled_first_address_does_not_hide_reachable_answer() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let completed = tokio::time::timeout(
            Duration::from_millis(200),
            connect_addresses(
                vec![
                    "127.0.0.1:1".parse().unwrap(),
                    "127.0.0.2:1".parse().unwrap(),
                ],
                |address| {
                    let dropped = dropped.clone();
                    async move {
                        if address.ip().to_string() == "127.0.0.1" {
                            struct PendingAttempt(Arc<AtomicUsize>);
                            impl Drop for PendingAttempt {
                                fn drop(&mut self) {
                                    self.0.fetch_add(1, Ordering::SeqCst);
                                }
                            }
                            let _pending = PendingAttempt(dropped);
                            std::future::pending::<()>().await;
                        }
                        // Let the first attempt start before this one succeeds.
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        Ok(address)
                    }
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(completed.ip().to_string(), "127.0.0.2");
        tokio::time::timeout(Duration::from_millis(200), async {
            while dropped.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn special_purpose_ipv4_blocks_fail_before_connecting() {
        for (first, last) in [
            ("100.64.0.0", "100.127.255.255"),
            ("192.31.196.0", "192.31.196.255"),
            ("192.52.193.0", "192.52.193.255"),
            ("192.88.99.0", "192.88.99.255"),
            ("192.175.48.0", "192.175.48.255"),
            ("198.51.100.0", "198.51.100.255"),
            ("203.0.113.0", "203.0.113.255"),
        ] {
            for raw in [
                first.to_string(),
                last.to_string(),
                format!("::ffff:{first}"),
            ] {
                let ip: IpAddr = raw.parse().unwrap();
                assert!(!permitted(ip), "{raw}");
                let guard = HttpGuard::new(
                    Arc::new(Fake {
                        calls: Arc::default(),
                        answers: vec!["127.0.0.1".parse().unwrap(), ip],
                    }),
                    Duration::from_secs(1),
                    100,
                )
                .unwrap();
                let error = guard.get("http://printer.test:1/", None).await.unwrap_err();
                assert!(
                    error.to_string().contains("Prohibited DNS answer"),
                    "{raw}: {error}"
                );
                let literal = match ip {
                    IpAddr::V4(ip) => format!("http://{ip}:1/"),
                    IpAddr::V6(ip) => format!("http://[{ip}]:1/"),
                };
                let error = guard.get(&literal, None).await.unwrap_err();
                assert!(
                    error.to_string().contains("Prohibited DNS answer"),
                    "{raw}: {error}"
                );
            }
        }
        for raw in [
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.1.1",
            "127.0.0.1",
            "100.63.255.255",
            "100.128.0.0",
            "192.31.195.255",
            "192.31.197.0",
            "198.51.99.255",
            "198.51.101.0",
            "203.0.112.255",
            "203.0.114.0",
        ] {
            assert!(permitted(raw.parse().unwrap()), "{raw}");
        }
    }

    #[tokio::test]
    async fn moonraker_auth_requires_three_nonempty_jwt_segments() {
        for (key, expected_header, forbidden_header) in [
            (
                "abc.def.ghi",
                "authorization: Bearer abc.def.ghi",
                "x-api-key:",
            ),
            ("abc..def", "x-api-key: abc..def", "authorization:"),
            (".abc.def", "x-api-key: .abc.def", "authorization:"),
            ("abc.def.", "x-api-key: abc.def.", "authorization:"),
            ("..", "x-api-key: ..", "authorization:"),
            (
                "plain-api-key",
                "x-api-key: plain-api-key",
                "authorization:",
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 1024];
                    let count = stream.read(&mut chunk).await.unwrap();
                    assert!(count > 0);
                    bytes.extend_from_slice(&chunk[..count]);
                    if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                        break;
                    }
                }
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .await
                    .unwrap();
                String::from_utf8(bytes).unwrap()
            });
            let guard =
                HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(1), 100).unwrap();
            assert_eq!(
                guard
                    .get(&format!("http://{address}/"), Some(key))
                    .await
                    .unwrap()
                    .status,
                200
            );
            let request = server.await.unwrap();
            assert!(request.contains(expected_header), "{key}: {request}");
            assert!(!request.contains(forbidden_header), "{key}: {request}");
        }
    }

    #[tokio::test]
    async fn ticket_t_4_prohibited_and_mixed_dns_answers() {
        for ip in [
            "0.0.0.0",
            "0.1.2.3",
            "169.254.169.254",
            "169.254.1.1",
            "224.0.0.1",
            "255.255.255.255",
            "100.100.100.200",
            "192.0.0.1",
            "192.0.0.255",
            "192.0.2.1",
            "198.18.0.1",
            "198.19.255.254",
            "::ffff:192.0.2.1",
            "::",
            "fe80::1",
            "ff02::1",
            "fd00:ec2::254",
            "::ffff:169.254.169.254",
            "2002:a9fe:a9fe::1",
            "64:ff9b::a9fe:a9fe",
            "2001::1",
            "2001:1::1",
            "2001:2::1",
            "2001:3::1",
            "2001:4:112::1",
            "2001:10::1",
            "2001:20::1",
            "2001:30::1",
            "2001:1ff:ffff:ffff:ffff:ffff:ffff:ffff",
            "2001:db8::",
            "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff",
            "2620:4f:8000::",
            "2620:4f:8000:ffff:ffff:ffff:ffff:ffff",
            "3fff::",
            "3fff:fff:ffff:ffff:ffff:ffff:ffff:ffff",
        ] {
            assert!(!permitted(ip.parse().unwrap()), "{ip}");
            let guard = HttpGuard::new(
                Arc::new(Fake {
                    calls: Arc::default(),
                    answers: vec!["127.0.0.1".parse().unwrap(), ip.parse().unwrap()],
                }),
                Duration::from_secs(1),
                100,
            )
            .unwrap();
            assert!(
                guard
                    .get("http://printer.test:1/", None)
                    .await
                    .unwrap_err()
                    .to_string()
                    .contains("Prohibited")
            );
        }
        for ip in [
            "127.0.0.1",
            "192.168.1.4",
            "192.0.1.1",
            "198.17.0.1",
            "198.20.0.1",
            "10.0.0.1",
            "::1",
            "fd12::1",
            "2606:4700::1111",
            "2001:200::1",
            "2001:db7:ffff:ffff:ffff:ffff:ffff:ffff",
            "2001:db9::",
            "2620:4f:7fff:ffff:ffff:ffff:ffff:ffff",
            "2620:4f:8001::",
            "3ffe:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
        ] {
            assert!(permitted(ip.parse().unwrap()));
        }
    }
    #[tokio::test]
    async fn ticket_t_4_pins_dns_and_rechecks_next_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let calls = Arc::new(AtomicUsize::new(0));
        let guard = HttpGuard::new(
            Arc::new(Fake {
                calls: calls.clone(),
                answers: vec!["127.0.0.1".parse().unwrap()],
            }),
            Duration::from_secs(1),
            100,
        )
        .unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut data = [0; 1024];
            let len = socket.read(&mut data).await.unwrap();
            let request = String::from_utf8_lossy(&data[..len]);
            assert!(request.contains("host: printer.test:"));
            assert!(request.starts_with("GET /printer/objects/query"));
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}")
                .await
                .unwrap();
        });
        let url = format!("http://printer.test:{port}/printer/objects/query");
        assert_eq!(guard.get(&url, None).await.unwrap().body, b"{}");
        server.await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(guard.get(&url, None).await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn ticket_t_4_deadline_and_body_limit_and_no_redirect() {
        for mode in [
            "stall",
            "large",
            "redirect",
            "redirect-large",
            "redirect-stall",
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = [0; 1024];
                let _ = socket.read(&mut data).await;
                match mode {
                    "large" => {
                        let _ = socket
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n123456789")
                            .await;
                    }
                    "redirect" => {
                        let _=socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\n\r\n").await;
                    }
                    "redirect-large" => {
                        let _ = socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 9\r\n\r\n123456789").await;
                    }
                    "redirect-stall" => {
                        socket.write_all(b"HTTP/1.1 302 Found\r\nLocation: /next\r\nContent-Length: 9\r\n\r\n").await.unwrap();
                        let mut end = [0; 1];
                        assert_eq!(socket.read(&mut end).await.unwrap(), 0);
                    }
                    _ => {
                        let mut end = [0; 1];
                        assert_eq!(socket.read(&mut end).await.unwrap(), 0);
                    }
                }
            });
            let guard =
                HttpGuard::new(Arc::new(SystemResolver), Duration::from_millis(100), 4).unwrap();
            let result = guard.get(&url, None).await;
            if mode.starts_with("redirect") {
                assert_eq!(result.unwrap().status, 302);
            } else {
                assert!(result.is_err());
            }
            tokio::time::timeout(Duration::from_secs(1), server)
                .await
                .unwrap()
                .unwrap();
        }
    }
    #[tokio::test]
    async fn ticket_t_4_dns_deadline_and_url_policy() {
        struct Hung;
        impl Resolver for Hung {
            fn resolve(&self, _: String) -> Resolution {
                Box::pin(std::future::pending())
            }
        }
        let guard = HttpGuard::new(Arc::new(Hung), Duration::from_millis(50), 1024).unwrap();
        let started = tokio::time::Instant::now();
        assert!(guard.get("http://printer.test/", None).await.is_err());
        assert!(started.elapsed() < Duration::from_millis(250));
        for url in [
            "https://printer.test/",
            "http://user:password@printer.test/",
            "ftp://printer.test/",
            "/printer/objects/query",
        ] {
            assert!(guard.get(url, None).await.is_err());
        }
    }
    #[tokio::test]
    async fn ticket_t_4_sleep_does_not_extend_socket_deadline() {
        struct JumpClock(AtomicUsize);
        impl WallClock for JumpClock {
            fn now(&self) -> SystemTime {
                SystemTime::UNIX_EPOCH + Duration::from_secs(self.0.load(Ordering::SeqCst) as u64)
            }
        }
        let clock = Arc::new(JumpClock(AtomicUsize::new(0)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let mut guard =
            HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(5), 1024).unwrap();
        guard.clock = clock.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut data = [0; 1024];
            let _ = socket.read(&mut data).await.unwrap();
            clock.0.store(60, Ordering::SeqCst); // OS sleep advances wall time without Tokio time.
            let mut end = [0; 1];
            assert_eq!(socket.read(&mut end).await.unwrap(), 0);
        });
        let started = tokio::time::Instant::now();
        assert!(guard.get(&url, None).await.is_err());
        assert!(started.elapsed() < Duration::from_millis(250));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn ticket_t_21_post_deadline_and_policy_failures_are_unsent() {
        let guard = HttpGuard::new(
            Arc::new(Fake {
                calls: Arc::default(),
                answers: vec!["169.254.169.254".parse().unwrap()],
            }),
            Duration::from_secs(1),
            100,
        )
        .unwrap();
        for deadline in [Duration::ZERO, MAX_POST_DEADLINE + Duration::from_millis(1)] {
            let error = guard
                .post("http://127.0.0.1:1/", None, None, deadline)
                .await
                .unwrap_err();
            assert!(matches!(error, PostError::Unsent(_)), "{error}");
        }
        for url in [
            "http://printer.test/upload",
            "https://printer.test/upload",
            "http://user:pw@printer.test/upload",
        ] {
            let error = guard
                .post(url, None, None, MAX_POST_DEADLINE)
                .await
                .unwrap_err();
            assert!(matches!(error, PostError::Unsent(_)), "{url}: {error}");
        }
    }

    #[tokio::test]
    async fn ticket_t_22_request_sends_headers_and_keeps_the_challenge() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/v1/status", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            socket
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest nonce=\"n\"\r\nWWW-Authenticate: Basic\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            String::from_utf8(bytes).unwrap()
        });
        let guard = HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(1), 100).unwrap();
        let reply = guard
            .request(
                "GET",
                &url,
                &[("authorization", "Digest x")],
                None,
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(reply.status, 401);
        assert_eq!(
            reply.www_authenticate.as_deref(),
            Some("Digest nonce=\"n\", Basic")
        );
        let request = server.await.unwrap();
        assert!(
            request.starts_with("GET /api/v1/status HTTP/1.1\r\n"),
            "{request}"
        );
        assert!(
            request.contains("\r\nauthorization: Digest x\r\n"),
            "{request}"
        );
        assert!(!request.contains("content-length"), "{request}");

        for (method, headers) in [
            ("POST", &[][..]),
            ("PUT", &[("Content-Length", "1")][..]),
            ("PUT", &[("host", "elsewhere")][..]),
        ] {
            let error = guard
                .request(method, &url, headers, None, Duration::from_secs(1))
                .await
                .unwrap_err();
            assert!(matches!(error, PostError::Unsent(_)), "{method}: {error}");
        }
    }

    #[tokio::test]
    async fn ticket_t_21_post_streams_exact_length_once() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/server/files/upload",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"hello") {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
            }
            socket
                .write_all(b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\n\r\n{}")
                .await
                .unwrap();
            // A second request on any socket would be a resend.
            assert!(
                tokio::time::timeout(Duration::from_millis(200), listener.accept())
                    .await
                    .is_err()
            );
            String::from_utf8(bytes).unwrap()
        });
        let guard = HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(1), 100).unwrap();
        let body = PostBody {
            content_type: "text/plain".into(),
            reader: Box::pin(&b"hello and bytes past the declared length"[..]),
            length: 5,
        };
        let reply = guard
            .post(&url, Some("key"), Some(body), Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!((reply.status, reply.body), (201, b"{}".to_vec()));
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /server/files/upload HTTP/1.1\r\n"));
        for header in [
            "content-type: text/plain",
            "content-length: 5",
            "x-api-key: key",
        ] {
            assert!(request.contains(&format!("\r\n{header}\r\n")), "{request}");
        }
        assert!(!request.contains("transfer-encoding"), "{request}");
    }

    #[tokio::test]
    async fn ticket_t_21_post_short_source_is_source_failed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/server/files/upload",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut chunk = [0; 1024];
            // Read until the client gives up on the short body and closes.
            while let Ok(count) = socket.read(&mut chunk).await {
                if count == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..count]);
            }
            String::from_utf8(bytes).unwrap()
        });
        let guard = HttpGuard::new(Arc::new(SystemResolver), Duration::from_secs(1), 100).unwrap();
        let body = PostBody {
            content_type: "text/plain".into(),
            reader: Box::pin(&b"short"[..]),
            length: 50,
        };
        let error = guard
            .post(&url, None, Some(body), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(error, PostError::SourceFailed(_)), "{error}");
        // hyper may drop the head along with the failed body; either way the
        // printer never receives the 50 declared bytes.
        let request = server.await.unwrap();
        let body = request.split_once("\r\n\r\n").map_or("", |(_, body)| body);
        assert!(body.len() < 50, "{request:?}");
    }
}
