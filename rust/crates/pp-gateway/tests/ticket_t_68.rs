use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Request, StatusCode, request::Builder},
};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use pp_compat::CompatHandle;
use pp_gateway::Gateway;
use std::{path::PathBuf, time::Duration};

async fn gateway() -> Gateway {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    Gateway::start(
        listener,
        PathBuf::new(),
        CompatHandle::unpublished(),
        &pp_gateway::ownership::cutover::OperationOwners::node_only(),
    )
    .unwrap()
}

async fn exchange(origin: &str, request: Builder) -> (StatusCode, HeaderMap, Vec<u8>) {
    let socket = tokio::net::TcpStream::connect(origin.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    let connection = tokio::spawn(connection);
    let response = sender
        .send_request(
            request
                .header("connection", "close")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (head, body) = response.into_parts();
    let body = body.collect().await.unwrap().to_bytes().to_vec();
    drop(sender);
    connection.await.unwrap().unwrap();
    (head.status, head.headers, body)
}

fn request(origin: &str, target: &str) -> Builder {
    Request::builder()
        .uri(target)
        .header("host", origin.strip_prefix("http://").unwrap())
}

fn private_response(headers: &HeaderMap) {
    assert_eq!(
        headers.get("cache-control"),
        Some(&HeaderValue::from_static("no-store"))
    );
    assert_eq!(
        headers.get("referrer-policy"),
        Some(&HeaderValue::from_static("no-referrer"))
    );
}

#[tokio::test]
async fn ticket_t_68_rejections_keep_bootstrap_private_and_unconsumed() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let target = launch.strip_prefix(&origin).unwrap();
    let token = target.split_once("token=").unwrap().1;
    let invalid_target = target.replace("/__desktop/", "/%2e%2e/__desktop/");
    let cases = [
        (
            request(&origin, "/__desktop/bootstrap?token=invalid"),
            StatusCode::UNAUTHORIZED,
        ),
        (
            Request::builder()
                .uri(target)
                .header("host", "foreign.invalid"),
            StatusCode::BAD_REQUEST,
        ),
        (
            request(&origin, target).header("origin", "http://foreign.invalid"),
            StatusCode::FORBIDDEN,
        ),
        (request(&origin, &invalid_target), StatusCode::BAD_REQUEST),
        (
            request(&origin, target).method("POST"),
            StatusCode::FORBIDDEN,
        ),
    ];
    for (request, expected) in cases {
        let (status, headers, body) = exchange(&origin, request).await;
        assert_eq!(status, expected);
        private_response(&headers);
        assert!(!headers.contains_key("set-cookie"));
        assert!(!headers.contains_key("location"));
        assert!(!String::from_utf8(body).unwrap().contains(token));
    }
    let (status, headers, body) = exchange(&origin, request(&origin, target)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    private_response(&headers);
    assert_eq!(headers["location"], "/");
    assert!(body.is_empty());
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let (status, _, body) = exchange(
        &origin,
        request(&origin, "/__runtime").header("cookie", cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["compat"]["state"],
        "starting"
    );
    let (status, headers, _) = exchange(&origin, request(&origin, target)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    private_response(&headers);
    gateway.begin_drain();
    let (status, headers, _) = exchange(&origin, request(&origin, target)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    private_response(&headers);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_ambiguous_host_cannot_redeem_bootstrap() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let target = launch.strip_prefix(&origin).unwrap();
    for builder in [
        request(&origin, target).header("host", "foreign.invalid"),
        request(&origin, target).header("host", origin.strip_prefix("http://").unwrap()),
        Request::builder()
            .uri(target)
            .header("host", HeaderValue::from_bytes(b"\xff").unwrap()),
    ] {
        let (status, headers, _) = exchange(&origin, builder).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        private_response(&headers);
        assert!(!headers.contains_key("set-cookie"));
    }
    let (status, _, _) = exchange(&origin, request(&origin, target)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_ambiguous_origin_cannot_redeem_bootstrap() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let target = launch.strip_prefix(&origin).unwrap();
    let cases = [
        request(&origin, target)
            .header("origin", &origin)
            .header("origin", "http://foreign.invalid"),
        request(&origin, target)
            .header("origin", "http://foreign.invalid")
            .header("origin", &origin),
        request(&origin, target)
            .header("origin", &origin)
            .header("origin", &origin),
        request(&origin, target).header("origin", HeaderValue::from_bytes(b"\xff").unwrap()),
        request(&origin, target).header("origin", "null"),
        request(&origin, target).header("origin", ""),
    ];
    for request in cases {
        let (status, headers, _) = exchange(&origin, request).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!headers.contains_key("set-cookie"));
        private_response(&headers);
    }
    let (status, headers, _) =
        exchange(&origin, request(&origin, target).header("origin", &origin)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    private_response(&headers);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_nontext_upgrade_cannot_redeem_bootstrap_without_origin() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let target = launch.strip_prefix(&origin).unwrap();
    let token = target.split_once("token=").unwrap().1;
    let (status, headers, body) = exchange(
        &origin,
        request(&origin, target).header("upgrade", HeaderValue::from_bytes(b"\xff").unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    private_response(&headers);
    assert!(!headers.contains_key("set-cookie"));
    assert!(!headers.contains_key("location"));
    assert!(!String::from_utf8(body).unwrap().contains(token));
    let (status, headers, _) = exchange(&origin, request(&origin, target)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    private_response(&headers);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_nontext_upgrade_requires_origin_for_session_requests() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let (cookie, token) = bootstrap_session(&mut gateway).await;
    let runtime = || request(&origin, "/__runtime").header("cookie", &cookie);
    let cases = [
        runtime().header("upgrade", HeaderValue::from_bytes(b"\xff").unwrap()),
        runtime()
            .header("upgrade", HeaderValue::from_bytes(b"\xff").unwrap())
            .header("upgrade", "websocket"),
        runtime().header("upgrade", "websocket"),
        runtime().header("upgrade", ""),
    ];
    let mut unexpected = Vec::new();
    for builder in cases {
        let (status, headers, body) = exchange(&origin, builder).await;
        if status != StatusCode::FORBIDDEN {
            unexpected.push(status);
        } else {
            private_response(&headers);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
                serde_json::json!({"detail": "Invalid Origin"})
            );
        }
        assert!(!headers.contains_key("set-cookie"));
        assert!(!headers.contains_key("location"));
        assert!(!String::from_utf8(body).unwrap().contains(&token));
    }
    for builder in [
        runtime(),
        runtime().header("x-unrelated", HeaderValue::from_bytes(b"\xff").unwrap()),
        runtime()
            .header("origin", &origin)
            .header("upgrade", "websocket"),
    ] {
        let (status, _, body) = exchange(&origin, builder).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["compat"]["state"],
            "starting"
        );
    }
    gateway.stop().await.unwrap();
    assert!(
        unexpected.is_empty(),
        "expected FORBIDDEN, got {unexpected:?}"
    );
}

#[tokio::test]
async fn ticket_t_68_session_requires_origin_for_logout_and_revokes_access() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let (status, headers, _) = exchange(
        &origin,
        request(&origin, launch.strip_prefix(&origin).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    for builder in [
        request(&origin, "/auth/logout").method("POST"),
        request(&origin, "/__runtime").header("upgrade", "websocket"),
        request(&origin, "/__runtime").header("origin", HeaderValue::from_bytes(b"\xff").unwrap()),
    ] {
        let (status, headers, _) = exchange(&origin, builder.header("cookie", cookie)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        private_response(&headers);
    }
    let (status, _, _) = exchange(
        &origin,
        request(&origin, "/__runtime").header("cookie", cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for (builder, expected) in [
        (
            request(&origin, "/api/v1/assistant/history"),
            StatusCode::FORBIDDEN,
        ),
        (
            request(&origin, "/api/v1/assistant/history").header("origin", &origin),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            request(&origin, "/api/v1/assistant/history").header("sec-fetch-site", "same-origin"),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            request(&origin, "/api/v1/assistant/history")
                .header("sec-fetch-site", "same-origin")
                .header("origin", HeaderValue::from_bytes(b"\xff").unwrap()),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let (status, _, _) = exchange(&origin, builder.header("cookie", cookie)).await;
        assert_eq!(status, expected);
    }
    let (status, _, body) = exchange(
        &origin,
        request(&origin, "/auth/logout")
            .method("POST")
            .header("cookie", cookie)
            .header("origin", &origin),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"ok":true})
    );
    let (status, headers, _) = exchange(
        &origin,
        request(&origin, "/__runtime").header("cookie", cookie),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    private_response(&headers);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_concurrent_redemption_has_one_private_success() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let target = launch.strip_prefix(&origin).unwrap();
    let mut requests = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let request = request(&origin, target);
        let origin = origin.clone();
        requests.spawn(async move { exchange(&origin, request).await });
    }
    let mut accepted = 0;
    let mut denied = 0;
    while let Some(result) = requests.join_next().await {
        let (status, headers, _) = result.unwrap();
        private_response(&headers);
        match status {
            StatusCode::SEE_OTHER => {
                accepted += 1;
                assert_eq!(headers["location"], "/");
                assert!(headers.contains_key("set-cookie"));
            }
            StatusCode::UNAUTHORIZED => {
                denied += 1;
                assert!(!headers.contains_key("set-cookie"));
            }
            other => panic!("Unexpected bootstrap status {other}"),
        }
    }
    assert_eq!((accepted, denied), (1, 15));
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_expired_bootstrap_remains_private() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    tokio::time::sleep(Duration::from_secs(61)).await;
    let (status, headers, body) = exchange(
        &origin,
        request(&origin, launch.strip_prefix(&origin).unwrap()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    private_response(&headers);
    assert!(!headers.contains_key("set-cookie"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"detail": "Bootstrap expired or consumed"})
    );
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_ipv4_wildcard_listener_is_rejected() {
    rejects_listener("0.0.0.0:0").await;
}

#[tokio::test]
async fn ticket_t_68_ipv6_wildcard_listener_is_rejected() {
    rejects_listener("[::]:0").await;
}

async fn rejects_listener(address: &str) {
    let listener = tokio::net::TcpListener::bind(address).await.unwrap();
    match Gateway::start(
        listener,
        PathBuf::new(),
        CompatHandle::unpublished(),
        &pp_gateway::ownership::cutover::OperationOwners::node_only(),
    ) {
        Ok(gateway) => {
            gateway.stop().await.unwrap();
            panic!("Gateway admitted a non-loopback listener at {address}");
        }
        Err(error) => assert_eq!(
            error.to_string(),
            "Desktop gateway requires a loopback listener"
        ),
    }
}

#[tokio::test]
async fn ticket_t_68_loopback_listeners_preserve_protected_bootstrap() {
    for address in ["127.0.0.1:0", "[::1]:0"] {
        let listener = tokio::net::TcpListener::bind(address).await.unwrap();
        let mut gateway = Gateway::start(
            listener,
            PathBuf::new(),
            CompatHandle::unpublished(),
            &pp_gateway::ownership::cutover::OperationOwners::node_only(),
        )
        .unwrap();
        let origin = gateway.origin().to_owned();
        let launch = gateway.take_launch_target().unwrap().into_url();
        let (status, _, _) = exchange(&origin, request(&origin, "/__runtime")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, headers, _) = exchange(
            &origin,
            request(&origin, launch.strip_prefix(&origin).unwrap()),
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        private_response(&headers);
        assert_eq!(headers["location"], "/");
        let cookie = headers["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        let (status, _, body) = exchange(
            &origin,
            request(&origin, "/__runtime").header("cookie", cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["compat"]["state"],
            "starting"
        );
        gateway.stop().await.unwrap();
    }
}

async fn bootstrap_session(gateway: &mut Gateway) -> (String, String) {
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let target = launch.strip_prefix(&origin).unwrap();
    let token = target.split_once("token=").unwrap().1.to_owned();
    let (status, headers, body) = exchange(&origin, request(&origin, target)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    private_response(&headers);
    assert!(body.is_empty());
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (cookie, token)
}

fn session_rejection(response: (StatusCode, HeaderMap, Vec<u8>), cookie: &str, token: &str) {
    let (status, headers, body) = response;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    private_response(&headers);
    assert!(!headers.contains_key("set-cookie"));
    assert!(!headers.contains_key("location"));
    let body = String::from_utf8(body).unwrap();
    assert!(!body.contains(token));
    assert!(!body.contains(cookie.split_once('=').unwrap().1));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"detail": "Desktop session required"})
    );
}

#[tokio::test]
async fn ticket_t_68_effectful_get_requires_single_same_origin_fetch_site() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let (cookie, token) = bootstrap_session(&mut gateway).await;
    let history = || request(&origin, "/api/v1/assistant/history").header("cookie", &cookie);
    let cases = [
        (
            "same-origin then cross-site",
            history()
                .header("sec-fetch-site", "same-origin")
                .header("sec-fetch-site", "cross-site"),
        ),
        (
            "same-origin then nontext",
            history()
                .header("sec-fetch-site", "same-origin")
                .header("sec-fetch-site", HeaderValue::from_bytes(b"\xff").unwrap()),
        ),
        (
            "identical duplicates",
            history()
                .header("sec-fetch-site", "same-origin")
                .header("sec-fetch-site", "same-origin"),
        ),
        (
            "cross-site then same-origin",
            history()
                .header("sec-fetch-site", "cross-site")
                .header("sec-fetch-site", "same-origin"),
        ),
        (
            "comma joined",
            history().header("sec-fetch-site", "same-origin, cross-site"),
        ),
        ("missing", history()),
        ("empty", history().header("sec-fetch-site", "")),
        (
            "cross-site",
            history().header("sec-fetch-site", "cross-site"),
        ),
    ];
    let mut unexpected = Vec::new();
    for (case, builder) in cases {
        let (status, headers, body) = exchange(&origin, builder).await;
        if status != StatusCode::FORBIDDEN {
            unexpected.push((case, status));
        } else {
            private_response(&headers);
        }
        assert!(!headers.contains_key("set-cookie"), "{case}");
        assert!(!headers.contains_key("location"), "{case}");
        let body = String::from_utf8(body).unwrap();
        assert!(!body.contains(&token), "{case}");
        assert!(!body.contains(cookie.split_once('=').unwrap().1), "{case}");
        let (status, headers, body) = exchange(
            &origin,
            request(&origin, "/__runtime").header("cookie", &cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{case}");
        assert!(!headers.contains_key("set-cookie"), "{case}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["compat"]["state"],
            "starting"
        );
    }
    for builder in [
        history().header("sec-fetch-site", "same-origin"),
        history().header("origin", &origin),
        history()
            .header("origin", &origin)
            .header("sec-fetch-site", "same-origin")
            .header("sec-fetch-site", "cross-site"),
    ] {
        let (status, headers, body) = exchange(&origin, builder).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()["detail"],
            "Compatibility backend unavailable"
        );
        assert_eq!(headers["cache-control"], "no-store");
        assert!(!headers.contains_key("set-cookie"));
    }
    gateway.stop().await.unwrap();
    assert!(
        unexpected.is_empty(),
        "expected FORBIDDEN, got {unexpected:?}"
    );
}

#[tokio::test]
async fn ticket_t_68_cookie_same_line_duplicate_valid_first_is_rejected() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let (cookie, token) = bootstrap_session(&mut gateway).await;
    let name = cookie.split_once('=').unwrap().0;
    session_rejection(
        exchange(
            &origin,
            request(&origin, "/__runtime").header("cookie", format!("{cookie}; {name}=wrong")),
        )
        .await,
        &cookie,
        &token,
    );
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_cookie_split_duplicate_valid_first_is_rejected() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let (cookie, token) = bootstrap_session(&mut gateway).await;
    let name = cookie.split_once('=').unwrap().0;
    session_rejection(
        exchange(
            &origin,
            request(&origin, "/__runtime")
                .header("cookie", &cookie)
                .header("cookie", format!("{name}=wrong")),
        )
        .await,
        &cookie,
        &token,
    );
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_cookie_non_text_second_header_is_rejected() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let (cookie, token) = bootstrap_session(&mut gateway).await;
    session_rejection(
        exchange(
            &origin,
            request(&origin, "/__runtime")
                .header("cookie", &cookie)
                .header("cookie", HeaderValue::from_bytes(b"\xff").unwrap()),
        )
        .await,
        &cookie,
        &token,
    );
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_cookie_non_text_first_header_is_rejected() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let (cookie, token) = bootstrap_session(&mut gateway).await;
    session_rejection(
        exchange(
            &origin,
            request(&origin, "/__runtime")
                .header("cookie", HeaderValue::from_bytes(b"\xff").unwrap())
                .header("cookie", &cookie),
        )
        .await,
        &cookie,
        &token,
    );
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_cookie_duplicate_variants_are_rejected() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let (cookie, token) = bootstrap_session(&mut gateway).await;
    let name = cookie.split_once('=').unwrap().0;
    for other in [format!("{name}=wrong"), cookie.clone(), format!("{name}=")] {
        for (first, second) in [(&cookie, &other), (&other, &cookie)] {
            for builder in [
                request(&origin, "/__runtime").header("cookie", format!("{first}; {second}")),
                request(&origin, "/__runtime")
                    .header("cookie", first)
                    .header("cookie", second),
            ] {
                session_rejection(exchange(&origin, builder).await, &cookie, &token);
            }
        }
    }
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_cookie_unrelated_pairs_and_split_valid_second_are_admitted() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let (cookie, token) = bootstrap_session(&mut gateway).await;
    let unrelated = "bare_flag; theme=dark; pp_desktop_0=old; pp_desktop_0=older";
    for builder in [
        request(&origin, "/__runtime").header("cookie", &cookie),
        request(&origin, "/__runtime").header("cookie", format!("{unrelated}; {cookie}")),
        request(&origin, "/__runtime").header("cookie", format!("{cookie}; {unrelated}")),
        request(&origin, "/__runtime")
            .header("cookie", unrelated)
            .header("cookie", &cookie),
        request(&origin, "/__runtime")
            .header("cookie", &cookie)
            .header("cookie", unrelated),
    ] {
        let (status, headers, body) = exchange(&origin, builder).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!headers.contains_key("set-cookie"));
        let body = String::from_utf8(body).unwrap();
        assert!(!body.contains(&token));
        assert!(!body.contains(cookie.split_once('=').unwrap().1));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["compat"]["state"],
            "starting"
        );
    }
    let name = cookie.split_once('=').unwrap().0;
    for builder in [
        request(&origin, "/__runtime"),
        request(&origin, "/__runtime").header("cookie", unrelated),
        request(&origin, "/__runtime").header("cookie", format!("{name}=wrong")),
    ] {
        session_rejection(exchange(&origin, builder).await, &cookie, &token);
    }
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_cookie_ambiguous_logout_cannot_revoke_session() {
    let mut gateway = gateway().await;
    let origin = gateway.origin().to_owned();
    let (cookie, token) = bootstrap_session(&mut gateway).await;
    let name = cookie.split_once('=').unwrap().0;
    for builder in [
        request(&origin, "/auth/logout").header("cookie", format!("{cookie}; {name}=wrong")),
        request(&origin, "/auth/logout")
            .header("cookie", &cookie)
            .header("cookie", &cookie),
        request(&origin, "/auth/logout")
            .header("cookie", &cookie)
            .header("cookie", HeaderValue::from_bytes(b"\xff").unwrap()),
    ] {
        session_rejection(
            exchange(&origin, builder.method("POST").header("origin", &origin)).await,
            &cookie,
            &token,
        );
        let (status, headers, _) = exchange(
            &origin,
            request(&origin, "/__runtime").header("cookie", &cookie),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(!headers.contains_key("set-cookie"));
    }
    let (status, headers, body) = exchange(
        &origin,
        request(&origin, "/auth/logout")
            .method("POST")
            .header("origin", &origin)
            .header("cookie", &cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!headers.contains_key("set-cookie"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
        serde_json::json!({"ok": true})
    );
    session_rejection(
        exchange(
            &origin,
            request(&origin, "/__runtime").header("cookie", &cookie),
        )
        .await,
        &cookie,
        &token,
    );
    gateway.stop().await.unwrap();
}

/// One request as the Node child saw it: method, path and headers.
type Seen = (String, String, Vec<(String, String)>);

/// Stand-in for the Node child. It answers every request with an empty JSON
/// list, so the gateway's own integration snapshot reads succeed, and logs
/// each request before answering it.
fn logging_node(mut child: pp_compat::FakeChild) -> std::sync::Arc<std::sync::Mutex<Vec<Seen>>> {
    let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = log.clone();
    tokio::spawn(async move {
        while let Ok(stream) = child.accept().await {
            let seen = seen.clone();
            tokio::spawn(async move {
                let service = move |request: Request<hyper::body::Incoming>| {
                    let headers = request
                        .headers()
                        .iter()
                        .map(|(name, value)| {
                            (
                                name.as_str().to_owned(),
                                String::from_utf8_lossy(value.as_bytes()).into_owned(),
                            )
                        })
                        .collect();
                    seen.lock().unwrap().push((
                        request.method().to_string(),
                        request.uri().path().to_owned(),
                        headers,
                    ));
                    async {
                        Ok::<_, std::convert::Infallible>(
                            axum::http::Response::builder()
                                .header("content-type", "application/json")
                                .body(Body::from("[]"))
                                .unwrap(),
                        )
                    }
                };
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), hyper::service::service_fn(service))
                    .await;
            });
        }
    });
    log
}

/// A gateway whose Node child is `logging_node`, with a desktop session.
async fn gateway_with_logging_node(
    dir: &std::path::Path,
) -> (Gateway, String, std::sync::Arc<std::sync::Mutex<Vec<Seen>>>) {
    let (endpoint, child) = pp_compat::Endpoint::fake(dir).unwrap();
    let log = logging_node(child);
    let (compat, _publication) = CompatHandle::replaceable(Some(endpoint));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut gateway = Gateway::start(
        listener,
        PathBuf::new(),
        compat,
        &pp_gateway::ownership::cutover::OperationOwners::node_only(),
    )
    .unwrap();
    let (cookie, _) = bootstrap_session(&mut gateway).await;
    (gateway, cookie, log)
}

#[tokio::test]
async fn ticket_t_68_forged_trust_headers_never_reach_node() {
    let scratch = tempfile::tempdir().unwrap();
    let (gateway, cookie, log) = gateway_with_logging_node(scratch.path()).await;
    let origin = gateway.origin().to_owned();
    let session = cookie.split_once('=').unwrap().1.to_owned();
    let forged = request(&origin, "/api/v1/printers")
        .header("origin", &origin)
        .header("cookie", &cookie)
        .header("x-pp-principal", "forged")
        .header("x-pp-signature", "forged")
        .header("x-pp-tenant", "account-b")
        .header("x-forwarded-for", "203.0.113.9")
        .header("x-forwarded-host", "foreign.invalid")
        .header("forwarded", "for=203.0.113.9;host=foreign.invalid")
        .header("authorization", "Bearer forged")
        .header("x-print-partner-api-key", "pp_forged");
    let (status, _, body) = exchange(&origin, forged).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(body, b"[]");
    let headers = log
        .lock()
        .unwrap()
        .iter()
        .find(|(_, path, _)| path == "/api/v1/printers")
        .map(|(_, _, headers)| headers.clone())
        .expect("the printers request reached Node");
    let value = |name: &str| {
        headers
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>()
    };
    for stripped in [
        "x-pp-tenant",
        "x-forwarded-for",
        "x-forwarded-host",
        "forwarded",
        "authorization",
        "x-print-partner-api-key",
        "cookie",
    ] {
        assert!(
            value(stripped).is_empty(),
            "{stripped} reached Node: {headers:?}"
        );
    }
    assert!(
        headers.iter().all(|(_, value)| !value.contains(&session)),
        "the desktop session reached Node"
    );
    assert_eq!(value("host"), ["desktop-compat"]);
    let [principal] = value("x-pp-principal")[..] else {
        panic!("expected one gateway principal: {headers:?}")
    };
    let principal: serde_json::Value =
        serde_json::from_slice(&hex::decode(principal).unwrap()).unwrap();
    assert_eq!(principal["generation"], "test-generation");
    assert_eq!(principal["tenant"], "default");
    assert_eq!(principal["actor"], "desktop-owner");
    assert_eq!(principal["method"], "GET");
    assert_eq!(principal["target"], "/api/v1/printers");
    let [signature] = value("x-pp-signature")[..] else {
        panic!("expected one gateway signature: {headers:?}")
    };
    assert_eq!(signature.len(), 64);
    assert!(signature.bytes().all(|byte| byte.is_ascii_hexdigit()));
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_68_keyless_mcp_is_refused_on_loopback_and_never_reaches_node() {
    let scratch = tempfile::tempdir().unwrap();
    let (gateway, cookie, log) = gateway_with_logging_node(scratch.path()).await;
    let origin = gateway.origin().to_owned();
    let mcp = |method: &str, path: &str| {
        request(&origin, path)
            .method(method)
            .header("origin", &origin)
            .header("content-type", "application/json")
    };
    // A desktop session alone, or with a client-supplied key, is not MCP authorization.
    for path in ["/mcp", "/api/v1/mcp"] {
        for method in ["GET", "POST", "DELETE"] {
            for builder in [
                mcp(method, path).header("cookie", &cookie),
                mcp(method, path)
                    .header("cookie", &cookie)
                    .header("authorization", "Bearer pp_forged")
                    .header("x-print-partner-api-key", "pp_forged"),
            ] {
                let (status, headers, body) = exchange(&origin, builder).await;
                assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
                private_response(&headers);
                assert!(
                    String::from_utf8(body)
                        .unwrap()
                        .contains("API-key authorization is required"),
                    "{method} {path}"
                );
            }
        }
        // Without a desktop session the gateway refuses before the MCP check.
        let (status, headers, _) = exchange(&origin, mcp("POST", path)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        private_response(&headers);
    }
    // Look-alike paths are not registered operations, so they are not forwarded either.
    for path in ["/mcp/", "/api/v1/mcp/", "/api/v1/mcp/sessions"] {
        let (status, _, _) = exchange(&origin, mcp("POST", path).header("cookie", &cookie)).await;
        assert!(!status.is_success(), "{path} returned {status}");
    }
    let forwarded: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .map(|(method, path, _)| format!("{method} {path}"))
        .filter(|seen| seen.contains("mcp"))
        .collect();
    assert!(
        forwarded.is_empty(),
        "MCP requests reached Node: {forwarded:?}"
    );
    gateway.stop().await.unwrap();
}
