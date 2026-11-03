//! The gateway side of mounting `pp-core`'s Rust product routes.
use super::*;
use crate::scratch::Scratch;

/// Registered compatibility routes and denials of the shipped gateway. A
/// changed digest means the route table changed; update it only on purpose.
const ROUTE_TABLE: (usize, &str) = (
    838,
    "b9df03fe0ddf4062ef03f2e8b1b40cf8316381ac51accc55de56dd5af6bba3c8",
);

async fn started(product: Option<axum::Router>) -> (Gateway, String, Scratch) {
    let root = Scratch::new("pp-slice0-mount-");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut gateway = Gateway::start_with_product(
        listener,
        root.to_path_buf(),
        CompatHandle::unpublished(),
        &OperationOwners::node_only(),
        product,
    )
    .unwrap();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let bootstrap = Request::builder()
        .uri(launch.strip_prefix(gateway.origin()).unwrap())
        .header("host", &gateway.state.host)
        .body(Body::empty())
        .unwrap();
    let response = handle(State(gateway.state.clone()), bootstrap).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (gateway, cookie, root)
}

fn get(gateway: &Gateway, path: &str, cookie: Option<&str>, peer: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .uri(path)
        .header("host", &gateway.state.host)
        .header("origin", &gateway.state.origin);
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let mut request = request.body(Body::empty()).unwrap();
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    }
    request
}

async fn text(response: Response<Body>) -> (StatusCode, String) {
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

fn stub() -> axum::Router {
    axum::Router::new().route(
        "/api/v1/plans",
        axum::routing::get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move {
            format!("rust {}", peer.ip())
        }),
    )
}

#[tokio::test]
async fn slice0_mount_feature_off_route_table_is_unchanged() {
    let (gateway, cookie, _root) = started(None).await;
    let mut lines: Vec<String> = gateway
        .state
        .routes
        .iter()
        .map(|route| {
            let effect = match route.effect {
                EffectClass::Observation => "observation",
                EffectClass::LocalCommit => "local_commit",
                EffectClass::ExternalEffect => "external_effect",
            };
            format!("route {} {} {effect}", route.method, route.path)
        })
        .chain(
            gateway
                .state
                .denied
                .iter()
                .map(|route| format!("denied {} {}", route.method, route.path)),
        )
        .collect();
    lines.sort();
    let digest = hex::encode(Sha256::digest(lines.join("\n").as_bytes()));
    assert_eq!((lines.len(), digest.as_str()), ROUTE_TABLE);

    // Without product routes a Plan read still goes to the compatibility proxy.
    let response = handle(
        State(gateway.state.clone()),
        get(
            &gateway,
            "/api/v1/plans",
            Some(&cookie),
            Some("127.0.0.1:9"),
        ),
    )
    .await;
    let (status, body) = text(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.contains("Compatibility backend unavailable"), "{body}");
    assert_eq!(gateway.compat_forwards(), 1);
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn slice0_mount_product_routes_answer_without_the_proxy() {
    let (gateway, cookie, _root) = started(Some(stub())).await;
    let state = || State(gateway.state.clone());

    let (status, body) = text(
        handle(
            state(),
            get(
                &gateway,
                "/api/v1/plans",
                Some(&cookie),
                Some("127.0.0.1:9"),
            ),
        )
        .await,
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "rust 127.0.0.1"));

    // Paths the product routes do not know get their 404, not the proxy.
    let (status, _) = text(
        handle(
            state(),
            get(&gateway, "/api/v1/printers", Some(&cookie), Some("[::1]:9")),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Desktop admission still runs first.
    let (status, body) = text(
        handle(
            state(),
            get(&gateway, "/api/v1/plans", None, Some("127.0.0.1:9")),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    // A non-loopback or unknown peer is refused before the product routes.
    for peer in [Some("192.0.2.10:443"), None] {
        let (status, body) =
            text(handle(state(), get(&gateway, "/api/v1/plans", Some(&cookie), peer)).await).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{peer:?}");
        assert!(body.contains("Loopback transport required"), "{body}");
    }

    assert_eq!(gateway.compat_forwards(), 0);
    gateway.stop().await.unwrap();
}
