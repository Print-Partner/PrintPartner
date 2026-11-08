//! A loopback request to a mounted router, with the Host, Origin and peer
//! address the accepted gate admits, and the JSON body of its answer. A body
//! is sent as `application/json`; `None` sends no body and no type.
use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use serde_json::Value;
use std::net::SocketAddr;
use tower::ServiceExt;

pub const ORIGIN: &str = "http://127.0.0.1:4100";

pub async fn send(
    router: &Router,
    method: &str,
    path: &str,
    body: Option<&str>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let content_type = body.is_some().then_some("application/json");
    let body = body.unwrap_or_default().as_bytes().to_vec();
    send_as(router, method, path, content_type, body, cookie).await
}

/// [`send`] with any content type and body, as a multipart upload is sent.
pub async fn send_as(
    router: &Router,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
    cookie: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:4100")
        .header("origin", ORIGIN);
    if let Some(content_type) = content_type {
        request = request.header("content-type", content_type);
    }
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let mut request = request.body(Body::from(body)).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 50_000))));
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 22)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
