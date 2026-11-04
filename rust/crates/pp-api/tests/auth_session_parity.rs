#![cfg(feature = "desktop_rebuild")]

use pp_api::auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router};
use pp_storage::{
    Limits, WriterOwner,
    auth::{
        AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy,
        rebuild::AuthLifetimes,
    },
};
use reqwest::{Client, Method};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf};
use tokio::process::Command;

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

#[tokio::test]
async fn node_session_fixture_is_current() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let node = std::env::var_os("PP_SCHEMA_PARITY_NODE")
        .or_else(|| std::env::var_os("PP_TEST_NODE"))
        .unwrap_or_else(|| "node".into());
    let generator = root.join("fixtures/parity/auth-session/generate.mjs");
    let output = Command::new(&node)
        .arg(&generator)
        .arg("--check")
        .current_dir(&root)
        .output()
        .await
        .unwrap_or_else(|error| {
            panic!(
                "Cannot run {node:?} {generator:?} --check: {error}. Install Node 22 and web dependencies. Set PP_SCHEMA_PARITY_NODE or PP_TEST_NODE to select Node."
            )
        });
    assert!(
        output.status.success(),
        "Auth session fixture freshness check failed with {}. Install Node 22 and web dependencies. If node.json is out of date, regenerate it with {node:?} {generator:?}.\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    generated: String,
    sources: Vec<String>,
    seed: Seed,
    cases: Vec<Transition>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Seed {
    user: User,
    session: Session,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct User {
    id: String,
    email: Option<String>,
    display_name: String,
    password_hash: Option<String>,
    is_admin: i64,
    created_at: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Session {
    token: String,
    id: String,
    user_id: String,
    expires_at: String,
    provider: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Transition {
    name: String,
    method: String,
    path: String,
    credential: Credential,
    storage: Storage,
    expected: Expected,
    node_set_cookie: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Credential {
    Original,
    Absent,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Storage {
    Current,
    Reopen,
}
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Expected {
    status: u16,
    body: Value,
    clearing_cookies: Vec<Value>,
    session_rows: i64,
}

struct Directory(Scratch);
impl Directory {
    fn new() -> Self {
        Self(Scratch::new("pp-auth-session-rust-"))
    }
    fn db(&self) -> Connection {
        Connection::open(self.0.join("print-partner.db")).unwrap()
    }
}

struct Server {
    origin: String,
    client: Client,
    owner: Option<WriterOwner>,
    task: tokio::task::JoinHandle<()>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}
impl Server {
    async fn start(directory: &Directory) -> Self {
        let owner = WriterOwner::open(&directory.0, Limits::default())
            .unwrap()
            .0;
        let auth = owner
            .rebuild_auth_with_policy(
                AuthPolicy {
                    registration: RegistrationPolicy::Open,
                    first_user: FirstUserTenant::NewUser,
                    session_tenant: SessionTenantPolicy::AccountTenant,
                },
                AuthLifetimes::default(),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = auth_router(
            AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, true, None, false).unwrap(),
            auth,
            ProviderClient::new(None, None).unwrap(),
            ResetMailer::disabled(),
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
        Self {
            origin,
            client: Client::builder()
                .no_proxy()
                .timeout(std::time::Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            owner: Some(owner),
            task,
            shutdown: Some(shutdown),
        }
    }
    async fn stop(mut self) {
        self.shutdown.take().unwrap().send(()).unwrap();
        (&mut self.task).await.unwrap();
        self.owner.take().unwrap().shutdown().unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
        if let Some(owner) = self.owner.take() {
            owner.shutdown().unwrap();
        }
    }
}

fn clearing_cookies(headers: &[String]) -> Vec<Value> {
    headers
        .iter()
        .map(|header| {
            let mut parts = header.split(';').map(str::trim);
            let (name, value) = parts.next().unwrap().split_once('=').unwrap();
            let attributes: BTreeMap<_, _> = parts
                .map(|part| {
                    let (name, value) = part.split_once('=').unwrap_or((part, ""));
                    (name.to_ascii_lowercase(), value)
                })
                .collect();
            let expires_immediately = match attributes.get("max-age") {
                Some(age) => age.parse::<i64>().unwrap() <= 0,
                None => attributes.get("expires") == Some(&"Thu, 01 Jan 1970 00:00:00 GMT"),
            };
            json!({
                "name": name, "value": value, "path": attributes.get("path"),
                "domain": attributes.get("domain"),
                "same_site": attributes.get("samesite").map(|value| value.to_ascii_lowercase()),
                "secure": attributes.contains_key("secure"),
                "expires_immediately": expires_immediately,
            })
        })
        .collect()
}

#[tokio::test]
async fn node_session_transitions_match_real_auth_router() {
    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../../../fixtures/parity/auth-session/node.json"
    ))
    .unwrap();
    assert!(fixture.generated.contains("auth-session/generate.mjs"));
    assert_eq!(fixture.sources.len(), 2);
    assert_eq!(fixture.cases.len(), 7);
    let directory = Directory::new();
    let mut server = Server::start(&directory).await;
    let user = &fixture.seed.user;
    let session = &fixture.seed.session;
    assert_eq!(
        session.id,
        hex::encode(Sha256::digest(session.token.as_bytes()))
    );
    {
        let mut db = directory.db();
        let tx = db.transaction().unwrap();
        tx.execute(
            "INSERT INTO users(id,email,display_name,password_hash,is_admin,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![user.id,user.email,user.display_name,user.password_hash,user.is_admin,user.created_at],
        ).unwrap();
        tx.execute(
            "INSERT INTO sessions(id,user_id,expires_at,provider) VALUES(?1,?2,?3,?4)",
            params![
                session.id,
                session.user_id,
                session.expires_at,
                session.provider
            ],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    for transition in fixture.cases {
        if matches!(transition.storage, Storage::Reopen) {
            server.stop().await;
            server = Server::start(&directory).await;
        }
        let mut request = server
            .client
            .request(
                Method::from_bytes(transition.method.as_bytes()).unwrap(),
                format!("{}{}", server.origin, transition.path),
            )
            .header("Origin", &server.origin);
        if matches!(transition.credential, Credential::Original) {
            request = request.header("Cookie", format!("pp_session={}", session.token));
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        let headers: Vec<_> = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|value| value.to_str().unwrap().to_owned())
            .collect();
        let actual = Expected {
            status,
            body: pp_api::problem::without_envelope(response.json().await.unwrap()),
            clearing_cookies: clearing_cookies(&headers),
            session_rows: directory
                .db()
                .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
                .unwrap(),
        };
        eprintln!(
            "{}: Node Set-Cookie={:?}; Rust Set-Cookie={headers:?}",
            transition.name, transition.node_set_cookie
        );
        assert_eq!(
            actual, transition.expected,
            "transition {}",
            transition.name
        );
    }
    server.stop().await;
}
