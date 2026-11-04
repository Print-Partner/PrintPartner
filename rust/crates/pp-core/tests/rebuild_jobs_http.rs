//! Disposable loopback proof for rebuild job route composition (GRE-821).

#[cfg(not(feature = "desktop_rebuild"))]
mod without_feature {
    #[tokio::test]
    async fn runtime_jobs_ws_requires_current_listener() {}

    #[tokio::test]
    async fn runtime_shutdown_closes_job_subscriptions() {}
}

#[cfg(feature = "desktop_rebuild")]
#[path = "support/jobs.rs"]
mod support_jobs;

#[cfg(feature = "desktop_rebuild")]
mod with_feature {
    use pp_core::DesktopRebuild;
    use pp_core::rebuild_api::{job_router, router};
    use pp_storage::{
        Limits, WriterOwner,
        auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    };
    use std::{path::Path, time::Duration};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn policy() -> AuthPolicy {
        AuthPolicy {
            registration: RegistrationPolicy::Open,
            first_user: FirstUserTenant::NewUser,
            session_tenant: SessionTenantPolicy::AccountTenant,
        }
    }

    fn open() -> (WriterOwner, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap().keep();
        (
            WriterOwner::open(&directory, Limits::default()).unwrap().0,
            directory,
        )
    }

    fn job_rows(directory: &Path) -> Vec<(String, String, String)> {
        let db = rusqlite::Connection::open_with_flags(
            directory.join("print-partner.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        let mut statement = db
            .prepare("SELECT tenant, id, kind FROM durable_jobs ORDER BY tenant, id")
            .unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    fn enqueue(owner: &WriterOwner, token: &str) -> String {
        super::support_jobs::enqueue_check(&owner.jobs(policy()).unwrap(), token, "stream")
    }

    struct Server {
        origin: String,
        client: reqwest::Client,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
        task: Option<tokio::task::JoinHandle<()>>,
    }

    impl Server {
        async fn spawn(listener: tokio::net::TcpListener, app: axum::Router) -> Self {
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let (shutdown, stopped) = tokio::sync::oneshot::channel();
            let task = tokio::spawn(async move {
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await
                .unwrap();
            });
            Self {
                origin,
                client: reqwest::Client::builder().no_proxy().build().unwrap(),
                shutdown: Some(shutdown),
                task: Some(task),
            }
        }

        async fn stop(mut self) {
            let _ = self.shutdown.take().unwrap().send(());
            self.task.take().unwrap().await.unwrap();
        }
    }

    fn routes(origin: &str) -> pp_core::rebuild_api::AuthRoutes {
        use pp_core::rebuild_api::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer};
        use pp_storage::auth::rebuild::AuthLifetimes;

        pp_core::rebuild_api::AuthRoutes {
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

    async fn serve(owner: &WriterOwner) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let jobs = job_router(
            DesktopRebuild::from_value(Some("1")),
            owner,
            policy(),
            &origin,
            "default",
        )
        .unwrap()
        .expect("flag on mounts jobs");
        let auth = router(
            DesktopRebuild::from_value(Some("1")),
            owner,
            routes(&origin),
        )
        .unwrap()
        .expect("flag on mounts auth");
        Server::spawn(listener, auth.merge(jobs)).await
    }

    async fn register(server: &Server, email: &str) -> (String, String) {
        let registered = server
            .client
            .post(format!("{}/auth/register", server.origin))
            .header("Origin", &server.origin)
            .json(&serde_json::json!({"email": email, "password": "password-1234"}))
            .send()
            .await
            .unwrap();
        assert_eq!(registered.status(), 200, "{email}");
        let cookie = registered.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
        (cookie, token)
    }

    async fn exchange(origin: &str, host: &str, path: &str, cookie: &str) -> (u16, String) {
        let mut stream = tokio::net::TcpStream::connect(origin.trim_start_matches("http://"))
            .await
            .unwrap();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nCookie: {cookie}\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut seen = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).await.unwrap();
            seen.push(byte[0]);
            if seen.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let header = String::from_utf8(seen).unwrap();
        let status: u16 = header.split_whitespace().nth(1).unwrap().parse().unwrap();
        let length = header
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0u8; length];
        if length > 0 {
            stream.read_exact(&mut body).await.unwrap();
        }
        (status, String::from_utf8(body).unwrap())
    }

    async fn open_socket(origin: &str, path: &str, cookie: &str) -> tokio::net::TcpStream {
        let host = origin.trim_start_matches("http://");
        let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nCookie: {cookie}\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut seen = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).await.unwrap();
            seen.push(byte[0]);
            if seen.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8(seen).unwrap();
        assert!(text.starts_with("HTTP/1.1 101"), "{text}");
        stream
    }

    async fn read_frame(stream: &mut tokio::net::TcpStream) -> (u8, Vec<u8>) {
        let mut head = [0u8; 2];
        stream.read_exact(&mut head).await.unwrap();
        let opcode = head[0] & 0x0f;
        let len = match head[1] & 0x7f {
            126 => {
                let mut ext = [0u8; 2];
                stream.read_exact(&mut ext).await.unwrap();
                u16::from_be_bytes(ext) as usize
            }
            len => len as usize,
        };
        let mut payload = vec![0u8; len];
        if len > 0 {
            stream.read_exact(&mut payload).await.unwrap();
        }
        (opcode, payload)
    }

    async fn expect_text(stream: &mut tokio::net::TcpStream) -> serde_json::Value {
        let (opcode, payload) = read_frame(stream).await;
        assert_eq!(opcode, 1, "text frame");
        serde_json::from_slice(&payload).unwrap()
    }

    async fn expect_close(stream: &mut tokio::net::TcpStream) -> (u16, String) {
        let (opcode, payload) = read_frame(stream).await;
        assert_eq!(opcode, 8, "close frame");
        let code = u16::from_be_bytes([payload[0], payload[1]]);
        (code, String::from_utf8(payload[2..].to_vec()).unwrap())
    }

    #[tokio::test]
    async fn runtime_jobs_ws_requires_current_listener() {
        let (owner, directory) = open();
        assert!(
            job_router(
                DesktopRebuild::from_value(None),
                &owner,
                policy(),
                "http://127.0.0.1:9",
                "default",
            )
            .unwrap()
            .is_none()
        );
        let server = serve(&owner).await;
        let (cookie, token) = register(&server, "job-listener@example.test").await;
        let job_id = enqueue(&owner, &token);
        let rows = job_rows(&directory);
        let (status, body) = exchange(
            &server.origin,
            "127.0.0.1:9",
            &format!("/ws/jobs/{job_id}"),
            &cookie,
        )
        .await;
        assert_eq!(status, 403);
        assert!(body.contains("Invalid request host"), "{body}");
        assert_eq!(job_rows(&directory), rows);
        let mut socket = open_socket(&server.origin, &format!("/ws/jobs/{job_id}"), &cookie).await;
        let snapshot = expect_text(&mut socket).await;
        assert_eq!(snapshot["job_id"], job_id);
        drop(socket);
        server.stop().await;
        owner.shutdown().unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn runtime_shutdown_closes_job_subscriptions() {
        let (owner, directory) = open();
        let server = serve(&owner).await;
        let (cookie, token) = register(&server, "job-shutdown@example.test").await;
        let job_id = enqueue(&owner, &token);
        let rows = job_rows(&directory);
        let mut socket = open_socket(&server.origin, &format!("/ws/jobs/{job_id}"), &cookie).await;
        assert_eq!(expect_text(&mut socket).await["job_id"], job_id);
        owner.shutdown().unwrap();
        assert_eq!(
            expect_close(&mut socket).await,
            (1011, "Job stream unavailable".to_owned())
        );
        assert_eq!(job_rows(&directory), rows);
        server.stop().await;
        std::fs::remove_dir_all(directory).unwrap();
    }
}
