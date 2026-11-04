#![cfg(test)]

use super::*;
use crate::scratch::Scratch;
use http_body_util::BodyExt;

const APP_GZIP: &[u8] = &[
    31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 75, 173, 40, 200, 47, 42, 81, 72, 73, 77, 75, 44, 205, 41,
    81, 48, 180, 6, 0, 134, 108, 106, 28, 17, 0, 0, 0,
];
const INDEX_GZIP: &[u8] = &[
    31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 179, 201, 40, 201, 205, 177, 75, 73, 45, 206, 46, 201, 47,
    176, 209, 7, 243, 0, 185, 73, 124, 178, 20, 0, 0, 0,
];

struct Fixture {
    gateway: Gateway,
    root: PathBuf,
    cookie: String,
    /// Owns `root` and the symlink targets beside it; drops after the gateway.
    _scratch: Scratch,
}

impl Fixture {
    async fn start() -> Self {
        let scratch = Scratch::new("pp-t11-");
        let root = scratch.join("site");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), b"<html>desktop</html>").unwrap();
        std::fs::write(root.join("assets/app.js"), b"export default 1;").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut gateway = Gateway::start(
            listener,
            root.clone(),
            CompatHandle::unpublished(),
            &crate::ownership::cutover::OperationOwners::node_only(),
        )
        .unwrap();
        let cookie = gateway.test_session().await;
        Self {
            gateway,
            root,
            cookie,
            _scratch: scratch,
        }
    }

    fn request(&self, method: &str, path: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .header("host", &self.gateway.state.host)
            .header("origin", self.gateway.origin())
            .header("cookie", &self.cookie)
            .header("accept", "text/html")
            .body(Body::empty())
            .unwrap()
    }

    async fn response(&self, method: &str, path: &str) -> Response<Body> {
        handle(
            State(self.gateway.state.clone()),
            self.request(method, path),
        )
        .await
    }

    async fn encoded_response(&self, method: &str, path: &str, fields: &[&str]) -> Response<Body> {
        let mut request = self.request(method, path);
        for field in fields {
            request
                .headers_mut()
                .append("accept-encoding", HeaderValue::from_str(field).unwrap());
        }
        handle(State(self.gateway.state.clone()), request).await
    }

    fn add_gzip(&self) {
        std::fs::write(self.root.join("assets/app.js.gz"), APP_GZIP).unwrap();
        std::fs::write(self.root.join("index.html.gz"), INDEX_GZIP).unwrap();
    }

    async fn stop(self) {
        self.gateway.stop().await.unwrap();
    }
}

#[tokio::test]
async fn ticket_t_11_gzip_get_head_and_spa_metadata() {
    let fixture = Fixture::start().await;
    fixture.add_gzip();
    for (path, mime, expected) in [
        ("/assets/app.js", "text/javascript", APP_GZIP),
        ("/plans/42/studio", "text/html; charset=utf-8", INDEX_GZIP),
    ] {
        for method in ["GET", "HEAD"] {
            let response = fixture.encoded_response(method, path, &["gzip"]).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["content-encoding"], "gzip");
            assert_eq!(response.headers()["vary"], "accept-encoding");
            assert_eq!(response.headers()["content-type"], mime);
            assert_eq!(
                response.headers()["content-length"],
                expected.len().to_string()
            );
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(
                bytes.as_ref(),
                if method == "HEAD" { &[] } else { expected }
            );
        }
    }
    std::fs::write(fixture.root.join("manifest.json"), b"{}").unwrap();
    let manifest_gzip = &[
        31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 171, 174, 5, 0, 67, 191, 166, 163, 2, 0, 0, 0,
    ];
    std::fs::write(fixture.root.join("manifest.json.gz"), manifest_gzip).unwrap();
    let response = fixture
        .encoded_response("GET", "/manifest.json", &["gzip"])
        .await;
    assert_eq!(
        response.headers()["content-type"],
        "application/manifest+json"
    );
    assert_eq!(response.headers()["content-encoding"], "gzip");
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        manifest_gzip
    );
    let raw = fixture
        .encoded_response("GET", "/assets/app.js.gz", &["gzip"])
        .await;
    assert_eq!(raw.status(), StatusCode::OK);
    assert_eq!(raw.headers()["content-type"], "application/octet-stream");
    assert!(raw.headers().get("content-encoding").is_none());
    assert_eq!(
        raw.into_body().collect().await.unwrap().to_bytes().as_ref(),
        APP_GZIP
    );
    fixture.stop().await;
}

#[tokio::test]
async fn ticket_t_11_gzip_negotiates_quality_and_repeated_fields() {
    let fixture = Fixture::start().await;
    fixture.add_gzip();
    for (fields, expected_status, gzip) in [
        (vec![], StatusCode::OK, false),
        (vec![""], StatusCode::OK, false),
        (vec!["br"], StatusCode::OK, false),
        (vec!["gzip;q=0"], StatusCode::OK, false),
        (vec!["GZip;Q=0.5"], StatusCode::OK, true),
        (vec!["gzip;q=0.4, identity;q=0.8"], StatusCode::OK, false),
        (vec!["gzip;q=0.8, identity;q=0.4"], StatusCode::OK, true),
        (vec!["gzip;q=0.5, identity;q=0.5"], StatusCode::OK, true),
        (vec!["*;q=0.5"], StatusCode::OK, true),
        (vec!["gzip;q=0.2, *;q=0.8"], StatusCode::OK, true),
        (vec!["gzip;q=0, *;q=1"], StatusCode::OK, false),
        (vec!["gzip;q=0", "*;q=1"], StatusCode::OK, false),
        (vec!["br", "gzip"], StatusCode::OK, true),
        (vec!["gzip", "gzip;q=0"], StatusCode::OK, false),
        (vec!["gzip;q=0", "gzip"], StatusCode::OK, false),
        (vec!["*;q=0"], StatusCode::NOT_ACCEPTABLE, false),
        (vec!["identity;q=0"], StatusCode::NOT_ACCEPTABLE, false),
        (vec!["br, identity;q=0"], StatusCode::NOT_ACCEPTABLE, false),
        (
            vec!["gzip;q=0, identity;q=0"],
            StatusCode::NOT_ACCEPTABLE,
            false,
        ),
        (vec!["gzip, identity;q=0"], StatusCode::OK, true),
        (vec!["gzip;q=1.001"], StatusCode::OK, false),
        (vec!["gzip;q=NaN"], StatusCode::OK, false),
        (vec!["gzip;q=-1"], StatusCode::OK, false),
        (vec!["gzip;q=1e0"], StatusCode::OK, false),
        (vec!["gzip;q=0.1234"], StatusCode::OK, false),
        (vec!["gzip;q=.5"], StatusCode::OK, false),
        (vec!["gzip;q=0."], StatusCode::OK, false),
        (vec!["gzip;q=1."], StatusCode::OK, true),
        (vec!["gzip;q=0.7;other=yes"], StatusCode::OK, false),
        (
            vec!["gzip;q=bad, identity;q=0"],
            StatusCode::NOT_ACCEPTABLE,
            false,
        ),
    ] {
        let response = fixture
            .encoded_response("GET", "/assets/app.js", &fields)
            .await;
        assert_eq!(response.status(), expected_status, "{fields:?}");
        assert_eq!(response.headers()["vary"], "accept-encoding", "{fields:?}");
        assert_eq!(
            response
                .headers()
                .get("content-encoding")
                .map(|value| value.to_str().unwrap()),
            gzip.then_some("gzip"),
            "{fields:?}"
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let expected: &[u8] = if expected_status == StatusCode::NOT_ACCEPTABLE {
            br#"{"detail":"No acceptable asset encoding"}"#
        } else if gzip {
            APP_GZIP
        } else {
            b"export default 1;"
        };
        assert_eq!(body.as_ref(), expected, "{fields:?}");
    }
    let mut unreadable = fixture.request("GET", "/assets/app.js");
    unreadable.headers_mut().append(
        "accept-encoding",
        HeaderValue::from_bytes(b"gzip,\xff").unwrap(),
    );
    unreadable
        .headers_mut()
        .append("accept-encoding", HeaderValue::from_static("identity;q=0"));
    let response = handle(State(fixture.gateway.state.clone()), unreadable).await;
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        r#"{"detail":"No acceptable asset encoding"}"#
    );
    let get = fixture
        .encoded_response("GET", "/assets/app.js", &["*;q=0"])
        .await;
    let head = fixture
        .encoded_response("HEAD", "/assets/app.js", &["*;q=0"])
        .await;
    assert_eq!(head.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(head.headers(), get.headers());
    assert_eq!(head.headers()["content-length"], "41");
    assert!(
        head.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    drop(get);
    fixture.stop().await;
}

#[tokio::test]
async fn ticket_t_11_gzip_missing_and_escaped_companions() {
    let fixture = Fixture::start().await;
    for fields in [&["gzip"][..], &["gzip, identity;q=0"][..]] {
        let response = fixture
            .encoded_response("GET", "/assets/app.js", fields)
            .await;
        let expected_status = if fields[0].contains("identity") {
            StatusCode::NOT_ACCEPTABLE
        } else {
            StatusCode::OK
        };
        assert_eq!(response.status(), expected_status);
        assert!(response.headers().get("content-encoding").is_none());
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            if expected_status == StatusCode::OK {
                &b"export default 1;"[..]
            } else {
                &br#"{"detail":"No acceptable asset encoding"}"#[..]
            }
        );
    }
    let outside = fixture.root.with_extension("gz");
    std::fs::write(&outside, b"outside-secret").unwrap();
    std::os::unix::fs::symlink(&outside, fixture.root.join("assets/app.js.gz")).unwrap();
    let response = fixture
        .encoded_response("GET", "/assets/app.js", &["gzip"])
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "export default 1;"
    );
    assert_eq!(
        fixture
            .encoded_response("GET", "/assets/app.js", &["gzip, identity;q=0"])
            .await
            .status(),
        StatusCode::NOT_ACCEPTABLE
    );
    std::fs::remove_file(outside).unwrap();
    std::fs::write(fixture.root.join("assets/orphan.js.gz"), APP_GZIP).unwrap();
    let orphan = fixture
        .encoded_response("GET", "/assets/orphan.js", &["gzip"])
        .await;
    assert_eq!(orphan.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        orphan.into_body().collect().await.unwrap().to_bytes(),
        r#"{"detail":"Asset not found"}"#
    );
    fixture.stop().await;
}

#[tokio::test]
async fn ticket_t_11_gzip_stream_is_bounded_and_cancellable() {
    let fixture = Fixture::start().await;
    let encoded = APP_GZIP.repeat(8000);
    std::fs::write(
        fixture.root.join("assets/large.js"),
        b"export default 1;".repeat(8000),
    )
    .unwrap();
    std::fs::write(fixture.root.join("assets/large.js.gz"), &encoded).unwrap();
    let response = fixture
        .encoded_response("GET", "/assets/large.js", &["gzip"])
        .await;
    assert_eq!(response.headers()["content-encoding"], "gzip");
    assert_eq!(
        response.headers()["content-length"],
        encoded.len().to_string()
    );
    let mut body = response.into_body();
    let mut received = Vec::new();
    while let Some(frame) = body.frame().await {
        let bytes = frame.unwrap().into_data().unwrap();
        assert!(bytes.len() <= 64 * 1024);
        received.extend_from_slice(&bytes);
        assert_eq!(fixture.gateway.inflight(), 1);
    }
    assert_eq!(received, encoded);
    drop(body);
    assert_eq!(fixture.gateway.inflight(), 0);
    let mut interrupted = fixture
        .encoded_response("GET", "/assets/large.js", &["gzip"])
        .await
        .into_body();
    assert_eq!(fixture.gateway.inflight(), 1);
    fixture.gateway.finish_drain();
    assert!(interrupted.frame().await.unwrap().is_err());
    drop(interrupted);
    assert_eq!(fixture.gateway.inflight(), 0);
    fixture.stop().await;
}

#[tokio::test]
async fn ticket_t_11_gzip_real_http_and_session_guards() {
    let fixture = Fixture::start().await;
    fixture.add_gzip();
    let socket = tokio::net::TcpStream::connect(&fixture.gateway.state.host)
        .await
        .unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(socket))
            .await
            .unwrap();
    let connection = tokio::spawn(connection);
    for method in ["GET", "HEAD"] {
        let mut request = fixture.request(method, "/plans/42/studio");
        request
            .headers_mut()
            .insert("accept-encoding", HeaderValue::from_static("gzip"));
        let response = sender.send_request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-encoding"], "gzip");
        assert_eq!(
            response.headers()["content-length"],
            INDEX_GZIP.len().to_string()
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            bytes.as_ref(),
            if method == "HEAD" { &[] } else { INDEX_GZIP }
        );
    }
    let mut request = fixture.request("GET", "/assets/app.js");
    request
        .headers_mut()
        .insert("accept-encoding", HeaderValue::from_static("gzip"));
    request.headers_mut().remove("cookie");
    let response = sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        r#"{"detail":"Desktop session required"}"#
    );
    let response = fixture
        .encoded_response("POST", "/assets/app.js", &["gzip"])
        .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"], "GET, HEAD");
    drop(response);
    drop(sender);
    connection.await.unwrap().unwrap();
    fixture.stop().await;
}

#[tokio::test]
async fn ticket_t_11_spa_head_and_asset_methods() {
    let fixture = Fixture::start().await;
    for path in ["/plans/42/studio", "/assets/app.js"] {
        let get = fixture.response("GET", path).await;
        assert_eq!(get.status(), StatusCode::OK);
        let mime = get.headers()["content-type"].clone();
        let bytes = get.into_body().collect().await.unwrap().to_bytes();
        let head = fixture.response("HEAD", path).await;
        assert_eq!(head.status(), StatusCode::OK, "{path}");
        assert_eq!(head.headers()["content-type"], mime);
        assert_eq!(head.headers()["content-length"], bytes.len().to_string());
        assert!(
            head.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );
    }
    for method in ["POST", "PUT", "DELETE", "OPTIONS"] {
        let response = fixture.response(method, "/assets/app.js").await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method}"
        );
        assert_eq!(response.headers()["allow"], "GET, HEAD");
    }
    fixture.stop().await;
}

#[tokio::test]
async fn ticket_t_11_asset_content_types_and_containment() {
    let fixture = Fixture::start().await;
    for (name, mime) in [
        ("app.js", "text/javascript"),
        ("app.css", "text/css"),
        ("font.woff2", "font/woff2"),
        ("image.webp", "image/webp"),
        ("favicon.ico", "image/x-icon"),
        ("module.wasm", "application/wasm"),
        ("data.json", "application/json"),
    ] {
        std::fs::write(fixture.root.join("assets").join(name), b"asset").unwrap();
        let response = fixture.response("GET", &format!("/assets/{name}")).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["content-type"], mime, "{name}");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            "asset"
        );
    }
    std::fs::write(fixture.root.join("manifest.json"), b"{}").unwrap();
    let manifest = fixture.response("GET", "/manifest.json").await;
    assert_eq!(
        manifest.headers()["content-type"],
        "application/manifest+json"
    );
    drop(manifest);
    for path in ["/assets/missing.js", "/assets/", "/not-a-spa-page"] {
        assert_eq!(
            fixture.response("GET", path).await.status(),
            StatusCode::NOT_FOUND
        );
    }
    let outside = fixture.root.with_extension("txt");
    std::fs::write(&outside, b"outside-secret").unwrap();
    std::os::unix::fs::symlink(&outside, fixture.root.join("assets/outside.txt")).unwrap();
    let escaped = fixture.response("GET", "/assets/outside.txt").await;
    assert_eq!(escaped.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        escaped.into_body().collect().await.unwrap().to_bytes(),
        r#"{"detail":"Asset not found"}"#
    );
    std::fs::remove_file(outside).unwrap();
    assert_eq!(
        fixture
            .response("GET", "/assets/%2e%2e/index.html")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fixture
            .response("GET", "/api/v1/filaments/custom")
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let mut unauthenticated = fixture.request("GET", "/assets/app.js");
    unauthenticated.headers_mut().remove("cookie");
    assert_eq!(
        handle(State(fixture.gateway.state.clone()), unauthenticated)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    fixture.stop().await;
}

#[tokio::test]
async fn ticket_t_11_asset_stream_is_bounded_and_retains_admission() {
    let fixture = Fixture::start().await;
    let payload = vec![0x5a; 2 * 1024 * 1024];
    std::fs::write(fixture.root.join("assets/large.bin"), &payload).unwrap();
    let response = fixture.response("GET", "/assets/large.bin").await;
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert!(
        first.len() <= 64 * 1024,
        "one frame buffered {} bytes",
        first.len()
    );
    assert_eq!(first, vec![0x5a; first.len()]);
    assert_eq!(fixture.gateway.inflight(), 1);
    assert!(!fixture.gateway.wait_idle(Duration::from_millis(20)).await);
    let mut total = first.len();
    while let Some(frame) = body.frame().await {
        let bytes = frame.unwrap().into_data().unwrap();
        assert!(bytes.len() <= 64 * 1024);
        assert_eq!(bytes, vec![0x5a; bytes.len()]);
        total += bytes.len();
    }
    assert_eq!(total, payload.len());
    drop(body);
    assert_eq!(fixture.gateway.inflight(), 0);
    let mut interrupted = fixture
        .response("GET", "/assets/large.bin")
        .await
        .into_body();
    assert_eq!(fixture.gateway.inflight(), 1);
    fixture.gateway.finish_drain();
    assert!(interrupted.frame().await.unwrap().is_err());
    drop(interrupted);
    assert_eq!(fixture.gateway.inflight(), 0);
    fixture.stop().await;
}

#[tokio::test]
async fn ticket_t_11_slow_asset_client_does_not_block_requests_or_shutdown() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let fixture = Fixture::start().await;
    let file = std::fs::File::create(fixture.root.join("assets/slow.bin")).unwrap();
    file.set_len(64 * 1024 * 1024).unwrap();
    drop(file);
    let socket = tokio::net::TcpStream::connect(&fixture.gateway.state.host)
        .await
        .unwrap();
    let mut socket = BufReader::new(socket);
    socket.write_all(format!(
        "GET /assets/slow.bin HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\nConnection: close\r\n\r\n",
        fixture.gateway.state.host, fixture.cookie
    ).as_bytes()).await.unwrap();
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), socket.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        assert!(!line.is_empty(), "connection closed before headers");
        headers.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }
    assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"), "{headers}");
    assert!(
        headers.contains("content-length: 67108864\r\n"),
        "{headers}"
    );
    assert!(!fixture.gateway.wait_idle(Duration::from_millis(100)).await);
    let probe = tokio::net::TcpStream::connect(&fixture.gateway.state.host)
        .await
        .unwrap();
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(probe))
            .await
            .unwrap();
    let connection = tokio::spawn(connection);
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        sender.send_request(fixture.request("HEAD", "/plans/42/studio")),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-length"], "20");
    assert!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    drop(sender);
    connection.await.unwrap().unwrap();
    let root = fixture.root;
    fixture.gateway.drain_for(Duration::from_millis(20)).await;
    tokio::time::timeout(Duration::from_secs(3), fixture.gateway.stop())
        .await
        .unwrap()
        .unwrap();
    let mut received = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), socket.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert!(
        received.len() < 64 * 1024 * 1024,
        "shutdown did not cancel the stream"
    );
    assert!(received.iter().all(|byte| *byte == 0));
    std::fs::remove_dir_all(root).unwrap();
}
