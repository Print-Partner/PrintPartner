//! Replays the Node reset-completion golden against the landed Rust auth
//! router. The pooled client runs by default and the rebuild client runs with
//! `desktop_rebuild`, so each CI configuration covers one backend.
use pp_api::auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
};
use reqwest::{Client, Method};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{collections::HashMap, net::SocketAddr};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

const FIXTURE: &str = include_str!("../../../../fixtures/parity/auth-reset-complete/node.json");

fn fixture() -> Value {
    serde_json::from_str(FIXTURE).unwrap()
}
fn password(label: &str) -> String {
    if label == "short" {
        "short".into()
    } else {
        format!("pp-fixture-{label}-password")
    }
}
struct Server {
    origin: String,
    client: Client,
    owner: WriterOwner,
    db: Connection,
    task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    /// Last field: removed only after the storage owner has dropped, even on panic.
    _directory: Scratch,
}
impl Server {
    async fn start() -> Self {
        let directory = Scratch::new("pp-reset-complete-");
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = auth_router(
            AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, true, None, true).unwrap(),
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
        let db = Connection::open(directory.join("print-partner.db")).unwrap();
        db.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
        Self {
            origin,
            client: Client::builder().no_proxy().build().unwrap(),
            owner,
            db,
            task,
            shutdown,
            _directory: directory,
        }
    }
    async fn close(self) {
        self.shutdown.send(()).unwrap();
        self.task.await.unwrap();
        drop(self.db);
        self.owner.shutdown().unwrap();
    }
    /// (password hash, session count, reset-token count) for the fixture user.
    fn state(&self, email: &str) -> (Option<String>, i64, i64) {
        let user: Option<(String, Option<String>)> = self
            .db
            .query_row(
                "SELECT id, password_hash FROM users WHERE email=?1",
                [email],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .unwrap();
        let Some((id, hash)) = user else {
            return (None, 0, 0);
        };
        let count = |table: &str| -> i64 {
            self.db
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE user_id=?1"),
                    [&id],
                    |row| row.get(0),
                )
                .unwrap()
        };
        (hash, count("sessions"), count("password_reset_tokens"))
    }
}

/// Runs the golden's steps and returns observations in the golden's shape.
/// `restore_consumed_before` re-inserts the consumed reset-token row before the
/// named step, standing in for a backend that never consumed it.
async fn replay(restore_consumed_before: Option<&str>) -> Vec<Value> {
    let fixture = fixture();
    let email = fixture["email"].as_str().unwrap();
    let server = Server::start().await;
    let mut tokens = HashMap::from([(
        "unknown".to_owned(),
        "fixture-unknown-reset-token".to_owned(),
    )]);
    let mut sessions = HashMap::<String, String>::new();
    let mut consumed = None;
    let mut observations = Vec::new();
    for step in fixture["steps"].as_array().unwrap() {
        let id = step["id"].as_str().unwrap();
        let call = step["call"].as_str().unwrap();
        if restore_consumed_before == Some(id) {
            let (token_id, user_id, expires_at, created_at): (String, String, String, String) =
                consumed.clone().unwrap();
            server
                .db
                .execute(
                    "INSERT INTO password_reset_tokens(id,user_id,expires_at,created_at) VALUES(?1,?2,?3,?4)",
                    params![token_id, user_id, expires_at, created_at],
                )
                .unwrap();
        }
        let before = server.state(email);
        let response = match call {
            "expire" => {
                server
                    .db
                    .execute(
                        "UPDATE password_reset_tokens SET expires_at='2000-01-01T00:00:00.000Z'",
                        [],
                    )
                    .unwrap();
                None
            }
            "me" => {
                let session = &sessions[step["session"].as_str().unwrap()];
                Some(
                    server
                        .client
                        .get(format!("{}/auth/me", server.origin))
                        .header("Cookie", format!("pp_session={session}"))
                        .send()
                        .await
                        .unwrap(),
                )
            }
            _ => {
                let label = step["password"].as_str().map(password);
                let (path, body) = match call {
                    "register" | "login" => (
                        format!("/auth/{call}"),
                        json!({"email":email,"password":label}),
                    ),
                    "forgot" => ("/auth/forgot-password".into(), json!({"email":email})),
                    "reset" => {
                        let mut body = json!({"password":label});
                        if let Some(token) = step["token"].as_str() {
                            body["token"] = json!(tokens[token]);
                        }
                        ("/auth/reset-password".into(), body)
                    }
                    other => panic!("unknown call {other}"),
                };
                if call == "reset" {
                    consumed = server
                        .db
                        .query_row(
                            "SELECT id,user_id,expires_at,created_at FROM password_reset_tokens",
                            [],
                            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                        )
                        .optional()
                        .unwrap()
                        .or(consumed);
                }
                Some(
                    server
                        .client
                        .request(Method::POST, format!("{}{path}", server.origin))
                        .header("Origin", &server.origin)
                        .json(&body)
                        .send()
                        .await
                        .unwrap(),
                )
            }
        };
        let after = server.state(email);
        let mut observation = json!({
            "id": id,
            "identity_write": before.0 != after.0,
            "sessions": after.1,
            "reset_tokens": after.2,
        });
        if let Some(response) = response {
            let status = response.status().as_u16();
            let session = response
                .headers()
                .get_all("set-cookie")
                .iter()
                .filter_map(|value| {
                    value
                        .to_str()
                        .ok()?
                        .split(';')
                        .next()?
                        .strip_prefix("pp_session=")
                })
                .find(|value| !value.is_empty())
                .map(str::to_owned);
            let mut body: Value = pp_api::problem::without_envelope(response.json().await.unwrap());
            if body["user"]["user_id"].is_string() {
                body["user"]["user_id"] = json!("<user_id>");
            }
            if let Some(url) = body["dev_reset_url"].as_str() {
                let url = reqwest::Url::parse(url).unwrap();
                let (_, token) = url.query_pairs().find(|(key, _)| key == "token").unwrap();
                tokens.insert(step["capture"].as_str().unwrap().into(), token.into_owned());
                body["dev_reset_url"] = json!("<reset_url>");
            }
            if let Some(session) = &session {
                sessions.insert(id.into(), session.clone());
            }
            observation["status"] = json!(status);
            observation["body"] = body;
            observation["session_issued"] = json!(session.is_some());
        }
        observations.push(observation);
    }
    server.close().await;
    observations
}

#[tokio::test]
async fn reset_completion_matches_node_and_tokens_are_single_use() {
    assert!(!FIXTURE.contains("pp-fixture-") && !FIXTURE.contains("token="));
    let expected = fixture()["observations"].as_array().unwrap().clone();
    assert_eq!(expected.len(), 18);
    let observed = replay(None).await;
    for (observed, expected) in observed.iter().zip(&expected) {
        assert_eq!(observed, expected, "step {}", expected["id"]);
    }
    assert_eq!(observed.len(), expected.len());
}

#[tokio::test]
async fn golden_rejects_a_backend_that_reaccepts_the_consumed_token() {
    let expected = fixture()["observations"].as_array().unwrap().clone();
    let observed = replay(Some("replay_a")).await;
    let step = |rows: &[Value]| {
        rows.iter()
            .find(|row| row["id"] == "replay_a")
            .cloned()
            .unwrap()
    };
    assert_eq!(
        (
            step(&expected)["status"].clone(),
            step(&expected)["identity_write"].clone()
        ),
        (json!(400), json!(false))
    );
    assert_eq!(
        (
            step(&observed)["status"].clone(),
            step(&observed)["identity_write"].clone()
        ),
        (json!(200), json!(true))
    );
    assert_ne!(observed, expected);
}
