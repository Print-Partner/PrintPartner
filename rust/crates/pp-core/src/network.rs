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
    let Ok((origin, host)) = pp_net::inbound::origin(&request) else {
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
        let routes = crate::rebuild_api::product_router(
            crate::DesktopRebuild::from_value(Some("1")),
            owner.clone(),
            &origin,
            None,
        )
        .unwrap()
        .unwrap();
        let hosts = Arc::new(crate::printer_status::StoredMoonrakerHosts::new(
            owner.lock().unwrap().client(),
        ));
        let (_, status) = tokio::sync::watch::channel(RustStatus::Ready);
        let gateway = Gateway::start_rust_only_with_network(
            listener,
            pp_gateway::RustOnlyMode {
                network: listen.is_some(),
                sign_out: pp_gateway::SignOut::EndsRuntime,
            },
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
                    (
                        "POST",
                        "/mcp",
                        host,
                        origin.as_str(),
                        Some(cookie.as_str()),
                        StatusCode::FORBIDDEN,
                    ),
                    (
                        "POST",
                        "/api/v1/mcp",
                        host,
                        origin.as_str(),
                        Some(cookie.as_str()),
                        StatusCode::FORBIDDEN,
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
                // A presented API key does not route a LAN peer to the MCP guard.
                for path in ["/mcp", "/api/v1/mcp"] {
                    let mut keyed = request("POST", path, host, &origin, Some(&cookie));
                    keyed.headers_mut().insert(
                        "authorization",
                        "Bearer ppk_not_a_real_key".parse().unwrap(),
                    );
                    let (status, body) = exchange(&gateway, keyed).await;
                    assert_eq!(status, StatusCode::FORBIDDEN, "keyed POST {path}");
                    assert!(
                        body.contains("MCP is disabled"),
                        "keyed POST {path}: {body}"
                    );
                }
            }
            gateway.drain().await;
            gateway.stop().await.unwrap();
            Arc::into_inner(owner)
                .unwrap()
                .into_inner()
                .unwrap()
                .shutdown()
                .unwrap();
        }
    }
}
