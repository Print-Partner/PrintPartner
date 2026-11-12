use axum::{
    body::Body,
    http::{HeaderMap, Method, Response, StatusCode},
};
use http_body::Body as _;
use std::{
    num::NonZeroU16,
    path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

enum IdentityPreference {
    Fallback,
    Excluded,
    Weighted(NonZeroU16),
}

struct Preferences {
    gzip: u16,
    identity: IdentityPreference,
}

impl Preferences {
    fn parse(headers: &HeaderMap) -> Self {
        let mut gzip: Option<u16> = None;
        let mut identity: Option<u16> = None;
        let mut wildcard: Option<u16> = None;
        let mut readable = true;
        for field in headers.get_all("accept-encoding") {
            let Ok(field) = field.to_str() else {
                readable = false;
                continue;
            };
            for member in field.split(',') {
                let mut parts = member.split(';');
                let coding = parts.next().unwrap_or_default().trim();
                let slot = if coding.eq_ignore_ascii_case("gzip") {
                    &mut gzip
                } else if coding.eq_ignore_ascii_case("identity") {
                    &mut identity
                } else if coding == "*" {
                    &mut wildcard
                } else {
                    continue;
                };
                let weight = match (parts.next(), parts.next()) {
                    (None, None) => 1000,
                    (Some(parameter), None) => parameter
                        .split_once('=')
                        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("q"))
                        .and_then(|(_, value)| quality(value.trim()))
                        .unwrap_or(0),
                    _ => 0,
                };
                *slot = Some(slot.map_or(weight, |previous| previous.min(weight)));
            }
        }
        let identity = match identity {
            Some(weight) => NonZeroU16::new(weight)
                .map_or(IdentityPreference::Excluded, IdentityPreference::Weighted),
            None if wildcard == Some(0) => IdentityPreference::Excluded,
            None => IdentityPreference::Fallback,
        };
        Self {
            gzip: if readable {
                gzip.or(wildcard).unwrap_or(0)
            } else {
                0
            },
            identity,
        }
    }

    fn prefers_gzip(&self) -> bool {
        self.gzip > 0
            && match self.identity {
                IdentityPreference::Weighted(weight) => self.gzip >= weight.get(),
                IdentityPreference::Fallback | IdentityPreference::Excluded => true,
            }
    }

    fn accepts_identity(&self) -> bool {
        !matches!(self.identity, IdentityPreference::Excluded)
    }
}

fn quality(raw: &str) -> Option<u16> {
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    if fraction.len() > 3 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match whole {
        "1" if fraction.bytes().all(|byte| byte == b'0') => Some(1000),
        "0" => Some(
            fraction
                .bytes()
                .fold(0, |value, byte| value * 10 + u16::from(byte - b'0'))
                * match fraction.len() {
                    0 | 3 => 1,
                    1 => 100,
                    2 => 10,
                    _ => return None,
                },
        ),
        _ => None,
    }
}

struct AssetFile {
    path: PathBuf,
    file: tokio::fs::File,
    length: u64,
}

async fn open_contained(root: &Path, path: &Path) -> Option<AssetFile> {
    let path = tokio::fs::canonicalize(path).await.ok()?;
    if !path.starts_with(root) || !tokio::fs::metadata(&path).await.ok()?.is_file() {
        return None;
    }
    let file = tokio::fs::File::open(&path).await.ok()?;
    let metadata = file.metadata().await.ok()?;
    if !metadata.is_file() {
        return None;
    }
    Some(AssetFile {
        path,
        file,
        length: metadata.len(),
    })
}

/// The Content-Security-Policy every asset response carries.
pub(super) enum Policy {
    /// The Node-backed runtime: frame, object, base and ancestor rules only.
    Legacy,
    /// The Rust-only runtime, which only a `desktop_rebuild` build started with
    /// `PP_DESKTOP_REBUILD` runs: `replacement-csp.txt`, with the gateway's own
    /// WebSocket origin for the printer event stream.
    Replacement {
        ws_origin: String,
        files: pp_compat::AssetSnapshot,
    },
}

const LEGACY_POLICY: &str =
    "frame-src 'none'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'";
/// Shared with `web/apps/ui/test/browser/csp.browser.mjs`, which loads the
/// built replacement UI under this exact policy.
const REPLACEMENT_POLICY: &str = include_str!("replacement-csp.txt");
/// The document carries its style nonce on this element's `nonce` attribute.
/// `web/apps/ui/src/csp.ts` hands it to the libraries that insert
/// `<style>` elements at run time (Radix scroll lock).
const NONCE_META_NAME: &str = "pp-style-nonce";
/// `index.html` is read whole to add the nonce; the built one is under 1 KiB.
const MAX_DOCUMENT_BYTES: u64 = 1 << 20;

impl Policy {
    pub(super) fn is_spa_client_path(&self, path: &str) -> bool {
        match self {
            Self::Legacy => super::is_spa_client_path(path),
            Self::Replacement { .. } => is_replacement_client_path(path),
        }
    }

    pub(super) fn replacement(origin: &str, files: pp_compat::AssetSnapshot) -> Self {
        let authority = origin.strip_prefix("http://").unwrap_or(origin);
        Self::Replacement {
            ws_origin: format!("ws://{authority}"),
            files,
        }
    }

    /// `nonce` is set only on a replacement document; other responses allow
    /// no inline style at all.
    fn header(&self, nonce: Option<&str>) -> axum::http::HeaderValue {
        match self {
            Self::Legacy => axum::http::HeaderValue::from_static(LEGACY_POLICY),
            Self::Replacement { ws_origin, .. } => {
                let template = REPLACEMENT_POLICY.trim_end();
                let policy = match nonce {
                    Some(nonce) => template.replace("{nonce}", nonce),
                    None => template.replace(" 'nonce-{nonce}'", ""),
                }
                .replace("{ws_origin}", ws_origin);
                // The template and a loopback origin are plain ASCII; if that
                // ever fails, fail closed with a policy that allows nothing.
                axum::http::HeaderValue::from_str(&policy)
                    .unwrap_or_else(|_| axum::http::HeaderValue::from_static("default-src 'none'"))
            }
        }
    }
}

/// Template only the immutable, measured document bytes.
fn nonce_document(bytes: &[u8], nonce: &str) -> Option<Vec<u8>> {
    if bytes.len() as u64 > MAX_DOCUMENT_BYTES {
        return None;
    }
    let mut html = bytes.to_vec();
    let element = format!("<meta name=\"{NONCE_META_NAME}\" nonce=\"{nonce}\">");
    let at = nonce_position(&html);
    html.splice(at..at, element.into_bytes());
    Some(html)
}

/// The byte offset just after the first `<head ...>` start tag, in any letter
/// case and with any attributes. A plain byte scan, not an HTML parser: it is
/// meant for the Vite-built `index.html`, which has no comment or script text
/// before `<head>`. Without one, just after a leading `<!doctype ...>`
/// (the parser then opens `<head>` itself and the element lands in it, and the
/// page stays out of quirks mode); otherwise the start.
fn nonce_position(html: &[u8]) -> usize {
    let tag_end = |from: usize| {
        html[from..]
            .iter()
            .position(|&byte| byte == b'>')
            .map(|end| from + end + 1)
    };
    let starts = |at: usize, name: &[u8]| {
        html.get(at..at + name.len())
            .is_some_and(|bytes| bytes.eq_ignore_ascii_case(name))
    };
    let head = (0..html.len()).find(|&at| {
        starts(at, b"<head")
            && html
                .get(at + 5)
                .is_some_and(|&next| next == b'>' || next.is_ascii_whitespace())
    });
    if let Some(end) = head.and_then(tag_end) {
        return end;
    }
    let leading = html
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(html.len());
    if starts(leading, b"<!doctype") {
        return tag_end(leading).unwrap_or(0);
    }
    0
}

pub(super) async fn serve(
    root: &Path,
    relative: &str,
    document: bool,
    policy: &Policy,
    method: &Method,
    request_headers: &HeaderMap,
) -> Response<Body> {
    if !matches!(*method, Method::GET | Method::HEAD) {
        let mut response =
            super::error(StatusCode::METHOD_NOT_ALLOWED, "Assets require GET or HEAD");
        response
            .headers_mut()
            .insert("allow", axum::http::HeaderValue::from_static("GET, HEAD"));
        return response;
    }
    if let Policy::Replacement { files, .. } = policy {
        return serve_snapshot(files, relative, document, policy, method, request_headers);
    }
    let identity = match open_contained(root, &root.join(relative)).await {
        Some(asset) => asset,
        _ => return super::error(StatusCode::NOT_FOUND, "Asset not found"),
    };
    let mime = content_type(&identity.path, relative);
    let preferences = Preferences::parse(request_headers);
    let gzip = if preferences.prefers_gzip() && !relative.ends_with(".gz") {
        let mut sibling = identity.path.as_os_str().to_owned();
        sibling.push(".gz");
        open_contained(root, Path::new(&sibling)).await
    } else {
        None
    };
    let (asset, content_encoding) = if let Some(gzip) = gzip {
        (gzip, Some("gzip"))
    } else if preferences.accepts_identity() {
        (identity, None)
    } else {
        return unacceptable_encoding(method);
    };
    let length = asset.length;
    let body = if *method == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(ReaderStream::with_capacity(
            asset.file.take(length),
            64 * 1024,
        ))
    };
    asset_response(body, length, mime, policy, None, content_encoding)
}

fn asset_response(
    body: Body,
    length: u64,
    mime: &'static str,
    policy: &Policy,
    nonce: Option<&str>,
    content_encoding: Option<&'static str>,
) -> Response<Body> {
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert("content-type", axum::http::HeaderValue::from_static(mime));
    headers.insert("content-length", length.into());
    headers.insert(
        "vary",
        axum::http::HeaderValue::from_static("accept-encoding"),
    );
    if let Some(encoding) = content_encoding {
        headers.insert(
            "content-encoding",
            axum::http::HeaderValue::from_static(encoding),
        );
    }
    headers.insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    headers.insert(
        "referrer-policy",
        axum::http::HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        "x-content-type-options",
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert("content-security-policy", policy.header(nonce));
    response
}

fn unacceptable_encoding(method: &Method) -> Response<Body> {
    let mut response = super::error(StatusCode::NOT_ACCEPTABLE, "No acceptable asset encoding");
    response.headers_mut().insert(
        "vary",
        axum::http::HeaderValue::from_static("accept-encoding"),
    );
    if let Some(length) = response.body().size_hint().exact() {
        response
            .headers_mut()
            .insert("content-length", length.into());
    }
    if *method == Method::HEAD {
        *response.body_mut() = Body::empty();
    }
    response
}

fn serve_snapshot(
    files: &pp_compat::AssetSnapshot,
    relative: &str,
    document: bool,
    policy: &Policy,
    method: &Method,
    headers: &HeaderMap,
) -> Response<Body> {
    let Some(bytes) = files.get(relative) else {
        return super::error(StatusCode::NOT_FOUND, "Asset not found");
    };
    let nonce = document.then(|| hex::encode(rand::random::<[u8; 16]>()));
    let bytes = if let Some(nonce) = &nonce {
        let Some(html) = nonce_document(bytes, nonce) else {
            return super::error(StatusCode::INTERNAL_SERVER_ERROR, "Document unreadable");
        };
        html
    } else {
        if !Preferences::parse(headers).accepts_identity() {
            return unacceptable_encoding(method);
        }
        bytes.to_vec()
    };
    let length = bytes.len() as u64;
    let body = if *method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    asset_response(
        body,
        length,
        content_type(Path::new(relative), relative),
        policy,
        nonce.as_deref(),
        None,
    )
}

impl super::Gateway {
    /// Start with the replacement bytes retained by release verification.
    pub fn start_rust_only_with_assets(
        listener: tokio::net::TcpListener,
        assets: PathBuf,
        files: pp_compat::AssetSnapshot,
        product: impl FnOnce(super::MoonrakerStatus) -> axum::Router + 'static,
        status: tokio::sync::watch::Receiver<super::RustStatus>,
        hosts: std::sync::Arc<dyn super::MoonrakerHosts>,
        sign_out: super::SignOut,
    ) -> anyhow::Result<Self> {
        Self::start_inner(
            listener,
            assets,
            (super::ServedUi::Replacement, Some(files)),
            pp_compat::CompatHandle::unpublished(),
            &super::OperationOwners::node_only(),
            Some(Box::new(product)),
            Some((status, hosts, sign_out)),
        )
    }
}

fn content_type(path: &Path, relative: &str) -> &'static str {
    if relative == "manifest.json" {
        return "application/manifest+json";
    }
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("html") => "text/html; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("json" | "map") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("wasm") => "application/wasm",
        _ => "application/octet-stream",
    }
}

// Restrict fallback to UI roots so legacy root-level API aliases remain routes.
pub(super) fn is_replacement_client_path(path: &str) -> bool {
    !path.split('/').any(|segment| segment.contains('.'))
        && (matches!(
            path,
            "/" | "/settings" | "/login" | "/setup" | "/forgot-password" | "/reset-password"
        ) || ["/today", "/builds", "/farm", "/library", "/states", "/type"]
            .iter()
            .any(|prefix| {
                path == *prefix
                    || path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            }))
}
