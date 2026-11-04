//! GRE-438: legal documents and the custom filament list over HTTP, checked
//! against `fixtures/admin/node-oracle.json`, which
//! `web/apps/server/src/routes/admin-routes.oracle.test.ts` records from Node.
#![cfg(feature = "desktop_rebuild")]

use base64::Engine;
use pp_api::admin::{AdminHttpConfig, admin_router};
use pp_storage::{Limits, WriterOwner};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

const SESSION: &str = "pp_session=read-fixture-secret";
const ORACLE: &str = include_str!("../../../../fixtures/admin/node-oracle.json");
const CASES: &str = include_str!("../../../../fixtures/admin/custom-filaments-cases.json");
/// Rust parses with serde_json's 128-level nesting limit; Node has none. Node
/// writes flat entries, so only a hand-edited store nests this deep.
const DEEPER_THAN_SERDE: &str = "deep-nesting";

struct Fixture {
    owner: Option<WriterOwner>,
    origin: String,
    client: Client,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
    // Dropped last so storage closes before the directory is removed.
    directory: Scratch,
}

impl Fixture {
    async fn new(flag: Option<&str>) -> Self {
        let directory = Scratch::new("pp-admin-http-");
        std::fs::write(
            directory.join("print-partner.db"),
            include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = admin_router(
            AdminHttpConfig::from_startup(&origin, "default", flag).unwrap(),
            owner.accepted_reads().unwrap(),
            owner.admin_files().unwrap(),
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
            owner: Some(owner),
            origin,
            client: Client::builder().no_proxy().build().unwrap(),
            shutdown: Some(shutdown),
            task: Some(task),
            directory,
        }
    }

    /// Status, the headers the oracle records, and the body.
    async fn send(&self, method: Method, path: &str, headers: &[(&str, &str)]) -> (Value, Vec<u8>) {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.unwrap();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .map_or(Value::Null, |value| json!(value.to_str().unwrap()))
        };
        let reply = json!({
            "status": response.status().as_u16(),
            "content_type": header("content-type"),
            "cache_control": header("cache-control"),
            "content_length": header("content-length"),
        });
        (reply, response.bytes().await.unwrap().to_vec())
    }

    async fn get(&self, path: &str) -> (Value, Vec<u8>) {
        self.send(Method::GET, path, &[("cookie", SESSION)]).await
    }

    async fn head(&self, path: &str) -> Value {
        let (mut reply, body) = self.send(Method::HEAD, path, &[("cookie", SESSION)]).await;
        reply["body_length"] = json!(body.len());
        reply
    }

    fn store(&self) -> std::path::PathBuf {
        self.directory.join("custom_filaments.json")
    }

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.await.unwrap();
        }
        self.owner.take().unwrap().shutdown().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_62_admin_routes_match_the_node_oracle() {
    let oracle: Value = serde_json::from_str(ORACLE).unwrap();
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let fixture = Fixture::new(Some("1")).await;

    let mut legal = serde_json::Map::new();
    for path in oracle["legal"].as_object().unwrap().keys() {
        let (mut reply, body) = fixture.get(path).await;
        reply["body_sha256"] = json!(hex::encode(Sha256::digest(&body)));
        reply["head"] = fixture.head(path).await;
        legal.insert(path.clone(), reply);
    }
    assert_eq!(Value::Object(legal), oracle["legal"]);

    let mut matched = 0;
    for (name, case) in cases["cases"].as_object().unwrap() {
        let store = fixture.store();
        if store.is_dir() {
            std::fs::remove_dir(&store).unwrap();
        } else if store.exists() {
            std::fs::remove_file(&store).unwrap();
        }
        if case["directory"] == true {
            std::fs::create_dir(&store).unwrap();
        } else if let Some(text) = case["text"].as_str() {
            std::fs::write(&store, text).unwrap();
        } else if let Some(encoded) = case["base64"].as_str() {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap();
            std::fs::write(&store, bytes).unwrap();
        }
        let mut replies = serde_json::Map::new();
        for (key, path) in [
            ("/", "/filaments/custom"),
            ("/api/v1", "/api/v1/filaments/custom"),
        ] {
            let (mut reply, body) = fixture.get(path).await;
            reply["body"] = json!(String::from_utf8(body).unwrap());
            replies.insert(key.into(), reply);
        }
        replies.insert("head".into(), fixture.head("/filaments/custom").await);
        let expected = &oracle["custom_filaments"][name];
        // Node's error replies carry Fastify's TypeError text; Rust answers a
        // problem for the same status and keeps the other headers.
        for (key, reply) in &mut replies {
            let node = &expected[key.as_str()];
            if node["status"].as_u64().is_some_and(|status| status >= 400) {
                assert_eq!(
                    reply["content_type"], "application/problem+json",
                    "{name} {key}"
                );
                let internal = r#""type":"urn:print-partner:problem:internal""#;
                assert!(
                    reply["body"]
                        .as_str()
                        .is_none_or(|body| body.contains(internal))
                );
                for field in ["content_type", "content_length", "body"] {
                    if let Some(value) = node.get(field) {
                        reply[field] = value.clone();
                    }
                }
            }
        }
        let actual = Value::Object(replies);
        if name == DEEPER_THAN_SERDE {
            assert_ne!(
                &actual, expected,
                "{name}: the nesting limit no longer differs"
            );
            assert_eq!(actual["/"]["body"], r#"{"filaments":[]}"#, "{name}");
            continue;
        }
        assert_eq!(&actual, expected, "{name}");
        matched += 1;
    }
    // Every oracle case ran, and only the nesting case differs.
    assert_eq!(
        matched + 1,
        oracle["custom_filaments"].as_object().unwrap().len()
    );
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_62_admin_routes_require_a_valid_credential() {
    let fixture = Fixture::new(Some("1")).await;
    let refused = json!({"detail":"Authentication required"});
    for path in ["/legal/summary", "/api/v1/filaments/custom"] {
        for headers in [
            &[][..],
            &[("cookie", "pp_session=wrong-secret")][..],
            &[("authorization", "Bearer not-a-key")][..],
        ] {
            let (reply, body) = fixture.send(Method::GET, path, headers).await;
            assert_eq!(reply["status"], 401, "{path} {headers:?}");
            assert_eq!(
                pp_api::problem::without_envelope(serde_json::from_slice::<Value>(&body).unwrap()),
                refused
            );
        }
    }
    // Node registers core routes at `/` and `/api/v1` only.
    let (reply, _) = fixture.get("/api/v2/legal/summary").await;
    assert_eq!(reply["status"], 404);
    fixture.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn ticket_t_62_admin_routes_mount_only_with_the_rebuild_flag() {
    for flag in [None, Some("0"), Some("yes")] {
        let fixture = Fixture::new(flag).await;
        for path in ["/legal/license", "/filaments/custom"] {
            let (reply, _) = fixture.get(path).await;
            assert_eq!(reply["status"], 404, "{flag:?} {path}");
        }
        fixture.stop().await;
    }
}
