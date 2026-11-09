#![cfg(test)]

use crate::scratch::Scratch;
use std::{
    net::SocketAddr,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) static REFRESH_AFTER_PUBLISHED: AtomicBool = AtomicBool::new(false);

pub(super) fn note_published_refresh() {
    REFRESH_AFTER_PUBLISHED.store(true, Ordering::SeqCst);
}

use axum::{
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use pp_compat::CompatHandle;

async fn exchange(
    origin: &str,
    path: &str,
    method: &str,
    cookie: Option<&str>,
    accept: Option<&str>,
    body: Option<&str>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let authority = origin.strip_prefix("http://").unwrap();
    let socket = tokio::net::TcpStream::connect(authority).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut builder = Request::builder()
        .uri(path)
        .method(method)
        .header("host", authority)
        .header("origin", origin);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    if let Some(accept) = accept {
        builder = builder.header("accept", accept);
    }
    let body = if let Some(body) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(body.to_owned())
    } else {
        Body::empty()
    };
    let response = sender
        .send_request(builder.body(body).unwrap())
        .await
        .unwrap();
    let (head, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes().to_vec();
    (head.status, head.headers, bytes)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers.get(name).unwrap().to_str().unwrap()
}

fn session_cookie(headers: &HeaderMap) -> String {
    header(headers, "set-cookie")
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

fn assert_unready_problem(status: StatusCode, headers: &HeaderMap, body: &[u8]) {
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(header(headers, "content-type"), "application/problem+json");
    assert_eq!(header(headers, "retry-after"), "1");
    assert_eq!(header(headers, "cache-control"), "no-store");
    let problem: serde_json::Value = serde_json::from_slice(body).unwrap();
    assert_eq!(problem["type"], "about:blank");
    assert_eq!(problem["title"], "Service Unavailable");
    assert_eq!(problem["status"], 503);
    assert_eq!(problem["detail"], "Compatibility backend unavailable");
    assert!(problem.get("generation").is_none());
}

struct Assets(Scratch);

impl Assets {
    fn create() -> Self {
        let path = Scratch::new("pp-t2-");
        std::fs::create_dir(path.join("assets")).unwrap();
        std::fs::write(path.join("index.html"), b"spa-ready").unwrap();
        std::fs::write(path.join("assets/app.js"), b"asset-ready").unwrap();
        Self(path)
    }
}

#[tokio::test]
async fn ticket_t_2_unready_node_routes_return_problem_json_and_keep_rust_surfaces() {
    let assets = Assets::create();
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let mut gateway = super::Gateway::start(
        listener,
        assets.0.to_path_buf(),
        CompatHandle::unpublished(),
        &crate::ownership::cutover::OperationOwners::node_only(),
    )
    .unwrap();
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let bootstrap = launch.strip_prefix(&origin).unwrap();

    let (status, headers, _) = exchange(&origin, bootstrap, "GET", None, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = session_cookie(&headers);

    let (status, headers, body) =
        exchange(&origin, "/", "GET", Some(&cookie), Some("text/html"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"spa-ready");
    assert_eq!(header(&headers, "content-type"), "text/html; charset=utf-8");

    let (status, _, body) =
        exchange(&origin, "/assets/app.js", "GET", Some(&cookie), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"asset-ready");

    let (status, _, body) = exchange(&origin, "/__runtime", "GET", Some(&cookie), None, None).await;
    assert_eq!(status, StatusCode::OK);
    let runtime: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(runtime["compat"]["state"], "starting");
    assert!(runtime["compat"].get("pid").is_none());

    let observation = exchange(
        &origin,
        "/api/v1/filaments/custom",
        "GET",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_unready_problem(observation.0, &observation.1, &observation.2);

    let status_route = exchange(
        &origin,
        "/api/v1/integrations/printer-1/status",
        "GET",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_unready_problem(status_route.0, &status_route.1, &status_route.2);

    let mutation = exchange(
        &origin,
        "/api/v1/filaments/custom",
        "POST",
        Some(&cookie),
        None,
        Some(r#"{"name":"pla"}"#),
    )
    .await;
    assert_unready_problem(mutation.0, &mutation.1, &mutation.2);

    let replay = exchange(
        &origin,
        "/api/v1/filaments/custom",
        "POST",
        Some(&cookie),
        None,
        Some(r#"{"name":"pla"}"#),
    )
    .await;
    assert_unready_problem(replay.0, &replay.1, &replay.2);

    let (status, _, body) = exchange(&origin, "/__runtime", "GET", Some(&cookie), None, None).await;
    assert_eq!(status, StatusCode::OK);
    let runtime: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        runtime["compat"]["state"], "starting",
        "a rejected mutation must not publish another backend"
    );

    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_2_status_refresh_unpublish_returns_problem_json() {
    REFRESH_AFTER_PUBLISHED.store(false, Ordering::SeqCst);
    let sock_dir = Scratch::new("pp-t2-refresh-");
    let (endpoint, mut child) = pp_compat::Endpoint::fake(&sock_dir).unwrap();
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let stream = child.accept().await.unwrap();
        let _ = held_tx.send(stream);
    });
    let (compat, publication) = CompatHandle::replaceable(Some(endpoint));
    let assets = Assets::create();
    let tcp = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();
    let mut gateway = super::Gateway::start(
        tcp,
        assets.0.to_path_buf(),
        compat,
        &crate::ownership::cutover::OperationOwners::node_only(),
    )
    .unwrap();
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let bootstrap = launch.strip_prefix(&origin).unwrap().to_owned();
    let (status, headers, _) = exchange(&origin, &bootstrap, "GET", None, None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = session_cookie(&headers);
    let held = tokio::time::timeout(std::time::Duration::from_secs(2), held_rx)
        .await
        .unwrap()
        .unwrap();
    let origin_for_status = origin.clone();
    let status_task = tokio::spawn(async move {
        exchange(
            &origin_for_status,
            "/api/v1/integrations/printer-1/status",
            "GET",
            Some(&cookie),
            None,
            None,
        )
        .await
    });
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !REFRESH_AFTER_PUBLISHED.load(Ordering::SeqCst) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "status read never passed the publication check"
        );
        tokio::task::yield_now().await;
    }
    publication.send(None).unwrap();
    drop(held);
    let status_route = status_task.await.unwrap();
    assert_unready_problem(status_route.0, &status_route.1, &status_route.2);
    gateway.stop().await.unwrap();
}
