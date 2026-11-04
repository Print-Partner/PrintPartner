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
        Limits, WriterOwner,
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
        let client = owner
            .lock()
            .unwrap()
            .build_graph_with_policy(desktop_policy())
            .unwrap();
        match client
            .execute(
                read_model::Credential::Session(Secret::new(token.into())),
                BuildCommand::Create {
                    name: BuildName::parse("Slice Zero".into()).unwrap(),
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

            assert_eq!(fixture.gateway.compat_forwards(), 0, "{value}");

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
}
