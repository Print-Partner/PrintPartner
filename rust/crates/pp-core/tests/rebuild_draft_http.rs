//! Disposable loopback proof for rebuild draft mutation composition (GRE-820).

#[cfg(not(feature = "desktop_rebuild"))]
mod without_feature {
    #[tokio::test]
    async fn draft_aliases_and_workers_share_executor() {}

    #[tokio::test]
    async fn rust_draft_failure_never_falls_back_to_node() {}
}

#[cfg(feature = "desktop_rebuild")]
mod with_feature {
    use pp_api::problem::without_envelope;
    use pp_core::DesktopRebuild;
    use pp_core::rebuild_api::{RebuildDraftOwner, draft_router};
    use pp_storage::{
        Limits, WriterOwner,
        auth::{
            AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy,
            rebuild::AuthLifetimes,
        },
        read_model::Credential,
        working_drafts::{EditRequest, Failure, PositiveId},
    };
    use reqwest::{Client, Method};
    use serde_json::{Value, json};
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, atomic::AtomicBool},
        time::Duration,
    };

    const DIGEST: &str = "ae11e670337e3e5eab736b29b4e294edbea8b564661cf15972625783f8930ee3";
    const FIXTURE_COOKIE: &str = "pp_session=required-unit-fixture-secret";
    const FIXTURE_DB: &[u8] =
        include_bytes!("../../pp-storage/tests/fixtures/required-units/unchanged.db");

    fn policy() -> AuthPolicy {
        AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        }
    }

    fn open() -> (WriterOwner, PathBuf) {
        let directory = tempfile::tempdir().unwrap().keep();
        std::fs::write(directory.join("print-partner.db"), FIXTURE_DB).unwrap();
        (
            WriterOwner::open(&directory, Limits::default()).unwrap().0,
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

    fn draft_row(dir: &Path) -> (String, i64, String) {
        let db = rusqlite::Connection::open_with_flags(
            dir.join("print-partner.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db.query_row(
            "SELECT state, lifecycle_version, snapshot_digest FROM plan_drafts WHERE id=2",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    }

    fn included(dir: &Path, part_id: i64) -> i64 {
        let db = rusqlite::Connection::open_with_flags(
            dir.join("print-partner.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db.query_row(
            "SELECT included FROM plan_draft_parts WHERE id=?1",
            [part_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn shutdown(owner: WriterOwner, directory: PathBuf) {
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn edit(digest: &str) -> Value {
        json!({"expected_snapshot_digest":digest,"decisions":[
            {"kind":"set_included","draft_part_ids":[5],"value":true},
            {"kind":"set_quantity_override","draft_part_ids":[4],"value":null}
        ]})
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

        fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
            self.client
                .request(method, format!("{}{path}", self.origin))
                .header("Origin", &self.origin)
                .header("Cookie", FIXTURE_COOKIE)
        }

        async fn json(&self, method: Method, path: &str, body: Option<&Value>) -> (u16, Value) {
            let mut request = self.request(method, path);
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

    #[tokio::test]
    async fn draft_aliases_and_workers_share_executor() {
        let (owner, directory) = open();
        let bound = bind().await;
        let origin = format!("http://{}", bound.local_addr().unwrap());
        let router = draft_router(
            DesktopRebuild::from_value(Some("1")),
            &owner,
            policy(),
            AuthLifetimes::default(),
            &origin,
            "default",
            None,
        )
        .unwrap()
        .expect("flag on mounts draft routers");
        let mut server = Server::spawn(bound, router).await;

        let mut listed = Vec::new();
        for prefix in ["", "/api/v2"] {
            let (status, body) = server
                .json(Method::GET, &format!("{prefix}/plans/1/drafts"), None)
                .await;
            assert_eq!(status, 200, "{prefix}");
            assert_eq!(body["drafts"][1]["draft_id"], 2, "{prefix}");
            assert_eq!(body["drafts"][1]["snapshot_digest"], DIGEST, "{prefix}");
            listed.push(body);
        }
        assert_eq!(listed[0], listed[1]);

        let (status, changed) = server
            .json(
                Method::PATCH,
                "/plans/1/drafts/2/parts",
                Some(&edit(DIGEST)),
            )
            .await;
        assert_eq!(status, 200, "{changed}");
        let receipt = changed["draft"].clone();
        assert_eq!(receipt["draft_id"], 2);
        assert_ne!(receipt["snapshot_digest"], DIGEST);
        assert_eq!(included(&directory, 5), 1);

        let (a, b) = tokio::join!(
            server.json(Method::GET, "/plans/1/drafts/2", None),
            server.json(Method::GET, "/api/v2/plans/1/drafts/2", None),
        );
        let mut workspaces = Vec::new();
        for (status, body) in [a, b] {
            assert_eq!(status, 200, "{body}");
            assert_eq!(body["draft"], receipt, "{body}");
            workspaces.push(body);
        }
        assert_eq!(workspaces[0], workspaces[1]);

        // Cancellation on a sibling client sharing the same owner must not
        // erase the recorded edit receipt aliases already observed.
        let sibling = owner.working_drafts_with_policy(policy()).unwrap();
        let request: EditRequest =
            serde_json::from_value(edit(receipt["snapshot_digest"].as_str().unwrap())).unwrap();
        let cancelled = sibling
            .edit(
                Credential::Session(Secret::new("required-unit-fixture-secret".into())),
                PositiveId::new(1).unwrap(),
                PositiveId::new(2).unwrap(),
                request,
                Arc::new(AtomicBool::new(true)),
                Duration::from_millis(100),
            )
            .unwrap_err();
        assert!(
            matches!(cancelled.downcast_ref(), Some(Failure::Cancelled)),
            "{cancelled}"
        );
        assert_eq!(draft_row(&directory).2, receipt["snapshot_digest"]);
        let (status, after) = server
            .json(Method::GET, "/api/v2/plans/1/drafts/2", None)
            .await;
        assert_eq!(status, 200);
        assert_eq!(after["draft"], receipt);

        server.stop().await;
        shutdown(owner, directory);
    }

    #[tokio::test]
    async fn rust_draft_failure_never_falls_back_to_node() {
        for name in ["", "node", "jobs", "auth", "Drafts", "local-owner"] {
            assert!(RebuildDraftOwner::admit(name).is_none(), "{name:?}");
        }
        assert_eq!(
            RebuildDraftOwner::admit("drafts"),
            Some(RebuildDraftOwner::Drafts)
        );

        let (owner, directory) = open();
        let snapshot = files(&directory);
        let before = draft_row(&directory);
        let before_included = included(&directory, 5);

        for value in [None, Some("0"), Some("false"), Some("TRUE")] {
            assert!(
                draft_router(
                    DesktopRebuild::from_value(value),
                    &owner,
                    policy(),
                    AuthLifetimes::default(),
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
        assert_eq!(draft_row(&directory), before);

        // Empty router: no Node process and no draft mounts.
        let mut empty = Server::spawn(bind().await, axum::Router::new()).await;
        for path in ["/plans/1/drafts", "/api/v2/plans/1/drafts/2/parts"] {
            let (status, body) = empty.json(Method::GET, path, None).await;
            assert_eq!(status, 404, "{path}");
            assert!(body.get("drafts").is_none() && body.get("code").is_none());
        }
        empty.stop().await;

        let bound = bind().await;
        let origin = format!("http://{}", bound.local_addr().unwrap());
        let router = draft_router(
            DesktopRebuild::from_value(Some("1")),
            &owner,
            policy(),
            AuthLifetimes::default(),
            &origin,
            "default",
            None,
        )
        .unwrap()
        .unwrap();
        let mut server = Server::spawn(bound, router).await;

        let workspace = server.json(Method::GET, "/plans/1/drafts/2", None).await.1;
        let mut expected = json!({"code":"draft_changed","workspace":workspace});
        let (status, conflict) = server
            .json(
                Method::PATCH,
                "/api/v2/plans/1/drafts/2/parts",
                Some(&edit(&"0".repeat(64))),
            )
            .await;
        assert_eq!(status, 409, "{conflict}");
        assert_eq!(conflict, expected);
        assert!(conflict.get("detail").is_none(), "{conflict}");
        assert_eq!(draft_row(&directory), before);
        assert_eq!(included(&directory, 5), before_included);
        assert_eq!(files(&directory), snapshot);

        // Rust-only disposable fixture: no Node gateway env and the conflict
        // shape is the pp-api draft router receipt, not a Node fallback body.
        assert!(
            std::env::var_os("PP_NODE_GATEWAY").is_none(),
            "fixture must not wire a Node gateway"
        );
        assert!(
            conflict.get("message").is_none() && conflict.get("error").is_none(),
            "{conflict}"
        );

        expected["workspace"] = server.json(Method::GET, "/plans/1/drafts/2", None).await.1;
        let (status, again) = server
            .json(
                Method::PATCH,
                "/plans/1/drafts/2/parts",
                Some(&edit(&"0".repeat(64))),
            )
            .await;
        assert_eq!(status, 409, "{again}");
        assert_eq!(again, expected);
        assert_eq!(draft_row(&directory), before);
        assert_eq!(included(&directory, 5), before_included);

        server.stop().await;
        shutdown(owner, directory);
    }
}
