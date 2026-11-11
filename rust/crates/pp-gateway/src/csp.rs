#![cfg(test)]
//! GRE-1117: the Content-Security-Policy on gateway document and asset
//! responses. A Rust-only runtime serving the replacement UI sends
//! `replacement-csp.txt`; every other runtime keeps its frame, object, base
//! and ancestor rules.
use super::*;
use crate::scratch::Scratch;
use crate::slice0_mount::{NoHosts, get};

const LEGACY: &str = "frame-src 'none'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'";
const INDEX: &str = "<!doctype html><html><head><title>PP</title></head><body></body></html>";

#[derive(Clone, Copy)]
enum Runtime {
    NodeBacked,
    RustOnly(ServedUi),
}

struct Fixture {
    gateway: Gateway,
    cookie: String,
    _scratch: Scratch,
}

impl Fixture {
    async fn start(runtime: Runtime) -> Self {
        Self::with_index(runtime, INDEX).await
    }

    async fn with_index(runtime: Runtime, index: &str) -> Self {
        let scratch = Scratch::new("pp-csp-");
        let root = scratch.join("site");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), index).unwrap();
        // A precompressed sibling that differs, so serving it is visible.
        std::fs::write(root.join("index.html.gz"), b"not the identity document").unwrap();
        std::fs::write(root.join("assets/app.js"), b"export default 1;").unwrap();
        let root = root.canonicalize().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut gateway = match runtime {
            Runtime::RustOnly(ui) => {
                let (_status, receiver) = tokio::sync::watch::channel(RustStatus::Ready);
                Gateway::start_rust_only(
                    listener,
                    root,
                    ui,
                    |_| {
                        axum::Router::new()
                            .route(
                                "/printers/{id}/files/content",
                                axum::routing::get(|| async { "printer bytes" }),
                            )
                            .route(
                                "/settings/api-keys",
                                axum::routing::get(|| async { "settings response" }),
                            )
                    },
                    receiver,
                    Arc::new(NoHosts),
                )
                .unwrap()
            }
            Runtime::NodeBacked => Gateway::start(
                listener,
                root,
                CompatHandle::unpublished(),
                &OperationOwners::node_only(),
            )
            .unwrap(),
        };
        let cookie = gateway.test_session().await;
        Self {
            gateway,
            cookie,
            _scratch: scratch,
        }
    }

    async fn get(&self, method: &str, path: &str, accept_encoding: Option<&str>) -> Response<Body> {
        let mut request = get(&self.gateway, path, Some(&self.cookie), None);
        *request.method_mut() = method.parse().unwrap();
        let headers = request.headers_mut();
        headers.insert("accept", HeaderValue::from_static("text/html"));
        if let Some(value) = accept_encoding {
            headers.insert("accept-encoding", HeaderValue::from_str(value).unwrap());
        }
        handle(State(self.gateway.state.clone()), request).await
    }

    /// The policy this gateway must send, built by hand from the gateway's port.
    fn replacement(&self, nonce: Option<&str>) -> String {
        let port = self.gateway.origin().rsplit(':').next().unwrap();
        let style = match nonce {
            Some(nonce) => format!("style-src 'self' 'nonce-{nonce}'"),
            None => "style-src 'self'".to_owned(),
        };
        format!(
            "default-src 'none'; script-src 'self'; {style}; font-src 'self'; img-src 'self'; \
             connect-src 'self' ws://127.0.0.1:{port}/ws/printers; worker-src 'self'; \
             manifest-src 'self'; frame-src 'none'; object-src 'none'; base-uri 'none'; \
             form-action 'self'; frame-ancestors 'none'"
        )
    }
}

fn policy(response: &Response<Body>) -> String {
    response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned()
}

async fn body(response: Response<Body>) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The text of the element the gateway adds, with `nonce` from the response.
fn element(nonce: &str) -> String {
    format!("<meta name=\"pp-style-nonce\" nonce=\"{nonce}\">")
}

/// The nonce of the element the gateway added after `<head>`.
fn nonce_of(html: &str) -> &str {
    let rest = html
        .strip_prefix("<!doctype html><html><head><meta name=\"pp-style-nonce\" nonce=\"")
        .unwrap_or_else(|| panic!("no nonce element first in <head>: {html}"));
    let (nonce, rest) = rest.split_once("\">").unwrap();
    assert_eq!(rest, &INDEX["<!doctype html><html><head>".len()..]);
    nonce
}

#[tokio::test]
async fn csp_rust_only_document_limits_every_source_to_self_and_the_event_stream() {
    let fixture = Fixture::start(Runtime::RustOnly(ServedUi::Replacement)).await;
    let first = fixture.get("GET", "/builds", Some("gzip")).await;
    assert_eq!(first.status(), StatusCode::OK);
    let header = policy(&first);
    // Read from the identity file with a fresh nonce, never the .gz sibling.
    assert!(first.headers().get("content-encoding").is_none());
    let length: usize = first.headers()["content-length"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let html = body(first).await;
    assert_eq!(length, html.len());
    let nonce = nonce_of(&html).to_owned();
    assert_eq!(nonce.len(), 32);
    assert!(nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(header, fixture.replacement(Some(&nonce)));

    let second = fixture.get("GET", "/builds", None).await;
    let header = policy(&second);
    let html = body(second).await;
    let again = nonce_of(&html);
    assert_ne!(again, nonce, "each document gets its own nonce");
    assert_eq!(header, fixture.replacement(Some(again)));

    let head = fixture.get("HEAD", "/builds", None).await;
    assert_eq!(head.headers()["content-length"], html.len().to_string());
    assert!(body(head).await.is_empty());
}

#[tokio::test]
async fn csp_rust_only_assets_allow_no_inline_style() {
    let fixture = Fixture::start(Runtime::RustOnly(ServedUi::Replacement)).await;
    let response = fixture.get("GET", "/assets/app.js", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(policy(&response), fixture.replacement(None));
    assert_eq!(body(response).await, "export default 1;");
}

/// The old `apps/web` build keeps its headers in every runtime: the
/// Node-backed one, and a Rust-only one that still serves that build.
#[tokio::test]
async fn csp_old_ui_keeps_the_legacy_policy_and_document_in_every_runtime() {
    for runtime in [Runtime::NodeBacked, Runtime::RustOnly(ServedUi::Legacy)] {
        let fixture = Fixture::start(runtime).await;
        let document = fixture.get("GET", "/builds", None).await;
        assert_eq!(policy(&document), LEGACY);
        assert_eq!(body(document).await, INDEX);
        // The precompressed sibling is still served to a gzip client.
        let gzip = fixture.get("GET", "/builds", Some("gzip")).await;
        assert_eq!(gzip.headers()["content-encoding"], "gzip");
        assert_eq!(policy(&gzip), LEGACY);
        assert_eq!(body(gzip).await, "not the identity document");
        let asset = fixture.get("GET", "/assets/app.js", None).await;
        assert_eq!(policy(&asset), LEGACY);
        // A client that refuses identity still gets the old 406.
        let refused = fixture
            .get("GET", "/assets/app.js", Some("br, identity;q=0"))
            .await;
        assert_eq!(refused.status(), StatusCode::NOT_ACCEPTABLE);
    }
}

/// A replacement document only exists uncoded, so a client that refuses
/// identity gets it uncoded rather than a 406 page.
#[tokio::test]
async fn csp_replacement_document_is_sent_uncoded_to_an_identity_refusing_client() {
    let fixture = Fixture::start(Runtime::RustOnly(ServedUi::Replacement)).await;
    let response = fixture
        .get("GET", "/builds", Some("gzip, identity;q=0"))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("content-encoding").is_none());
    let header = policy(&response);
    let html = body(response).await;
    assert_eq!(header, fixture.replacement(Some(nonce_of(&html))));
}

/// The element goes right after a `<head>` start tag in any case and with
/// attributes; without one, after the doctype, so the page stays in
/// standards mode.
#[tokio::test]
async fn csp_nonce_element_lands_in_head_and_never_before_the_doctype() {
    let cases = [
        (
            "<!DOCTYPE html><HTML><HEAD lang=en><title>x</title>",
            "<!DOCTYPE html><HTML><HEAD lang=en>",
        ),
        ("<!doctype html><header>no head</header>", "<!doctype html>"),
        ("\n<!doctype html>\n<body>", "\n<!doctype html>"),
        ("<p>bare", ""),
    ];
    for (index, prefix) in cases {
        let fixture = Fixture::with_index(Runtime::RustOnly(ServedUi::Replacement), index).await;
        let response = fixture.get("GET", "/builds", None).await;
        let header = policy(&response);
        let html = body(response).await;
        let nonce = header
            .split("'nonce-")
            .nth(1)
            .and_then(|rest| rest.split('\'').next())
            .unwrap();
        assert_eq!(
            html,
            format!("{prefix}{}{}", element(nonce), &index[prefix.len()..]),
            "{index:?}"
        );
    }
}

#[test]
fn csp_template_is_one_header_line_with_both_placeholders() {
    let template = include_str!("replacement-csp.txt");
    assert_eq!(template.lines().count(), 1);
    assert_eq!(template.matches("{nonce}").count(), 1);
    assert_eq!(template.matches("{ws_origin}").count(), 1);
    assert!(!template.contains("unsafe"));
}

#[tokio::test]
async fn replacement_spa_documents_and_deep_links_keep_the_nonce_policy() {
    let fixture = Fixture::start(Runtime::RustOnly(ServedUi::Replacement)).await;
    for path in [
        "/",
        "/today",
        "/farm",
        "/farm/log",
        "/builds/12/history",
        "/states/printers",
    ] {
        let response = fixture.get("GET", path, None).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers()["content-type"],
            "text/html; charset=utf-8"
        );
        let header = policy(&response);
        let html = body(response).await;
        assert_eq!(header, fixture.replacement(Some(nonce_of(&html))), "{path}");
    }
    for path in ["/assets/missing.js", "/icons/missing.svg", "/missing.css"] {
        let response = fixture.get("GET", path, None).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(!body(response).await.contains("<html>"));
    }
}

#[test]
fn replacement_fallback_excludes_service_namespaces_and_file_paths() {
    for path in [
        "/printers/1/files/content",
        "/printers/1/cameras/view",
        "/unknown-screen",
        "/auth/register",
        "/mcp",
        "/__desktop/bootstrap",
        "/__runtime",
        "/api",
        "/api/v2/builds",
        "/ws",
        "/ws/printers",
        "/health",
        "/health/ready",
        "/assets",
        "/icons",
        "/builds/app.js",
    ] {
        assert!(!assets::is_replacement_client_path(path), "{path}");
    }
}

#[tokio::test]
async fn replacement_navigation_preserves_root_api_routes() {
    let fixture = Fixture::start(Runtime::RustOnly(ServedUi::Replacement)).await;
    for (path, expected) in [
        ("/printers/1/files/content", "printer bytes"),
        ("/settings/api-keys", "settings response"),
    ] {
        let response = fixture.get("GET", path, None).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, expected);
    }
}
