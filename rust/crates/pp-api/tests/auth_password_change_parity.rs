#![cfg(feature = "desktop_rebuild")]

use pp_api::auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
};
use reqwest::{Client, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::SocketAddr;

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

#[derive(Deserialize)]
struct Fixture {
    original_password: String,
    email: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionKind {
    Absent,
    Valid,
    Invalid,
    Revoked,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    session: SessionKind,
    body: Value,
    proposed_password: String,
    observation: Observation,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Observation {
    before: Logins,
    change: Change,
    after: Logins,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Logins {
    old: bool,
    new: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Change {
    status: u16,
    body: Value,
}

fn matches_node(observed: &Observation, recorded: &Observation) -> bool {
    observed == recorded
}

fn fixture() -> Fixture {
    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../../../fixtures/parity/auth-password-change/node.json"
    ))
    .unwrap();
    assert_eq!(fixture.cases.len(), 24);
    let ids: std::collections::HashSet<_> = fixture.cases.iter().map(|case| &case.id).collect();
    assert_eq!(ids.len(), fixture.cases.len());
    fixture
}

struct Server {
    origin: String,
    client: Client,
    owner: WriterOwner,
    task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    directory: Scratch,
}

impl Server {
    async fn start() -> Self {
        let directory = Scratch::new("pp-password-parity-");
        let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
        let auth = owner
            .auth_with_policy(AuthPolicy {
                registration: RegistrationPolicy::Open,
                first_user: FirstUserTenant::NewUser,
                session_tenant: SessionTenantPolicy::AccountTenant,
            })
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
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
            owner,
            task,
            shutdown,
            directory,
        }
    }

    async fn post(&self, path: &str, body: &Value, cookie: Option<&str>) -> Response {
        let mut request = self
            .client
            .post(format!("{}{path}", self.origin))
            .header("Origin", &self.origin)
            .json(body);
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        request.send().await.unwrap()
    }

    async fn login(&self, email: &str, password: &str) -> bool {
        let response = self
            .post(
                "/auth/login",
                &json!({"email":email,"password":password}),
                None,
            )
            .await;
        let status = response.status().as_u16();
        assert!(matches!(status, 200 | 400 | 401), "login status {status}");
        if status == 200 {
            assert!(!session_cookie(&response).is_empty());
            let body: Value = pp_api::problem::without_envelope(response.json().await.unwrap());
            assert_eq!(body["user"]["email"], email);
        }
        status == 200
    }

    async fn logins(&self, fixture: &Fixture, case: &Case) -> Logins {
        Logins {
            old: self.login(&fixture.email, &fixture.original_password).await,
            new: self.login(&fixture.email, &case.proposed_password).await,
        }
    }

    async fn close(self) {
        self.shutdown.send(()).unwrap();
        self.task.await.unwrap();
        self.owner.shutdown().unwrap();
        drop(self.directory);
    }
}

fn session_cookie(response: &Response) -> String {
    let cookie = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    assert!(cookie.starts_with("pp_session="));
    assert!(cookie.len() > "pp_session=".len());
    cookie.to_owned()
}

async fn cookie_for(server: &Server, kind: &SessionKind, issued: &str) -> Option<String> {
    match kind {
        SessionKind::Absent => None,
        SessionKind::Valid => Some(issued.to_owned()),
        SessionKind::Invalid => Some("pp_session=invalid-session-token-873".to_owned()),
        SessionKind::Revoked => {
            let logout = server.post("/auth/logout", &json!({}), Some(issued)).await;
            assert_eq!(logout.status(), 200);
            Some(issued.to_owned())
        }
    }
}

#[tokio::test]
async fn password_change_matches_node_response_and_login_outcomes() {
    let fixture = fixture();
    let mut mismatches = Vec::new();
    for case in &fixture.cases {
        let server = Server::start().await;
        let registration = server
            .post(
                "/auth/register",
                &json!({
                    "email":fixture.email,
                    "password":fixture.original_password,
                    "display_name":"Password parity",
                }),
                None,
            )
            .await;
        assert_eq!(registration.status(), 200, "{}", case.id);
        let issued = session_cookie(&registration);
        let cookie = cookie_for(&server, &case.session, &issued).await;
        let before = server.logins(&fixture, case).await;
        let response = server
            .post("/auth/change-password", &case.body, cookie.as_deref())
            .await;
        let change = Change {
            status: response.status().as_u16(),
            body: pp_api::problem::without_envelope(response.json().await.unwrap()),
        };
        let observed = Observation {
            before,
            change,
            after: server.logins(&fixture, case).await,
        };
        server.close().await;
        if !matches_node(&observed, &case.observation) {
            mismatches.push((&case.id, observed, &case.observation));
        }
    }
    assert!(mismatches.is_empty(), "(case, Rust, Node): {mismatches:#?}");
}

#[test]
fn recorded_cases_require_password_preservation_or_replacement() {
    for case in fixture().cases {
        assert_eq!(
            case.observation.before,
            Logins {
                old: true,
                new: false
            }
        );
        let success = case.id == "valid_change";
        assert_eq!(
            case.observation.change.status == 200,
            success,
            "{}",
            case.id
        );
        assert_eq!(
            case.observation.after,
            Logins {
                old: !success,
                new: success
            },
            "{}",
            case.id,
        );
        if matches!(
            case.session,
            SessionKind::Absent | SessionKind::Invalid | SessionKind::Revoked
        ) {
            assert_eq!(case.observation.change.status, 401, "{}", case.id);
            assert_eq!(
                case.observation.change.body,
                json!({"detail":"Authentication required"}),
                "{}",
                case.id
            );
        }
    }
}

#[test]
fn parity_rejects_altered_refusal_body() {
    let fixture = fixture();
    let expected = &fixture
        .cases
        .iter()
        .find(|case| case.id == "wrong_current")
        .unwrap()
        .observation;
    let mut altered = expected.clone();
    altered
        .change
        .body
        .as_object_mut()
        .unwrap()
        .remove("code")
        .unwrap();
    assert!(!matches_node(&altered, expected));
}

#[test]
fn parity_rejects_old_password_accepted_after_success() {
    let fixture = fixture();
    let expected = &fixture
        .cases
        .iter()
        .find(|case| case.id == "valid_change")
        .unwrap()
        .observation;
    let mut altered = expected.clone();
    altered.after.old = true;
    assert!(!matches_node(&altered, expected));
}

#[test]
fn parity_rejects_invalid_session_downgraded_to_field_error() {
    let fixture = fixture();
    let expected = &fixture
        .cases
        .iter()
        .find(|case| case.id == "invalid_session_missing_both")
        .unwrap()
        .observation;
    let mut altered = expected.clone();
    altered.change.status = 400;
    altered.change.body = json!({"detail":"Current and new passwords are required"});
    assert!(!matches_node(&altered, expected));
}
