use pp_api::auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router};
use pp_storage::{
    Limits, WriterOwner,
    auth::{
        AuthClient, AuthFailure, AuthPolicy, FirstUserTenant, Outcome, Provider,
        RegistrationPolicy, Request, Secret, SessionTenantPolicy,
    },
};
use reqwest::{Client, Method, Response};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;
#[path = "../../pp-storage/tests/common/cleanup.rs"]
mod cleanup;

struct Server {
    origin: String,
    client: Client,
    owner: WriterOwner,
    auth: AuthClient,
    task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::oneshot::Sender<()>,
    /// Last field: removed only after `owner` has dropped, even on panic.
    directory: Scratch,
}
impl Server {
    async fn start(
        registration: RegistrationPolicy,
        single: bool,
        dev: bool,
        public: Option<&str>,
    ) -> Self {
        let directory = Scratch::new("pp-http-test-");
        let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
        Self::from_owner(owner, directory, registration, single, dev, public).await
    }
    async fn from_owner(
        owner: WriterOwner,
        directory: Scratch,
        registration: RegistrationPolicy,
        single: bool,
        dev: bool,
        public: Option<&str>,
    ) -> Self {
        let auth = owner
            .auth_with_policy(AuthPolicy {
                registration,
                first_user: FirstUserTenant::NewUser,
                session_tenant: if single {
                    SessionTenantPolicy::SingleAccountDefault
                } else {
                    SessionTenantPolicy::AccountTenant
                },
            })
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("http://{address}");
        let router = auth_router(
            AuthHttpConfig::new(&origin, CookieTransport::LoopbackHttp, !single, public, dev)
                .unwrap(),
            auth.clone(),
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
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap();
        Self {
            origin,
            client,
            owner,
            auth,
            task,
            shutdown,
            directory,
        }
    }
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        cookie: Option<&str>,
    ) -> Response {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.origin))
            .header("Origin", &self.origin);
        if let Some(body) = body {
            request = request.json(&body);
        }
        if let Some(cookie) = cookie {
            request = request.header("Cookie", cookie);
        }
        request.send().await.unwrap()
    }
    async fn register(&self, email: &str) -> Response {
        self.request(Method::POST,"/auth/register",Some(json!({"email":email,"password":"password-1234","tenant_id":"forged","is_admin":false})),None).await
    }
    async fn stop(self) -> Scratch {
        self.shutdown.send(()).unwrap();
        self.task.await.unwrap();
        self.owner.shutdown().unwrap();
        self.directory
    }
    async fn close(self) {
        drop(self.stop().await);
    }
}
fn cookie(response: &Response) -> String {
    response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into()
}
async fn call(auth: &AuthClient, request: Request) -> anyhow::Result<Outcome> {
    let auth = auth.clone();
    tokio::task::spawn_blocking(move || {
        auth.submit(
            request,
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(1),
        )?
        .recv()
        .unwrap()
    })
    .await
    .unwrap()
}
fn secret(value: &str) -> Secret {
    Secret::new(value.into())
}

#[tokio::test]
async fn registration_race_and_owner_policy_are_transactional() {
    let server = Server::start(RegistrationPolicy::FirstAccountOnly, true, false, None).await;
    let (a, b) = tokio::join!(
        server.register("first@example.com"),
        server.register("second@example.com")
    );
    let mut statuses = vec![a.status().as_u16(), b.status().as_u16()];
    statuses.sort();
    assert_eq!(statuses, vec![200, 403]);
    let success = if a.status().is_success() { a } else { b };
    let cookie = cookie(&success);
    let user: Value = pp_api::problem::without_envelope(success.json().await.unwrap());
    assert_eq!(user["user"]["is_admin"], true);
    assert_eq!(user["user"]["user_id"].as_str().unwrap().len(), 36);
    assert!(user["user"].get("tenant_id").is_none());
    let Outcome::User(Some(user)) = call(
        &server.auth,
        Request::ResolveSession {
            token: secret(cookie.strip_prefix("pp_session=").unwrap()),
        },
    )
    .await
    .unwrap() else {
        panic!()
    };
    assert_eq!(user.tenant_id, "default");
    let health: Value = server
        .request(Method::GET, "/health", None, Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(health["authentication_required"], true);
    assert_eq!(health["authenticated"], true);
    assert_eq!(health["registration_open"], false);
    let closed = server
        .owner
        .auth_with_policy(AuthPolicy {
            registration: RegistrationPolicy::Closed,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        })
        .unwrap();
    let error = call(
        &closed,
        Request::Register {
            email: "third@example.com".into(),
            display_name: "Third".into(),
            password: secret("password-1234"),
        },
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(
        error.downcast_ref(),
        Some(AuthFailure::RegistrationClosed)
    ));
    let neutral = server.owner.auth(FirstUserTenant::NewUser);
    call(
        &neutral,
        Request::Register {
            email: "third@example.com".into(),
            display_name: "Third".into(),
            password: secret("password-1234"),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        server
            .request(Method::GET, "/auth/me", None, Some(&cookie))
            .await
            .status(),
        403
    );
    assert_eq!(
        server
            .request(Method::GET, "/settings/api-keys", None, Some(&cookie))
            .await
            .status(),
        403
    );
    let health: Value = server
        .request(Method::GET, "/health", None, Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        health["degraded_detail"],
        "Explicit account owner mapping is required"
    );
    assert_eq!(health["owner_mapping_required"], true);
    assert_eq!(health["authenticated"], false);
    server.close().await;
}

#[tokio::test]
async fn health_propagates_invalid_stored_session_provider() {
    let server = Server::start(RegistrationPolicy::Open, false, false, None).await;
    let response = server.register("corrupt-session@example.com").await;
    assert_eq!(response.status(), 200);
    let cookie = cookie(&response);
    let db = Connection::open(server.directory.join("print-partner.db")).unwrap();
    db.execute("UPDATE sessions SET provider='unknown'", [])
        .unwrap();
    drop(db);

    let response = server
        .request(Method::GET, "/health", None, Some(&cookie))
        .await;
    assert_eq!(response.status(), 500);
    assert_eq!(
        pp_api::problem::without_envelope(response.json::<Value>().await.unwrap())["detail"],
        "Authentication unavailable"
    );
    server.close().await;
}

#[tokio::test]
async fn guards_missing_routes_and_declared_rate_limits() {
    let server = Server::start(RegistrationPolicy::Open, false, false, None).await;
    for header in [
        "Forwarded",
        "X-Forwarded-Host",
        "X-Forwarded-For",
        "X-Forwarded-Proto",
    ] {
        assert_eq!(
            server
                .client
                .get(format!("{}/auth/me", server.origin))
                .header(header, "evil")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    for (header, value) in [
        ("Host", "evil.example"),
        ("Origin", "http://evil.example"),
        ("Origin", "null"),
        ("Sec-Fetch-Site", "cross-site"),
    ] {
        assert_eq!(
            server
                .client
                .get(format!("{}/auth/me", server.origin))
                .header(header, value)
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
    }
    assert_eq!(
        server
            .client
            .post(format!("{}/auth/login", server.origin))
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    for path in [
        "/auth/dev-login",
        "/api/v1/auth/me",
        "/api/v2/auth/me",
        "/auth/logout-all",
        "/auth/link-identity",
        "/api/v1/mcp",
        "/auth/discord/callback",
    ] {
        assert_eq!(
            server.request(Method::GET, path, None, None).await.status(),
            404
        );
    }
    assert_eq!(
        server
            .request(Method::HEAD, "/auth/github", None, None)
            .await
            .status(),
        501
    );
    assert_eq!(
        server
            .request(Method::HEAD, "/auth/discord", None, None)
            .await
            .status(),
        501
    );
    for attempt in 1..=21 {
        assert_eq!(
            server
                .request(Method::POST, "/auth/login", Some(json!({})), None)
                .await
                .status()
                .as_u16(),
            if attempt <= 20 { 400 } else { 429 },
            "attempt {attempt}"
        );
    }
    server.close().await;
    let server = Server::start(RegistrationPolicy::Open, false, false, None).await;
    let cookie = cookie(&server.register("keys@example.com").await);
    for attempt in 1..=11 {
        assert_eq!(
            server
                .request(
                    Method::POST,
                    "/settings/api-keys",
                    Some(json!({})),
                    Some(&cookie)
                )
                .await
                .status()
                .as_u16(),
            if attempt <= 10 { 201 } else { 429 },
            "attempt {attempt}"
        );
    }
    server.close().await;
}

#[tokio::test]
async fn keys_are_flattened_one_time_and_rotate_revoke_persist() {
    let server = Server::start(RegistrationPolicy::FirstAccountOnly, true, false, None).await;
    let cookie = cookie(&server.register("keys@example.com").await);
    assert_eq!(
        server
            .request(Method::POST, "/settings/api-keys", Some(json!({})), None)
            .await
            .status(),
        401
    );
    let created = server
        .request(
            Method::POST,
            "/settings/api-keys",
            Some(json!({})),
            Some(&cookie),
        )
        .await;
    assert_eq!(created.status(), 201);
    let key: Value = pp_api::problem::without_envelope(created.json().await.unwrap());
    let raw = key["key"].as_str().unwrap();
    let id = key["id"].as_str().unwrap();
    assert!(key.get("info").is_none());
    assert!(key.get("createdAt").is_some());
    assert!(key.get("keyHash").is_none());
    let Outcome::KeyResolved {
        principal: Some(principal),
        ..
    } = call(
        &server.auth,
        Request::ResolveKey {
            tenant_id: "default".into(),
            key: secret(raw),
        },
    )
    .await
    .unwrap()
    else {
        panic!()
    };
    assert_eq!(principal.tenant_id, "default");
    let rotated = server
        .request(
            Method::POST,
            &format!("/settings/api-keys/{id}/regenerate"),
            Some(json!({})),
            Some(&cookie),
        )
        .await;
    assert_eq!(rotated.status(), 201);
    let rotated: Value = pp_api::problem::without_envelope(rotated.json().await.unwrap());
    assert!(matches!(
        call(
            &server.auth,
            Request::ResolveKey {
                tenant_id: "default".into(),
                key: secret(raw)
            }
        )
        .await
        .unwrap(),
        Outcome::KeyResolved {
            principal: None,
            ..
        }
    ));
    let list: Value = server
        .request(Method::GET, "/settings/api-keys", None, Some(&cookie))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(list["total"], 2);
    assert!(!list.to_string().contains(raw));
    assert!(!list.to_string().contains("keyHash"));
    assert_eq!(
        server
            .request(Method::HEAD, "/settings/api-keys", None, Some(&cookie))
            .await
            .status(),
        200
    );
    assert_eq!(
        server
            .request(
                Method::DELETE,
                "/settings/api-keys/missing",
                None,
                Some(&cookie)
            )
            .await
            .status(),
        404
    );
    let response = server
        .request(
            Method::DELETE,
            &format!("/settings/api-keys/{}", rotated["id"].as_str().unwrap()),
            None,
            Some(&cookie),
        )
        .await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        pp_api::problem::without_envelope(response.json::<Value>().await.unwrap()),
        json!({"success":true})
    );
    let directory = server.stop().await;
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    let server = Server::from_owner(
        owner,
        directory,
        RegistrationPolicy::FirstAccountOnly,
        true,
        false,
        None,
    )
    .await;
    assert_eq!(
        server
            .request(Method::GET, "/settings/api-keys", None, Some(&cookie))
            .await
            .status(),
        200
    );
    assert!(matches!(
        call(
            &server.auth,
            Request::ResolveKey {
                tenant_id: "default".into(),
                key: secret(rotated["key"].as_str().unwrap())
            }
        )
        .await
        .unwrap(),
        Outcome::KeyResolved {
            principal: None,
            ..
        }
    ));
    assert_eq!(
        server
            .request(Method::POST, "/auth/logout", Some(json!({})), Some(&cookie))
            .await
            .status(),
        200
    );
    assert_eq!(
        server
            .request(Method::GET, "/settings/api-keys", None, Some(&cookie))
            .await
            .status(),
        401
    );
    server.close().await;
}

#[tokio::test]
async fn oauth_only_reset_preserves_identity_and_ordinary_no_enumeration() {
    let server = Server::start(RegistrationPolicy::Open, false, true, None).await;
    let Outcome::Session { user, token } = call(
        &server.auth,
        Request::OAuthLogin {
            provider: Provider::Github,
            provider_user_id: "123".into(),
            email: Some("oauth@example.com".into()),
            display_name: "OAuth".into(),
        },
    )
    .await
    .unwrap() else {
        panic!()
    };
    let old = format!("pp_session={}", token.expose());
    let change = server
        .request(
            Method::POST,
            "/auth/change-password",
            Some(json!({"current_password":"irrelevant","new_password":"password-new"})),
            Some(&old),
        )
        .await;
    assert_eq!(change.status(), 400);
    assert_eq!(
        pp_api::problem::without_envelope(change.json::<Value>().await.unwrap())["detail"],
        "This account uses OAuth sign-in only"
    );
    let forgot: Value = server
        .request(
            Method::POST,
            "/auth/forgot-password",
            Some(json!({"email":"  OAUTH@example.com  "})),
            None,
        )
        .await
        .json()
        .await
        .unwrap();
    let url = reqwest::Url::parse(forgot["dev_reset_url"].as_str().unwrap()).unwrap();
    let token = url
        .query_pairs()
        .find(|(k, _)| k == "token")
        .unwrap()
        .1
        .into_owned();
    let reset = server
        .request(
            Method::POST,
            "/auth/reset-password",
            Some(json!({"token":token,"password":"password-new"})),
            None,
        )
        .await;
    assert_eq!(reset.status(), 200);
    assert_eq!(
        pp_api::problem::without_envelope(reset.json::<Value>().await.unwrap())["user"]["user_id"],
        user.user_id
    );
    assert_eq!(
        server
            .request(Method::GET, "/auth/me", None, Some(&old))
            .await
            .status(),
        401
    );
    assert_eq!(
        server
            .request(
                Method::POST,
                "/auth/reset-password",
                Some(json!({"token":token,"password":"password-new"})),
                None
            )
            .await
            .status(),
        400
    );
    assert_eq!(
        server
            .request(
                Method::POST,
                "/auth/login",
                Some(json!({"email":"oauth@example.com","password":"password-new"})),
                None
            )
            .await
            .status(),
        200
    );
    server.close().await;
    for public in [None, Some("https://canonical.example")] {
        let server = Server::start(RegistrationPolicy::Open, false, false, public).await;
        server.register("known@example.com").await;
        let a: Value = server
            .request(
                Method::POST,
                "/auth/forgot-password",
                Some(json!({"email":"known@example.com"})),
                None,
            )
            .await
            .json()
            .await
            .unwrap();
        let b: Value = server
            .request(
                Method::POST,
                "/auth/forgot-password",
                Some(json!({"email":"unknown@example.com"})),
                None,
            )
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(a, b);
        assert!(a.get("dev_reset_url").is_none());
        let directory = server.stop().await;
        let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM password_reset_tokens", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, if public.is_some() { 1 } else { 0 });
        drop(db);
        drop(directory);
    }
}

#[tokio::test]
async fn old_default_keys_expiry_session_expiry_and_storage_errors_are_safe() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let server = Server::start(RegistrationPolicy::FirstAccountOnly, true, false, None).await;
    let cookie = cookie(&server.register("imported@example.com").await);
    let directory = server.stop().await;
    let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    let keys = json!([
        {"id":"legacy","keyHash":STANDARD.encode("legacy-token"),"createdAt":"2020-01-01T00:00:00.000Z","lastUsedAt":null,"expiresAt":null,"isActive":true},
        {"id":"expired","keyHash":STANDARD.encode("expired-token"),"createdAt":"2020-01-01T00:00:00.000Z","lastUsedAt":null,"expiresAt":"2000-01-01T00:00:00.000Z","isActive":true}
    ]);
    db.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','api_keys_v1',?1)",
        [keys.to_string()],
    )
    .unwrap();
    db.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES('default','fixture_domain_row','preserved')",[]).unwrap();
    drop(db);
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    let server = Server::from_owner(
        owner,
        directory,
        RegistrationPolicy::FirstAccountOnly,
        true,
        false,
        None,
    )
    .await;
    let list = server
        .request(Method::GET, "/settings/api-keys", None, Some(&cookie))
        .await;
    assert_eq!(list.status(), 200);
    let list: Value = pp_api::problem::without_envelope(list.json().await.unwrap());
    assert_eq!(list["total"], 2);
    assert!(!list.to_string().contains("token"));
    for (tenant, key, valid) in [
        ("default", "legacy-token", true),
        ("default", "expired-token", false),
        ("forged", "legacy-token", false),
    ] {
        let result = call(
            &server.auth,
            Request::ResolveKey {
                tenant_id: tenant.into(),
                key: secret(key),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            matches!(
                result,
                Outcome::KeyResolved {
                    principal: Some(_),
                    ..
                }
            ),
            valid
        );
    }
    let directory = server.stop().await;
    let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    let keys: String = db
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='api_keys_v1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!keys.contains("token"));
    let keys: Value = serde_json::from_str(&keys).unwrap();
    assert_eq!(keys[0]["keyHash"].as_str().unwrap().len(), 64);
    assert_eq!(
        db.query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='fixture_domain_row'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "preserved"
    );
    db.execute_batch("CREATE TRIGGER fail_key_write BEFORE UPDATE ON app_settings WHEN NEW.key='api_keys_v1' BEGIN SELECT RAISE(ABORT,'secret-sql-fixture'); END;").unwrap();
    drop(db);
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    let server = Server::from_owner(
        owner,
        directory,
        RegistrationPolicy::FirstAccountOnly,
        true,
        false,
        None,
    )
    .await;
    let response = server
        .request(
            Method::POST,
            "/settings/api-keys",
            Some(json!({})),
            Some(&cookie),
        )
        .await;
    assert_eq!(response.status(), 500);
    let body = response.text().await.unwrap();
    assert!(!body.contains("secret-sql-fixture"));
    assert!(!body.contains("SQL"));
    let directory = server.stop().await;
    let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
    db.execute(
        "UPDATE sessions SET expires_at='2000-01-01T00:00:00.000Z'",
        [],
    )
    .unwrap();
    drop(db);
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    let server = Server::from_owner(
        owner,
        directory,
        RegistrationPolicy::FirstAccountOnly,
        true,
        false,
        None,
    )
    .await;
    assert_eq!(
        server
            .request(Method::GET, "/auth/me", None, Some(&cookie))
            .await
            .status(),
        401
    );
    assert_eq!(
        server
            .request(Method::GET, "/settings/api-keys", None, Some(&cookie))
            .await
            .status(),
        401
    );
    server.close().await;
}

#[tokio::test]
async fn credential_error_codes_and_config_validation() {
    assert!(
        AuthHttpConfig::new(
            "http://0.0.0.0:9000",
            CookieTransport::LoopbackHttp,
            false,
            None,
            false
        )
        .is_err()
    );
    assert!(
        AuthHttpConfig::new(
            "http://127.0.0.1:9000",
            CookieTransport::Secure,
            false,
            None,
            false
        )
        .is_err()
    );
    assert!(
        AuthHttpConfig::new(
            "http://user@127.0.0.1:9000",
            CookieTransport::LoopbackHttp,
            false,
            None,
            false
        )
        .is_err()
    );
    assert!(
        AuthHttpConfig::new(
            "http://127.0.0.1:9000",
            CookieTransport::LoopbackHttp,
            false,
            Some("javascript:alert(1)"),
            false
        )
        .is_err()
    );
    let server = Server::start(RegistrationPolicy::Open, false, false, None).await;
    let cookie = cookie(&server.register("duplicate@example.com").await);
    assert_eq!(server.register("duplicate@example.com").await.status(), 409);
    assert_eq!(
        server
            .request(
                Method::POST,
                "/auth/login",
                Some(json!({"email":"duplicate@example.com","password":"wrong-password"})),
                None
            )
            .await
            .status(),
        401
    );
    assert_eq!(
        server
            .request(
                Method::POST,
                "/auth/change-password",
                Some(
                    json!({"current_password":"wrong-password","new_password":"replacement-good"})
                ),
                Some(&cookie)
            )
            .await
            .status(),
        401
    );
    assert_eq!(
        server
            .request(
                Method::POST,
                "/auth/change-password",
                Some(json!({"current_password":"password-1234","new_password":"short"})),
                Some(&cookie)
            )
            .await
            .status(),
        400
    );
    assert_eq!(
        server
            .request(Method::GET, "/auth/me", None, Some(&cookie))
            .await
            .status(),
        200
    );
    assert_eq!(
        server
            .request(
                Method::POST,
                "/auth/register",
                Some(json!({"email":"invalid","password":"valid-password"})),
                None
            )
            .await
            .status(),
        400
    );
    assert_eq!(
        server
            .request(
                Method::POST,
                "/auth/register",
                Some(json!({"email":"valid@example.com","password":"x".repeat(4097)})),
                None
            )
            .await
            .status(),
        400
    );
    server.close().await;
}

#[tokio::test]
async fn stopped_writer_is_service_unavailable() {
    let server = Server::start(RegistrationPolicy::Open, false, false, None).await;
    let Server {
        origin,
        client,
        owner,
        task,
        shutdown,
        directory,
        ..
    } = server;
    owner.shutdown().unwrap();
    let health = client.get(format!("{origin}/health")).send().await.unwrap();
    assert_eq!(health.status(), 503);
    let response = client
        .post(format!("{origin}/auth/login"))
        .header("Origin", &origin)
        .json(&json!({"email":"fixture@example.com","password":"password-good"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(
        pp_api::problem::without_envelope(response.json::<Value>().await.unwrap())["detail"],
        "Authentication temporarily unavailable"
    );
    shutdown.send(()).unwrap();
    task.await.unwrap();
    drop(directory);
}

fn domain_policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::FirstAccountOnly,
        session_tenant: SessionTenantPolicy::SingleAccountDefault,
        first_user: FirstUserTenant::NewUser,
    }
}
fn domain_catalog(owner: &WriterOwner, token: &str) -> pp_storage::catalog::SourceCatalogClient {
    owner
        .source_catalog_with_policy(
            pp_storage::catalog::Credentials::Session(secret(token)),
            domain_policy(),
        )
        .unwrap()
}
fn domain_read(
    client: &pp_storage::read_model::ReadClient,
    token: &str,
) -> anyhow::Result<pp_storage::read_model::Batch> {
    client.read(
        pp_storage::read_model::Credential::Session(secret(token)),
        &[1],
        &AtomicBool::new(false),
        Duration::from_secs(1),
    )
}
fn domain_key_read(
    client: &pp_storage::read_model::ReadClient,
    tenant: &str,
    key: &str,
) -> anyhow::Result<pp_storage::read_model::Batch> {
    client.read(
        pp_storage::read_model::Credential::ApiKey {
            routed_tenant: tenant.into(),
            secret: secret(key),
        },
        &[1],
        &AtomicBool::new(false),
        Duration::from_secs(1),
    )
}
fn domain_key_catalog(
    owner: &WriterOwner,
    tenant: &str,
    key: &str,
) -> pp_storage::catalog::SourceCatalogClient {
    owner
        .source_catalog_with_policy(
            pp_storage::catalog::Credentials::Key {
                tenant_id: tenant.into(),
                key: secret(key),
            },
            domain_policy(),
        )
        .unwrap()
}
async fn domain_server() -> Server {
    let directory = Scratch::new("pp-domain-test-");
    std::fs::create_dir_all(directory.join("repos/1/revisions/accepted")).unwrap();
    std::fs::write(
        directory.join("print-partner.db"),
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    {
        let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
        db.execute_batch("DELETE FROM sessions; DELETE FROM users;")
            .unwrap();
    }
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    Server::from_owner(
        owner,
        directory,
        RegistrationPolicy::FirstAccountOnly,
        true,
        false,
        None,
    )
    .await
}
fn domain_job(
    client: &pp_storage::jobs::AtomicJobClient,
    credential: pp_storage::jobs::Credential,
    key: &str,
) -> anyhow::Result<pp_storage::jobs::JobRecord> {
    use pp_storage::jobs::{Outcome, Payload, UserOperation};
    match client
        .submit(
            credential,
            UserOperation::Enqueue {
                key: key.into(),
                payload_version: 1,
                payload: Payload::CheckSourceUpdates {},
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )?
        .receive()?
    {
        Outcome::Job(job, _) => Ok(job),
        _ => panic!("Expected job"),
    }
}
fn job_session(token: &str) -> pp_storage::jobs::Credential {
    pp_storage::jobs::Credential::Session(secret(token))
}
fn job_key(tenant: &str, raw: &str) -> pp_storage::jobs::Credential {
    pp_storage::jobs::Credential::RoutedKey {
        tenant: tenant.into(),
        key: secret(raw),
    }
}
#[tokio::test]
async fn public_cookie_catalog_and_accepted_reads_share_default_tenant() {
    use pp_storage::{
        catalog::{CreateSource, Outcome as CatalogOutcome, Request as CatalogRequest},
        read_model::AcceptedRead,
    };
    let server = domain_server().await;
    let registered = server.register("domain@example.test").await;
    assert_eq!(registered.status(), 200);
    let registered: Value = pp_api::problem::without_envelope(registered.json().await.unwrap());
    let account = registered["user"]["user_id"].as_str().unwrap();
    assert_eq!(account.len(), 36);
    assert_ne!(account, "default");
    let login = server
        .request(
            Method::POST,
            "/auth/login",
            Some(json!({"email":"domain@example.test", "password":"password-1234"})),
            None,
        )
        .await;
    assert_eq!(login.status(), 200);
    let browser_cookie = cookie(&login);
    let token = browser_cookie.strip_prefix("pp_session=").unwrap();
    let catalog = domain_catalog(&server.owner, token);
    let jobs = server.owner.jobs(domain_policy()).unwrap();
    let cookie_job = domain_job(&jobs, job_session(token), "cookie-job").unwrap();
    assert_eq!(cookie_job.tenant, "default");
    let reader = server
        .owner
        .accepted_reads_with_policy(domain_policy())
        .unwrap();
    let before = domain_read(&reader, token).unwrap();
    let AcceptedRead::Ready { snapshot } = &before.builds[0].accepted else {
        panic!("existing accepted graph is ready")
    };
    let accepted_before = serde_json::to_value(snapshot).unwrap();
    let CatalogOutcome::Sources(existing) = catalog.execute(CatalogRequest::List {}).unwrap()
    else {
        panic!()
    };
    assert_eq!(existing[0].id, 1);
    let CatalogOutcome::Source(Some(created)) = catalog
        .execute(CatalogRequest::Create {
            source: CreateSource {
                name: "Authenticated source".into(),
                ..Default::default()
            },
        })
        .unwrap()
    else {
        panic!()
    };
    assert_ne!(created.id, 1);
    assert!(
        domain_catalog(&server.owner, "invalid")
            .execute(CatalogRequest::List {})
            .is_err()
    );
    assert!(domain_read(&reader, "invalid").is_err());
    assert!(domain_job(&jobs, job_session("invalid"), "invalid").is_err());
    let mut neutral_policy = domain_policy();
    neutral_policy.session_tenant = pp_storage::auth::SessionTenantPolicy::AccountTenant;
    let neutral_jobs = server.owner.jobs(neutral_policy).unwrap();
    assert_eq!(
        domain_job(&neutral_jobs, job_session(token), "neutral-job")
            .unwrap()
            .tenant,
        account
    );
    assert_eq!(
        domain_job(&jobs, job_session(token), "strict-still-default")
            .unwrap()
            .tenant,
        "default"
    );
    let neutral = server.owner.accepted_reads().unwrap();
    assert!(matches!(
        domain_read(&neutral, token).unwrap().builds[0].accepted,
        AcceptedRead::Missing
    ));
    let CatalogOutcome::Sources(neutral_sources) = server
        .owner
        .source_catalog(pp_storage::catalog::Credentials::Session(secret(token)))
        .execute(CatalogRequest::List {})
        .unwrap()
    else {
        panic!()
    };
    assert!(neutral_sources.is_empty());
    assert!(matches!(
        domain_read(&reader, token).unwrap().builds[0].accepted,
        AcceptedRead::Ready { .. }
    ));
    let key = server
        .request(
            Method::POST,
            "/settings/api-keys",
            Some(json!({})),
            Some(&browser_cookie),
        )
        .await;
    assert_eq!(key.status(), 201);
    let key: Value = pp_api::problem::without_envelope(key.json().await.unwrap());
    let raw = key["key"].as_str().unwrap();
    let key_catalog = domain_key_catalog(&server.owner, "default", raw);
    assert_eq!(
        domain_job(&jobs, job_key("default", raw), "key-job")
            .unwrap()
            .tenant,
        "default"
    );
    assert!(domain_job(&jobs, job_key(account, raw), "wrong-key-route").is_err());
    assert!(matches!(
        domain_key_read(&reader, "default", raw).unwrap().builds[0].accepted,
        AcceptedRead::Ready { .. }
    ));
    key_catalog
        .execute(CatalogRequest::Update {
            id: created.id,
            patch: pp_storage::catalog::SourcePatch {
                name: Some("Key source".into()),
                ..Default::default()
            },
        })
        .unwrap();
    assert!(domain_key_read(&reader, account, raw).is_err());
    assert!(
        domain_key_catalog(&server.owner, account, raw)
            .execute(CatalogRequest::List {})
            .is_err()
    );
    let revoked = server
        .request(
            Method::DELETE,
            &format!("/settings/api-keys/{}", key["id"].as_str().unwrap()),
            None,
            Some(&browser_cookie),
        )
        .await;
    assert_eq!(revoked.status(), 200);
    assert!(domain_key_read(&reader, "default", raw).is_err());
    assert!(key_catalog.execute(CatalogRequest::List {}).is_err());
    assert!(domain_job(&jobs, job_key("default", raw), "revoked-key").is_err());
    let changed = server
        .request(
            Method::POST,
            "/auth/change-password",
            Some(json!({"current_password":"password-1234","new_password":"password-5678"})),
            Some(&browser_cookie),
        )
        .await;
    assert_eq!(changed.status(), 200);
    let changed_cookie = cookie(&changed);
    let new_token = changed_cookie.strip_prefix("pp_session=").unwrap();
    assert!(domain_read(&reader, token).is_err());
    assert!(catalog.execute(CatalogRequest::List {}).is_err());
    assert!(domain_job(&jobs, job_session(token), "replaced-password").is_err());
    assert_eq!(
        domain_job(&jobs, job_session(new_token), "new-password")
            .unwrap()
            .tenant,
        "default"
    );
    let new_catalog = domain_catalog(&server.owner, new_token);
    new_catalog.execute(CatalogRequest::List {}).unwrap();
    let after = domain_read(&reader, new_token).unwrap();
    let AcceptedRead::Ready { snapshot } = &after.builds[0].accepted else {
        panic!()
    };
    assert_eq!(serde_json::to_value(snapshot).unwrap(), accepted_before);
    let logout = server
        .request(
            Method::POST,
            "/auth/logout",
            Some(json!({})),
            Some(&changed_cookie),
        )
        .await;
    assert_eq!(logout.status(), 200);
    assert!(domain_read(&reader, new_token).is_err());
    assert!(new_catalog.execute(CatalogRequest::List {}).is_err());
    assert!(domain_job(&jobs, job_session(new_token), "logged-out").is_err());
    let directory = server.stop().await;
    let owner = WriterOwner::open(&directory, Limits::default()).unwrap().0;
    let server = Server::from_owner(
        owner,
        directory,
        RegistrationPolicy::FirstAccountOnly,
        true,
        false,
        None,
    )
    .await;
    let login = server
        .request(
            Method::POST,
            "/auth/login",
            Some(json!({"email":"domain@example.test", "password":"password-5678"})),
            None,
        )
        .await;
    assert_eq!(login.status(), 200);
    let restarted_cookie = cookie(&login);
    let restarted_token = restarted_cookie.strip_prefix("pp_session=").unwrap();
    let restarted_jobs = server.owner.jobs(domain_policy()).unwrap();
    assert_eq!(
        domain_job(&restarted_jobs, job_session(restarted_token), "cookie-job")
            .unwrap()
            .job_id,
        cookie_job.job_id
    );
    assert!(domain_job(&restarted_jobs, job_key("default", raw), "restart-revoked").is_err());
    assert!(domain_job(&restarted_jobs, job_session(new_token), "restart-logout").is_err());
    let restarted_reader = server
        .owner
        .accepted_reads_with_policy(domain_policy())
        .unwrap();
    let restarted = domain_read(&restarted_reader, restarted_token).unwrap();
    let AcceptedRead::Ready { snapshot } = &restarted.builds[0].accepted else {
        panic!()
    };
    assert_eq!(serde_json::to_value(snapshot).unwrap(), accepted_before);
    let CatalogOutcome::Source(Some(persisted)) = domain_catalog(&server.owner, restarted_token)
        .execute(CatalogRequest::Get { id: created.id })
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(persisted.name, "Key source");
    assert!(domain_key_read(&restarted_reader, "default", raw).is_err());
    assert!(domain_read(&restarted_reader, new_token).is_err());
    let directory = server.stop().await;
    {
        let db = rusqlite::Connection::open(directory.join("print-partner.db")).unwrap();
        let build_tenant: String = db
            .query_row("SELECT tenant_id FROM build_profiles WHERE id=1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(build_tenant, "default");
        let sources: Vec<String> = db
            .prepare("SELECT tenant_id FROM projects ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(sources.iter().all(|tenant| tenant == "default"));
        let stored: String = db
            .query_row("SELECT id FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, account);
    }
    drop(directory);
}
#[tokio::test]
async fn imported_multiple_accounts_close_all_session_domain_clients() {
    use pp_storage::catalog::Request as CatalogRequest;
    let server = domain_server().await;
    let registered = server.register("domain@example.test").await;
    let browser_cookie = cookie(&registered);
    let token = browser_cookie.strip_prefix("pp_session=").unwrap();
    let catalog = domain_catalog(&server.owner, token);
    let jobs = server.owner.jobs(domain_policy()).unwrap();
    let cookie_job = domain_job(&jobs, job_session(token), "cookie-job").unwrap();
    assert_eq!(cookie_job.tenant, "default");
    let reader = server
        .owner
        .accepted_reads_with_policy(domain_policy())
        .unwrap();
    domain_read(&reader, token).unwrap();
    let neutral = server.owner.auth(FirstUserTenant::NewUser);
    call(
        &neutral,
        Request::Register {
            email: "imported@example.test".into(),
            display_name: "Imported".into(),
            password: secret("password-1234"),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        server
            .request(Method::GET, "/auth/me", None, Some(&browser_cookie))
            .await
            .status(),
        403
    );
    assert!(
        domain_read(&reader, token)
            .unwrap_err()
            .to_string()
            .contains("owner mapping")
    );
    assert!(
        catalog
            .execute(CatalogRequest::List {})
            .unwrap_err()
            .to_string()
            .contains("owner mapping")
    );
    assert!(
        domain_job(&jobs, job_session(token), "multiple-accounts")
            .err()
            .unwrap()
            .to_string()
            .contains("owner mapping")
    );
    server
        .owner
        .local_source_catalog()
        .execute(CatalogRequest::List {})
        .unwrap();
    server.close().await;
}

#[tokio::test]
async fn me_reports_password_presence_independent_of_session_provider() {
    let server = Server::start(RegistrationPolicy::Open, false, false, None).await;
    let password_cookie = cookie(&server.register("password@example.com").await);
    let Outcome::Session {
        token: oauth_token, ..
    } = call(
        &server.auth,
        Request::OAuthLogin {
            provider: Provider::Github,
            provider_user_id: "oauth-only".into(),
            email: Some("oauth-only@example.com".into()),
            display_name: "OAuth only".into(),
        },
    )
    .await
    .unwrap()
    else {
        panic!()
    };
    let linked_cookie = cookie(&server.register("linked@example.com").await);
    call(
        &server.auth,
        Request::LinkIdentity {
            session: secret(linked_cookie.strip_prefix("pp_session=").unwrap()),
            provider: Provider::Github,
            provider_user_id: "linked".into(),
        },
    )
    .await
    .unwrap();
    let Outcome::Session {
        token: linked_token,
        ..
    } = call(
        &server.auth,
        Request::OAuthLogin {
            provider: Provider::Github,
            provider_user_id: "linked".into(),
            email: Some("linked@example.com".into()),
            display_name: "Linked".into(),
        },
    )
    .await
    .unwrap()
    else {
        panic!()
    };
    for (cookie, provider, has_password) in [
        (password_cookie, "email", true),
        (
            format!("pp_session={}", oauth_token.expose()),
            "github",
            false,
        ),
        (
            format!("pp_session={}", linked_token.expose()),
            "github",
            true,
        ),
    ] {
        let response = server
            .request(Method::GET, "/auth/me", None, Some(&cookie))
            .await;
        assert_eq!(response.status(), 200);
        let body: Value = pp_api::problem::without_envelope(response.json().await.unwrap());
        assert_eq!(body["user"]["provider"], provider);
        assert_eq!(body["user"]["hasPassword"], has_password);
        assert!(body["user"].get("hash").is_none());
        assert!(body["user"].get("password_hash").is_none());
    }
    server.close().await;
}

#[test]
fn scratch_root_is_empty_after_auth_http_tests() {
    cleanup::assert_test_run_clean(&["--skip", "scratch_root_is_empty_after_auth_http_tests"]);
}
