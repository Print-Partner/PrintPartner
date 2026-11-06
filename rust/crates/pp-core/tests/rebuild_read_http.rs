//! Disposable loopback proof for rebuild authenticated read composition (GRE-819).

#[cfg(not(feature = "desktop_rebuild"))]
mod without_feature {
    #[tokio::test]
    async fn rebuild_read_aliases_use_one_owner() {}

    #[tokio::test]
    async fn unknown_read_owner_is_refused_before_execution() {}

    #[tokio::test]
    async fn rebuild_workflow_routes_and_mcp_agree() {}

    #[tokio::test]
    async fn rebuild_read_router_serves_the_domain_event_stream() {}
}

#[cfg(feature = "desktop_rebuild")]
mod with_feature {
    use pp_api::problem::without_envelope;
    use pp_core::DesktopRebuild;
    use pp_core::rebuild_api::{
        AuthHttpConfig, AuthRoutes, CookieTransport, ProviderClient, RebuildReadOwner, ResetMailer,
        read_router,
    };
    use pp_storage::{
        Limits, WriterOwner,
        auth::{
            AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy,
            rebuild::AuthLifetimes,
        },
        build_graph::{BuildCommand, BuildName, BuildOutcome},
        read_model::Credential,
    };
    use reqwest::{Client, Method};
    use serde_json::{Value, json};
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, Mutex, atomic::AtomicBool},
        time::Duration,
    };

    fn open() -> (Arc<Mutex<WriterOwner>>, PathBuf) {
        let directory = tempfile::tempdir().unwrap().keep();
        (
            Arc::new(Mutex::new(
                WriterOwner::open(&directory, Limits::default()).unwrap().0,
            )),
            directory,
        )
    }

    fn files(dir: &Path) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        let mut pending = vec![dir.to_path_buf()];
        while let Some(path) = pending.pop() {
            for entry in std::fs::read_dir(&path).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    pending.push(entry.path());
                } else {
                    out.push((
                        entry
                            .path()
                            .strip_prefix(dir)
                            .unwrap()
                            .to_string_lossy()
                            .into_owned(),
                        entry.metadata().unwrap().len(),
                    ));
                }
            }
        }
        out.sort();
        out
    }

    fn profiles(dir: &Path) -> Vec<(String, i64, String)> {
        let db = rusqlite::Connection::open_with_flags(
            dir.join("print-partner.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        let mut statement = db
            .prepare("SELECT tenant_id, id, name FROM build_profiles ORDER BY tenant_id, id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn shutdown(owner: Arc<Mutex<WriterOwner>>, directory: PathBuf) {
        Arc::try_unwrap(owner)
            .unwrap_or_else(|_| panic!("router still holds storage"))
            .into_inner()
            .unwrap()
            .shutdown()
            .unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn policy() -> AuthPolicy {
        AuthPolicy {
            registration: RegistrationPolicy::FirstAccountOnly,
            first_user: FirstUserTenant::NewUser,
            session_tenant: SessionTenantPolicy::SingleAccountDefault,
        }
    }

    fn auth_routes(origin: &str) -> AuthRoutes {
        AuthRoutes {
            http: AuthHttpConfig::new(origin, CookieTransport::LoopbackHttp, false, None, false)
                .unwrap(),
            policy: policy(),
            lifetimes: AuthLifetimes::shorter(
                Duration::from_secs(3600),
                Some(Duration::from_secs(600)),
            )
            .unwrap(),
            providers: ProviderClient::new(None, None).unwrap(),
            mail: ResetMailer::disabled(),
        }
    }

    struct Server {
        origin: String,
        client: Client,
        serving: Option<pp_api::serving::Serving>,
    }

    impl Server {
        async fn spawn(listener: tokio::net::TcpListener, router: axum::Router) -> Self {
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let serving = pp_api::serving::Serving::start(listener, router);
            Self {
                origin,
                client: Client::builder().no_proxy().build().unwrap(),
                serving: Some(serving),
            }
        }

        async fn json(
            &self,
            method: Method,
            path: &str,
            headers: &[(&str, &str)],
            body: Option<&Value>,
        ) -> (u16, Value) {
            let mut request = self
                .client
                .request(method, format!("{}{path}", self.origin))
                .header("Origin", &self.origin);
            for (name, value) in headers {
                request = request.header(*name, *value);
            }
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request.send().await.unwrap();
            (
                response.status().as_u16(),
                without_envelope(response.json().await.unwrap_or(Value::Null)),
            )
        }

        async fn stop(&mut self) {
            if let Some(mut serving) = self.serving.take() {
                serving.stop().await.unwrap();
            }
        }
    }

    async fn bind() -> tokio::net::TcpListener {
        tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap()
    }

    async fn cookie(server: &Server, email: &str) -> String {
        let response = server
            .client
            .post(format!("{}/auth/register", server.origin))
            .header("Origin", &server.origin)
            .json(&json!({"email": email, "password": "password-1234"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{email}");
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    /// The flag-on read composition from `owner`, served on a fresh port.
    async fn serve_reads(owner: &Arc<Mutex<WriterOwner>>) -> Server {
        let bound = bind().await;
        let origin = format!("http://{}", bound.local_addr().unwrap());
        let router = read_router(
            DesktopRebuild::from_value(Some("1")),
            Arc::clone(owner),
            auth_routes(&origin),
            &origin,
            "default",
            None,
        )
        .unwrap()
        .expect("flag on mounts read routers");
        Server::spawn(bound, router).await
    }

    #[tokio::test]
    async fn rebuild_read_aliases_use_one_owner() {
        let (owner, directory) = open();
        let mut server = serve_reads(&owner).await;
        let cookie = cookie(&server, "reads@example.test").await;
        let token = cookie.strip_prefix("pp_session=").unwrap();
        let headers = [("Cookie", cookie.as_str())];

        let id = {
            let client = owner
                .lock()
                .unwrap()
                .build_graph_with_policy(policy())
                .unwrap();
            match client
                .execute(
                    Credential::Session(Secret::new(token.into())),
                    BuildCommand::Create {
                        name: BuildName::parse("Alias Plan".into()).unwrap(),
                        base_source: None,
                    },
                    Arc::new(AtomicBool::new(false)),
                    Duration::from_secs(2),
                )
                .unwrap()
            {
                BuildOutcome::Created { profile, .. } => profile.id,
                other => panic!("{other:?}"),
            }
        };

        let mut plans = Vec::new();
        for path in ["/plans", "/api/v1/plans", "/api/v2/plans"] {
            let (status, body) = server.json(Method::GET, path, &headers, None).await;
            assert_eq!(status, 200, "{path}");
            assert_eq!(body["profiles"][0]["id"], id, "{path}");
            assert_eq!(body["profiles"][0]["name"], "Alias Plan", "{path}");
            plans.push(body);
        }
        assert_eq!(plans[0], plans[2]);
        // `/api/v1` keeps Node's legacy summary: unit counts instead of
        // `accepted_progress`, otherwise the same Plan from the same owner.
        let mut legacy = plans[0].clone();
        let summary = legacy["profiles"][0].as_object_mut().unwrap();
        assert_eq!(
            summary.remove("accepted_progress"),
            Some(json!({"kind": "empty"}))
        );
        summary.insert("remaining_units".into(), json!(0));
        summary.insert("total_units".into(), json!(0));
        assert_eq!(plans[1], legacy);

        // Accepted Plan Checkoff reads the same owner on every alias, with
        // Node's body for a Plan that has no accepted revision yet.
        for prefix in ["", "/api/v1", "/api/v2"] {
            let path = format!("{prefix}/plans/{id}/checkoff");
            assert_eq!(
                server.json(Method::GET, &path, &headers, None).await,
                (
                    200,
                    json!({
                        "profile_id": id,
                        "summary": "0/0 parts fully printed · 0/0 units",
                        "parts": [],
                    })
                ),
                "{path}"
            );
        }
        // Build history reads the same owner on `/` and `/api/v2`, one page at
        // a time, newest first.
        rusqlite::Connection::open(directory.join("print-partner.db"))
            .unwrap()
            .execute_batch(&format!(
                "INSERT INTO plan_decisions (tenant_id,profile_id,created_at,actor,kind,summary) VALUES
                 ('default',{id},'2026-10-01T09:00:00.000Z','assistant','choice','Older'),
                 ('default',{id},'2026-10-02T09:00:00.000Z','user','user_note','Newer');"
            ))
            .unwrap();
        let (status, first) = server
            .json(
                Method::GET,
                &format!("/plans/{id}/history?limit=1"),
                &headers,
                None,
            )
            .await;
        assert_eq!(status, 200, "{first}");
        assert_eq!(first["plan_id"], id);
        assert_eq!(first["entries"][0]["summary"], "Newer");
        assert_eq!(first["entries"][0]["actor"], "user");
        let cursor = first["next_cursor"].as_str().unwrap();
        let (status, second) = server
            .json(
                Method::GET,
                &format!("/api/v2/plans/{id}/history?limit=1&cursor={cursor}"),
                &headers,
                None,
            )
            .await;
        assert_eq!(status, 200, "{second}");
        assert_eq!(second["entries"][0]["summary"], "Older");
        assert_eq!(second["entries"][0]["type"], "decision");
        assert_eq!(second["next_cursor"], Value::Null);
        for (path, credential, status, detail) in [
            (
                format!("/plans/{id}/history"),
                false,
                401,
                "Authentication required",
            ),
            (
                "/plans/999999/history".into(),
                true,
                404,
                "Profile not found",
            ),
            ("/plans/0x1/history".into(), true, 404, "Profile not found"),
            (
                format!("/plans/{id}/history?limit=0"),
                true,
                400,
                "Invalid history limit",
            ),
            (
                format!("/plans/{id}/history?limit=201"),
                true,
                400,
                "Invalid history limit",
            ),
            (
                format!("/plans/{id}/history?limit=1&limit=2"),
                true,
                400,
                "Invalid history limit",
            ),
            (
                format!("/plans/{id}/history?cursor=bogus"),
                true,
                400,
                "Invalid history cursor",
            ),
        ] {
            let sent: &[(&str, &str)] = if credential { &headers } else { &[] };
            assert_eq!(
                server.json(Method::GET, &path, sent, None).await,
                (status, json!({"detail": detail})),
                "{path}"
            );
        }
        // An id that cannot name a Build still checks the credential first.
        for path in ["/plans/0/history", "/plans/0x1/history"] {
            assert_eq!(
                server
                    .json(
                        Method::GET,
                        path,
                        &[("Cookie", "pp_session=not-a-session")],
                        None
                    )
                    .await,
                (401, json!({"detail": "Authentication required"})),
                "{path}"
            );
        }
        // The same router answers Part assembled reads, which an empty Plan
        // has none of.
        for path in ["/parts/1/assembled", "/api/v1/parts/1/assembled"] {
            assert_eq!(
                server.json(Method::GET, path, &headers, None).await,
                (404, json!({"detail": "Part not found"})),
                "{path}"
            );
        }
        // Custom filaments read the owner's data dir, which has no store yet.
        for path in ["/filaments/custom", "/api/v1/filaments/custom"] {
            assert_eq!(
                server.json(Method::GET, path, &headers, None).await,
                (200, json!({"filaments": []})),
                "{path}"
            );
        }

        // MCP is mounted on both aliases and takes API keys only, so a
        // signed-in same-origin session is refused (GRE-348).
        for path in ["/mcp", "/api/v1/mcp"] {
            assert_eq!(
                server
                    .json(Method::POST, path, &headers, Some(&json!({})))
                    .await,
                (401, json!({"detail": "Valid API key required"})),
                "{path}"
            );
        }

        let (status, created) = server
            .json(
                Method::POST,
                "/sources",
                &headers,
                Some(&json!({"name": "Alias Source", "source_kind": "local"})),
            )
            .await;
        assert!(matches!(status, 200 | 201), "{status} {created}");
        let source_id = &created["id"];

        let mut sources = Vec::new();
        for path in ["/sources", "/api/v1/sources"] {
            let (status, body) = server.json(Method::GET, path, &headers, None).await;
            assert_eq!(status, 200, "{path}");
            assert_eq!(&body["sources"][0]["id"], source_id, "{path}");
            sources.push(body);
        }
        assert_eq!(sources[0], sources[1]);

        server.stop().await;
        shutdown(owner, directory);
    }

    /// GRE-1123: the mounted workflow route, the task feed and MCP
    /// `get_build_workflow` answer from one storage projection, so they agree
    /// on the next action, its reason and its target.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rebuild_workflow_routes_and_mcp_agree() {
        const SESSION: &str = "read-fixture-secret";
        let directory = tempfile::tempdir().unwrap().keep();
        std::fs::create_dir_all(directory.join("repos/1/revisions/accepted")).unwrap();
        std::fs::write(
            directory.join("print-partner.db"),
            include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        let owner = Arc::new(Mutex::new(
            WriterOwner::open(&directory, Limits::default()).unwrap().0,
        ));
        let bound = bind().await;
        let origin = format!("http://{}", bound.local_addr().unwrap());
        // The fixture's session belongs to an account tenant.
        let policy = AuthPolicy {
            registration: RegistrationPolicy::Open,
            first_user: FirstUserTenant::NewUser,
            session_tenant: SessionTenantPolicy::AccountTenant,
        };
        let routes = AuthRoutes {
            policy,
            ..auth_routes(&origin)
        };
        let lifetimes = routes.lifetimes;
        let router = read_router(
            DesktopRebuild::from_value(Some("1")),
            Arc::clone(&owner),
            routes,
            &origin,
            "default",
            None,
        )
        .unwrap()
        .expect("flag on mounts read routers");
        let mut server = Server::spawn(bound, router).await;
        let cookie = format!("pp_session={SESSION}");
        let session = [("Cookie", cookie.as_str())];

        let mut workflows = Vec::new();
        for path in [
            "/plans/1/workflow",
            "/api/v1/plans/1/workflow",
            "/api/v2/plans/1/workflow",
        ] {
            let (status, body) = server.json(Method::GET, path, &session, None).await;
            assert_eq!(status, 200, "{path} {body}");
            workflows.push(body);
        }
        assert!(workflows.iter().all(|body| *body == workflows[0]));
        let workflow = workflows.remove(0);
        assert_eq!(
            workflow["next_action"]["kind"], "check_off_manually",
            "{workflow}"
        );

        let (status, feed) = server
            .json(Method::GET, "/api/v1/tasks", &session, None)
            .await;
        assert_eq!(status, 200, "{feed}");
        assert_eq!(
            server
                .json(Method::GET, "/api/v2/tasks", &session, None)
                .await,
            (200, feed.clone())
        );
        let task = feed["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|task| task["plan_id"] == 1)
            .unwrap();

        let key = {
            let auth = owner
                .lock()
                .unwrap()
                .rebuild_auth_with_policy(policy, lifetimes)
                .unwrap();
            let session = Secret::new(SESSION.into());
            let create = pp_storage::auth::Request::CreateKey { session };
            let outcome = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
                let stop = Arc::new(AtomicBool::new(false));
                auth.submit(create, stop, Duration::from_secs(2))?.recv()?
            });
            let outcome = outcome.await.unwrap().unwrap();
            let pp_storage::auth::Outcome::KeyCreated { key, .. } = outcome else {
                panic!("API key creation");
            };
            key.expose().to_owned()
        };
        let bearer = format!("Bearer {key}");
        let (status, by_key) = server
            .json(
                Method::GET,
                "/plans/1/workflow",
                &[("Authorization", bearer.as_str())],
                None,
            )
            .await;
        assert_eq!((status, &by_key), (200, &workflow));
        let mcp = mcp_workflow(&server, &bearer, json!(1)).await;
        assert_eq!(mcp_workflow(&server, &bearer, json!(1.0)).await, mcp);

        // Compare the three answers to each other, not to written copies.
        for field in ["kind", "reason", "target"] {
            assert_eq!(
                task["next_action"][field], workflow["next_action"][field],
                "{field}"
            );
            assert_eq!(
                mcp["next_action"][field], workflow["next_action"][field],
                "{field}"
            );
        }
        assert_eq!(task["next_action"], workflow["next_action"]);
        assert_eq!(mcp, workflow);

        for (path, headers) in [
            ("/plans/1/workflow", &[] as &[(&str, &str)]),
            ("/api/v1/tasks", &[]),
            (
                "/api/v2/plans/1/workflow",
                &[("Cookie", "pp_session=not-a-session")],
            ),
            ("/api/v1/tasks", &[("Cookie", "pp_session=not-a-session")]),
            (
                "/plans/0x1/workflow",
                &[("Cookie", "pp_session=not-a-session")],
            ),
        ] {
            assert_eq!(
                server.json(Method::GET, path, headers, None).await,
                (401, json!({"detail": "Authentication required"})),
                "{path}"
            );
        }
        for path in ["/plans/999999/workflow", "/plans/0x1/workflow"] {
            assert_eq!(
                server.json(Method::GET, path, &session, None).await,
                (404, json!({"detail": "Profile not found"})),
                "{path}"
            );
        }

        server.stop().await;
        shutdown(owner, directory);
    }

    /// Calls MCP `get_build_workflow` for Build `plan_id` with an API key and
    /// returns the workflow it answered.
    async fn mcp_workflow(server: &Server, bearer: &str, plan_id: Value) -> Value {
        let post = |body: Value, session: Option<String>| {
            let mut request = server
                .client
                .post(format!("{}/mcp", server.origin))
                .header("Origin", &server.origin)
                .header("Authorization", bearer)
                .header("content-type", "application/json")
                .header("accept", "application/json, text/event-stream")
                .body(body.to_string());
            if let Some(session) = session {
                request = request.header("mcp-session-id", session);
            }
            request.send()
        };
        let response = post(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "gre-1123", "version": "0.0.1"}}}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), 200);
        let session = response.headers()["mcp-session-id"]
            .to_str()
            .unwrap()
            .to_owned();
        response.text().await.unwrap();
        let initialized = post(
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            Some(session.clone()),
        )
        .await
        .unwrap();
        assert!(initialized.status().is_success());
        initialized.text().await.unwrap();
        let body = post(
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "get_build_workflow", "arguments": {"plan_id": plan_id}}}),
            Some(session.clone()),
        )
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
        let data = body
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .find(|data| !data.is_empty())
            .unwrap_or(&body);
        let rpc: Value = serde_json::from_str(data).unwrap();
        // Close the session so no session task outlives the server.
        server
            .client
            .delete(format!("{}/mcp", server.origin))
            .header("Origin", &server.origin)
            .header("Authorization", bearer)
            .header("mcp-session-id", session)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let result = &rpc["result"];
        assert_ne!(result["isError"], true, "{rpc}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn unknown_read_owner_is_refused_before_execution() {
        let (owner, directory) = open();
        let snapshot = files(&directory);
        let before = profiles(&directory);

        for name in ["", "local-owner", "node", "drafts", "jobs", "Auth"] {
            assert!(RebuildReadOwner::admit(name).is_none(), "{name:?}");
        }
        assert_eq!(
            RebuildReadOwner::admit("auth"),
            Some(RebuildReadOwner::Auth)
        );
        assert_eq!(files(&directory), snapshot);
        assert_eq!(profiles(&directory), before);

        for value in [None, Some("0"), Some("false"), Some("TRUE")] {
            assert!(
                read_router(
                    DesktopRebuild::from_value(value),
                    Arc::clone(&owner),
                    auth_routes("http://127.0.0.1:9"),
                    "http://127.0.0.1:9",
                    "default",
                    None,
                )
                .unwrap()
                .is_none(),
                "{value:?}"
            );
        }
        assert_eq!(files(&directory), snapshot);
        assert_eq!(profiles(&directory), before);

        let mut server = Server::spawn(bind().await, axum::Router::new()).await;
        for path in ["/plans", "/sources"] {
            let (status, body) = server.json(Method::GET, path, &[], None).await;
            assert_eq!(status, 404, "{path}");
            assert!(body.get("profiles").is_none() && body.get("sources").is_none());
        }
        server.stop().await;

        let mut server = serve_reads(&owner).await;
        let before = profiles(&directory);
        let snapshot = files(&directory);
        for (path, headers) in [
            ("/plans", &[] as &[(&str, &str)]),
            ("/sources", &[]),
            ("/plans", &[("Cookie", "pp_session=not-a-session")]),
        ] {
            let (status, body) = server.json(Method::GET, path, headers, None).await;
            assert_eq!(status, 401, "{path} {body}");
            let detail = body["detail"].as_str().unwrap_or("");
            assert!(
                detail.contains("Authentication required") || detail.contains("Invalid session"),
                "{path} {body}"
            );
            assert_eq!(files(&directory), snapshot);
            assert_eq!(profiles(&directory), before);
        }
        server.stop().await;
        shutdown(owner, directory);
    }
    /// GRE-1162: the Rust-only read composition mounts `GET /api/v1/events`.
    #[tokio::test]
    async fn rebuild_read_router_serves_the_domain_event_stream() {
        let (owner, directory) = open();
        let mut server = serve_reads(&owner).await;
        let url = format!("{}/api/v1/events", server.origin);
        let refused = server
            .client
            .get(&url)
            .header("Origin", &server.origin)
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), 401);
        let cookie = cookie(&server, "events@example.test").await;
        let mut stream = server
            .client
            .get(&url)
            .header("Origin", &server.origin)
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(stream.status(), 200);
        assert_eq!(stream.headers()["content-type"], "text/event-stream");
        let first = tokio::time::timeout(Duration::from_secs(5), stream.chunk())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            std::str::from_utf8(&first)
                .unwrap()
                .starts_with("event: ready\nid: "),
            "{first:?}"
        );
        drop(stream);
        server.stop().await;
        shutdown(owner, directory);
    }
} // mod with_feature
