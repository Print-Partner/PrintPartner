//! Disposable loopback proof for rebuild authenticated read composition (GRE-819).

#[cfg(not(feature = "desktop_rebuild"))]
mod without_feature {
    #[tokio::test]
    async fn rebuild_read_aliases_use_one_owner() {}

    #[tokio::test]
    async fn unknown_read_owner_is_refused_before_execution() {}
}

#[cfg(feature = "desktop_rebuild")]
mod with_feature {
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
        stop: Option<tokio::sync::oneshot::Sender<()>>,
        task: Option<tokio::task::JoinHandle<()>>,
    }

    impl Server {
        async fn spawn(listener: tokio::net::TcpListener, router: axum::Router) -> Self {
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let (stop, stopped) = tokio::sync::oneshot::channel();
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
                origin,
                client: Client::builder().no_proxy().build().unwrap(),
                stop: Some(stop),
                task: Some(task),
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
                response.json().await.unwrap_or(Value::Null),
            )
        }

        async fn stop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(task) = self.task.take() {
                task.await.unwrap();
            }
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            if let Some(task) = self.task.take() {
                task.abort();
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

    #[tokio::test]
    async fn rebuild_read_aliases_use_one_owner() {
        let (owner, directory) = open();
        let bound = bind().await;
        let origin = format!("http://{}", bound.local_addr().unwrap());
        let router = read_router(
            DesktopRebuild::from_value(Some("1")),
            Arc::clone(&owner),
            auth_routes(&origin),
            &origin,
            "default",
            None,
        )
        .unwrap()
        .expect("flag on mounts read routers");
        let mut server = Server::spawn(bound, router).await;
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

        let bound = bind().await;
        let origin = format!("http://{}", bound.local_addr().unwrap());
        let router = read_router(
            DesktopRebuild::from_value(Some("1")),
            Arc::clone(&owner),
            auth_routes(&origin),
            &origin,
            "default",
            None,
        )
        .unwrap()
        .unwrap();
        let mut server = Server::spawn(bound, router).await;
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
} // mod with_feature
