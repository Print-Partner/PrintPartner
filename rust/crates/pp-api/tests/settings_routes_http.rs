//! GRE-1357: date format, build tracking and default Spoolman settings over
//! HTTP, replaying `fixtures/admin/settings-cases.json` and checking each reply
//! and the stored rows against `fixtures/admin/settings-oracle.json`, which
//! `web/apps/server/src/routes/settings-routes.oracle.test.ts` records from Node.
#![cfg(feature = "desktop_rebuild")]

use pp_api::{admin::AdminHttpConfig, settings::settings_router};
use pp_storage::{
    Limits, SettingSnapshot, WriterOwner,
    auth::{
        self, AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy,
        rebuild::AuthLifetimes,
    },
};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const SESSION: &str = "pp_session=read-fixture-secret";
const CASES: &str = include_str!("../../../../fixtures/admin/settings-cases.json");
const ORACLE: &str = include_str!("../../../../fixtures/admin/settings-oracle.json");
const PROBLEM: &str = "application/problem+json";

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

struct Server {
    owner: WriterOwner,
    origin: String,
    client: Client,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start(directory: &Path, flag: Option<&str>) -> Self {
        let owner = WriterOwner::open(directory, Limits::default()).unwrap().0;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = settings_router(
            AdminHttpConfig::from_startup(&origin, "default", flag).unwrap(),
            owner.accepted_reads().unwrap(),
            owner.app_settings_with_policy(policy()).unwrap(),
        );
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
            .unwrap();
        });
        Self {
            owner,
            origin,
            client: Client::builder().no_proxy().build().unwrap(),
            shutdown,
            task,
        }
    }

    async fn send(
        &self,
        step: &Value,
        cookie: Option<&str>,
    ) -> (u16, Option<String>, Option<String>, Vec<u8>) {
        let method = Method::from_bytes(step["method"].as_str().unwrap().as_bytes()).unwrap();
        let path = step["path"].as_str().unwrap();
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin));
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        // A browser sends its own origin; a cookie write without it is refused.
        if step["cross_site"] != Value::Bool(true) {
            request = request.header("origin", &self.origin);
        }
        if let Some(body) = step["body"].as_str() {
            request = request
                .header("content-type", step["content_type"].as_str().unwrap())
                .body(body.to_owned());
        }
        let response = request.send().await.unwrap();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .map(|value| value.to_str().unwrap().to_owned())
        };
        let (content_type, cache_control) = (header("content-type"), header("cache-control"));
        let status = response.status().as_u16();
        (
            status,
            content_type,
            cache_control,
            response.bytes().await.unwrap().to_vec(),
        )
    }

    fn stored(&self, keys: &Value) -> Value {
        let reader = self.owner.client().reader(Duration::from_secs(5)).unwrap();
        let mut stored = serde_json::Map::new();
        for key in keys.as_array().unwrap() {
            let key = key.as_str().unwrap();
            let value = match reader.snapshot("default", key).unwrap() {
                SettingSnapshot::Stored { value } => json!(value),
                SettingSnapshot::Missing => Value::Null,
            };
            stored.insert(key.to_owned(), value);
        }
        Value::Object(stored)
    }

    async fn stop(self) {
        let _ = self.shutdown.send(());
        self.task.await.unwrap();
        self.owner.shutdown().unwrap();
    }
}

/// A copy of the Node fixture database with the cases' `integrations` row.
fn seeded(cases: &Value) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("print-partner.db");
    std::fs::write(
        &database,
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','integrations',?1)",
            [cases["integrations"].as_str().unwrap()],
        )
        .unwrap();
    directory
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_routes_match_the_node_oracle_and_survive_restart() {
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let directory = seeded(&cases);
    let server = Server::start(directory.path(), Some("1")).await;

    let steps = cases["steps"].as_array().unwrap();
    let replies = oracle["replies"].as_array().unwrap();
    assert_eq!(steps.len(), replies.len());
    let mut exact = 0;
    for (index, (step, node)) in steps.iter().zip(replies).enumerate() {
        let context = format!("step {index}: {step}");
        let (status, content_type, cache_control, body) = server.send(step, Some(SESSION)).await;
        assert_eq!(json!(status), node["status"], "{context}");
        assert_eq!(
            cache_control.as_deref(),
            Some("private, no-store"),
            "{context}"
        );
        assert_eq!(server.stored(&cases["keys"]), node["stored"], "{context}");
        let body: Value = serde_json::from_slice(&body).unwrap();
        let fastify_refusal = node["body"]["code"]
            .as_str()
            .is_some_and(|code| code.starts_with("FST_"));
        if status == 200 {
            assert_eq!(json!(content_type), node["content_type"], "{context}");
            assert_eq!(body, node["body"], "{context}");
            exact += 1;
        } else if status >= 500 || fastify_refusal {
            // Rust answers a problem with its own text instead of Fastify's
            // error code or Node's TypeError.
            assert_eq!(content_type.as_deref(), Some(PROBLEM), "{context}");
        } else {
            assert_eq!(content_type.as_deref(), Some(PROBLEM), "{context}");
            assert_eq!(
                pp_api::problem::without_envelope(body),
                node["body"],
                "{context}"
            );
            exact += 1;
        }
    }
    assert_eq!(exact, 43, "replies matched exactly");
    let saved = server.stored(&cases["keys"]);
    assert_eq!(saved, replies.last().unwrap()["stored"]);
    server.stop().await;

    let server = Server::start(directory.path(), Some("1")).await;
    assert_eq!(server.stored(&cases["keys"]), saved);
    for (path, body) in [
        ("/settings/date-format", json!({"format": "dmy_short"})),
        (
            "/settings/build-tracking",
            json!({"assembly_tracking": true}),
        ),
        (
            "/api/v1/settings/spoolman-default",
            json!({"integration_id": "sp1"}),
        ),
    ] {
        let (status, _, _, reply) = server
            .send(&json!({"method": "GET", "path": path}), Some(SESSION))
            .await;
        assert_eq!(status, 200, "{path}");
        assert_eq!(
            serde_json::from_slice::<Value>(&reply).unwrap(),
            body,
            "{path}"
        );
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_routes_refuse_bad_credentials_and_cross_site_writes_without_writing() {
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let directory = seeded(&cases);
    let server = Server::start(directory.path(), Some("1")).await;
    let before = server.stored(&cases["keys"]);
    for path in [
        "/settings/date-format",
        "/api/v1/settings/build-tracking",
        "/settings/spoolman-default",
    ] {
        for cookie in [None, Some("pp_session=not-a-session")] {
            for step in [
                json!({"method": "GET", "path": path}),
                json!({"method": "PUT", "path": path, "content_type": "application/json",
                       "body": r#"{"format":"iso","assembly_tracking":true,"integration_id":"sp1"}"#}),
            ] {
                let (status, content_type, _, _) = server.send(&step, cookie).await;
                assert_eq!(status, 401, "{step} {cookie:?}");
                assert_eq!(content_type.as_deref(), Some(PROBLEM));
            }
        }
    }
    let write = json!({"method": "PUT", "path": "/settings/date-format", "cross_site": true,
                       "content_type": "application/json", "body": r#"{"format":"iso"}"#});
    assert_eq!(server.send(&write, Some(SESSION)).await.0, 403);
    assert_eq!(server.stored(&cases["keys"]), before);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_routes_stay_unmounted_without_the_rebuild_flag() {
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let directory = seeded(&cases);
    let server = Server::start(directory.path(), None).await;
    let step = json!({"method": "GET", "path": "/settings/date-format"});
    assert_eq!(server.send(&step, Some(SESSION)).await.0, 404);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_routes_take_an_api_key_without_origin_and_answer_head() {
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let directory = seeded(&cases);
    let server = Server::start(directory.path(), Some("1")).await;
    let auth::Outcome::KeyCreated { key, .. } = server
        .owner
        .rebuild_auth_with_policy(policy(), AuthLifetimes::default())
        .unwrap()
        .submit(
            auth::Request::CreateKey {
                session: Secret::new("read-fixture-secret".into()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    else {
        panic!("key expected");
    };
    let bearer = format!("Bearer {}", key.expose());
    let url = format!("{}/api/v1/settings/build-tracking", server.origin);
    let written = server
        .client
        .put(&url)
        .header("authorization", &bearer)
        .header("content-type", "application/json")
        .body(r#"{"assembly_tracking":true}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(written.status().as_u16(), 200);
    assert_eq!(
        written.json::<Value>().await.unwrap(),
        json!({"assembly_tracking": true})
    );
    assert_eq!(
        server.stored(&cases["keys"])["build_tracking_assembly"],
        json!("1")
    );
    let head = server
        .client
        .head(&url)
        .header("authorization", &bearer)
        .send()
        .await
        .unwrap();
    assert_eq!(head.status().as_u16(), 200);
    assert_eq!(
        head.headers()["content-length"],
        r#"{"assembly_tracking":true}"#.len().to_string().as_str()
    );
    assert!(head.bytes().await.unwrap().is_empty());
    server.stop().await;
}
