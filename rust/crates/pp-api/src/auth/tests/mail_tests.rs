use super::*;
use crate::auth::{AuthHttpConfig, CookieTransport, ProviderClient, auth_router};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Clone, Copy)]
enum Mode {
    Success,
    Reject,
    DropAfterData,
    Blocked,
}
struct SmtpFake {
    port: u16,
    messages: Arc<Mutex<Vec<(String, String, String)>>>,
    connections: Arc<AtomicUsize>,
    release: Arc<tokio::sync::Semaphore>,
    task: tokio::task::JoinHandle<()>,
}
impl SmtpFake {
    async fn start(mode: Mode) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let messages = Arc::new(Mutex::new(Vec::new()));
        let captured = messages.clone();
        let connections = Arc::new(AtomicUsize::new(0));
        let accepted = connections.clone();
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let smtp_release = release.clone();
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                accepted.fetch_add(1, Ordering::Relaxed);
                let captured = captured.clone();
                let release = smtp_release.clone();
                tokio::spawn(async move {
                    let mut stream = BufReader::new(stream);
                    stream
                        .get_mut()
                        .write_all(b"220 fixture SMTP\r\n")
                        .await
                        .unwrap();
                    let mut from = String::new();
                    let mut to = String::new();
                    loop {
                        let mut line = String::new();
                        if stream.read_line(&mut line).await.unwrap() == 0 {
                            break;
                        }
                        let response = if line.starts_with("EHLO") {
                            "250-fixture\r\n250 8BITMIME\r\n"
                        } else if line.starts_with("MAIL FROM:") {
                            from = line.trim().to_owned();
                            "250 sender accepted\r\n"
                        } else if line.starts_with("RCPT TO:") {
                            to = line.trim().to_owned();
                            "250 recipient accepted\r\n"
                        } else if line.starts_with("DATA") {
                            stream
                                .get_mut()
                                .write_all(b"354 send data\r\n")
                                .await
                                .unwrap();
                            let mut message = String::new();
                            loop {
                                let mut part = String::new();
                                if stream.read_line(&mut part).await.unwrap() == 0 {
                                    return;
                                }
                                if part == ".\r\n" {
                                    break;
                                }
                                message.push_str(&part);
                            }
                            captured
                                .lock()
                                .unwrap()
                                .push((from.clone(), to.clone(), message));
                            match mode {
                                Mode::Blocked => {
                                    let _permit = release.acquire().await.unwrap();
                                    "250 queued\r\n"
                                }
                                Mode::Success => "250 queued\r\n",
                                Mode::Reject => "550 rejected\r\n",
                                Mode::DropAfterData => return,
                            }
                        } else if line.starts_with("QUIT") {
                            let _ = stream.get_mut().write_all(b"221 bye\r\n").await;
                            break;
                        } else {
                            "250 OK\r\n"
                        };
                        if stream
                            .get_mut()
                            .write_all(response.as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
        });
        Self {
            port,
            messages,
            connections,
            release,
            task,
        }
    }
    fn mailer(&self) -> ResetMailer {
        ResetMailer::new(Some((
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
                .port(self.port)
                .timeout(Some(Duration::from_secs(5)))
                .build(),
            "Print Partner <fixture@example.com>".parse().unwrap(),
        )))
    }
    async fn close(self) {
        self.release.add_permits(16);
        self.task.abort();
        let _ = self.task.await;
    }
}
fn directory() -> crate::scratch::Scratch {
    crate::scratch::Scratch::new("pp-mail-")
}

#[tokio::test]
async fn smtp_delivers_real_envelope_subject_link_and_reports_uncertain_send() {
    assert_eq!(
        ResetMailer::disabled()
            .deliver(
                "fixture@example.com",
                "https://canonical.example/reset-password?token=fixture"
            )
            .await,
        Delivery::Unsent
    );
    for (mode, expected) in [
        (Mode::Success, Delivery::Sent),
        (Mode::Reject, Delivery::FailedOrUnknown),
        (Mode::DropAfterData, Delivery::FailedOrUnknown),
    ] {
        let fake = SmtpFake::start(mode).await;
        let delivery = fake
            .mailer()
            .deliver(
                "normalized@example.com",
                "https://canonical.example/reset-password?token=fixture-only",
            )
            .await;
        assert_eq!(delivery, expected);
        assert_eq!(fake.connections.load(Ordering::Relaxed), 1);
        {
            let messages = fake.messages.lock().unwrap();
            assert_eq!(messages.len(), 1);
            let (from, to, body) = &messages[0];
            assert_eq!(from, "MAIL FROM:<fixture@example.com>");
            assert_eq!(to, "RCPT TO:<normalized@example.com>");
            assert!(body.contains("Subject: Reset your Print Partner password"));
            assert!(body.contains("valid for 1 hour"));
            assert!(
                body.replace("=\r\n", "")
                    .replace("=3D", "=")
                    .contains("https://canonical.example/reset-password?token=fixture-only")
            );
        }
        fake.close().await;
    }
}

#[tokio::test]
async fn failure_counter_is_shared_and_disabled_mail_does_not_count() {
    let disabled = ResetMailer::disabled();
    assert_eq!(
        disabled
            .deliver("invalid", "https://example.com/private")
            .await,
        Delivery::Unsent
    );
    assert_eq!(disabled.delivery_failure_count(), 0);
    let fake = SmtpFake::start(Mode::Success).await;
    let mail = fake.mailer();
    assert_eq!(
        mail.clone()
            .deliver("invalid", "https://example.com/private")
            .await,
        Delivery::FailedOrUnknown
    );
    assert_eq!(mail.delivery_failure_count(), 1);
    assert_eq!(fake.connections.load(Ordering::Relaxed), 0);
    fake.close().await;
}

#[tokio::test]
async fn accepted_reset_delivery_drains_after_http_shutdown() {
    let fake = SmtpFake::start(Mode::Blocked).await;
    let directory = directory();
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    let auth = crate::auth::test_backend(
        &owner,
        AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mail = fake.mailer();
    let router = auth_router(
        AuthHttpConfig::new(
            &origin,
            CookieTransport::LoopbackHttp,
            true,
            Some("https://canonical.example"),
            false,
        )
        .unwrap(),
        auth,
        ProviderClient::new(None, None).unwrap(),
        mail.clone(),
    );
    let (stop, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let status = client
        .get(format!("{origin}/auth/status"))
        .header("Origin", &origin)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(status["recovery_email_available"], true);
    assert_eq!(
        client
            .post(format!("{origin}/auth/register"))
            .header("Origin", &origin)
            .json(&json!({"email":"shutdown@example.com", "password":"password-good"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .post(format!("{origin}/auth/forgot-password"))
            .header("Origin", &origin)
            .json(&json!({"email":"shutdown@example.com"}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while fake.messages.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    stop.send(()).unwrap();
    server.await.unwrap();
    let draining_mail = mail.clone();
    let mut drain = tokio::spawn(async move {
        draining_mail.drain_deliveries().await;
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut drain)
            .await
            .is_err()
    );
    fake.release.add_permits(1);
    tokio::time::timeout(DELIVERY_TIMEOUT + Duration::from_secs(1), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mail.delivery_failure_count(), 0);
    assert_eq!(fake.connections.load(Ordering::Relaxed), 1);
    mail.drain_deliveries().await;
    owner.shutdown().unwrap();
    drop(directory);
    fake.close().await;
}

struct CapturedLogs(Mutex<Vec<String>>);
static LOGS: CapturedLogs = CapturedLogs(Mutex::new(Vec::new()));
impl log::Log for CapturedLogs {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target() == "pp_api::reset_mail"
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.0
                .lock()
                .unwrap()
                .push(format!("{} {}", record.level(), record.args()));
        }
    }
    fn flush(&self) {}
}

#[tokio::test]
async fn smtp_http_failure_keeps_committed_token_without_enumeration_or_retry() {
    log::set_logger(&LOGS).unwrap();
    log::set_max_level(log::LevelFilter::Warn);
    let mut baseline = None;
    for mode in [
        Mode::Success,
        Mode::Reject,
        Mode::DropAfterData,
        Mode::Blocked,
    ] {
        let fake = SmtpFake::start(mode).await;
        let directory = directory();
        let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
        let auth = crate::auth::test_backend(
            &owner,
            AuthPolicy {
                registration: RegistrationPolicy::Open,
                session_tenant: SessionTenantPolicy::AccountTenant,
                first_user: FirstUserTenant::NewUser,
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mail = fake.mailer();
        let router = auth_router(
            AuthHttpConfig::new(
                &origin,
                CookieTransport::LoopbackHttp,
                true,
                Some("https://canonical.example/base"),
                true,
            )
            .unwrap(),
            auth,
            ProviderClient::new(None, None).unwrap(),
            mail.clone(),
        );
        let (stop, rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        assert_eq!(
            client
                .post(format!("{origin}/auth/register"))
                .header("Origin", &origin)
                .json(&json!({"email":"  NORMALIZED@example.com  ","password":"password-good"}))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let known = client
            .post(format!("{origin}/auth/forgot-password"))
            .header("Origin", &origin)
            .json(&json!({"email":" NORMALIZED@example.com "}))
            .send()
            .await
            .unwrap();
        assert_eq!(known.status(), 200);
        let known = known.bytes().await.unwrap();
        let unknown = client
            .post(format!("{origin}/auth/forgot-password"))
            .header("Origin", &origin)
            .json(&json!({"email":"unknown@example.com"}))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(known, unknown);
        if let Some(baseline) = &baseline {
            assert_eq!(&known, baseline);
        } else {
            baseline = Some(known.clone());
        }
        assert!(
            serde_json::from_slice::<Value>(&known)
                .unwrap()
                .get("dev_reset_url")
                .is_none()
        );
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let received = !fake.messages.lock().unwrap().is_empty();
                // LOGS is process-wide, so a parallel test can already contain
                // smtp_permanent. This attempt's counter is the local signal.
                let counted = !matches!(mode, Mode::Reject) || mail.delivery_failure_count() >= 1;
                let logged = !matches!(mode, Mode::Reject)
                    || LOGS
                        .0
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|line| line.contains("error_class=smtp_permanent"));
                if received && counted && logged {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        if matches!(mode, Mode::Blocked) {
            assert_eq!(fake.release.available_permits(), 0);
            // Both HTTP responses completed while the SMTP acknowledgement was blocked.
            fake.release.add_permits(1);
        }
        if matches!(mode, Mode::Reject) {
            assert_eq!(mail.delivery_failure_count(), 1);
        }
        if matches!(mode, Mode::Reject) {
            let logs = LOGS.0.lock().unwrap();
            assert!(logs.iter().any(|line| line.contains(
                "WARN Password reset mail delivery failed; provider=smtp error_class=smtp_permanent"
            )));
            assert!(logs.iter().all(|line| !line.contains('@')
                && !line.contains("token=")
                && !line.contains("canonical.example")));
        }
        assert_eq!(fake.connections.load(Ordering::Relaxed), 1);
        let token = {
            let messages = fake.messages.lock().unwrap();
            assert_eq!(messages.len(), 1);
            assert_eq!(messages[0].1, "RCPT TO:<normalized@example.com>");
            let body = messages[0].2.replace("=\r\n", "").replace("=3D", "=");
            let url = body
                .lines()
                .find(|v| v.starts_with("https://canonical.example/base/reset-password?"))
                .unwrap();
            let url = reqwest::Url::parse(url).unwrap();
            url.query_pairs()
                .find(|(k, _)| k == "token")
                .unwrap()
                .1
                .into_owned()
        };
        let reset = client
            .post(format!("{origin}/auth/reset-password"))
            .header("Origin", &origin)
            .json(&json!({"token":token,"password":"password-after"}))
            .send()
            .await
            .unwrap();
        assert_eq!(reset.status(), 200);
        // Known and unknown requests each reserve a slot for the same window,
        // even after SMTP succeeds, rejects, or drops the connection. This uses
        // all 20 credential requests in the rate window, matching the attack.
        for _ in 0..14 {
            let response = client
                .post(format!("{origin}/auth/forgot-password"))
                .header("Origin", &origin)
                .json(&json!({"email":"unknown@example.com"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(response.bytes().await.unwrap(), known);
        }
        let mut overload = None;
        for email in ["normalized@example.com", "unknown@example.com"] {
            let response = client
                .post(format!("{origin}/auth/forgot-password"))
                .header("Origin", &origin)
                .json(&json!({"email":email}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 503);
            let body = response.bytes().await.unwrap();
            if let Some(previous) = &overload {
                assert_eq!(&body, previous);
            } else {
                overload = Some(body);
            }
        }
        stop.send(()).unwrap();
        task.await.unwrap();
        owner.shutdown().unwrap();
        drop(directory);
        fake.close().await;
    }
}

#[tokio::test]
async fn smtp_without_canonical_origin_does_not_mint_or_send_even_in_dev() {
    let fake = SmtpFake::start(Mode::Success).await;
    let directory = directory();
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    let auth = owner.auth(FirstUserTenant::NewUser);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = auth_router(
        AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, true, None, true).unwrap(),
        auth,
        ProviderClient::new(None, None).unwrap(),
        fake.mailer(),
    );
    let (stop, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async {
            let _ = rx.await;
        })
        .await
        .unwrap();
    });
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    client
        .post(format!("{origin}/auth/register"))
        .header("Origin", &origin)
        .json(&json!({"email":"fixture@example.com","password":"password-good"}))
        .send()
        .await
        .unwrap();
    let response: Value = client
        .post(format!("{origin}/auth/forgot-password"))
        .header("Origin", &origin)
        .json(&json!({"email":"fixture@example.com"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(response.get("dev_reset_url").is_none());
    assert_eq!(fake.connections.load(Ordering::Relaxed), 0);
    stop.send(()).unwrap();
    task.await.unwrap();
    owner.shutdown().unwrap();
    let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM password_reset_tokens", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    drop(directory);
    fake.close().await;
}

#[tokio::test]
async fn smtp_rustls_encrypts_data_and_rejects_untrusted_certificate() {
    use lettre::transport::smtp::client::{Certificate, Tls, TlsParameters};
    for security in [SmtpSecurity::Tls, SmtpSecurity::StartTls] {
        let cert = include_bytes!("../../../tests/fixtures/loopback-cert.der").to_vec();
        let key = include_bytes!("../../../tests/fixtures/loopback-key.der").to_vec();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.clone().into()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.into()),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream =
                BufReader::new(accept_smtp_tls(stream, &acceptor, security).await.unwrap());
            if matches!(security, SmtpSecurity::Tls) {
                stream
                    .get_mut()
                    .write_all(b"220 fixture SMTP\r\n")
                    .await
                    .unwrap();
            }
            let mut message = String::new();
            loop {
                let mut line = String::new();
                if stream.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                if line.starts_with("DATA") {
                    stream
                        .get_mut()
                        .write_all(b"354 send data\r\n")
                        .await
                        .unwrap();
                    loop {
                        let mut line = String::new();
                        stream.read_line(&mut line).await.unwrap();
                        if line == ".\r\n" {
                            break;
                        }
                        message.push_str(&line);
                    }
                }
                if line.starts_with("QUIT") {
                    stream.get_mut().write_all(b"221 bye\r\n").await.unwrap();
                    break;
                }
                stream.get_mut().write_all(b"250 OK\r\n").await.unwrap();
            }
            let (stream, _) = listener.accept().await.unwrap();
            assert!(accept_smtp_tls(stream, &acceptor, security).await.is_err());
            message
        });
        let tls = TlsParameters::builder("localhost".into())
            .add_root_certificate(Certificate::from_der(cert).unwrap())
            .build_rustls()
            .unwrap();
        let mailer = ResetMailer::new(Some((
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
                .port(port)
                .tls(match security {
                    SmtpSecurity::Tls => Tls::Wrapper(tls),
                    SmtpSecurity::StartTls => Tls::Required(tls),
                })
                .build(),
            "fixture@example.com".parse().unwrap(),
        )));
        assert_eq!(
            mailer
                .deliver(
                    "recipient@example.com",
                    "https://canonical.example/reset-password?token=tls-fixture"
                )
                .await,
            Delivery::Sent
        );
        let production = ResetMailer::smtp(SmtpConfig {
            security,
            host: "127.0.0.1".into(),
            port,
            from: "fixture@example.com".into(),
            credentials: None,
        })
        .unwrap();
        assert_eq!(
            production
                .deliver(
                    "recipient@example.com",
                    "https://canonical.example/reset-password?token=untrusted"
                )
                .await,
            Delivery::FailedOrUnknown
        );
        let message = task.await.unwrap();
        assert!(message.contains("Subject: Reset your Print Partner password"));
        assert!(message.contains("tls-fixture"));
        assert!(!message.contains("untrusted"));
    }
}

async fn accept_smtp_tls(
    stream: tokio::net::TcpStream,
    acceptor: &tokio_rustls::TlsAcceptor,
    security: SmtpSecurity,
) -> std::io::Result<tokio_rustls::server::TlsStream<tokio::net::TcpStream>> {
    let stream = if matches!(security, SmtpSecurity::StartTls) {
        let mut stream = BufReader::new(stream);
        stream.get_mut().write_all(b"220 fixture SMTP\r\n").await?;
        let mut line = String::new();
        stream.read_line(&mut line).await?;
        assert!(line.starts_with("EHLO"));
        stream
            .get_mut()
            .write_all(b"250-fixture\r\n250 STARTTLS\r\n")
            .await?;
        line.clear();
        stream.read_line(&mut line).await?;
        assert_eq!(line, "STARTTLS\r\n");
        stream.get_mut().write_all(b"220 Ready for TLS\r\n").await?;
        stream.into_inner()
    } else {
        stream
    };
    acceptor.accept(stream).await
}
