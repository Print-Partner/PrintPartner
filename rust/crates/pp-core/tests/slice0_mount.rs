//! Rust product routes mounted in the gateway on a disposable loopback
//! listener, with no compatibility child.
#![cfg(feature = "desktop_rebuild")]

#[path = "support/jobs.rs"]
mod support_jobs;

mod with_feature {
    use pp_core::{
        DesktopRebuild, VerifiedBundle,
        rebuild_api::{desktop_policy, product_router},
    };
    use pp_storage::{
        Limits, Setting, SettingCommand, WriterOwner,
        auth::Secret,
        build_graph::{BuildCommand, BuildName, BuildOutcome},
        read_model,
    };
    use serde_json::{Value, json};
    use std::{
        net::SocketAddr,
        path::{Path, PathBuf},
        sync::{Arc, Mutex, atomic::AtomicBool},
        time::Duration,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const SLUG: &str = "slice0-kit";

    fn open(directory: &Path) -> Arc<Mutex<WriterOwner>> {
        let (owner, _) =
            WriterOwner::open_with_job_feature_admission(directory, Limits::default()).unwrap();
        Arc::new(Mutex::new(owner))
    }

    /// Shut storage down once no router holds it.
    fn close(owner: Arc<Mutex<WriterOwner>>) {
        let Ok(owner) = Arc::try_unwrap(owner) else {
            panic!("a router still holds storage");
        };
        owner.into_inner().unwrap().shutdown().unwrap();
    }

    /// A disposable bundle layout: only the paths the mount reads.
    fn bundle(root: &Path) -> VerifiedBundle {
        let web = root.join("web");
        VerifiedBundle {
            node: root.join("node"),
            entry: web.join("apps/server/dist/current/desktop.js"),
            web_root: web,
            runtime_version: "0.0.0-web".into(),
            commit: "0".repeat(40),
        }
    }

    struct Fixture {
        root: PathBuf,
        owner: Arc<Mutex<WriterOwner>>,
        gateway: pp_gateway::Gateway,
        origin: String,
        desktop: String,
        client: reqwest::Client,
    }

    impl Fixture {
        async fn start(value: &str) -> Self {
            let root = tempfile::tempdir().unwrap().keep();
            let assets = root.join("assets");
            std::fs::create_dir_all(&assets).unwrap();
            let data = root.join("data");
            std::fs::create_dir_all(&data).unwrap();
            let owner = open(&data);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            // The same call the Rust-only runtime makes: the bundle names the
            // community manifests directory.
            let manifests = bundle(&root).community_manifests_dir();
            let product = product_router(
                DesktopRebuild::from_value(Some(value)),
                Arc::clone(&owner),
                &origin,
                manifests.as_deref(),
            )
            .unwrap()
            .expect("both gates on mount the product routes");
            let mut gateway = pp_gateway::Gateway::start_with_product(
                listener,
                assets,
                pp_compat::CompatHandle::unpublished(),
                &pp_gateway::ownership::cutover::OperationOwners::node_only(),
                Some(product),
            )
            .unwrap();
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let bootstrap = client
                .get(gateway.take_launch_target().unwrap().into_url())
                .send()
                .await
                .unwrap();
            assert_eq!(bootstrap.status(), 303);
            let desktop = first_cookie(&bootstrap);
            Self {
                root,
                owner,
                gateway,
                origin,
                desktop,
                client,
            }
        }

        async fn get(&self, path: &str, session: Option<&str>) -> (u16, Value) {
            let cookie = match session {
                Some(session) => format!("{}; {session}", self.desktop),
                None => self.desktop.clone(),
            };
            let response = self
                .client
                .get(format!("{}{path}", self.origin))
                .header("cookie", cookie)
                .send()
                .await
                .unwrap();
            (
                response.status().as_u16(),
                response.json().await.unwrap_or(Value::Null),
            )
        }

        /// A desktop-admitted request with the given Host and Origin headers;
        /// `None` keeps the client's Host or sends no Origin.
        async fn send(
            &self,
            method: reqwest::Method,
            path: &str,
            host: Option<&str>,
            origin: Option<&str>,
        ) -> (u16, String) {
            let mut request = self
                .client
                .request(method, format!("{}{path}", self.origin))
                .header("cookie", &self.desktop)
                .json(&json!({"email": "slice0@example.test", "password": "password-1234"}));
            if let Some(host) = host {
                request = request.header("host", host);
            }
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = request.send().await.unwrap();
            (response.status().as_u16(), response.text().await.unwrap())
        }

        async fn register(&self) -> String {
            let response = self
                .client
                .post(format!("{}/auth/register", self.origin))
                .header("origin", &self.origin)
                .header("cookie", &self.desktop)
                .json(&json!({"email": "slice0@example.test", "password": "password-1234"}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            first_cookie(&response)
        }

        async fn stop(self) {
            self.gateway.drain().await;
            self.gateway.stop().await.unwrap();
            close(self.owner);
            std::fs::remove_dir_all(self.root).unwrap();
        }
    }

    fn first_cookie(response: &reqwest::Response) -> String {
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    fn create_plan(owner: &Mutex<WriterOwner>, token: &str) -> i64 {
        create_named_plan(owner, token, "Slice Zero")
    }

    fn create_named_plan(owner: &Mutex<WriterOwner>, token: &str, name: &str) -> i64 {
        let client = owner
            .lock()
            .unwrap()
            .build_graph_with_policy(desktop_policy())
            .unwrap();
        match client
            .execute(
                read_model::Credential::Session(Secret::new(token.into())),
                BuildCommand::Create {
                    name: BuildName::parse(name.into()).unwrap(),
                    base_source: None,
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
        {
            BuildOutcome::Created { profile, .. } => profile.id,
            other => panic!("{other:?}"),
        }
    }

    fn enqueue(owner: &Mutex<WriterOwner>, token: &str) -> String {
        let jobs = owner.lock().unwrap().jobs(desktop_policy()).unwrap();
        super::support_jobs::enqueue_check(&jobs, token, "slice0")
    }

    /// Whether storage still accepts the Rust session `token`.
    fn session_valid(owner: &Mutex<WriterOwner>, token: &str) -> bool {
        let builds = owner
            .lock()
            .unwrap()
            .build_graph_with_policy(desktop_policy())
            .unwrap();
        builds
            .execute(
                read_model::Credential::Session(Secret::new(token.into())),
                BuildCommand::List,
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .is_ok()
    }

    /// The mounted routes inherit the gateway's Host and Origin checks.
    async fn refuses_foreign_host_and_origin(fixture: &Fixture, value: &str) {
        use reqwest::Method;
        let own = Some(fixture.origin.as_str());
        let foreign = Some("http://evil.example");
        for (method, path, host, origin, status, detail) in [
            (
                Method::GET,
                "/api/v1/plans",
                Some("evil.example"),
                own,
                400,
                "Invalid Host",
            ),
            (
                Method::POST,
                "/auth/register",
                Some("evil.example"),
                own,
                400,
                "Invalid Host",
            ),
            (
                Method::GET,
                "/api/v1/plans",
                None,
                foreign,
                403,
                "Invalid Origin",
            ),
            (
                Method::POST,
                "/auth/register",
                None,
                foreign,
                403,
                "Invalid Origin",
            ),
            (
                Method::POST,
                "/auth/register",
                None,
                None,
                403,
                "Invalid Origin",
            ),
        ] {
            let label = format!("{value} {method} {path} host={host:?} origin={origin:?}");
            let (got, body) = fixture.send(method, path, host, origin).await;
            assert_eq!(got, status, "{label}: {body}");
            assert!(body.contains(detail), "{label}: {body}");
        }
    }

    /// Sends two requests no Rust route serves, which get a Rust 404, and
    /// returns the proxy attempts the gateway counted at its route-miss path.
    async fn route_misses(fixture: &Fixture, session: &str) -> u64 {
        let (status, body) = fixture.get("/api/v1/printers", Some(session)).await;
        assert_eq!(status, 404, "{body}");
        let response = fixture
            .client
            .post(format!("{}/api/v1/integrations", fixture.origin))
            .header("origin", &fixture.origin)
            .header("cookie", format!("{}; {session}", fixture.desktop))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 404);
        fixture.gateway.compat_forwards()
    }

    /// The generated report the operation inventory quotes. Never edit it by
    /// hand: rerun this test with `PP_JOURNEY_REPORT=1`.
    const JOURNEY_REPORT: &str = "docs/parity/journey-route-misses.json";

    fn write_journey_report(runs: Vec<Value>) {
        if std::env::var_os("PP_JOURNEY_REPORT").is_none_or(|value| value != "1") {
            return;
        }
        let report = json!({
            "generated_by": "PP_JOURNEY_REPORT=1 cargo nextest run (slice0_mount_routes_answer_through_the_gateway)",
            "journey": "pp-core product routes in the gateway, desktop_rebuild feature on: register, Plan, draft and job reads, job WebSocket, manifest reads, two unrouted paths",
            "counted_at": "gateway route-miss path (Gateway::compat_forwards)",
            "runs": runs,
        });
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join(JOURNEY_REPORT);
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&report).unwrap()),
        )
        .unwrap();
    }

    /// Accounts registered with the test email.
    fn accounts(data: &Path) -> i64 {
        let db = rusqlite::Connection::open(data.join("print-partner.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db.query_row(
            "SELECT COUNT(*) FROM users WHERE email='slice0@example.test'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    /// One Source layer with a checkout and the community slug, added to the
    /// Plan directly because no mounted route sets a community slug.
    fn add_community_layer(data: &Path, checkout: &Path, plan: i64) {
        let db = rusqlite::Connection::open(data.join("print-partner.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db.execute(
            "INSERT INTO projects(id,tenant_id,name,url,local_path,manifest_community_slug) VALUES(900,'default','Slice Kit','https://example.test/slice-kit',?1,?2)",
            rusqlite::params![checkout.to_str().unwrap(), SLUG],
        )
        .unwrap();
        db.execute(
            "INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) VALUES('default',?1,1,'addon',900)",
            [plan],
        )
        .unwrap();
    }

    /// Opens a WebSocket through the gateway and returns the status line.
    async fn upgrade(fixture: &Fixture, path: &str, cookie: &str) -> String {
        let host = fixture.origin.trim_start_matches("http://");
        let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nCookie: {cookie}\r\n\r\n",
            fixture.origin
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut seen = Vec::new();
        while !seen.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).await.unwrap();
            seen.push(byte[0]);
        }
        let head = String::from_utf8(seen).unwrap();
        head.lines().next().unwrap().to_owned()
    }

    #[test]
    fn slice0_mount_runtime_setting_off_mounts_nothing() {
        let root = tempfile::tempdir().unwrap();
        let owner = open(root.path());
        for value in [None, Some(""), Some("0"), Some("false"), Some("TRUE")] {
            assert!(
                product_router(
                    DesktopRebuild::from_value(value),
                    Arc::clone(&owner),
                    "http://127.0.0.1:9",
                    None,
                )
                .unwrap()
                .is_none(),
                "{value:?}"
            );
        }
        close(owner);
    }

    #[tokio::test]
    async fn slice0_mount_routes_answer_through_the_gateway() {
        let mut runs = Vec::new();
        for value in ["1", "true"] {
            let fixture = Fixture::start(value).await;
            let routes = [
                "/api/v1/plans".to_owned(),
                "/api/v1/plans/1".to_owned(),
                "/api/v2/plans/1/drafts".to_owned(),
                "/api/v1/jobs".to_owned(),
            ];
            // Desktop admission alone is not a session: Rust auth answers 401.
            for path in &routes {
                let (status, body) = fixture.get(path, None).await;
                assert_eq!(status, 401, "{value} {path} {body}");
                assert_eq!(body["detail"], "Authentication required", "{path}");
            }
            assert!(
                upgrade(&fixture, "/ws/jobs/1", &fixture.desktop)
                    .await
                    .contains(" 401 "),
                "{value}"
            );
            // Without the desktop cookie the gateway refuses first.
            let response = fixture
                .client
                .get(format!("{}/api/v1/plans", fixture.origin))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 401);

            refuses_foreign_host_and_origin(&fixture, value).await;
            // None of the refused registrations reached storage.
            let data = fixture.root.join("data");
            assert_eq!(accounts(&data), 0, "{value}");
            let session = fixture.register().await;
            assert_eq!(accounts(&data), 1, "{value}");
            let token = session.strip_prefix("pp_session=").unwrap().to_owned();
            let plan = create_plan(&fixture.owner, &token);
            let (status, body) = fixture.get("/api/v1/plans", Some(&session)).await;
            assert_eq!(status, 200, "{body}");
            assert_eq!(body["profiles"][0]["id"], plan);
            let (status, body) = fixture
                .get(&format!("/api/v1/plans/{plan}"), Some(&session))
                .await;
            assert_eq!(status, 200, "{body}");
            assert_eq!(body["name"], "Slice Zero", "{body}");
            let (status, body) = fixture
                .get(&format!("/api/v2/plans/{plan}/drafts"), Some(&session))
                .await;
            assert_eq!(status, 200, "{body}");
            let job = enqueue(&fixture.owner, &token);
            let (status, body) = fixture.get("/api/v1/jobs", Some(&session)).await;
            assert_eq!(status, 200, "{body}");
            assert!(body.to_string().contains(&job), "{body}");
            let both = format!("{}; {session}", fixture.desktop);
            let line = upgrade(&fixture, &format!("/ws/jobs/{job}"), &both).await;
            assert!(line.starts_with("HTTP/1.1 101"), "{line}");

            // The bundle's community manifests directory reaches the reads.
            let checkout = fixture.root.join("checkout");
            std::fs::create_dir_all(checkout.join("toolhead/custom")).unwrap();
            std::fs::create_dir_all(checkout.join("toolhead/stock")).unwrap();
            std::fs::write(checkout.join("toolhead/custom/b.stl"), b"solid b").unwrap();
            std::fs::write(checkout.join("toolhead/stock/a.stl"), b"solid a").unwrap();
            add_community_layer(&fixture.root.join("data"), &checkout, plan);
            let builder = format!("/api/v1/plans/{plan}/plan-manifest-builder");
            let (status, missing) = fixture.get(&builder, Some(&session)).await;
            assert_eq!(status, 200, "{missing}");
            let manifests = bundle(&fixture.root).community_manifests_dir().unwrap();
            std::fs::create_dir_all(&manifests).unwrap();
            std::fs::write(
                manifests.join(format!("{SLUG}.yaml")),
                "selections:\n  toolhead: custom\n",
            )
            .unwrap();
            let (status, present) = fixture.get(&builder, Some(&session)).await;
            assert_eq!(status, 200, "{present}");
            assert_ne!(missing, present, "community manifest read as missing");
            assert!(present.to_string().contains("custom"), "{present}");

            let proxy_attempts = route_misses(&fixture, &session).await;
            assert_eq!(proxy_attempts, 0, "{value}");
            runs.push(json!({"PP_DESKTOP_REBUILD": value, "proxy_attempts": proxy_attempts}));

            // Logout revokes the Rust session as well as the desktop one.
            assert!(session_valid(&fixture.owner, &token));
            let response = fixture
                .client
                .post(format!("{}/auth/logout", fixture.origin))
                .header("origin", &fixture.origin)
                .header("cookie", &both)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            assert!(first_cookie(&response).starts_with("pp_session="));
            assert!(!session_valid(&fixture.owner, &token), "{value}");
            fixture.stop().await;
        }
        write_journey_report(runs);
    }

    /// The mounted routes refuse a non-loopback peer themselves, not only
    /// through the gateway's loopback listener.
    #[tokio::test]
    async fn slice0_mount_refuses_a_non_loopback_peer() {
        use tower::ServiceExt;
        let root = tempfile::tempdir().unwrap();
        let owner = open(root.path());
        let product = product_router(
            DesktopRebuild::from_value(Some("1")),
            Arc::clone(&owner),
            "http://127.0.0.1:9",
            None,
        )
        .unwrap()
        .unwrap();
        for path in ["/api/v1/plans", "/api/v1/jobs", "/api/v2/plans/1/drafts"] {
            let mut request = axum::http::Request::builder()
                .uri(path)
                .header("host", "127.0.0.1:9")
                .body(axum::body::Body::empty())
                .unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                "192.0.2.10:443".parse::<SocketAddr>().unwrap(),
            ));
            let response = product.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), 403, "{path}");
        }
        drop(product);
        close(owner);
    }

    /// Store a `default`-tenant setting while the gateway keeps serving.
    fn put(fixture: &Fixture, key: &str, value: &str) {
        let setting = Setting {
            tenant: "default".into(),
            key: key.into(),
            value: value.into(),
        };
        let settings = fixture.owner.lock().unwrap().client();
        let wait = Duration::from_secs(5);
        let reply = settings.submit(SettingCommand::Set(setting), &AtomicBool::new(false), wait);
        assert!(reply.unwrap().recv().unwrap().unwrap());
    }

    /// The `/health` body as sent, and its `modules` list.
    async fn health_modules(fixture: &Fixture, session: Option<&str>) -> (String, Value) {
        let (status, body) = fixture.get("/health", session).await;
        assert_eq!(status, 200, "{body}");
        (body.to_string(), body["modules"].clone())
    }

    fn off(module: &str, reason: &str) -> Value {
        json!({"state": "off", "module": module, "reason": reason})
    }

    fn on(module: &str) -> Value {
        json!({"state": "on", "module": module})
    }

    /// Every module is listed, and a stored change shows on the next request
    /// to the same running gateway.
    #[tokio::test]
    async fn health_modules_list_every_module_and_follow_changes_without_restart() {
        let fixture = Fixture::start("1").await;
        let (_, modules) = health_modules(&fixture, None).await;
        let names = ["farm", "spoolman", "orders", "bambu", "assembly_tracking"];
        let defaults: Vec<Value> = names.map(|name| off(name, "default_off")).to_vec();
        assert_eq!(modules, json!(defaults));

        put(
            &fixture,
            "feature_toggles",
            r#"{"farm":true,"spoolman":true}"#,
        );
        put(&fixture, "build_tracking_assembly", "1");
        let (_, modules) = health_modules(&fixture, None).await;
        assert_eq!(
            modules,
            json!([
                on("farm"),
                on("spoolman"),
                off("orders", "default_off"),
                off("bambu", "default_off"),
                on("assembly_tracking"),
            ])
        );

        put(
            &fixture,
            "feature_toggles",
            r#"{"farm":false,"spoolman":true}"#,
        );
        put(&fixture, "build_tracking_assembly", "0");
        let (_, modules) = health_modules(&fixture, None).await;
        assert_eq!(modules[0], off("farm", "turned_off"));
        assert_eq!(modules[1], on("spoolman"));
        assert_eq!(modules[4], off("assembly_tracking", "turned_off"));
        fixture.stop().await;
    }

    /// Canary: secret-looking values in module, Spoolman and Bambu settings,
    /// the session token, the account email and a Plan name never reach the
    /// body, and neither does any URL.
    #[tokio::test]
    async fn health_modules_canary_payload_has_no_secrets_urls_or_customer_data() {
        let fixture = Fixture::start("1").await;
        let session = fixture.register().await;
        let token = session.strip_prefix("pp_session=").unwrap().to_owned();
        let plan_name = "Canary Customer Order 7f3a";
        create_named_plan(&fixture.owner, &token, plan_name);
        put(
            &fixture,
            "feature_toggles",
            &json!({
                "farm": true,
                "bambu": true,
                "spoolman_url": "https://canary-spoolman.invalid:7912/api?token=canary-toggle-token-c0de",
                "bambu_access_code": "canary-bambu-access-code-91d2",
            })
            .to_string(),
        );
        put(
            &fixture,
            "integrations",
            &json!([
                {"id": "canary-integration-id-4e4e", "kind": "spoolman",
                 "base_url": "http://canary-spoolman.invalid:7912",
                 "api_key": "canary-spoolman-api-key-5b1e"},
                {"kind": "bambu", "host": "http://10.0.0.77",
                 "serial": "canary-bambu-serial-01P00C",
                 "access_code": "canary-bambu-access-code-91d2"},
            ])
            .to_string(),
        );
        put(
            &fixture,
            "default_spoolman_integration_id",
            "canary-integration-id-4e4e",
        );
        put(
            &fixture,
            "build_tracking_assembly",
            "canary-assembly-value-77aa",
        );
        let leaks = [
            "canary",
            "slice0@example.test",
            plan_name,
            token.as_str(),
            "://",
            "10.0.0.77",
        ];
        for session in [None, Some(session.as_str())] {
            let (body, modules) = health_modules(&fixture, session).await;
            assert_eq!(modules[0], on("farm"));
            assert_eq!(modules[3], on("bambu"));
            assert_eq!(modules[4], on("assembly_tracking"));
            for leak in leaks {
                assert!(!body.contains(leak), "health leaked {leak}: {body}");
            }
        }

        // An unreadable toggle setting fails closed and does not echo itself.
        put(
            &fixture,
            "feature_toggles",
            r#"{"farm":"canary-toggle-token-c0de"}"#,
        );
        let (body, modules) = health_modules(&fixture, None).await;
        assert_eq!(modules[0], off("farm", "setting_unreadable"));
        assert!(!body.contains("canary"), "{body}");
        fixture.stop().await;
    }

    /// `PUT /api/v2/modules/{module}` with `body`, and its status and body.
    async fn set_module(
        fixture: &Fixture,
        session: Option<&str>,
        module: &str,
        body: &str,
    ) -> (u16, Value) {
        let cookie = match session {
            Some(session) => format!("{}; {session}", fixture.desktop),
            None => fixture.desktop.clone(),
        };
        let response = fixture
            .client
            .put(format!("{}/api/v2/modules/{module}", fixture.origin))
            .header("origin", &fixture.origin)
            .header("cookie", cookie)
            .header("content-type", "application/json")
            .body(body.to_owned())
            .send()
            .await
            .unwrap();
        (
            response.status().as_u16(),
            response.json().await.unwrap_or(Value::Null),
        )
    }

    /// The stored text of a `default`-tenant setting.
    fn stored(fixture: &Fixture, key: &str) -> Option<String> {
        rusqlite::Connection::open(fixture.root.join("data").join("print-partner.db"))
            .unwrap()
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id='default' AND key=?1",
                [key],
                |row| row.get(0),
            )
            .ok()
    }

    /// Each Module turns on and off through the route; the answer, `/health`
    /// and the feature toggles that workers admit sends by all show it, and
    /// stored keys the route does not own stay.
    #[tokio::test]
    async fn module_route_turns_each_module_on_and_off_and_health_follows() {
        let fixture = Fixture::start("1").await;
        let session = fixture.register().await;
        put(&fixture, "feature_toggles", r#"{"farm":true,"kept_key":7}"#);
        let plan_layer = r#"{"orders":false}"#;
        put(&fixture, "feature_toggles:plan:1", plan_layer);
        let names = ["farm", "spoolman", "orders", "bambu", "assembly_tracking"];
        for (index, name) in names.into_iter().enumerate() {
            for (choice, expected) in [("false", off(name, "turned_off")), ("true", on(name))] {
                let body = format!(r#"{{"on": {choice}}}"#);
                let (status, answer) = set_module(&fixture, Some(&session), name, &body).await;
                assert_eq!(status, 200, "{name} {choice}: {answer}");
                assert_eq!(answer["modules"][index], expected, "{name} {choice}");
                let (_, modules) = health_modules(&fixture, None).await;
                assert_eq!(answer["modules"], modules, "{name} {choice}");
            }
        }
        let toggles: Value =
            serde_json::from_str(&stored(&fixture, "feature_toggles").unwrap()).unwrap();
        assert_eq!(
            toggles,
            json!({"bambu": true, "farm": true, "kept_key": 7, "orders": true, "spoolman": true})
        );
        assert_eq!(
            stored(&fixture, "build_tracking_assembly").as_deref(),
            Some("1")
        );
        assert_eq!(
            stored(&fixture, "feature_toggles:plan:1").as_deref(),
            Some(plan_layer),
            "a Plan override stays"
        );

        set_module(&fixture, Some(&session), "farm", r#"{"on": false}"#).await;
        use pp_storage::features::modules::Module;
        let token = session.strip_prefix("pp_session=").unwrap().to_owned();
        let features = fixture
            .owner
            .lock()
            .unwrap()
            .features_with_policy(pp_core::rebuild_api::desktop_policy())
            .unwrap();
        let view = features
            .read(
                pp_storage::read_model::Credential::Session(pp_storage::auth::Secret::new(token)),
                None,
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(
            !view.effective().enabled(Module::Farm.feature().unwrap()),
            "send admission reads the same toggle"
        );
        fixture.stop().await;
    }

    /// No session, an unknown Module or a body other than `{"on": bool}`
    /// changes nothing. Unreadable stored toggles refuse a feature Module with
    /// Node's 500 and stay as stored, as `/health` reports them.
    #[tokio::test]
    async fn module_route_refuses_without_writing() {
        let fixture = Fixture::start("1").await;
        let session = fixture.register().await;
        let on_body = r#"{"on": true}"#;
        let (status, _) = set_module(&fixture, None, "farm", on_body).await;
        assert_eq!(status, 401);
        let (status, _) =
            set_module(&fixture, Some("pp_session=not-a-session"), "farm", on_body).await;
        assert_eq!(status, 401, "storage refuses an unknown session");
        let (status, body) = set_module(&fixture, Some(&session), "printers", on_body).await;
        assert_eq!((status, &body["detail"]), (404, &json!("Module not found")));
        for bad in [
            r#"{"on": "yes"}"#,
            "{}",
            r#"{"on": true, "plan": 1}"#,
            "not json",
        ] {
            let (status, _) = set_module(&fixture, Some(&session), "farm", bad).await;
            assert_eq!(status, 400, "{bad}");
        }
        assert_eq!(stored(&fixture, "feature_toggles"), None);

        let corrupt = r#"{"farm":"yes"}"#;
        put(&fixture, "feature_toggles", corrupt);
        let (status, body) = set_module(&fixture, Some(&session), "orders", on_body).await;
        assert_eq!(status, 500, "{body}");
        assert_eq!(body["code"], json!("FEATURE_TOGGLES_INVALID"));
        assert_eq!(
            stored(&fixture, "feature_toggles").as_deref(),
            Some(corrupt)
        );
        let (_, modules) = health_modules(&fixture, None).await;
        assert_eq!(modules[2], off("orders", "setting_unreadable"));
        let (status, body) =
            set_module(&fixture, Some(&session), "assembly_tracking", on_body).await;
        assert_eq!(status, 200, "assembly tracking is its own setting: {body}");
        assert_eq!(body["modules"][4], on("assembly_tracking"));

        let mut statuses = Vec::new();
        for _ in 0..31 {
            let (status, _) =
                set_module(&fixture, Some(&session), "assembly_tracking", on_body).await;
            statuses.push(status);
        }
        // Two requests above (the refused orders change and the assembly
        // change) reached the write and used the window.
        assert_eq!(statuses.iter().filter(|status| **status == 200).count(), 28);
        assert_eq!(statuses.last(), Some(&429), "30 changes a minute");
        fixture.stop().await;
    }

    /// With the rebuild flag off the route is not mounted, so nothing in Rust
    /// writes a Module and Node keeps `PATCH /settings/features`.
    #[tokio::test]
    async fn module_route_is_absent_with_the_flag_off() {
        let root = tempfile::tempdir().unwrap();
        let owner = open(root.path());
        let product = product_router(
            DesktopRebuild::from_value(Some("0")),
            Arc::clone(&owner),
            "http://127.0.0.1:9",
            None,
        )
        .unwrap();
        assert!(product.is_none(), "no product route, this one included");
        close(owner);
    }

    /// A module read that cannot get a reader within its 100 ms wait must not
    /// fail liveness: `/health` still answers 200 with its other fields and
    /// every module `setting_unreadable`, and recovers when readers free up.
    #[tokio::test]
    async fn health_answers_200_with_modules_unreadable_when_the_reader_pool_is_saturated() {
        let fixture = Fixture::start("1").await;
        put(&fixture, "feature_toggles", r#"{"farm":true}"#);
        let settings = fixture.owner.lock().unwrap().client();
        let held: Vec<_> = (0..4)
            .map(|_| settings.reader(Duration::ZERO).unwrap())
            .collect();
        let (status, body) = fixture.get("/health", None).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["authentication_required"], json!(true));
        assert_eq!(body["authenticated"], json!(false));
        assert_eq!(body["degraded_detail"], Value::Null);
        let unreadable: Vec<Value> = ["farm", "spoolman", "orders", "bambu", "assembly_tracking"]
            .iter()
            .map(|module| off(module, "setting_unreadable"))
            .collect();
        assert_eq!(body["modules"], json!(unreadable));
        drop(held);
        let (_, modules) = health_modules(&fixture, None).await;
        assert_eq!(modules[0], on("farm"));
        fixture.stop().await;
    }
}
