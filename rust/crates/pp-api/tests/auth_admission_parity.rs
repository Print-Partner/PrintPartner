use anyhow::{Context, Result, ensure};
use axum::extract::ConnectInfo;
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use pp_api::auth::{
    AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, SmtpConfig, SmtpSecurity,
    auth_router,
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
};
use rusqlite::{Connection, types::ValueRef};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::oneshot,
    task::{JoinHandle, JoinSet},
    time::{Instant, timeout, timeout_at},
};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;
const FIXTURE: &str = include_str!("../../../../fixtures/parity/auth-admission/node.json");
const EMAIL: &str = "admission@example.test";
const REGISTER: &str = r#"{"email":"admission@example.test","password":"fixture-admission-password","display_name":"Admission"}"#;
const FORGOT: &str = r#"{"email":"admission@example.test"}"#;
const LOGIN: &str = r#"{"email":"admission@example.test","password":"fixture-admission-password"}"#;
const CHANGE: &str = r#"{"current_password":"fixture-admission-password","new_password":"fixture-admission-password-changed"}"#;
const RESET: &str =
    r#"{"token":"fixture-admission-reset-token","password":"fixture-admission-password-reset"}"#;
const TABLES: [&str; 5] = [
    "users",
    "sessions",
    "password_reset_tokens",
    "auth_identities",
    "app_settings",
];
const FORGOT_SUCCESS: &str =
    r#"{"ok":true,"message":"If an account exists for that email, a reset link was sent."}"#;
const ORIGIN_ERROR: &str = r#"{"detail":"Invalid request origin"}"#;
const HOST_ERROR: &str = r#"{"detail":"Invalid request host"}"#;
const JSON_ERROR: &str = r#"{"detail":"Invalid JSON body"}"#;
const NODE_JSON_ERROR: &str = r#"{"statusCode":400,"code":"FST_ERR_CTP_INVALID_JSON_BODY","error":"Bad Request","message":"Body is not valid JSON but content-type is set to 'application/json'"}"#;
const NODE_SIZE_ERROR: &str = r#"{"statusCode":413,"code":"FST_ERR_CTP_BODY_TOO_LARGE","error":"Payload Too Large","message":"Request body is too large"}"#;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Route {
    Register,
    Forgot,
    Login,
    Logout,
    Change,
    Reset,
    Me,
    Create,
    Revoke,
    Regenerate,
}
impl Route {
    const ALL: [Self; 10] = [
        Self::Register,
        Self::Forgot,
        Self::Login,
        Self::Logout,
        Self::Change,
        Self::Reset,
        Self::Me,
        Self::Create,
        Self::Revoke,
        Self::Regenerate,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Forgot => "forgot",
            Self::Login => "login",
            Self::Logout => "logout",
            Self::Change => "change",
            Self::Reset => "reset",
            Self::Me => "me",
            Self::Create => "create",
            Self::Revoke => "revoke",
            Self::Regenerate => "regenerate",
        }
    }
    fn path(self, key_id: Option<&str>) -> String {
        let auth = match self {
            Self::Create => return "/settings/api-keys".into(),
            Self::Revoke => return format!("/settings/api-keys/{}", key_id.unwrap()),
            Self::Regenerate => {
                return format!("/settings/api-keys/{}/regenerate", key_id.unwrap());
            }
            Self::Reset => "reset-password",
            Self::Me => "me",
            Self::Register => "register",
            Self::Forgot => "forgot-password",
            Self::Login => "login",
            Self::Logout => "logout",
            Self::Change => "change-password",
        };
        format!("/auth/{auth}")
    }
    fn base_body(self) -> &'static str {
        match self {
            Self::Register => REGISTER,
            Self::Forgot => FORGOT,
            Self::Login => LOGIN,
            Self::Logout | Self::Create | Self::Revoke | Self::Regenerate => "{}",
            Self::Me => "",
            Self::Reset => RESET,
            Self::Change => CHANGE,
        }
    }
    fn suffixes(self) -> &'static [&'static str] {
        const GUARDS: &[&str] = &[
            "control",
            "foreign-origin",
            "missing-origin",
            "foreign-host",
        ];
        match self {
            Self::Register | Self::Logout | Self::Change | Self::Reset => GUARDS,
            Self::Me => &[
                "control",
                "foreign-origin",
                "missing-origin",
                "foreign-host",
                "duplicate-host",
                "duplicate-origin",
                "x-forwarded-host",
                "x-forwarded-for",
                "x-forwarded-proto",
                "forwarded",
                "cross-site",
            ],
            Self::Forgot => &[
                "control",
                "at-16384",
                "foreign-origin",
                "missing-origin",
                "foreign-host",
                "over-16384",
                "over-1048576",
                "malformed",
                "text-plain",
                "foreign-origin-over-1048576",
            ],
            Self::Login | Self::Create | Self::Revoke | Self::Regenerate => &[
                "control",
                "foreign-origin",
                "missing-origin",
                "foreign-host",
                "duplicate-host",
                "duplicate-origin",
                "x-forwarded-host",
                "x-forwarded-for",
                "x-forwarded-proto",
                "forwarded",
            ],
        }
    }
    fn productive_rows(self) -> Rows {
        let mut rows = Rows::default();
        match self {
            Self::Register => (rows.users, rows.sessions) = (1, 1),
            Self::Forgot => rows.reset_tokens = 1,
            Self::Login => rows.sessions = 1,
            Self::Logout => rows.sessions = -1,
            Self::Reset => rows.reset_tokens = -1,
            Self::Change | Self::Me | Self::Create | Self::Revoke | Self::Regenerate => {}
        }
        rows
    }
    fn issues_session(self) -> bool {
        matches!(
            self,
            Self::Register | Self::Login | Self::Change | Self::Reset
        )
    }
    fn needs_cookie(self) -> bool {
        matches!(
            self,
            Self::Logout | Self::Change | Self::Me | Self::Create | Self::Revoke | Self::Regenerate
        )
    }
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Authority {
    Listener,
    Foreign,
    Absent,
}
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum BodyRecipe {
    Valid,
    ExactBytes { bytes: usize },
    Raw { text: String },
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Rows {
    users: i64,
    sessions: i64,
    reset_tokens: i64,
    identities: i64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Reply {
    status: u16,
    public_error: Option<Value>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    status: u16,
    public_error: Option<Value>,
    rows: Rows,
    state_changed: bool,
    mail_connections: u32,
    session_issued: bool,
}
impl Observation {
    fn reply(&self) -> Reply {
        Reply {
            status: self.status,
            public_error: self.public_error.clone(),
        }
    }
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    route: Route,
    origin: Authority,
    host: Authority,
    content_type: String,
    body: BodyRecipe,
    #[serde(default)]
    extra_header: Option<(String, String)>,
    body_len: usize,
    body_sha256: String,
    node: Observation,
}
impl Case {
    fn body(&self) -> Vec<u8> {
        let base = self.route.base_body();
        match &self.body {
            BodyRecipe::Valid => base.as_bytes().to_vec(),
            BodyRecipe::Raw { text } => text.as_bytes().to_vec(),
            BodyRecipe::ExactBytes { bytes } => {
                assert!(*bytes >= base.len());
                let mut result = base.as_bytes()[..base.len() - 1].to_vec();
                result.resize(bytes - 1, b' ');
                result.push(b'}');
                result
            }
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    version: u8,
    cors_origin: bool,
    cases: Vec<Case>,
}
#[derive(Clone, Copy)]
enum Kind {
    SecurityCorrection,
    ErrorMapping,
}
struct Lit {
    status: u16,
    error: Option<&'static str>,
}
impl Lit {
    fn reply(&self) -> Reply {
        Reply {
            status: self.status,
            public_error: self.error.map(|text| serde_json::from_str(text).unwrap()),
        }
    }
}
struct Difference {
    id: &'static str,
    kind: Kind,
    node: Lit,
    rust: Lit,
    reason: &'static str,
}
const fn origin_fix(id: &'static str, reason: &'static str) -> Difference {
    guard_fix(id, ORIGIN_ERROR, reason)
}
const fn host_fix(id: &'static str, reason: &'static str) -> Difference {
    guard_fix(id, HOST_ERROR, reason)
}
const fn guard_fix(id: &'static str, error: &'static str, reason: &'static str) -> Difference {
    Difference {
        id,
        kind: Kind::SecurityCorrection,
        node: Lit {
            status: 200,
            error: None,
        },
        rust: Lit {
            status: 403,
            error: Some(error),
        },
        reason,
    }
}
const fn admission_fix(id: &'static str, status: u16, error: &'static str) -> Difference {
    Difference {
        node: Lit {
            status,
            error: None,
        },
        ..guard_fix(
            id,
            error,
            "Reject untrusted request metadata before auth route handling",
        )
    }
}
macro_rules! difference {
    ($id:literal, $kind:ident, $ns:literal, $ne:expr, $rs:literal, $re:expr, $reason:literal) => {
        Difference {
            id: $id,
            kind: Kind::$kind,
            node: Lit {
                status: $ns,
                error: $ne,
            },
            rust: Lit {
                status: $rs,
                error: $re,
            },
            reason: $reason,
        }
    };
}
const DIFFERENCES: &[Difference] = &[
    admission_fix("reset/foreign-origin", 200, ORIGIN_ERROR),
    admission_fix("reset/missing-origin", 200, ORIGIN_ERROR),
    admission_fix("reset/foreign-host", 200, HOST_ERROR),
    admission_fix("me/foreign-origin", 200, ORIGIN_ERROR),
    admission_fix("me/foreign-host", 200, HOST_ERROR),
    admission_fix("me/duplicate-host", 200, HOST_ERROR),
    admission_fix("me/duplicate-origin", 200, ORIGIN_ERROR),
    admission_fix("me/x-forwarded-host", 200, HOST_ERROR),
    admission_fix("me/x-forwarded-for", 200, HOST_ERROR),
    admission_fix("me/x-forwarded-proto", 200, HOST_ERROR),
    admission_fix("me/forwarded", 200, HOST_ERROR),
    admission_fix("me/cross-site", 200, ORIGIN_ERROR),
    admission_fix("create/foreign-origin", 201, ORIGIN_ERROR),
    admission_fix("create/missing-origin", 201, ORIGIN_ERROR),
    admission_fix("create/foreign-host", 201, HOST_ERROR),
    admission_fix("create/duplicate-host", 201, HOST_ERROR),
    admission_fix("create/duplicate-origin", 201, ORIGIN_ERROR),
    admission_fix("create/x-forwarded-host", 201, HOST_ERROR),
    admission_fix("create/x-forwarded-for", 201, HOST_ERROR),
    admission_fix("create/x-forwarded-proto", 201, HOST_ERROR),
    admission_fix("create/forwarded", 201, HOST_ERROR),
    admission_fix("revoke/foreign-origin", 200, ORIGIN_ERROR),
    admission_fix("revoke/missing-origin", 200, ORIGIN_ERROR),
    admission_fix("revoke/foreign-host", 200, HOST_ERROR),
    admission_fix("revoke/duplicate-host", 200, HOST_ERROR),
    admission_fix("revoke/duplicate-origin", 200, ORIGIN_ERROR),
    admission_fix("revoke/x-forwarded-host", 200, HOST_ERROR),
    admission_fix("revoke/x-forwarded-for", 200, HOST_ERROR),
    admission_fix("revoke/x-forwarded-proto", 200, HOST_ERROR),
    admission_fix("revoke/forwarded", 200, HOST_ERROR),
    admission_fix("regenerate/foreign-origin", 201, ORIGIN_ERROR),
    admission_fix("regenerate/missing-origin", 201, ORIGIN_ERROR),
    admission_fix("regenerate/foreign-host", 201, HOST_ERROR),
    admission_fix("regenerate/duplicate-host", 201, HOST_ERROR),
    admission_fix("regenerate/duplicate-origin", 201, ORIGIN_ERROR),
    admission_fix("regenerate/x-forwarded-host", 201, HOST_ERROR),
    admission_fix("regenerate/x-forwarded-for", 201, HOST_ERROR),
    admission_fix("regenerate/x-forwarded-proto", 201, HOST_ERROR),
    admission_fix("regenerate/forwarded", 201, HOST_ERROR),
    origin_fix(
        "register/foreign-origin",
        "Reject foreign Origin before registration",
    ),
    origin_fix(
        "register/missing-origin",
        "Require Origin before registration",
    ),
    host_fix(
        "register/foreign-host",
        "Reject foreign Host before registration",
    ),
    origin_fix(
        "forgot/foreign-origin",
        "Reject foreign Origin before reset-token issuance",
    ),
    origin_fix(
        "forgot/missing-origin",
        "Require Origin before reset-token issuance",
    ),
    host_fix(
        "forgot/foreign-host",
        "Reject foreign Host before reset-token issuance",
    ),
    origin_fix(
        "login/foreign-origin",
        "Reject foreign Origin before password login",
    ),
    origin_fix(
        "login/missing-origin",
        "Require Origin before password login",
    ),
    host_fix(
        "login/foreign-host",
        "Reject foreign Host before password login",
    ),
    host_fix(
        "login/duplicate-host",
        "Reject a second Host instead of trusting the first",
    ),
    origin_fix(
        "login/duplicate-origin",
        "Reject a second Origin instead of joining values",
    ),
    host_fix(
        "login/x-forwarded-host",
        "Reject proxy Host claims on loopback auth",
    ),
    host_fix(
        "login/x-forwarded-for",
        "Reject proxy client claims on loopback auth",
    ),
    host_fix(
        "login/x-forwarded-proto",
        "Reject proxy scheme claims on loopback auth",
    ),
    host_fix(
        "login/forwarded",
        "Reject RFC 7239 proxy claims on loopback auth",
    ),
    origin_fix(
        "logout/foreign-origin",
        "Reject foreign Origin before session deletion",
    ),
    origin_fix(
        "logout/missing-origin",
        "Require Origin before session deletion",
    ),
    host_fix(
        "logout/foreign-host",
        "Reject foreign Host before session deletion",
    ),
    origin_fix(
        "change/foreign-origin",
        "Reject foreign Origin before password change",
    ),
    origin_fix(
        "change/missing-origin",
        "Require Origin before password change",
    ),
    host_fix(
        "change/foreign-host",
        "Reject foreign Host before password change",
    ),
    difference!(
        "forgot/over-16384",
        SecurityCorrection,
        200,
        None,
        400,
        Some(JSON_ERROR),
        "Bound reset bodies to 16 KiB"
    ),
    difference!(
        "forgot/over-1048576",
        ErrorMapping,
        413,
        Some(NODE_SIZE_ERROR),
        400,
        Some(JSON_ERROR),
        "Both refuse oversized JSON with different public errors"
    ),
    difference!(
        "forgot/malformed",
        ErrorMapping,
        400,
        Some(NODE_JSON_ERROR),
        400,
        Some(JSON_ERROR),
        "Both refuse malformed JSON with different public errors"
    ),
    difference!(
        "forgot/text-plain",
        ErrorMapping,
        400,
        Some(r#"{"detail":"Valid email is required"}"#),
        400,
        Some(JSON_ERROR),
        "Both refuse text/plain with different public errors"
    ),
    difference!(
        "forgot/foreign-origin-over-1048576",
        ErrorMapping,
        413,
        Some(NODE_SIZE_ERROR),
        403,
        Some(ORIGIN_ERROR),
        "Rust checks Origin before reading an oversized body"
    ),
];
fn fixture() -> Fixture {
    let fixture: Fixture = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(fixture.version, 1);
    assert!(fixture.cors_origin);
    let mut expected = HashSet::new();
    for route in Route::ALL {
        for suffix in route.suffixes() {
            expected.insert(format!("{}/{suffix}", route.name()));
        }
    }
    for case in &fixture.cases {
        assert!(
            expected.remove(&case.id),
            "duplicate or unknown case {}",
            case.id
        );
        let suffix = case
            .id
            .strip_prefix(&format!("{}/", case.route.name()))
            .unwrap();
        assert_eq!(
            case.origin,
            if suffix.starts_with("foreign-origin") {
                Authority::Foreign
            } else if suffix == "missing-origin" {
                Authority::Absent
            } else {
                Authority::Listener
            }
        );
        assert_eq!(
            case.host,
            if suffix == "foreign-host" {
                Authority::Foreign
            } else {
                Authority::Listener
            }
        );
        assert_eq!(
            case.content_type,
            if suffix == "text-plain" {
                "text/plain"
            } else {
                "application/json"
            }
        );
        match (&case.body, suffix) {
            (BodyRecipe::ExactBytes { bytes: 16384 }, "at-16384")
            | (BodyRecipe::ExactBytes { bytes: 16385 }, "over-16384")
            | (
                BodyRecipe::ExactBytes { bytes: 1048577 },
                "over-1048576" | "foreign-origin-over-1048576",
            ) => {}
            (BodyRecipe::Raw { text }, "malformed") => assert_eq!(text, "{\"email\":"),
            (
                BodyRecipe::Valid,
                "control" | "foreign-origin" | "missing-origin" | "foreign-host" | "text-plain",
            ) => {}
            (BodyRecipe::Valid, _) if extra_header(suffix).is_some() => {}
            _ => panic!("wrong body recipe for {}", case.id),
        }
        assert_eq!(
            case.extra_header,
            extra_header(suffix).map(|(name, value)| (name.into(), value.into())),
            "{}: extra header",
            case.id
        );
        let admitted = case.node.status < 400;
        let rows = if admitted {
            case.route.productive_rows()
        } else {
            Rows::default()
        };
        assert_eq!(case.node.rows, rows, "{}: Node work", case.id);
        assert_eq!(
            case.node.state_changed,
            admitted && case.route != Route::Me,
            "{}: Node state",
            case.id
        );
        assert_eq!(
            case.node.mail_connections,
            u32::from(admitted && case.route == Route::Forgot)
        );
        assert_eq!(
            case.node.session_issued,
            admitted && case.route.issues_session()
        );
        assert_eq!(case.node.public_error.is_none(), admitted);
        let body = case.body();
        assert_eq!(case.body_len, body.len());
        assert_eq!(case.body_sha256, hex::encode(Sha256::digest(&body)));
    }
    assert!(expected.is_empty(), "missing cases {expected:?}");
    let mut used = HashSet::new();
    for diff in DIFFERENCES {
        assert!(used.insert(diff.id), "duplicate difference");
        let case = fixture
            .cases
            .iter()
            .find(|case| case.id == diff.id)
            .expect("unused difference");
        assert!(!diff.reason.is_empty());
        assert_eq!(
            case.node.reply(),
            diff.node.reply(),
            "{}: literal Node contract",
            diff.id
        );
        assert_ne!(diff.node.reply(), diff.rust.reply(), "equal difference");
        match diff.kind {
            Kind::SecurityCorrection => {
                assert!(diff.node.status < 400 && diff.rust.status >= 400);
                assert_eq!(case.node.state_changed, case.route != Route::Me);
            }
            Kind::ErrorMapping => assert!(diff.node.status >= 400 && diff.rust.status >= 400),
        }
    }
    fixture
}
fn extra_header(suffix: &str) -> Option<(&'static str, &'static str)> {
    Some(match suffix {
        "cross-site" => ("sec-fetch-site", "cross-site"),
        "duplicate-host" => ("host", "evil.example"),
        "duplicate-origin" => ("origin", "http://evil.example"),
        "x-forwarded-host" => ("x-forwarded-host", "evil.example"),
        "x-forwarded-for" => ("x-forwarded-for", "203.0.113.9"),
        "x-forwarded-proto" => ("x-forwarded-proto", "https"),
        "forwarded" => ("forwarded", "for=203.0.113.9;host=evil.example;proto=https"),
        _ => return None,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot(BTreeMap<String, Vec<Value>>);
impl Snapshot {
    fn read(db: &Connection) -> Result<Self> {
        let mut tables = TABLES.to_vec();
        if db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='sqlite_sequence'",
            [],
            |row| row.get::<_, i64>(0),
        )? != 0
        {
            tables.push("sqlite_sequence");
        }
        let mut result = BTreeMap::new();
        for table in tables {
            let mut statement = db.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
            let columns: Vec<String> = statement
                .column_names()
                .into_iter()
                .map(str::to_owned)
                .collect();
            let rows = statement
                .query_map([], |row| {
                    columns
                        .iter()
                        .enumerate()
                        .map(|(i, column)| {
                            Ok((
                                column.clone(),
                                match row.get_ref(i)? {
                                    ValueRef::Null => Value::Null,
                                    ValueRef::Integer(n) => json!(n),
                                    ValueRef::Real(n) => json!(n),
                                    ValueRef::Text(bytes) => json!(String::from_utf8_lossy(bytes)),
                                    ValueRef::Blob(bytes) => json!({"blob":hex::encode(bytes)}),
                                },
                            ))
                        })
                        .collect::<rusqlite::Result<serde_json::Map<_, _>>>()
                        .map(Value::Object)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            result.insert(table.to_owned(), rows);
        }
        Ok(Self(result))
    }
    fn delta(&self, before: &Self) -> Rows {
        let count = |table: &str| self.0[table].len() as i64 - before.0[table].len() as i64;
        Rows {
            users: count("users"),
            sessions: count("sessions"),
            reset_tokens: count("password_reset_tokens"),
            identities: count("auth_identities"),
        }
    }
}
struct Server {
    /// Removed by its own drop, after `Drop for Server` shuts the owner down.
    directory: Scratch,
    owner: Option<WriterOwner>,
    db: Option<Connection>,
    origin: String,
    client: reqwest::Client,
    mailer: ResetMailer,
    connections: Arc<AtomicU32>,
    http: Option<JoinHandle<std::io::Result<()>>>,
    stop_http: Option<oneshot::Sender<()>>,
    smtp: Option<JoinHandle<Result<()>>>,
    stop_smtp: Option<oneshot::Sender<()>>,
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = self.http.take() {
            task.abort();
        }
        if let Some(task) = self.smtp.take() {
            task.abort();
        }
        drop(self.db.take());
        if let Some(owner) = self.owner.take() {
            let _ = owner.shutdown();
        }
    }
}
impl Server {
    async fn start() -> Result<Self> {
        let directory = Scratch::new("pp-auth-admission-");
        let owner = WriterOwner::open(&directory, Limits::default())?.0;
        let policy = AuthPolicy {
            registration: RegistrationPolicy::Open,
            first_user: FirstUserTenant::NewUser,
            session_tenant: SessionTenantPolicy::AccountTenant,
        };
        #[cfg(feature = "desktop_rebuild")]
        let auth = owner.rebuild_auth_with_policy(policy, Default::default())?;
        #[cfg(not(feature = "desktop_rebuild"))]
        let auth = owner.auth_with_policy(policy)?;
        let smtp = TcpListener::bind("127.0.0.1:0").await?;
        let mailer = ResetMailer::smtp(SmtpConfig {
            security: SmtpSecurity::StartTls,
            host: "127.0.0.1".into(),
            port: smtp.local_addr()?.port(),
            from: "fixture@example.test".into(),
            credentials: None,
        })?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let router = auth_router(
            AuthHttpConfig::new(
                &origin,
                CookieTransport::LoopbackHttp,
                true,
                Some("https://admission.example.test"),
                false,
            )?,
            auth,
            ProviderClient::new(None, None)?,
            mailer.clone(),
        );
        let db = Connection::open(directory.join("print-partner.db"))?;
        db.busy_timeout(Duration::from_secs(5))?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        let (stop_http, rx) = oneshot::channel();
        let http = tokio::spawn(async move {
            let mut rx = rx;
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = listener.accept() => {
                        let (stream, peer) = accepted?;
                        let router = router.clone().layer(axum::Extension(ConnectInfo(peer)));
                        connections.spawn(serve_lingering(stream, router));
                    }
                }
            }
            while connections.join_next().await.is_some() {}
            Ok::<(), std::io::Error>(())
        });
        let connections = Arc::new(AtomicU32::new(0));
        let counter = connections.clone();
        let (stop_smtp, mut rx) = oneshot::channel();
        let smtp = tokio::spawn(async move {
            let mut clients = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    accepted = smtp.accept() => {
                        let (stream, _) = accepted?;
                        counter.fetch_add(1, Ordering::SeqCst);
                        clients.spawn(async move {
                            let mut stream = BufReader::new(stream);
                            stream.get_mut().write_all(b"220 fixture ESMTP\r\n").await?;
                            let mut line = String::new();
                            while stream.read_line(&mut line).await? != 0 {
                                stream.get_mut().write_all(b"250 fixture\r\n").await?;
                                line.clear();
                            }
                            Ok::<_, std::io::Error>(())
                        });
                    }
                }
            }
            clients.abort_all();
            while clients.join_next().await.is_some() {}
            Ok(())
        });
        Ok(Self {
            directory,
            owner: Some(owner),
            db: Some(db),
            origin,
            client,
            mailer,
            connections,
            http: Some(http),
            stop_http: Some(stop_http),
            smtp: Some(smtp),
            stop_smtp: Some(stop_smtp),
        })
    }
    async fn send(
        &self,
        case: &Case,
        cookie: Option<&str>,
        key_id: Option<&str>,
    ) -> Result<(Reply, Value, Option<String>)> {
        let mut request = self
            .client
            .request(
                match case.route {
                    Route::Me => reqwest::Method::GET,
                    Route::Revoke => reqwest::Method::DELETE,
                    _ => reqwest::Method::POST,
                },
                format!("{}{}", self.origin, case.route.path(key_id)),
            )
            .header(
                "Host",
                if case.host == Authority::Foreign {
                    "evil.example"
                } else {
                    self.origin.strip_prefix("http://").unwrap()
                },
            )
            .header("Content-Type", &case.content_type)
            .header("Connection", "close")
            .body(case.body());
        match case.origin {
            Authority::Listener => request = request.header("Origin", &self.origin),
            Authority::Foreign => request = request.header("Origin", "http://evil.example"),
            Authority::Absent => {}
        }
        if let Some((name, value)) = &case.extra_header {
            request = request.header(name, value);
        }
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let cookie = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok()?.split(';').next())
            .find(|pair| pair.starts_with("pp_session=") && *pair != "pp_session=")
            .map(str::to_owned);
        let body: Value = pp_api::problem::without_envelope(response.json().await?);
        Ok((
            Reply {
                status,
                public_error: (status >= 400).then(|| body.clone()),
            },
            body,
            cookie,
        ))
    }
    async fn close(&mut self, deadline: Instant) -> (Result<Snapshot>, Vec<String>) {
        let mut errors = Vec::new();
        if let Some(stop) = self.stop_http.take() {
            let _ = stop.send(());
        }
        if let Some(mut task) = self.http.take() {
            match timeout_at(
                deadline.min(Instant::now() + Duration::from_secs(10)),
                &mut task,
            )
            .await
            {
                Ok(Ok(Ok(()))) => {}
                other => {
                    errors.push(format!("HTTP stop: {other:?}"));
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        if timeout_at(
            deadline.min(Instant::now() + Duration::from_secs(25)),
            self.mailer.drain_deliveries(),
        )
        .await
        .is_err()
        {
            errors.push("mail drain timed out".into());
        }
        if let Some(stop) = self.stop_smtp.take() {
            let _ = stop.send(());
        }
        if let Some(mut task) = self.smtp.take() {
            match timeout_at(
                deadline.min(Instant::now() + Duration::from_secs(5)),
                &mut task,
            )
            .await
            {
                Ok(Ok(Ok(()))) => {}
                other => {
                    errors.push(format!("SMTP stop: {other:?}"));
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        if let Some(owner) = self.owner.take()
            && let Err(error) = owner.shutdown()
        {
            errors.push(format!("owner shutdown: {error}"));
        }
        let snapshot = Snapshot::read(self.db.as_ref().unwrap());
        drop(self.db.take());
        if let Err(error) = std::fs::remove_dir_all(&self.directory) {
            errors.push(format!("remove directory: {error}"));
        }
        if self.directory.exists() {
            errors.push("directory remains".into());
        }
        (snapshot, errors)
    }
}
/// Serves one connection, then reads and discards what the client still sends,
/// for up to two seconds, before closing. A server that closes with unread
/// request bytes resets the connection, and on Windows the client loses the
/// early response to an over-limit body (RFC 9112, section 9.6).
async fn serve_lingering(stream: TcpStream, router: axum::Router) {
    let service = hyper::service::service_fn(move |request: hyper::Request<Incoming>| {
        tower::ServiceExt::oneshot(router.clone(), request.map(axum::body::Body::new))
    });
    let Ok(parts) = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .without_shutdown()
        .await
    else {
        return;
    };
    let mut stream = parts.io.into_inner();
    let _ = stream.shutdown().await;
    let _ = timeout(Duration::from_secs(2), async {
        let mut discarded = [0; 8192];
        while matches!(stream.read(&mut discarded).await, Ok(read) if read > 0) {}
    })
    .await;
}
struct Evidence {
    observation: Observation,
    before: Snapshot,
    after: Snapshot,
    body: Value,
    mail_failures: u64,
    cleanup_errors: Vec<String>,
}
async fn observe(case: &Case, seed: &Case, key_seed: &Case) -> Result<Evidence> {
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut server = timeout(Duration::from_secs(10), Server::start())
        .await
        .context("case setup lifetime")??;
    let result = timeout_at(deadline, async {
        let mut session = None;
        if case.route != Route::Register {
            let (reply, _, cookie) = server.send(seed, None, None).await?;
            ensure!(
                reply.status == 200 && cookie.is_some(),
                "seed registration failed: {reply:?}"
            );
            session = cookie.filter(|_| case.route.needs_cookie());
        }
        let db = server.db.as_ref().unwrap();
        if case.route == Route::Reset {
            let user: String = db.query_row("SELECT id FROM users WHERE email=?1", [EMAIL], |row| row.get(0))?;
            db.execute("INSERT INTO password_reset_tokens(id,user_id,expires_at,created_at) VALUES(?1,?2,?3,?4)", rusqlite::params![hex::encode(Sha256::digest(b"fixture-admission-reset-token")), user, "2099-01-01T00:00:00.000Z", "2026-01-01T00:00:00.000Z"])?;
        }
        let key_id = if matches!(case.route, Route::Create | Route::Revoke | Route::Regenerate) {
            let (reply, body, _) = server.send(key_seed, session.as_deref(), None).await?;
            ensure!(reply.status == 201, "seed key failed: {reply:?}");
            Some(body["id"].as_str().context("seed key id")?.to_owned())
        } else { None };
        let before = Snapshot::read(db)?;
        let (reply, body, cookie) = server.send(case, session.as_deref(), key_id.as_deref()).await?;
        let session_issued = cookie.is_some();
        Ok::<_, anyhow::Error>((before, reply, body, session_issued))
    })
    .await
    .context("case request lifetime")
    .and_then(|result| result);
    let (after, mut cleanup_errors) = server.close(deadline).await;
    let (before, reply, body, session_issued) = match result {
        Ok(value) => value,
        Err(error) => {
            eprintln!("cleanup after request failure: {cleanup_errors:?}; snapshot: {after:?}");
            return Err(error);
        }
    };
    let after = after.with_context(|| {
        format!("settled snapshot failed after {reply:?}; cleanup {cleanup_errors:?}")
    })?;
    if Instant::now() > deadline {
        cleanup_errors.push("case exceeded 45 seconds".into());
    }
    Ok(Evidence {
        observation: Observation {
            status: reply.status,
            public_error: reply.public_error,
            rows: after.delta(&before),
            state_changed: after != before,
            mail_connections: server.connections.load(Ordering::SeqCst),
            session_issued,
        },
        before,
        after,
        body,
        mail_failures: server.mailer.delivery_failure_count(),
        cleanup_errors,
    })
}
fn validate(case: &Case, evidence: &Evidence) -> Result<()> {
    let actual = &evidence.observation;
    let expected = DIFFERENCES
        .iter()
        .find(|diff| diff.id == case.id)
        .map_or_else(|| case.node.reply(), |diff| diff.rust.reply());
    ensure!(
        actual.reply() == expected,
        "reply mismatch, expected {expected:?}"
    );
    if actual.status >= 400 {
        ensure!(
            evidence.before == evidence.after && !actual.state_changed,
            "refusal changed full auth snapshot"
        );
        ensure!(actual.rows == Rows::default(), "refusal row delta");
        ensure!(
            actual.mail_connections == 0 && evidence.mail_failures == 0,
            "refusal mail traffic"
        );
        ensure!(!actual.session_issued, "refusal issued session");
    } else {
        ensure!(
            actual.rows == case.route.productive_rows()
                && actual.state_changed == (case.route != Route::Me),
            "productive work"
        );
        ensure!(
            actual.session_issued == case.route.issues_session(),
            "productive session cookie"
        );
        match case.route {
            Route::Forgot => {
                ensure!(
                    actual.mail_connections >= 1 && evidence.mail_failures == 1,
                    "reset must attempt SMTP and fail STARTTLS"
                );
                ensure!(
                    evidence.body == serde_json::from_str::<Value>(FORGOT_SUCCESS)?,
                    "forgot success fields"
                );
            }
            route => {
                ensure!(
                    actual.mail_connections == 0 && evidence.mail_failures == 0,
                    "{} mail traffic",
                    route.name()
                );
            }
        }
        match case.route {
            Route::Register | Route::Login | Route::Reset | Route::Me => {
                ensure!(
                    evidence.body["user"]["email"] == EMAIL
                        && evidence.body["user"]["display_name"] == "Admission",
                    "user success fields"
                );
                let id = evidence.body["user"]["user_id"]
                    .as_str()
                    .context("response user id")?;
                ensure!(
                    evidence.after.0["users"].iter().any(|row| row["id"] == id),
                    "response user persisted"
                );
            }
            Route::Logout | Route::Change => {
                ensure!(evidence.body == json!({"ok":true}), "ok success body");
            }
            Route::Create | Route::Regenerate | Route::Revoke => {
                let keys = |snapshot: &Snapshot| -> Result<Vec<Value>> {
                    let row = snapshot.0["app_settings"]
                        .iter()
                        .find(|row| row["key"] == "api_keys_v1")
                        .context("key settings")?;
                    Ok(serde_json::from_str(
                        row["value"].as_str().context("key JSON")?,
                    )?)
                };
                let before = keys(&evidence.before)?;
                let after = keys(&evidence.after)?;
                if case.route == Route::Revoke {
                    ensure!(
                        evidence.body == json!({"success":true})
                            && after.len() == before.len()
                            && after[0]["isActive"] == false,
                        "key revoked"
                    );
                } else {
                    ensure!(
                        actual.status == 201
                            && evidence.body["key"]
                                .as_str()
                                .is_some_and(|key| key.starts_with("ppk_")),
                        "created key response"
                    );
                    ensure!(
                        !serde_json::to_string(&evidence.after.0)?
                            .contains(evidence.body["key"].as_str().unwrap()),
                        "plaintext key persisted"
                    );
                    ensure!(
                        after.len() == before.len() + 1
                            && after.iter().any(|key| key["id"] == evidence.body["id"]
                                && key["isActive"] == true
                                && key["keyHash"].as_str().is_some_and(|hash| hash.len() == 64
                                    && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))),
                        "created key persisted"
                    );
                    if case.route == Route::Regenerate {
                        ensure!(
                            after[0]["isActive"] == false && after[0]["id"] == before[0]["id"],
                            "old key deactivated"
                        );
                    }
                }
            }
            Route::Forgot => {}
        }
        if case.route == Route::Me {
            ensure!(
                evidence.body["multi_user"] == true,
                "me multi-user response"
            );
        }
        if case.route == Route::Reset {
            ensure!(
                evidence.after.0["password_reset_tokens"].is_empty()
                    && evidence.after.0["sessions"].len() == 1
                    && evidence.after.0["sessions"][0]["id"]
                        != evidence.before.0["sessions"][0]["id"],
                "reset consumes token and replaces old session"
            );
        }
        if matches!(case.route, Route::Change | Route::Reset) {
            ensure!(
                evidence.after.0["users"][0]["password_hash"]
                    != evidence.before.0["users"][0]["password_hash"],
                "password change must rewrite the password hash"
            );
        }
    }
    ensure!(
        evidence.cleanup_errors.is_empty(),
        "cleanup: {:?}",
        evidence.cleanup_errors
    );
    Ok(())
}
async fn compare(route: Route) {
    let fixture = fixture();
    let seed = fixture
        .cases
        .iter()
        .find(|case| case.id == "register/control")
        .unwrap();
    for case in fixture.cases.iter().filter(|case| case.route == route) {
        let evidence = observe(
            case,
            seed,
            fixture
                .cases
                .iter()
                .find(|case| case.id == "create/control")
                .unwrap(),
        )
        .await
        .unwrap_or_else(|error| panic!("{}: {error:#}", case.id));
        eprintln!(
            "{} observed {:?}, mail failures {}",
            case.id, evidence.observation, evidence.mail_failures
        );
        validate(case, &evidence).unwrap_or_else(|error| {
            panic!(
                "{}: {error:#}; actual {:?}; cleanup {:?}",
                case.id, evidence.observation, evidence.cleanup_errors
            )
        });
    }
}
#[tokio::test]
async fn register_auth_admission_parity() {
    compare(Route::Register).await;
}
#[tokio::test]
async fn forgot_auth_admission_parity() {
    compare(Route::Forgot).await;
}
#[tokio::test]
async fn login_auth_admission_parity() {
    compare(Route::Login).await;
}
#[tokio::test]
async fn logout_auth_admission_parity() {
    compare(Route::Logout).await;
}
#[tokio::test]
async fn change_password_auth_admission_parity() {
    compare(Route::Change).await;
}
#[tokio::test]
async fn reset_password_auth_admission_parity() {
    compare(Route::Reset).await;
}
#[tokio::test]
async fn me_auth_admission_parity() {
    compare(Route::Me).await;
}
#[tokio::test]
async fn api_key_auth_admission_parity() {
    for route in [Route::Create, Route::Revoke, Route::Regenerate] {
        compare(route).await;
    }
}
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "macOS runners intermittently reset the Node probe's upload with write ECONNRESET before it reads the response; the Linux auth-admission-freshness workflow owns the canonical check"
)]
fn node_auth_admission_snapshot_is_current() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let node = std::env::var_os("PP_TEST_NODE").unwrap_or_else(|| "node".into());
    let output = child_process::command(node)
        .current_dir(root)
        .args([
            "--conditions=development",
            "fixtures/parity/auth-admission/generate.mjs",
            "--check",
            // The auth-admission-freshness workflow owns the canonical Node 22.23.3 gate.
            "--additional-runtime",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Node freshness failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("auth admission: 77 real-listener cases passed on "));
}
#[test]
fn comparator_rejects_response_and_work_changes() {
    let fixture = fixture();
    let case = fixture
        .cases
        .iter()
        .find(|case| case.id == "register/foreign-host")
        .unwrap();
    let empty = || {
        Snapshot(
            TABLES
                .into_iter()
                .map(|table| (table.into(), Vec::new()))
                .collect(),
        )
    };
    let baseline = || Evidence {
        observation: Observation {
            status: 403,
            public_error: Some(json!({"detail":"Invalid request host"})),
            rows: Rows::default(),
            state_changed: false,
            mail_connections: 0,
            session_issued: false,
        },
        before: empty(),
        after: empty(),
        body: json!({"detail":"Invalid request host"}),
        mail_failures: 0,
        cleanup_errors: Vec::new(),
    };
    assert!(validate(case, &baseline()).is_ok());
    for diff in DIFFERENCES {
        if let Kind::SecurityCorrection = diff.kind {
            let case = fixture
                .cases
                .iter()
                .find(|case| case.id == diff.id)
                .unwrap();
            let bypassed = Evidence {
                observation: case.node.clone(),
                ..baseline()
            };
            let error = validate(case, &bypassed).unwrap_err().to_string();
            assert!(
                error.starts_with("reply mismatch"),
                "{}: guard bypass {error}",
                diff.id
            );
        }
    }
    for mutation in 0..12 {
        let mut evidence = baseline();
        match mutation {
            0 => evidence.observation.status = 400,
            1 => evidence.observation.public_error = Some(json!({"detail":"changed"})),
            2 => evidence.observation.rows.users = 1,
            3 => evidence.observation.rows.sessions = 1,
            4 => evidence.observation.rows.reset_tokens = 1,
            5 => evidence.observation.rows.identities = 1,
            6 => evidence.observation.mail_connections = 1,
            7 => evidence.mail_failures = 1,
            8 => evidence.observation.session_issued = true,
            9 => {
                evidence
                    .after
                    .0
                    .get_mut("users")
                    .unwrap()
                    .push(json!({"id":"changed"}));
            }
            10 => evidence.observation.state_changed = true,
            11 => evidence
                .after
                .0
                .get_mut("app_settings")
                .unwrap()
                .push(json!({"key":"api_keys_v1","value":"changed"})),
            _ => unreachable!(),
        }
        assert!(
            validate(case, &evidence).is_err(),
            "accepted mutation {mutation}"
        );
    }
}
