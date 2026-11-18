//! T-20-network (GRE-455): checked outbound HTTPS and raw sockets against fake
//! DNS, a local TLS server, and an ambient proxy that must never be used.
//! Every server listens on loopback; no test leaves the machine.

use pp_net::{
    network::{CheckedDns, Policy, checked_client, checked_client_trusting, tls_config},
    outbound::{HttpGuard, Resolution, Resolver},
};
use reqwest::dns::{Name, Resolve};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const OK: &str = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok";

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

/// Answers from a fixed table and records every name it was asked for.
#[derive(Clone, Default)]
struct FakeDns {
    answers: HashMap<&'static str, Vec<IpAddr>>,
    asked: Arc<Mutex<Vec<String>>>,
}

impl FakeDns {
    fn with(mut self, name: &'static str, answer: &[&str]) -> Self {
        self.answers
            .insert(name, answer.iter().map(|text| ip(text)).collect());
        self
    }
    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
    fn checked(&self, policy: Policy) -> CheckedDns {
        CheckedDns::new(Arc::new(self.clone()), policy, "Refused DNS answer")
    }
}

impl Resolver for FakeDns {
    fn resolve(&self, host: String) -> Resolution {
        self.asked.lock().unwrap().push(host.clone());
        let answer = self.answers.get(host.as_str()).cloned();
        Box::pin(async move { answer.ok_or_else(|| anyhow::anyhow!("no such name")) })
    }
}

/// Every message in an error's source chain, joined.
fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(next) = source {
        text = format!("{text}: {next}");
        source = next.source();
    }
    text
}

/// Reads one request head and returns its path.
async fn request_path(socket: &mut TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0; 1];
    while !head.ends_with(b"\r\n\r\n") && socket.read(&mut byte).await.unwrap_or(0) == 1 {
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    head.split(' ').nth(1).unwrap_or_default().to_owned()
}

/// A plain HTTP server on loopback. `reply` maps a path to the raw response;
/// every requested path is recorded.
async fn http_server(
    reply: impl Fn(&str, u16) -> String + Send + Sync + 'static,
) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let paths = Arc::new(Mutex::new(Vec::new()));
    let seen = paths.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let path = request_path(&mut socket).await;
            seen.lock().unwrap().push(path.clone());
            let _ = socket.write_all(reply(&path, port).as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (port, paths)
}

/// A local TLS server whose certificate names only `name`, issued by a test
/// CA. It records the server name each ClientHello asked for.
struct TlsServer {
    ca: CertificateDer<'static>,
    port: u16,
    names: Arc<Mutex<Vec<String>>>,
}

impl TlsServer {
    async fn start(name: &str) -> Self {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec![name.to_owned()])
            .unwrap()
            .signed_by(&key, &issuer)
            .unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
        .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let names = Arc::new(Mutex::new(Vec::new()));
        let seen = names.clone();
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (config, seen) = (config.clone(), seen.clone());
                tokio::spawn(async move {
                    let acceptor = tokio_rustls::LazyConfigAcceptor::new(
                        rustls::server::Acceptor::default(),
                        socket,
                    );
                    let Ok(start) = acceptor.await else { return };
                    let hello = start.client_hello();
                    let name = hello.server_name().unwrap_or_default().to_owned();
                    seen.lock().unwrap().push(name);
                    let Ok(mut stream) = start.into_stream(config).await else {
                        return;
                    };
                    let mut buffer = [0; 1024];
                    let _ = stream.read(&mut buffer).await;
                    let _ = stream.write_all(OK.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            ca: ca.der().clone(),
            port,
            names,
        }
    }

    fn names(&self) -> Vec<String> {
        self.names.lock().unwrap().clone()
    }
}

#[test]
fn ticket_t_20_network_policies_split_metadata_private_and_public() {
    let metadata = [
        "169.254.169.254",
        "fd00:ec2::254",
        "100.100.100.200",
        "::ffff:169.254.169.254",
        "fe80::1",
        "0.0.0.0",
        "224.0.0.1",
        "192.0.2.1",
    ];
    for text in metadata {
        assert!(!Policy::Lan.allows(ip(text)), "{text}");
        assert!(!Policy::Public.allows(ip(text)), "{text}");
    }
    let private = [
        "127.0.0.1",
        "10.0.0.1",
        "172.16.0.1",
        "192.168.1.2",
        "::1",
        "fd00::1",
        "::ffff:10.0.0.1",
    ];
    for text in private {
        assert!(Policy::Lan.allows(ip(text)), "{text}");
        assert!(!Policy::Public.allows(ip(text)), "{text}");
    }
    for text in ["140.82.112.6", "2606:50c0:8000::153", "::ffff:140.82.112.6"] {
        assert!(Policy::Lan.allows(ip(text)), "{text}");
        assert!(Policy::Public.allows(ip(text)), "{text}");
    }
}

#[tokio::test]
async fn ticket_t_20_network_dns_refuses_metadata_mixed_empty_and_oversized_answers() {
    let name = || "printer.test".parse::<Name>().unwrap();
    let oversized: Vec<String> = (1..=33).map(|n| format!("10.0.0.{n}")).collect();
    let oversized: Vec<&str> = oversized.iter().map(String::as_str).collect();
    let refused: [(&[&str], Policy); 5] = [
        (&["169.254.169.254"], Policy::Lan),
        (&["10.0.0.5", "169.254.169.254"], Policy::Lan),
        (&[], Policy::Lan),
        (&oversized, Policy::Lan),
        (&["140.82.112.6", "10.0.0.5"], Policy::Public),
    ];
    for (answer, policy) in refused {
        let dns = FakeDns::default()
            .with("printer.test", answer)
            .checked(policy);
        let error = dns.resolve(name()).await.err().unwrap();
        assert_eq!(error.to_string(), "Refused DNS answer", "{answer:?}");
    }
    let dns = FakeDns::default()
        .with("printer.test", &["10.0.0.5", "::1"])
        .checked(Policy::Lan);
    let addresses: Vec<SocketAddr> = dns.resolve(name()).await.unwrap().collect();
    assert_eq!(
        addresses,
        ["10.0.0.5:0".parse().unwrap(), "[::1]:0".parse().unwrap()]
    );
}

#[tokio::test]
async fn ticket_t_20_network_every_redirect_hop_resolves_through_checked_dns() {
    let (port, paths) = http_server(|path, port| match path {
        "/to-lan" => format!(
            "HTTP/1.1 302 Found\r\nlocation: http://lan.test:{port}/done\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        ),
        "/to-metadata" => format!(
            "HTTP/1.1 302 Found\r\nlocation: http://metadata.test:{port}/stolen\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        ),
        _ => OK.to_owned(),
    })
    .await;
    // metadata.test also answers with the loopback server, so a hop that
    // skipped the check would reach it and record `/stolen`.
    let dns = FakeDns::default()
        .with("printer.test", &["127.0.0.1"])
        .with("lan.test", &["127.0.0.1"])
        .with("metadata.test", &["127.0.0.1", "169.254.169.254"]);
    let client = checked_client(dns.checked(Policy::Lan))
        .unwrap()
        .redirect(reqwest::redirect::Policy::limited(3))
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let reply = client
        .get(format!("http://printer.test:{port}/to-lan"))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 200);
    assert_eq!(reply.text().await.unwrap(), "ok");

    let error = client
        .get(format!("http://printer.test:{port}/to-metadata"))
        .send()
        .await
        .unwrap_err();
    assert!(chain(&error).contains("Refused DNS answer"), "{error:?}");
    assert_eq!(*paths.lock().unwrap(), ["/to-lan", "/done", "/to-metadata"]);
    assert_eq!(
        dns.asked(),
        ["printer.test", "lan.test", "printer.test", "metadata.test"]
    );
}

#[tokio::test]
async fn ticket_t_20_network_tls_verifies_the_original_host_name() {
    let server = TlsServer::start("printer.test").await;
    let port = server.port;
    let dns = FakeDns::default()
        .with("printer.test", &["127.0.0.1"])
        .with("other.test", &["127.0.0.1"]);
    let client = checked_client_trusting(dns.checked(Policy::Lan), vec![server.ca.clone()])
        .unwrap()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let reply = client
        .get(format!("https://printer.test:{port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), 200);
    assert_eq!(reply.text().await.unwrap(), "ok");

    // Same address, different URL host: the certificate is checked against
    // the URL's host, so the connection is refused.
    let error = client
        .get(format!("https://other.test:{port}/"))
        .send()
        .await
        .unwrap_err();
    assert!(chain(&error).contains("not valid for name"), "{error:?}");

    // Without the test CA only the platform roots are trusted.
    let untrusted = checked_client(dns.checked(Policy::Lan))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let error = untrusted
        .get(format!("https://printer.test:{port}/"))
        .send()
        .await
        .unwrap_err();
    assert!(chain(&error).contains("certificate"), "{error:?}");

    assert_eq!(
        server.names(),
        ["printer.test", "other.test", "printer.test"]
    );
}

#[tokio::test]
async fn ticket_t_20_network_tls_names_aws_lc_rs_over_a_different_process_default() {
    // A process default with no cipher suites cannot complete any handshake.
    // The checked client must not pick it up.
    let crippled = rustls::crypto::CryptoProvider {
        cipher_suites: Vec::new(),
        ..rustls::crypto::aws_lc_rs::default_provider()
    };
    crippled.install_default().unwrap();

    let expected = rustls::crypto::aws_lc_rs::default_provider();
    let config = tls_config(Vec::new()).unwrap();
    let suites = |provider: &rustls::crypto::CryptoProvider| -> Vec<_> {
        provider.cipher_suites.iter().map(|s| s.suite()).collect()
    };
    assert_eq!(suites(config.crypto_provider()), suites(&expected));
    assert!(!suites(config.crypto_provider()).is_empty());
    assert_eq!(config.alpn_protocols, [b"http/1.1".to_vec()]);

    let server = TlsServer::start("printer.test").await;
    let dns = FakeDns::default().with("printer.test", &["127.0.0.1"]);
    let url = format!("https://printer.test:{}/", server.port);
    let checked = checked_client_trusting(dns.checked(Policy::Lan), vec![server.ca.clone()])
        .unwrap()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    assert_eq!(checked.get(&url).send().await.unwrap().status(), 200);

    // Control: a stock reqwest client uses the crippled default and fails.
    let stock = reqwest::Client::builder()
        .no_proxy()
        .dns_resolver(Arc::new(dns.checked(Policy::Lan)))
        .tls_certs_only([reqwest::Certificate::from_der(&server.ca).unwrap()])
        .timeout(Duration::from_secs(5))
        .build();
    let stock = match stock {
        Ok(client) => client.get(&url).send().await.map(|_| ()),
        Err(error) => Err(error),
    };
    assert!(stock.is_err());
}

#[tokio::test]
async fn ticket_t_20_network_raw_sockets_connect_only_to_checked_addresses() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let dns = FakeDns::default()
        .with("broker.test", &["127.0.0.1"])
        .with("metadata.test", &["127.0.0.1", "169.254.169.254"]);
    let guard = HttpGuard::new(Arc::new(dns.clone()), Duration::from_secs(2), 1024).unwrap();

    let stream = guard.connect("broker.test", port).await.unwrap();
    assert_eq!(
        stream.peer_addr().unwrap(),
        SocketAddr::from(([127, 0, 0, 1], port))
    );
    listener.accept().await.unwrap();

    for host in ["metadata.test", "169.254.169.254", "::ffff:169.254.169.254"] {
        let error = guard.connect(host, port).await.unwrap_err();
        assert_eq!(error.to_string(), "Prohibited DNS answer", "{host}");
    }
    // Literal hosts are checked without DNS, and nothing else connected.
    assert_eq!(dns.asked(), ["broker.test", "metadata.test"]);
    let accepted = tokio::time::timeout(Duration::from_millis(200), listener.accept()).await;
    assert!(accepted.is_err());
}

/// Set only on the child process that
/// `ticket_t_20_network_ambient_proxies_are_never_used` starts.
const PROXY_CHILD: &str = "PP_NET_PROXY_CHILD";
const PROXY_ORIGIN: &str = "PP_NET_PROXY_ORIGIN";
const PROXY_VARIABLES: [&str; 6] = [
    "HTTP_PROXY",
    "http_proxy",
    "HTTPS_PROXY",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// This test binary again, running only `proxy_environment_child`, with the
/// proxy variables set on that child process alone.
fn proxy_child(mode: &str, origin: &str, proxy: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "proxy_environment_child", "--nocapture"])
        .env(PROXY_CHILD, mode)
        .env(PROXY_ORIGIN, origin)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy");
    for name in PROXY_VARIABLES {
        command.env(name, proxy);
    }
    command
}

/// The client half of the proxy test. Without `PP_NET_PROXY_CHILD` it
/// returns at once, so a normal run passes it.
#[tokio::test]
async fn proxy_environment_child() {
    let Ok(mode) = std::env::var(PROXY_CHILD) else {
        return;
    };
    let origin = std::env::var(PROXY_ORIGIN).unwrap();
    let builder = if mode == "checked" {
        let dns = FakeDns::default().with("printer.test", &["127.0.0.1"]);
        checked_client(dns.checked(Policy::Lan)).unwrap()
    } else {
        reqwest::Client::builder()
    };
    let client = builder.timeout(Duration::from_secs(2)).build().unwrap();
    let sent = client.get(&origin).send().await;
    if mode == "checked" {
        assert_eq!(sent.unwrap().status(), 200);
    }
}

#[tokio::test]
async fn ticket_t_20_network_ambient_proxies_are_never_used() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let (port, paths) = http_server(|_, _| OK.to_owned()).await;
    let origin = format!("http://printer.test:{port}/direct");

    let checked = proxy_child("checked", &origin, &proxy_url)
        .status()
        .await
        .unwrap();
    assert!(checked.success(), "the checked child failed");
    assert_eq!(*paths.lock().unwrap(), ["/direct"]);
    let reached = tokio::time::timeout(Duration::from_millis(200), proxy.accept()).await;
    assert!(reached.is_err(), "the checked client used the proxy");

    // Control: a stock client in the same environment goes to the proxy.
    let mut stock = proxy_child("stock", &origin, &proxy_url);
    let (reached, stock) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(5), proxy.accept()),
        stock.status()
    );
    assert!(stock.unwrap().success());
    assert!(reached.is_ok(), "the proxy fixture was not live");
    assert_eq!(*paths.lock().unwrap(), ["/direct"]);
}

/// Reads until the peer closes; `true` if it closed within `limit`.
async fn closed_within(socket: &mut TcpStream, limit: Duration) -> bool {
    let mut buffer = [0; 1024];
    tokio::time::timeout(limit, async {
        while socket.read(&mut buffer).await.unwrap_or(0) > 0 {}
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn ticket_t_20_network_deadline_and_cancellation_release_a_stalled_handshake() {
    // Accepts TCP and never answers the ClientHello.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let dns = FakeDns::default().with("printer.test", &["127.0.0.1"]);
    let url = format!("https://printer.test:{port}/");

    let bounded = checked_client(dns.checked(Policy::Lan))
        .unwrap()
        .timeout(Duration::from_millis(300))
        .build()
        .unwrap();
    let started = Instant::now();
    let error = bounded.get(&url).send().await.unwrap_err();
    assert!(error.is_timeout(), "{error:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    let (mut socket, _) = listener.accept().await.unwrap();
    assert!(closed_within(&mut socket, Duration::from_secs(2)).await);

    // A caller that drops the request also closes the socket.
    let unbounded = checked_client(dns.checked(Policy::Lan))
        .unwrap()
        .build()
        .unwrap();
    let dropped =
        tokio::time::timeout(Duration::from_millis(300), unbounded.get(&url).send()).await;
    assert!(dropped.is_err());
    let (mut socket, _) = listener.accept().await.unwrap();
    assert!(closed_within(&mut socket, Duration::from_secs(2)).await);
}
