#![cfg(feature = "desktop_rebuild")]

use pp_api::auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router};
use pp_storage::{
    Limits, WriterOwner,
    auth::{
        AuthPolicy, FirstUserTenant, Outcome, RegistrationPolicy, Request, Secret,
        SessionTenantPolicy,
        rebuild::{AuthLifetimes, RebuildAuthClient},
    },
};
use reqwest::{Client, Method, Response};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

struct Server {
    origin: String,
    http: Client,
    auth: RebuildAuthClient,
    owner: WriterOwner,
    task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    /// Last field: removed only after the storage owner has dropped, even on panic.
    directory: Scratch,
}

impl Server {
    async fn start() -> Self {
        let directory = Scratch::new("pp-key-regenerate-parity-");
        let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
        let auth = owner
            .rebuild_auth_with_policy(
                AuthPolicy {
                    registration: RegistrationPolicy::Open,
                    first_user: FirstUserTenant::ClaimDefault,
                    session_tenant: SessionTenantPolicy::AccountTenant,
                },
                AuthLifetimes::default(),
            )
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = auth_router(
            AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, true, None, false).unwrap(),
            auth.clone(),
            ProviderClient::new(None, None).unwrap(),
            ResetMailer::disabled(),
        );
        let (shutdown, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = receiver.await;
            })
            .await
            .unwrap();
        });
        Self {
            origin,
            http: Client::builder().no_proxy().build().unwrap(),
            auth,
            owner,
            directory,
            task,
            shutdown,
        }
    }

    async fn post(&self, path: &str, body: Value, cookie: Option<&str>) -> Response {
        let mut request = self
            .http
            .request(Method::POST, format!("{}{path}", self.origin))
            .header("Origin", &self.origin)
            .json(&body);
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        request.send().await.unwrap()
    }

    fn database(&self) -> Connection {
        Connection::open(self.directory.join("print-partner.db")).unwrap()
    }

    async fn close(self) {
        self.shutdown.send(()).unwrap();
        self.task.await.unwrap();
        self.owner.shutdown().unwrap();
    }
}

#[derive(Default)]
struct Identities(BTreeMap<String, String>);

impl Identities {
    fn bind(&mut self, value: &str, label: &str) {
        assert!(!value.is_empty(), "missing generated identity");
        let label = format!("<{label}>");
        assert!(!self.0.contains_key(value), "generated identities collided");
        assert!(
            !self.0.values().any(|known| known == &label),
            "duplicate identity label"
        );
        self.0.insert(value.into(), label);
    }

    fn known(&self, value: &str) -> String {
        self.0
            .get(value)
            .expect("unmapped generated identity")
            .clone()
    }

    fn key(&mut self, body: &Value, label: &str) {
        let id = body["id"].as_str().expect("missing key id");
        let raw = body["key"].as_str().expect("missing key secret");
        assert!(id.starts_with("key_") && id.len() == 20, "invalid key id");
        assert!(
            raw.starts_with("ppk_") && raw.len() == 68,
            "invalid key secret"
        );
        self.bind(id, &format!("{label}-id"));
        self.bind(raw, &format!("{label}-secret"));
        self.bind(&key_digest(raw), &format!("{label}-digest"));
    }

    fn key_row(&self, mut row: Value) -> Value {
        row["id"] = json!(self.known(row["id"].as_str().expect("key row id")));
        for field in ["createdAt", "lastUsedAt", "expiresAt"] {
            if !row[field].is_null() {
                let timestamp = row[field].as_str().expect("timestamp text");
                assert!(
                    timestamp.contains('T') && timestamp.ends_with('Z'),
                    "invalid timestamp"
                );
                row[field] = json!("<timestamp>");
            }
        }
        for field in ["key", "keyHash"] {
            if let Some(value) = row.get(field) {
                row[field] = json!(self.known(value.as_str().expect("key identity")));
            }
        }
        assert_safe(&row);
        row
    }
}

fn key_digest(raw: &str) -> String {
    let context = b"print-partner:settings-api-key:v1";
    let mut inner = [0x36; 64];
    let mut outer = [0x5c; 64];
    for (index, byte) in context.iter().enumerate() {
        inner[index] ^= byte;
        outer[index] ^= byte;
    }
    let digest = Sha256::new()
        .chain_update(inner)
        .chain_update(raw.as_bytes())
        .finalize();
    hex::encode(
        Sha256::new()
            .chain_update(outer)
            .chain_update(digest)
            .finalize(),
    )
}

fn assert_safe(value: &Value) {
    match value {
        Value::String(text) => assert!(
            !text.contains("ppk_")
                && !(text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())),
            "unnormalized secret or digest"
        ),
        Value::Array(values) => values.iter().for_each(assert_safe),
        Value::Object(values) => values.values().for_each(assert_safe),
        _ => {}
    }
}

fn snapshot(server: &Server, names: &Identities) -> Value {
    let db = server.database();
    let users: Vec<Value> = db
        .prepare("SELECT id,email,is_admin FROM users ORDER BY email")
        .unwrap()
        .query_map([], |row| {
            Ok(json!({
                "id": names.known(&row.get::<_, String>(0)?),
                "email": row.get::<_, String>(1)?, "is_admin": row.get::<_, i64>(2)?,
            }))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut sessions: Vec<Value> = db.prepare("SELECT id,user_id,provider FROM sessions").unwrap()
        .query_map([], |row| Ok(json!({
            "id": names.known(&row.get::<_, String>(0)?),
            "user_id": names.known(&row.get::<_, String>(1)?), "provider": row.get::<_, String>(2)?,
        }))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    sessions.sort_by_key(|row| row["id"].as_str().unwrap().to_owned());
    let mut settings: Vec<Value> = db.prepare("SELECT tenant_id,key,value FROM app_settings WHERE key='api_keys_v1'").unwrap()
        .query_map([], |row| {
            let stored: String = row.get(2)?;
            let keys: Vec<Value> = serde_json::from_str(&stored).expect("persisted key JSON");
            Ok(json!({ "tenant_id": names.known(&row.get::<_, String>(0)?),
                "key": row.get::<_, String>(1)?, "value": keys.into_iter().map(|key| names.key_row(key)).collect::<Vec<_>>() }))
        }).unwrap().collect::<rusqlite::Result<_>>().unwrap();
    settings.sort_by_key(|row| row["tenant_id"].as_str().unwrap().to_owned());
    let result = json!({ "users": users, "sessions": sessions, "settings": settings });
    assert_safe(&result);
    result
}

async fn client_matches(
    auth: &RebuildAuthClient,
    names: &Identities,
    tenant: &str,
    raw: &str,
    entry: &Value,
) -> bool {
    assert_eq!(entry["transport"], "node-http");
    assert_eq!(entry["rustClient"]["request"], "ResolveKey");
    assert_eq!(json!(names.known(raw)), entry["credential"]);
    let auth = auth.clone();
    let request = Request::ResolveKey {
        tenant_id: tenant.into(),
        key: Secret::new(raw.into()),
    };
    let outcome = tokio::task::spawn_blocking(move || {
        auth.submit(
            request,
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    })
    .await
    .unwrap();
    let Outcome::KeyResolved { principal, .. } = outcome else {
        panic!("expected key resolution")
    };
    let actual = principal.map(|principal| json!({
        "tenant_id": names.known(&principal.tenant_id), "key_id": names.known(&principal.key_id),
    }));
    let actual = actual.unwrap_or(Value::Null);
    actual == entry["rustClient"]["principal"]
}

fn stored_settings(server: &Server) -> Vec<(String, String)> {
    server
        .database()
        .prepare(
            "SELECT tenant_id,value FROM app_settings WHERE key='api_keys_v1' ORDER BY tenant_id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

async fn run_case(case: &Value, mutate: bool) {
    let server = Server::start().await;
    let mut names = Identities::default();
    let mut cookies = BTreeMap::new();
    let mut users = BTreeMap::new();
    let mut keys = BTreeMap::new();
    for actor in ["owner", "foreign"] {
        let response = server.post("/auth/register", json!({
            "email": format!("{actor}@example.test"), "password": "temporary-password-1234", "display_name": actor,
        }), None).await;
        assert_eq!(response.status(), 200, "registration setup");
        let cookie = response
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let body: Value = node_view(response.json().await.unwrap());
        let user = body["user"]["user_id"].as_str().unwrap().to_owned();
        names.bind(&user, &format!("{actor}-user"));
        let token = cookie.strip_prefix("pp_session=").expect("session cookie");
        names.bind(
            &hex::encode(Sha256::digest(token.as_bytes())),
            &format!("{actor}-session-digest"),
        );
        if actor == "foreign" {
            server
                .database()
                .execute("UPDATE users SET is_admin=1 WHERE id=?1", [&user])
                .unwrap();
        }
        users.insert(actor, user);
        cookies.insert(actor, cookie);
    }
    for actor in ["owner", "foreign"] {
        let response = server
            .post("/settings/api-keys", json!({}), Some(&cookies[actor]))
            .await;
        assert_eq!(response.status(), 201, "key setup");
        let key: Value = node_view(response.json().await.unwrap());
        names.key(
            &key,
            if actor == "owner" {
                "old-key"
            } else {
                "foreign-key"
            },
        );
        keys.insert(actor, key);
    }
    let transcript = case["transcript"].as_array().unwrap();
    for entry in transcript
        .iter()
        .take_while(|entry| entry["id"] != "rotate")
    {
        assert!(
            client_matches(
                &server.auth,
                &names,
                &users["owner"],
                keys["owner"]["key"].as_str().unwrap(),
                entry
            )
            .await,
            "old key must work before rotation"
        );
    }
    assert_eq!(snapshot(&server, &names), case["before"], "before rotation");
    let stored_before = stored_settings(&server);
    let target = match case["id"].as_str().unwrap() {
        "owner-session-rotation" | "missing-session" | "invalid-session" => {
            keys["owner"]["id"].as_str().unwrap()
        }
        "foreign-tenant" => keys["foreign"]["id"].as_str().unwrap(),
        "missing-key" => "key_missing",
        _ => panic!("unknown fixture case"),
    };
    let entry = transcript
        .iter()
        .find(|entry| entry["id"] == "rotate")
        .unwrap();
    let path = format!("/settings/api-keys/{target}/regenerate");
    let normalized_target = if target == "key_missing" {
        target.into()
    } else {
        names.known(target)
    };
    assert_eq!(
        entry["path"],
        format!("/settings/api-keys/{normalized_target}/regenerate")
    );
    let cookie = match (case["id"].as_str().unwrap(), entry.get("credential")) {
        ("invalid-session", Some(credential)) => {
            assert_eq!(credential, "<invalid-session>");
            assert!(entry["actor"].is_null());
            Some("pp_session=invalid-session")
        }
        ("invalid-session", None) => panic!("invalid-session case requires an explicit credential"),
        (_, None) => entry["actor"].as_str().map(|actor| cookies[actor].as_str()),
        _ => panic!("unknown rotation credential"),
    };
    let response = server.post(&path, json!({}), cookie).await;
    let status = response.status().as_u16();
    assert_eq!(json!(status), entry["status"], "rotation HTTP status");
    let body: Value = node_view(response.json().await.unwrap());
    let new_key = if status == 201 {
        names.key(&body, "new-key");
        assert_eq!(
            names.key_row(body.clone()),
            entry["body"],
            "rotation HTTP body"
        );
        Some(body)
    } else {
        assert_safe(&body);
        assert_eq!(body, entry["body"], "rotation problem body");
        if status == 404 {
            assert_eq!(body["status"], json!(status), "problem status field");
            for field in ["title", "status"] {
                let mut mutant = body.clone();
                mutant.as_object_mut().unwrap().remove(field);
                assert_ne!(mutant, entry["body"], "comparison ignores {field}");
            }
            let mut mutant = body.clone();
            mutant["status"] = json!("404");
            assert_ne!(mutant, entry["body"], "comparison ignores status type");
        } else {
            assert_eq!(status, 401, "unexpected refusal status");
            assert_eq!(body, json!({"detail":"Authentication required"}));
        }
        None
    };
    assert_eq!(
        snapshot(&server, &names),
        case["afterRotation"],
        "rotation persisted rows"
    );
    let stored_after = stored_settings(&server);
    let foreign_before = stored_before
        .iter()
        .find(|(tenant, _)| tenant == &users["foreign"])
        .unwrap();
    let foreign_after = stored_after
        .iter()
        .find(|(tenant, _)| tenant == &users["foreign"])
        .unwrap();
    assert_eq!(
        json!({
            "foreignSettingUnchanged": foreign_before == foreign_after,
            "allSettingsUnchanged": stored_before == stored_after,
        }),
        case["rotationEffects"],
        "raw setting changes"
    );
    if new_key.is_none() {
        assert_eq!(
            stored_before, stored_after,
            "failed rotation changed settings"
        );
    }
    for entry in transcript
        .iter()
        .skip_while(|entry| entry["id"] != "rotate")
        .skip(1)
    {
        let actor = entry["actor"].as_str().unwrap();
        let key = match entry["credential"].as_str().unwrap() {
            "<old-key-secret>" => &keys["owner"],
            "<foreign-key-secret>" => &keys["foreign"],
            "<new-key-secret>" => new_key.as_ref().expect("rotation returned key"),
            _ => panic!("unknown credential label"),
        };
        assert!(
            client_matches(
                &server.auth,
                &names,
                &users[actor],
                key["key"].as_str().unwrap(),
                entry
            )
            .await,
            "key usability differs for {}",
            entry["id"]
        );
    }
    assert_eq!(
        snapshot(&server, &names),
        case["after"],
        "post-authentication persisted rows"
    );
    if mutate {
        let db = server.database();
        let stored: String = db
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id=?1 AND key='api_keys_v1'",
                [&users["owner"]],
                |row| row.get(0),
            )
            .unwrap();
        let mut rows: Value = serde_json::from_str(&stored).unwrap();
        rows[0]["isActive"] = json!(true);
        db.execute(
            "UPDATE app_settings SET value=?1 WHERE tenant_id=?2 AND key='api_keys_v1'",
            params![rows.to_string(), users["owner"]],
        )
        .unwrap();
        let old_after = transcript
            .iter()
            .find(|entry| entry["id"] == "old-after")
            .unwrap();
        assert!(
            !client_matches(
                &server.auth,
                &names,
                &users["owner"],
                keys["owner"]["key"].as_str().unwrap(),
                old_after
            )
            .await,
            "consumer failed to detect a reactivated old key"
        );
    }
    server.close().await;
}

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../fixtures/parity/auth-key-regenerate/node.json"
    ))
    .unwrap()
}

#[tokio::test]
async fn node_regeneration_transcripts_match_rust_router_and_client() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 5);
    for case in cases {
        run_case(case, false).await;
    }
}

#[tokio::test]
async fn consumer_detects_old_key_left_usable() {
    let fixture = fixture();
    run_case(&fixture["cases"][0], true).await;
}

#[path = "support/node_view.rs"]
mod node_view;
use node_view::node_view;
