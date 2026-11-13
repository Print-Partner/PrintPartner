use axum::{
    Router,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

/// Bind the server's explicit listener, or the saved loopback port. The origin
/// always names loopback, so local probes reach either listener.
pub(super) async fn bind(
    listen: Option<std::net::SocketAddr>,
    lease: Option<&pp_storage::lease::StorageLease>,
) -> anyhow::Result<(tokio::net::TcpListener, Option<crate::OriginChange>, String)> {
    use anyhow::Context;
    let (listener, origin_change) = match listen {
        Some(address) => (
            tokio::net::TcpListener::bind(address)
                .await
                .context("Bind PP_LISTEN_ADDR")?,
            None,
        ),
        None => crate::origin::bind_origin(lease.context("Storage lease unavailable")?).await?,
    };
    let mut address = listener.local_addr()?;
    if listen.is_some() {
        address.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    Ok((listener, origin_change, format!("http://{address}")))
}

pub(super) fn for_listener(routes: Router, listen: Option<std::net::SocketAddr>) -> Router {
    if listen.is_some() {
        routes.layer(axum::middleware::from_fn(network_admission))
    } else {
        routes
    }
}

pub(super) async fn network_admission(mut request: Request, next: Next) -> Response {
    // MCP clients send no Origin; the MCP key guard decides for those paths.
    let admitted = if matches!(request.uri().path(), "/mcp" | "/api/v1/mcp") {
        pp_net::inbound::authority(&request)
    } else {
        pp_net::inbound::origin(&request)
    };
    let Ok((origin, host)) = admitted else {
        return StatusCode::FORBIDDEN.into_response();
    };
    request
        .extensions_mut()
        .insert(pp_api::NetworkAdmission { origin, host });
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, extract::ConnectInfo, http::Request};
    use pp_gateway::{Gateway, RustStatus, ServedUi};
    use pp_storage::{Limits, WriterOwner};
    use std::{
        net::SocketAddr,
        sync::{Arc, Mutex},
    };

    async fn gateway(
        listen: Option<SocketAddr>,
    ) -> (Gateway, Arc<Mutex<WriterOwner>>, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(Mutex::new(
            WriterOwner::open(&root.path().join("data"), Limits::default())
                .unwrap()
                .0,
        ));
        let listener =
            tokio::net::TcpListener::bind(listen.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap()))
                .await
                .unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let routes = crate::rebuild_api::product_router_with_updates(
            crate::DesktopRebuild::from_value(Some("1")),
            owner.clone(),
            &origin,
            None,
            crate::rebuild_api::RuntimeInputs {
                server: crate::rebuild_api::ServerOptions {
                    explicit_listener: listen.is_some(),
                    ..Default::default()
                },
                ..crate::rebuild_api::RuntimeInputs::detached()
            },
        )
        .unwrap()
        .unwrap();
        let hosts = Arc::new(crate::printer_status::StoredMoonrakerHosts::new(
            owner.lock().unwrap().client(),
        ));
        let (_, status) = tokio::sync::watch::channel(RustStatus::Ready);
        let gateway = Gateway::start_rust_only_with_network(
            listener,
            listen.is_some(),
            root.path().to_path_buf(),
            (ServedUi::Legacy, None),
            move |_| for_listener(routes, listen),
            status,
            hosts,
        )
        .unwrap();
        (gateway, owner, root)
    }

    fn request(
        method: &str,
        path: &str,
        host: &str,
        origin: &str,
        cookie: Option<&str>,
    ) -> Request<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", host)
            .header("origin", origin)
            .extension(ConnectInfo(
                "192.0.2.10:12345".parse::<SocketAddr>().unwrap(),
            ));
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        request.body(Body::empty()).unwrap()
    }

    async fn desktop_cookie(gateway: &mut Gateway, host: &str, origin: &str) -> String {
        let url = gateway.take_launch_target().unwrap().into_url();
        let path = url.strip_prefix(gateway.origin()).unwrap();
        let response = gateway
            .respond(request("GET", path, host, origin, None))
            .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        pp_testkit::session_cookie(response.headers()["set-cookie"].to_str().unwrap())
    }

    async fn exchange(gateway: &Gateway, request: Request<Body>) -> (StatusCode, String) {
        let response = gateway.respond(request).await;
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 16384)
            .await
            .unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    const HOST: &str = "192.0.2.20:8080";
    const ORIGIN: &str = "http://192.0.2.20:8080";

    /// One MCP request from the remote peer with only `headers`: the status, the
    /// `mcp-session-id` it issued, and its JSON-RPC answer or error body (`Null`
    /// when it has none).
    async fn mcp(
        gateway: &Gateway,
        (method, path, host): (&str, &str, &str),
        headers: &[(&str, &str)],
        body: &serde_json::Value,
    ) -> (StatusCode, Option<String>, serde_json::Value) {
        use http_body_util::BodyExt;
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", host)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .extension(ConnectInfo(
                "192.0.2.10:12345".parse::<SocketAddr>().unwrap(),
            ));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = gateway
            .respond(request.body(Body::from(body.to_string())).unwrap())
            .await;
        let status = response.status();
        let session = response
            .headers()
            .get("mcp-session-id")
            .map(|value| value.to_str().unwrap().to_owned());
        // rmcp may hold the event stream open after its answer, so stop at the
        // first complete event that carries data; a plain body ends by itself.
        let mut stream = response.into_body();
        let mut text = String::new();
        loop {
            let event = text
                .split_inclusive('\n')
                .filter(|line| line.ends_with('\n'))
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .find(|data| !data.is_empty());
            if let Some(event) = event {
                return (status, session, serde_json::from_str(event).unwrap());
            }
            match tokio::time::timeout(std::time::Duration::from_secs(5), stream.frame()).await {
                Ok(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        text.push_str(&String::from_utf8_lossy(&data));
                    }
                }
                _ => break,
            }
        }
        let answer = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
        (status, session, answer)
    }

    fn initialize() -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "explicit-listener", "version": "0"},
            },
        })
    }

    /// Signs in through the explicit listener and makes an API key with that
    /// session, as the Docker docs describe. Returns the desktop and account
    /// cookies and the key as a bearer value.
    async fn api_key(gateway: &mut Gateway) -> (String, String) {
        let desktop = desktop_cookie(gateway, HOST, ORIGIN).await;
        let post = |path: &str, cookie: &str, body: serde_json::Value| {
            let mut request = request("POST", path, HOST, ORIGIN, Some(cookie));
            *request.body_mut() = Body::from(body.to_string());
            request
                .headers_mut()
                .insert("content-type", "application/json".parse().unwrap());
            request
        };
        let account =
            serde_json::json!({"email": "owner@example.com", "password": "password-1234"});
        let registered = gateway
            .respond(post("/auth/register", &desktop, account))
            .await;
        assert_eq!(registered.status(), StatusCode::OK);
        let session =
            pp_testkit::session_cookie(registered.headers()["set-cookie"].to_str().unwrap());
        // Its body holds a gateway admission until dropped.
        drop(registered);
        let cookies = format!("{desktop}; {session}");
        let (status, created) = exchange(
            gateway,
            post("/settings/api-keys", &cookies, serde_json::json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        let created: serde_json::Value = serde_json::from_str(&created).unwrap();
        (
            cookies,
            format!("Bearer {}", created["key"].as_str().unwrap()),
        )
    }

    async fn shutdown(gateway: Gateway, owner: Arc<Mutex<WriterOwner>>) {
        gateway.drain().await;
        gateway.stop().await.unwrap();
        Arc::into_inner(owner)
            .unwrap()
            .into_inner()
            .unwrap()
            .shutdown()
            .unwrap();
    }

    /// GRE-1459: on the explicit listener, MCP answers a remote peer on its API
    /// key alone, through the same guard as loopback.
    #[tokio::test]
    async fn explicit_listener_admits_mcp_with_a_valid_key_from_a_remote_peer() {
        let (mut gateway, owner, _root) = gateway(Some("0.0.0.0:0".parse().unwrap())).await;
        let (_, bearer) = api_key(&mut gateway).await;
        for path in ["/mcp", "/api/v1/mcp"] {
            let target = ("POST", path, HOST);
            // A key, no cookie, no Origin: how an MCP client connects.
            let keyed = [("authorization", bearer.as_str())];
            let (status, session, answer) = mcp(&gateway, target, &keyed, &initialize()).await;
            assert_eq!(status, StatusCode::OK, "{path}: {answer}");
            assert!(
                answer["result"]["serverInfo"].is_object(),
                "{path}: {answer}"
            );
            let session = session.expect("an MCP session");
            let headers = [
                ("authorization", bearer.as_str()),
                ("mcp-session-id", session.as_str()),
                ("mcp-protocol-version", "2025-06-18"),
            ];
            let notified =
                serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
            let (status, _, _) = mcp(&gateway, target, &headers, &notified).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{path}");
            let list = serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
            let (status, _, answer) = mcp(&gateway, target, &headers, &list).await;
            assert_eq!(status, StatusCode::OK, "{path}: {answer}");
            let tools = answer["result"]["tools"].as_array().unwrap();
            assert!(
                tools.iter().any(|tool| tool["name"] == "list_plans"),
                "{path}"
            );
            let call = serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                                          "params": {"name": "list_plans", "arguments": {}}});
            let (status, _, answer) = mcp(&gateway, target, &headers, &call).await;
            assert_eq!(status, StatusCode::OK, "{path}: {answer}");
            assert_ne!(answer["result"]["isError"], true, "{path}: {answer}");
            assert!(
                answer["result"]["content"][0]["text"].is_string(),
                "{path}: {answer}"
            );
        }
        shutdown(gateway, owner).await;
    }

    /// GRE-1459: on the explicit listener, MCP without a valid key is refused
    /// with the guard's codes and creates no session, and leaving out Origin
    /// stays limited to the MCP paths.
    #[tokio::test]
    async fn explicit_listener_refuses_mcp_without_a_valid_key() {
        let (mut gateway, owner, _root) = gateway(Some("0.0.0.0:0".parse().unwrap())).await;
        let (cookies, bearer) = api_key(&mut gateway).await;
        let keyed = ("authorization", bearer.as_str());
        let wrong = (
            "authorization",
            "Bearer ppk_0000000000000000000000000000000000000000",
        );
        for path in ["/mcp", "/api/v1/mcp"] {
            for (name, method, host, headers, expected) in [
                (
                    "no key, no Origin",
                    "POST",
                    HOST,
                    vec![],
                    StatusCode::FORBIDDEN,
                ),
                (
                    "no key, same Origin",
                    "POST",
                    HOST,
                    vec![("origin", ORIGIN)],
                    StatusCode::UNAUTHORIZED,
                ),
                (
                    "no key, GET stream",
                    "GET",
                    HOST,
                    vec![],
                    StatusCode::UNAUTHORIZED,
                ),
                (
                    "no key, DELETE",
                    "DELETE",
                    HOST,
                    vec![("origin", ORIGIN)],
                    StatusCode::UNAUTHORIZED,
                ),
                (
                    "wrong key",
                    "POST",
                    HOST,
                    vec![wrong],
                    StatusCode::UNAUTHORIZED,
                ),
                (
                    "cookies only",
                    "POST",
                    HOST,
                    vec![("origin", ORIGIN), ("cookie", cookies.as_str())],
                    StatusCode::UNAUTHORIZED,
                ),
                (
                    "key, foreign Origin",
                    "POST",
                    HOST,
                    vec![keyed, ("origin", "http://192.0.2.21:8080")],
                    StatusCode::FORBIDDEN,
                ),
                (
                    "key, forwarded",
                    "POST",
                    HOST,
                    vec![keyed, ("x-forwarded-host", HOST)],
                    StatusCode::FORBIDDEN,
                ),
                (
                    "key, DNS Host",
                    "POST",
                    "attacker.example:8080",
                    vec![keyed],
                    StatusCode::FORBIDDEN,
                ),
            ] {
                let (status, session, answer) =
                    mcp(&gateway, (method, path, host), &headers, &initialize()).await;
                assert_eq!(status, expected, "{path} {name}: {answer}");
                assert!(session.is_none(), "{path} {name}");
            }
        }
        // Outside MCP, an unsafe request without Origin is still refused.
        let mut login = request("POST", "/auth/login", HOST, ORIGIN, Some(&cookies));
        login.headers_mut().remove("origin");
        assert_eq!(exchange(&gateway, login).await.0, StatusCode::FORBIDDEN);
        shutdown(gateway, owner).await;
    }

    #[tokio::test]
    async fn explicit_listener_keeps_account_origin_and_mcp_gates_for_remote_peers() {
        for listen in [Some("0.0.0.0:0".parse().unwrap()), None] {
            let (mut gateway, owner, _root) = gateway(listen).await;
            let origin = if listen.is_some() {
                "http://192.0.2.20:8080".to_owned()
            } else {
                gateway.origin().to_owned()
            };
            let host = origin.trim_start_matches("http://");
            let cookie = desktop_cookie(&mut gateway, host, &origin).await;
            let (status, body) = exchange(
                &gateway,
                request("GET", "/api/v1/plans", host, &origin, Some(&cookie)),
            )
            .await;
            if listen.is_none() {
                assert_eq!(status, StatusCode::FORBIDDEN);
                assert!(body.contains("Loopback transport required"));
                // Without the explicit listener, MCP stays loopback-only, key or not.
                for path in ["/mcp", "/api/v1/mcp"] {
                    let keyed = [("authorization", "Bearer ppk_not_a_real_key")];
                    let target = ("POST", path, host);
                    let (status, _, answer) = mcp(&gateway, target, &keyed, &initialize()).await;
                    assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {answer}");
                    assert_eq!(answer["detail"], "Loopback transport required", "{path}");
                }
            } else {
                assert_eq!(
                    status,
                    StatusCode::UNAUTHORIZED,
                    "account login remains required"
                );
                for (method, path, host, origin, cookie, expected) in [
                    (
                        "POST",
                        "/auth/login",
                        host,
                        "http://192.0.2.21:8080",
                        Some(cookie.as_str()),
                        StatusCode::FORBIDDEN,
                    ),
                    (
                        "GET",
                        "/api/v1/plans",
                        "attacker.example:8080",
                        "http://attacker.example:8080",
                        Some(cookie.as_str()),
                        StatusCode::FORBIDDEN,
                    ),
                    // The desktop cookie is not an MCP key (GRE-1459).
                    (
                        "POST",
                        "/mcp",
                        host,
                        origin.as_str(),
                        Some(cookie.as_str()),
                        StatusCode::UNAUTHORIZED,
                    ),
                    (
                        "POST",
                        "/api/v1/mcp",
                        host,
                        origin.as_str(),
                        Some(cookie.as_str()),
                        StatusCode::UNAUTHORIZED,
                    ),
                    (
                        "GET",
                        "/health",
                        host,
                        origin.as_str(),
                        None,
                        StatusCode::UNAUTHORIZED,
                    ),
                ] {
                    let (status, body) =
                        exchange(&gateway, request(method, path, host, origin, cookie)).await;
                    assert_eq!(status, expected, "{method} {path}");
                    if path == "/health" {
                        assert!(
                            !body.contains("\"authentication_required\""),
                            "no health JSON"
                        );
                    }
                }
                // A LAN peer's unknown key, with no cookie, reaches the MCP guard
                // and is refused there.
                for path in ["/mcp", "/api/v1/mcp"] {
                    let mut keyed = request("POST", path, host, &origin, None);
                    keyed.headers_mut().insert(
                        "authorization",
                        "Bearer ppk_not_a_real_key".parse().unwrap(),
                    );
                    let (status, body) = exchange(&gateway, keyed).await;
                    assert_eq!(status, StatusCode::UNAUTHORIZED, "keyed POST {path}");
                    assert!(
                        body.contains("Valid API key required"),
                        "keyed POST {path}: {body}"
                    );
                }
            }
            shutdown(gateway, owner).await;
        }
    }
}
