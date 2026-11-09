#![cfg(test)]
//! GRE-1117: the Content-Security-Policy on gateway document and asset
//! responses. A Rust-only runtime sends `replacement-csp.txt`; the
//! Node-backed runtime keeps its frame, object, base and ancestor rules.
use super::*;
use crate::scratch::Scratch;

const LEGACY: &str = "frame-src 'none'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'";
const INDEX: &str = "<!doctype html><html><head><title>PP</title></head><body></body></html>";

struct Fixture {
    gateway: Gateway,
    cookie: String,
    _scratch: Scratch,
}

impl Fixture {
    async fn start(rust_only: bool) -> Self {
        let scratch = Scratch::new("pp-csp-");
        let root = scratch.join("site");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("index.html"), INDEX).unwrap();
        // A precompressed sibling that differs, so serving it is visible.
        std::fs::write(root.join("index.html.gz"), b"not the identity document").unwrap();
        std::fs::write(root.join("assets/app.js"), b"export default 1;").unwrap();
        let root = root.canonicalize().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut gateway = if rust_only {
            let (_status, receiver) = tokio::sync::watch::channel(RustStatus::Ready);
            Gateway::start_rust_only(
                listener,
                root,
                |_| axum::Router::new(),
                receiver,
                Arc::new(NoHosts),
            )
            .unwrap()
        } else {
            Gateway::start(
                listener,
                root,
                CompatHandle::unpublished(),
                &OperationOwners::node_only(),
            )
            .unwrap()
        };
        let cookie = gateway.test_session().await;
        Self {
            gateway,
            cookie,
            _scratch: scratch,
        }
    }

    async fn get(&self, method: &str, path: &str, accept_encoding: Option<&str>) -> Response<Body> {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", &self.gateway.state.host)
            .header("origin", self.gateway.origin())
            .header("cookie", &self.cookie)
            .header("accept", "text/html");
        if let Some(value) = accept_encoding {
            request = request.header("accept-encoding", value);
        }
        handle(
            State(self.gateway.state.clone()),
            request.body(Body::empty()).unwrap(),
        )
        .await
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
             manifest-src 'self'; frame-src 'none'; object-src 'none'; base-uri 'self'; \
             form-action 'self'; frame-ancestors 'none'"
        )
    }
}

struct NoHosts;
impl MoonrakerHosts for NoHosts {
    fn read(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<MoonrakerHost>, HostReadError>> + Send>,
    > {
        Box::pin(async { Ok(Vec::new()) })
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
    let fixture = Fixture::start(true).await;
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
    let fixture = Fixture::start(true).await;
    let response = fixture.get("GET", "/assets/app.js", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(policy(&response), fixture.replacement(None));
    assert_eq!(body(response).await, "export default 1;");
}

#[tokio::test]
async fn csp_node_backed_runtime_keeps_the_legacy_policy_and_document() {
    let fixture = Fixture::start(false).await;
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
}

#[test]
fn csp_template_is_one_header_line_with_both_placeholders() {
    let template = include_str!("replacement-csp.txt");
    assert_eq!(template.lines().count(), 1);
    assert_eq!(template.matches("{nonce}").count(), 1);
    assert_eq!(template.matches("{ws_origin}").count(), 1);
    assert!(!template.contains("unsafe"));
}
