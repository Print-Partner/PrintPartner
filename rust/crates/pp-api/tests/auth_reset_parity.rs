//! Replays Node's forgot-password golden. Known accounts, unknown accounts, and
//! a rejected loopback delivery share one public response. The mail-failure log
//! is the existing redacted class line; this test does not add another logger.
//! The pooled client runs by default and the rebuild client runs with
//! `desktop_rebuild`.

use pp_api::auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

const FIXTURE: &str = include_str!("../../../../fixtures/parity/auth-reset/node.json");
const KNOWN: &str = "known@reset.example.test";
const UNKNOWN: &str = "unknown@reset.example.test";
const PASSWORD: &str = "pp-fixture-reset-password";
const LOG_PREFIX: &str = "WARN Password reset mail delivery failed; provider=smtp error_class=";

#[derive(Debug, Deserialize)]
struct Fixture {
    public_response: PublicResponse,
    allowed_error_classes: Vec<String>,
    cases: Vec<Case>,
}
#[derive(Debug, Deserialize)]
struct PublicResponse {
    status: u16,
    body: Value,
}
#[derive(Debug, Deserialize)]
struct Case {
    id: String,
    deliveries: u64,
    reset_tokens: i64,
    error_class: Option<String>,
}

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE).expect("auth-reset golden")
}

fn is_non_enumerating(status: u16, body: &Value) -> bool {
    let text = body.to_string();
    status == 200
        && body.get("ok") == Some(&json!(true))
        && body.get("message")
            == Some(&json!(
                "If an account exists for that email, a reset link was sent."
            ))
        && body.as_object().is_some_and(|object| object.len() == 2)
        && !text.contains('@')
        && !text.contains("550")
        && !text.contains("EMESSAGE")
        && !text.contains("token=")
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
                .expect("log lock")
                .push(format!("{} {}", record.level(), record.args()));
        }
    }
    fn flush(&self) {}
}

#[derive(Clone, Copy)]
enum Mode {
    Accept,
    Reject,
}
struct SmtpFake {
    port: u16,
    messages: Arc<Mutex<Vec<(String, String, String)>>>,
    connections: Arc<AtomicUsize>,
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
        let task = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                accepted.fetch_add(1, Ordering::Relaxed);
                let captured = captured.clone();
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
                            captured.lock().expect("message lock").push((
                                from.clone(),
                                to.clone(),
                                message,
                            ));
                            match mode {
                                Mode::Accept => "250 queued\r\n",
                                Mode::Reject => "550 rejected\r\n",
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
            task,
        }
    }
    fn close(self) {
        self.task.abort();
    }
}

struct Server {
    origin: String,
    client: reqwest::Client,
    owner: WriterOwner,
    db: rusqlite::Connection,
    mail: ResetMailer,
    task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    smtp: SmtpFake,
    /// Last field: removed only after the storage owner has dropped, even on panic.
    _directory: Scratch,
}
impl Server {
    async fn start(mode: Mode) -> Self {
        let directory = Scratch::new("pp-auth-reset-");
        let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
        let policy = AuthPolicy {
            registration: RegistrationPolicy::Open,
            first_user: FirstUserTenant::NewUser,
            session_tenant: SessionTenantPolicy::AccountTenant,
        };
        #[cfg(feature = "desktop_rebuild")]
        let auth = owner
            .rebuild_auth_with_policy(policy, Default::default())
            .unwrap();
        #[cfg(not(feature = "desktop_rebuild"))]
        let auth = owner.auth_with_policy(policy).unwrap();
        let smtp = SmtpFake::start(mode).await;
        let mail = ResetMailer::loopback_cleartext_for_tests(smtp.port).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
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
        let (shutdown, rx) = tokio::sync::oneshot::channel();
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
        let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        Self {
            origin,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            owner,
            db,
            mail,
            task,
            shutdown,
            _directory: directory,
            smtp,
        }
    }
    async fn close(self) {
        self.shutdown.send(()).unwrap();
        self.task.await.unwrap();
        drop(self.db);
        self.owner.shutdown().unwrap();
        self.smtp.close();
    }
    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let response = self
            .client
            .post(format!("{}{path}", self.origin))
            .header("Origin", &self.origin)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = pp_api::problem::without_envelope(response.json().await.unwrap());
        (status, body)
    }
    fn reset_tokens(&self) -> i64 {
        self.db
            .query_row("SELECT count(*) FROM password_reset_tokens", [], |row| {
                row.get(0)
            })
            .unwrap()
    }
}

fn logged_class(allowed: &[String]) -> Option<String> {
    let logs = LOGS.0.lock().expect("log lock");
    match logs.as_slice() {
        [] => None,
        [line] => {
            assert!(
                line.starts_with(LOG_PREFIX) && !line.contains('@') && !line.contains("token"),
                "reset log is not the allowed redacted class"
            );
            let class = line[LOG_PREFIX.len()..].to_owned();
            assert!(
                allowed.iter().any(|allowed| allowed == &class),
                "reset log class is not allowlisted"
            );
            Some(class)
        }
        _ => panic!("reset log emitted more than the class line"),
    }
}

async fn settle(server: &Server, case: &Case) {
    if case.deliveries == 0 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        return;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let got = server.smtp.messages.lock().expect("message lock").len() as u64;
        let failures = server.mail.delivery_failure_count();
        let logged = LOGS.0.lock().expect("log lock").len();
        let done = if case.error_class.is_some() {
            got == case.deliveries && failures == 1 && logged == 1
        } else {
            got == case.deliveries && failures == 0 && logged == 0
        };
        if done {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("reset delivery did not settle");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn reset_requests_share_one_non_enumerating_response() {
    assert!(!FIXTURE.contains('@') && !FIXTURE.contains("token="));
    let _ = log::set_logger(&LOGS);
    log::set_max_level(log::LevelFilter::Warn);
    let fixture = fixture();
    assert_eq!(fixture.cases.len(), 3);
    assert!(is_non_enumerating(
        fixture.public_response.status,
        &fixture.public_response.body
    ));
    for case in &fixture.cases {
        LOGS.0.lock().expect("log lock").clear();
        let mode = if case.id == "mailer_failure" {
            Mode::Reject
        } else {
            Mode::Accept
        };
        let server = Server::start(mode).await;
        if case.id != "unknown_account" {
            let (status, _) = server
                .post(
                    "/auth/register",
                    json!({"email": KNOWN, "password": PASSWORD, "display_name": "Reset"}),
                )
                .await;
            assert_eq!(status, 200, "registration");
        }
        let email = if case.id == "unknown_account" {
            UNKNOWN
        } else {
            KNOWN
        };
        let (status, body) = server
            .post("/auth/forgot-password", json!({"email": email}))
            .await;
        assert_eq!(status, fixture.public_response.status);
        assert_eq!(body, fixture.public_response.body);
        assert!(is_non_enumerating(status, &body));
        settle(&server, case).await;
        let messages = server.smtp.messages.lock().expect("message lock").clone();
        assert_eq!(messages.len() as u64, case.deliveries);
        assert_eq!(
            server.smtp.connections.load(Ordering::Relaxed) as u64,
            case.deliveries
        );
        if case.deliveries == 1 {
            assert!(
                messages[0]
                    .1
                    .eq_ignore_ascii_case(&format!("RCPT TO:<{KNOWN}>")),
                "known account was not the SMTP recipient"
            );
            assert!(
                messages[0].2.contains("reset-password?token="),
                "delivery did not carry a reset link"
            );
        }
        assert_eq!(server.reset_tokens(), case.reset_tokens);
        assert_eq!(
            server.mail.delivery_failure_count(),
            u64::from(case.error_class.is_some())
        );
        assert_eq!(
            logged_class(&fixture.allowed_error_classes).as_deref(),
            case.error_class.as_deref()
        );
        server.close().await;
    }
}

#[test]
fn disclosing_response_body_fails_the_contract() {
    let fixture = fixture();
    let status = fixture.public_response.status;
    let original = fixture.public_response.body;
    assert!(is_non_enumerating(status, &original));
    let mut disclosed = original.clone();
    disclosed["message"] = json!("Message failed: 550 rejected user@reset.example.test");
    assert_ne!(disclosed, original);
    assert!(!is_non_enumerating(status, &disclosed));
}
