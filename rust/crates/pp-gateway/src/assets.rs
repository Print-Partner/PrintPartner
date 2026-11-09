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
    Replacement { ws_origin: String },
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
    pub(super) fn replacement(origin: &str) -> Self {
        let authority = origin.strip_prefix("http://").unwrap_or(origin);
        Self::Replacement {
            ws_origin: format!("ws://{authority}"),
        }
    }

    /// `nonce` is set only on a replacement document; other responses allow
    /// no inline style at all.
    fn header(&self, nonce: Option<&str>) -> axum::http::HeaderValue {
        match self {
            Self::Legacy => axum::http::HeaderValue::from_static(LEGACY_POLICY),
            Self::Replacement { ws_origin } => {
                let template = REPLACEMENT_POLICY.trim_end();
                let policy = match nonce {
                    Some(nonce) => template.replace("{nonce}", nonce),
                    None => template.replace(" 'nonce-{nonce}'", ""),
                }
                .replace("{ws_origin}", ws_origin);
                axum::http::HeaderValue::from_str(&policy)
                    .expect("Policy holds only header-safe characters")
            }
        }
    }
}

/// Reads `index.html` and adds the nonce element right after the `<head>`
/// start tag, so it exists before any module script runs.
async fn nonce_document(asset: AssetFile, nonce: &str) -> Option<Vec<u8>> {
    if asset.length > MAX_DOCUMENT_BYTES {
        return None;
    }
    let mut html = Vec::with_capacity(asset.length as usize + 64);
    asset
        .file
        .take(asset.length)
        .read_to_end(&mut html)
        .await
        .ok()?;
    let element = format!("<meta name=\"{NONCE_META_NAME}\" nonce=\"{nonce}\">");
    let at = nonce_position(&html);
    html.splice(at..at, element.into_bytes());
    Some(html)
}

/// The byte offset just after the `<head ...>` start tag, in any letter case
/// and with any attributes. Without one, just after a leading `<!doctype ...>`
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
    let identity = match open_contained(root, &root.join(relative)).await {
        Some(asset) => asset,
        _ => return super::error(StatusCode::NOT_FOUND, "Asset not found"),
    };
    let mime = content_type(&identity.path, relative);
    let preferences = Preferences::parse(request_headers);
    // A replacement document gets a fresh nonce per response, so it is always
    // read from the identity file, never from a precompressed sibling.
    let nonced = document && matches!(policy, Policy::Replacement { .. });
    let gzip = if !nonced && preferences.prefers_gzip() && !relative.ends_with(".gz") {
        let mut sibling = identity.path.as_os_str().to_owned();
        sibling.push(".gz");
        open_contained(root, Path::new(&sibling)).await
    } else {
        None
    };
    let (asset, content_encoding) = if let Some(gzip) = gzip {
        (gzip, Some("gzip"))
    } else if nonced || preferences.accepts_identity() {
        // A replacement document is rewritten per response, so it only
        // exists uncoded. RFC 9110 section 12.5.3 lets the server send no
        // content coding; that beats a 406 the browser would show as a page.
        (identity, None)
    } else {
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
        return response;
    };
    let (body, length, nonce) = if nonced {
        let nonce = hex::encode(rand::random::<[u8; 16]>());
        let Some(html) = nonce_document(asset, &nonce).await else {
            return super::error(StatusCode::INTERNAL_SERVER_ERROR, "Document unreadable");
        };
        let length = html.len() as u64;
        let body = if *method == Method::HEAD {
            Body::empty()
        } else {
            Body::from(html)
        };
        (body, length, Some(nonce))
    } else if *method == Method::HEAD {
        (Body::empty(), asset.length, None)
    } else {
        let length = asset.length;
        let body = Body::from_stream(ReaderStream::with_capacity(
            asset.file.take(asset.length),
            64 * 1024,
        ));
        (body, length, None)
    };
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
    headers.insert("content-security-policy", policy.header(nonce.as_deref()));
    response
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
