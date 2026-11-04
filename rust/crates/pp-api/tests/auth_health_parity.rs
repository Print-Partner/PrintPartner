use pp_api::auth::{
    AuthBackend, AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router,
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{
        AuthClient, AuthPolicy, FirstUserTenant, Outcome, RegistrationPolicy, Request, Secret,
        SessionTenantPolicy,
    },
};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

#[path = "../../pp-storage/tests/common/process.rs"]
mod child_process;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    status: u16,
    body: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    node: Response,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    #[serde(rename = "generated")]
    _generated: String,
    #[serde(rename = "sources")]
    _sources: Vec<String>,
    cases: Vec<Case>,
}

async fn register(auth: &AuthClient, email: &str) -> String {
    let auth = auth.clone();
    let email = email.to_owned();
    tokio::task::spawn_blocking(move || {
        let Outcome::Session { token, .. } = auth
            .submit(
                Request::Register {
                    email,
                    display_name: "Health".into(),
                    password: Secret::new("fixture-password".into()),
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(10),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
        else {
            panic!("registration must issue session")
        };
        format!("pp_session={}", token.expose())
    })
    .await
    .unwrap()
}

fn backend(owner: &WriterOwner, single: bool) -> AuthBackend {
    let policy = AuthPolicy {
        registration: if single {
            RegistrationPolicy::FirstAccountOnly
        } else {
            RegistrationPolicy::Open
        },
        first_user: FirstUserTenant::NewUser,
        session_tenant: if single {
            SessionTenantPolicy::SingleAccountDefault
        } else {
            SessionTenantPolicy::AccountTenant
        },
    };
    #[cfg(feature = "desktop_rebuild")]
    let client = owner.rebuild_auth_with_policy(policy, Default::default());
    #[cfg(not(feature = "desktop_rebuild"))]
    let client = owner.auth_with_policy(policy);
    client.unwrap().into()
}

fn expected(name: &str) -> (Response, Vec<&'static str>) {
    if name.ends_with("closed-owner") {
        return (
            Response {
                status: 503,
                body: json!({"detail":"Authentication temporarily unavailable"}),
            },
            vec!["status"],
        );
    }
    let single = name == "policy-rejected";
    let response = Response {
        status: 200,
        body: json!({
            "ok":true, "authentication_required":true, "authenticated":name == "valid",
            "degraded_detail":if single { Some("Explicit account owner mapping is required") } else { None },
            "capabilities":[], "multi_user":!single, "single_user_auth":single,
            "single_user_setup_required":false, "registration_open":!single,
            "owner_mapping_required":single, "github_oauth_configured":false,
            "discord_oauth_configured":false,
        }),
    };
    (
        response,
        if single {
            vec!["authenticated", "capabilities"]
        } else {
            vec!["capabilities"]
        },
    )
}

async fn compare(name: &str) {
    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../../../fixtures/parity/auth-health/node.json"
    ))
    .unwrap();
    assert_eq!(
        fixture
            .cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<Vec<_>>(),
        [
            "anonymous",
            "valid",
            "expired",
            "policy-rejected",
            "closed-owner",
            "anonymous-closed-owner"
        ]
    );
    let case = fixture
        .cases
        .into_iter()
        .find(|case| case.name == name)
        .unwrap();
    let directory = Scratch::new("pp-auth-health-");
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    let seed = owner.auth(FirstUserTenant::NewUser);
    let cookie = register(&seed, "health@example.test").await;
    let single = name == "policy-rejected";
    if single {
        register(&seed, "second@example.test").await;
    }
    if name == "expired" {
        let db = Connection::open(directory.join("print-partner.db")).unwrap();
        assert_eq!(
            db.execute(
                "UPDATE sessions SET expires_at='2000-01-01T00:00:00.000Z'",
                []
            )
            .unwrap(),
            1
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = auth_router(
        AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, !single, None, false).unwrap(),
        backend(&owner, single),
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
    let owner = if name.ends_with("closed-owner") {
        owner.shutdown().unwrap();
        None
    } else {
        Some(owner)
    };
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut request = client.get(format!("{origin}/health"));
    if !name.starts_with("anonymous") {
        request = request.header("Cookie", cookie);
    }
    let response = request.send().await.unwrap();
    let actual = Response {
        status: response.status().as_u16(),
        body: pp_api::problem::without_envelope(response.json().await.unwrap()),
    };
    shutdown.send(()).unwrap();
    task.await.unwrap();
    if let Some(owner) = owner {
        owner.shutdown().unwrap();
    }
    drop(directory);

    let (expected, declared_differences) = expected(name);
    assert_eq!(actual.status, expected.status, "{name}");
    assert_eq!(actual.body, expected.body, "{name}: complete Rust body");
    let mut differences = case
        .node
        .body
        .as_object()
        .unwrap()
        .iter()
        .filter_map(|(key, value)| {
            actual
                .body
                .get(key)
                .filter(|other| *other != value)
                .map(|_| key.as_str())
        })
        .collect::<Vec<_>>();
    if actual.status != case.node.status {
        differences.push("status");
    }
    differences.sort();
    assert_eq!(
        differences, declared_differences,
        "{name}: unrecorded contract difference"
    );
}

#[tokio::test]
async fn anonymous_health_parity() {
    compare("anonymous").await;
}
#[tokio::test]
async fn valid_session_health_parity() {
    compare("valid").await;
}
#[tokio::test]
async fn expired_session_health_parity() {
    compare("expired").await;
}
#[tokio::test]
async fn policy_rejected_session_health_parity() {
    compare("policy-rejected").await;
}
#[tokio::test]
async fn closed_owner_health_stays_fatal() {
    compare("closed-owner").await;
}

#[tokio::test]
async fn anonymous_closed_owner_health_stays_fatal() {
    compare("anonymous-closed-owner").await;
}

#[test]
fn node_health_snapshot_is_current() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap();
    let node = std::env::var_os("PP_TEST_NODE").unwrap_or_else(|| "node".into());
    let output = child_process::command(node)
        .current_dir(root)
        .args([
            "--conditions=development",
            "fixtures/parity/auth-health/generate.mjs",
            "--check",
        ])
        .output()
        .expect("install Node and run npm --prefix web ci before auth health parity tests");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Node health snapshot check failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("auth health: 6 registered-handler cases passed"));
}
