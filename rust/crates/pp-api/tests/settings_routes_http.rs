//! GRE-1357: date format, build tracking and default Spoolman settings over
//! HTTP, replaying `fixtures/admin/settings-cases.json` and checking each reply
//! and the stored rows against `fixtures/admin/settings-oracle.json`, which
//! `web/apps/server/src/routes/admin-routes.oracle.test.ts` records from Node.
#![cfg(feature = "desktop_rebuild")]

use pp_api::{admin::AdminHttpConfig, serving::Serving, settings::settings_router};
use pp_storage::{
    Limits, SettingSnapshot, WriterOwner,
    auth::{
        self, AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy,
        rebuild::AuthLifetimes,
    },
};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};

const COOKIE: (&str, &str) = ("cookie", "pp_session=read-fixture-secret");
const CASES: &str = include_str!("../../../../fixtures/admin/settings-cases.json");
const ORACLE: &str = include_str!("../../../../fixtures/admin/settings-oracle.json");
const PROBLEM: &str = "application/problem+json";
const WAIT: Duration = Duration::from_secs(5);

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

struct Reply {
    status: u16,
    content_type: Option<String>,
    cache_control: Option<String>,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

struct Server {
    owner: WriterOwner,
    origin: String,
    client: Client,
    serving: Serving,
    keys: Value,
}

impl Server {
    async fn start(directory: &Path, flag: Option<&str>) -> Self {
        let owner = WriterOwner::open(directory, Limits::default()).unwrap().0;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let config = AdminHttpConfig::from_startup(&origin, "default", flag).unwrap();
        let settings = owner.app_settings_with_policy(policy()).unwrap();
        let router = settings_router(config, owner.accepted_reads().unwrap(), settings);
        let cases: Value = serde_json::from_str(CASES).unwrap();
        Self {
            owner,
            origin,
            client: Client::builder().no_proxy().build().unwrap(),
            serving: Serving::start(listener, router),
            keys: cases["keys"].clone(),
        }
    }

    /// Sends `step` with `headers`, plus this listener's `Origin` unless the
    /// step is `cross_site`, as a browser would.
    async fn send(&self, step: &Value, headers: &[(&str, &str)]) -> Reply {
        let method = Method::from_bytes(step["method"].as_str().unwrap().as_bytes()).unwrap();
        let url = format!("{}{}", self.origin, step["path"].as_str().unwrap());
        let mut request = self.client.request(method, url);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if step["cross_site"] != Value::Bool(true) {
            request = request.header("origin", &self.origin);
        }
        if let Some(body) = step["body"].as_str() {
            let content_type = step["content_type"].as_str().unwrap();
            request = request
                .header("content-type", content_type)
                .body(body.to_owned());
        }
        let response = request.send().await.unwrap();
        let header = |name: &str| Some(response.headers().get(name)?.to_str().ok()?.to_owned());
        Reply {
            status: response.status().as_u16(),
            content_type: header("content-type"),
            cache_control: header("cache-control"),
            body: response.bytes().await.unwrap().to_vec(),
        }
    }

    /// The three rows as Node's oracle records them: the stored text, or `null`.
    fn stored(&self) -> Value {
        let reader = self.owner.client().reader(WAIT).unwrap();
        let rows = self.keys.as_array().unwrap().iter().map(|key| {
            let key = key.as_str().unwrap();
            let value = match reader.snapshot("default", key).unwrap() {
                SettingSnapshot::Stored { value } => Value::String(value),
                SettingSnapshot::Missing => Value::Null,
            };
            (key.to_owned(), value)
        });
        Value::Object(rows.collect())
    }

    async fn stop(mut self) {
        self.serving.stop().await.unwrap();
        self.owner.shutdown().unwrap();
    }
}

/// A copy of the Node fixture database with the cases' `integrations` row,
/// served with the rebuild flag on.
async fn seeded() -> (tempfile::TempDir, Server) {
    let cases: Value = serde_json::from_str(CASES).unwrap();
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
    let server = Server::start(directory.path(), Some("1")).await;
    (directory, server)
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_routes_match_the_node_oracle_and_survive_restart() {
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let (directory, server) = seeded().await;

    let steps = cases["steps"].as_array().unwrap();
    let replies = oracle["replies"].as_array().unwrap();
    assert_eq!(steps.len(), replies.len());
    let mut exact = 0;
    let mut intended_differences = 0;
    for (index, (step, node)) in steps.iter().zip(replies).enumerate() {
        let context = format!("step {index}: {step}");
        let reply = server.send(step, &[COOKIE]).await;
        // These four recorded Node TypeErrors are malformed requests in Rust.
        let malformed_body = matches!(index, 11 | 12 | 31 | 50);
        if malformed_body {
            assert_eq!(node["status"], 500, "{context}");
            assert_malformed(&reply);
            intended_differences += 1;
        } else {
            assert_eq!(json!(reply.status), node["status"], "{context}");
        }
        assert_eq!(
            reply.cache_control.as_deref(),
            Some("private, no-store"),
            "{context}"
        );
        assert_eq!(server.stored(), node["stored"], "{context}");
        let fastify_refusal = node["body"]["code"]
            .as_str()
            .is_some_and(|code| code.starts_with("FST_"));
        if reply.status == 200 {
            assert_eq!(json!(reply.content_type), node["content_type"], "{context}");
            assert_eq!(reply.json(), node["body"], "{context}");
            exact += 1;
            continue;
        }
        assert_eq!(reply.content_type.as_deref(), Some(PROBLEM), "{context}");
        // Rust answers its own problem text instead of Fastify's error code or
        // Node's TypeError; every other refusal keeps Node's `detail`.
        if reply.status < 500 && !fastify_refusal && !malformed_body {
            let body = pp_api::problem::without_envelope(reply.json());
            assert_eq!(body, node["body"], "{context}");
            exact += 1;
        }
    }
    assert_eq!(intended_differences, 4);
    assert_eq!(steps.len() - intended_differences, 48);
    assert_eq!(exact, 43, "replies matched exactly");
    let saved = server.stored();
    assert_eq!(saved, replies.last().unwrap()["stored"]);
    server.stop().await;

    let server = Server::start(directory.path(), Some("1")).await;
    assert_eq!(server.stored(), saved);
    let read_back = [
        ("/settings/date-format", json!({"format": "dmy_short"})),
        (
            "/settings/build-tracking",
            json!({"assembly_tracking": true}),
        ),
        (
            "/api/v1/settings/spoolman-default",
            json!({"integration_id": "sp1"}),
        ),
    ];
    for (path, body) in read_back {
        let reply = server
            .send(&json!({"method": "GET", "path": path}), &[COOKIE])
            .await;
        assert_eq!((reply.status, reply.json()), (200, body), "{path}");
    }
    server.stop().await;
}

fn assert_malformed(reply: &Reply) {
    assert_eq!(reply.status, 400);
    assert_eq!(reply.content_type.as_deref(), Some(PROBLEM));
    let body = reply.json();
    assert_eq!(body["status"], 400);
    assert_eq!(body["type"], "urn:print-partner:problem:malformed-request");
    assert_eq!(body["detail"], "Request is invalid");
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_routes_refuse_null_and_absent_bodies_without_writing() {
    let (_directory, server) = seeded().await;
    let before = server.stored();
    for prefix in ["", "/api/v1"] {
        for setting in ["date-format", "build-tracking", "spoolman-default"] {
            let path = format!("{prefix}/settings/{setting}");
            for step in [
                json!({"method": "PUT", "path": path,
                       "content_type": "application/json", "body": "null"}),
                json!({"method": "PUT", "path": path}),
            ] {
                assert_malformed(&server.send(&step, &[COOKIE]).await);
                assert_eq!(server.stored(), before, "{step}");
            }
        }
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_routes_refuse_bad_credentials_and_cross_site_writes_without_writing() {
    let (_directory, server) = seeded().await;
    let before = server.stored();
    let paths = [
        "/settings/date-format",
        "/api/v1/settings/build-tracking",
        "/settings/spoolman-default",
    ];
    let wrong = [("cookie", "pp_session=not-a-session")];
    for path in paths {
        for headers in [&[][..], &wrong[..]] {
            for step in [
                json!({"method": "GET", "path": path}),
                json!({"method": "PUT", "path": path, "content_type": "application/json",
                       "body": r#"{"format":"iso","assembly_tracking":true,"integration_id":"sp1"}"#}),
            ] {
                let reply = server.send(&step, headers).await;
                assert_eq!(reply.status, 401, "{step} {headers:?}");
                assert_eq!(reply.content_type.as_deref(), Some(PROBLEM));
            }
        }
    }
    let write = json!({"method": "PUT", "path": "/settings/date-format", "cross_site": true,
                       "content_type": "application/json", "body": r#"{"format":"iso"}"#});
    assert_eq!(server.send(&write, &[COOKIE]).await.status, 403);
    assert_eq!(server.stored(), before);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_routes_stay_unmounted_without_the_rebuild_flag() {
    let (directory, server) = seeded().await;
    server.stop().await;
    let server = Server::start(directory.path(), None).await;
    let step = json!({"method": "GET", "path": "/settings/date-format"});
    assert_eq!(server.send(&step, &[COOKIE]).await.status, 404);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_routes_take_an_api_key_without_origin_and_answer_head() {
    let (_directory, server) = seeded().await;
    let auth = server
        .owner
        .rebuild_auth_with_policy(policy(), AuthLifetimes::default())
        .unwrap();
    let create = auth::Request::CreateKey {
        session: Secret::new("read-fixture-secret".into()),
    };
    let created = auth.submit(create, Arc::default(), WAIT).unwrap().recv();
    let Ok(Ok(auth::Outcome::KeyCreated { key, .. })) = created else {
        panic!("key expected");
    };
    let bearer = format!("Bearer {}", key.expose());
    let path = "/api/v1/settings/build-tracking";
    let put = json!({"method": "PUT", "path": path, "cross_site": true,
                     "content_type": "application/json", "body": r#"{"assembly_tracking":true}"#});
    let written = server.send(&put, &[("authorization", &bearer)]).await;
    assert_eq!(
        (written.status, written.json()),
        (200, json!({"assembly_tracking": true}))
    );
    assert_eq!(server.stored()["build_tracking_assembly"], json!("1"));
    let head = json!({"method": "HEAD", "path": path, "cross_site": true});
    let head = server.send(&head, &[("authorization", &bearer)]).await;
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn secret_settings_match_node_bytes_redact_responses_and_restart() {
    let cases: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/admin/secret-settings-cases.json"
    ))
    .unwrap();
    let oracle: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/admin/secret-settings-oracle.json"
    ))
    .unwrap();
    let (directory, mut server) = seeded().await;
    server.keys = cases["keys"].clone();
    for (index, (step, node)) in cases["steps"]
        .as_array()
        .unwrap()
        .iter()
        .zip(oracle["replies"].as_array().unwrap())
        .enumerate()
    {
        let reply = server.send(step, &[COOKIE]).await;
        assert_eq!(
            reply.status,
            node["status"].as_u64().unwrap() as u16,
            "step {index}"
        );
        let mut expected = node["body"].clone();
        if expected["webhook_url"].is_string() {
            expected["webhook_url"] = json!("https://discord.com/[REDACTED]");
        }
        let actual = if reply.status >= 400 {
            pp_api::problem::without_envelope(reply.json())
        } else {
            reply.json()
        };
        assert_eq!(actual, expected, "step {index}");
        assert!(!String::from_utf8_lossy(&reply.body).contains("obviously-fake"));
        assert_eq!(server.stored(), node["stored"], "step {index}");
    }
    let before = server.stored();
    for prefix in ["", "/api/v1"] {
        for path in ["github-pat", "discord-notify"] {
            let step = json!({"method":"PUT","path":format!("{prefix}/settings/{path}"),"content_type":"application/json","body":"{}"});
            assert_eq!(server.send(&step, &[]).await.status, 401);
            let mut cross = step.clone();
            cross["cross_site"] = json!(true);
            assert_eq!(server.send(&cross, &[COOKIE]).await.status, 403);
        }
    }
    assert_eq!(server.stored(), before);
    server.stop().await;
    let mut server = Server::start(directory.path(), Some("true")).await;
    server.keys = cases["keys"].clone();
    assert_eq!(server.stored(), before);
    let step = json!({"method":"GET","path":"/api/v1/settings/discord-notify"});
    assert_eq!(
        server.send(&step, &[COOKIE]).await.json(),
        json!({"webhook_url":null,"notify_on_update":false,"notify_on_sync":true,"auto_sync_updates":true})
    );
    server.stop().await;
    let server = Server::start(directory.path(), None).await;
    for path in ["github-pat", "discord-notify"] {
        let step = json!({"method":"GET","path":format!("/settings/{path}")});
        assert_eq!(server.send(&step, &[COOKIE]).await.status, 404);
    }
    server.stop().await;
}
