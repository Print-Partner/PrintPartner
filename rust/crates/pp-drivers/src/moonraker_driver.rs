//! Moonraker driver (T-21), ported from the Node adapter: connection test,
//! status, filament used, `object_list`, cameras, print-file browse, and upload
//! with the start flag. File download and the camera proxy follow.
//! Until GRE-359, redirects are not followed (a 3xx is a non-OK reply), HTTPS
//! is refused, and replies are bounded by the guard's 2 MiB body limit.
use crate::moonraker::{QUERY, recorded_status};
use anyhow::{Context, Result, anyhow, bail, ensure};
use pp_api_types::printer_status::PrinterHostStatus as Status;
use pp_net::outbound::{BodyDeadline, HttpGuard, PostBody, PostError, StreamReply, key_header};
use serde::Serialize;
use serde_json::{Map, Value};
use std::{
    hash::{BuildHasher, RandomState},
    io::Cursor,
    path::PathBuf,
    pin::Pin,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};
use url::Url;

/// Moonraker's print-file root. Every browsable path is relative to it.
const GCODES_ROOT: &str = "gcodes";
const INVALID_PATH: &str = "Invalid Moonraker print-file path";
const UPLOAD_DEADLINE: Duration = Duration::from_secs(120);
const START_DEADLINE: Duration = Duration::from_secs(15);
/// Node's `files.open` aborts the whole download after 120 s.
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(120);
/// Node's `CAMERA_VIEW_TIMEOUT_MS`: an MJPEG stream never ends on its own, so
/// one proxied view holds the connection for at most a minute.
const CAMERA_VIEW_DEADLINE: Duration = Duration::from_secs(60);
const MAX_ERROR_TEXT_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CameraView {
    Mjpeg,
    Snapshot,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Camera {
    pub id: String,
    pub name: String,
    pub view: CameraView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect_ratio: Option<String>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum StorageEntry {
    Directory {
        path: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        modified_at: Option<String>,
    },
    File {
        path: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        size_bytes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        modified_at: Option<String>,
    },
}

#[derive(Debug, PartialEq, Serialize)]
pub struct StorageListing {
    pub path: String,
    pub entries: Vec<StorageEntry>,
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub struct Driver {
    guard: HttpGuard,
    base_url: String,
    api_key: Option<String>,
}

impl Driver {
    pub fn new(guard: HttpGuard, base_url: &str, api_key: Option<&str>) -> Self {
        Self {
            guard,
            base_url: base_url.into(),
            api_key: api_key.map(Into::into),
        }
    }

    fn base(&self) -> Result<&str> {
        let base = self.base_url.trim().trim_end_matches('/');
        ensure!(!base.is_empty(), "base_url is required");
        // Deliberate difference from Node, which appends the path into the query
        // or drops it with the fragment. The watcher refuses these bases too.
        ensure!(!base.contains(['?', '#']), "Invalid base_url");
        Ok(base)
    }

    /// `{base}{path}`, refused with Node's messages when Node would not fetch it.
    fn url(&self, path: &str) -> Result<String> {
        let url = format!("{}{path}", self.base()?);
        match Url::parse(&url)
            .map_err(|_| anyhow!("Invalid URL"))?
            .scheme()
        {
            "http" => Ok(url),
            "https" => bail!("HTTPS Moonraker hosts are not supported until GRE-359"),
            other => bail!("Unsupported URL protocol: {other}:"),
        }
    }

    async fn fetch(&self, path: &str) -> Result<Reply> {
        let url = self.url(path)?;
        let reply = self
            .guard
            .get(&url, self.api_key.as_deref().map(str::trim))
            .await?;
        Ok(Reply {
            status: reply.status,
            body: reply.body,
        })
    }

    pub(crate) async fn probe_reply(&self) -> Result<(u16, Vec<u8>)> {
        let reply = self.fetch("/server/info").await?;
        Ok((reply.status, reply.body))
    }

    pub async fn test_connection(&self) -> TestResult {
        let result = async {
            let reply = self.fetch("/server/info").await?;
            if !reply.ok() {
                bail!("Moonraker returned HTTP {}", reply.status);
            }
            let result = envelope(&reply.body)?;
            let state = result
                .get("klippy_state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            Ok(format!("Connected (klippy: {state})"))
        }
        .await;
        match result {
            Ok(message) => TestResult { ok: true, message },
            Err(error) => TestResult {
                ok: false,
                message: error.to_string(),
            },
        }
    }

    /// Status with `uptime_seconds` from `/machine/proc_stats` (the watcher omits it).
    pub async fn status(&self) -> Status {
        let result = async {
            let reply = self.fetch(QUERY).await?;
            if !reply.ok() {
                return Ok(Status::offline(&format!(
                    "Moonraker returned HTTP {}",
                    reply.status
                )));
            }
            let base = self.base()?;
            let mut status = match recorded_status(&reply.body, base) {
                Ok(status) => status,
                // Node reports JSON without `result.status` as unknown; the
                // watcher fails closed on it instead.
                Err(_) if serde_json::from_slice::<Value>(&reply.body).is_ok() => {
                    let mut status = Status::unknown("");
                    status.message = None;
                    status.ip_address = Url::parse(base)?.host_str().map(str::to_owned);
                    status
                }
                Err(error) => return Err(error),
            };
            status.uptime_seconds = self.uptime().await;
            Ok::<_, anyhow::Error>(status)
        }
        .await;
        result.unwrap_or_else(|error| Status::offline(&error.to_string()))
    }

    async fn uptime(&self) -> Option<f64> {
        let reply = self.fetch("/machine/proc_stats").await.ok()?;
        reply.ok().then_some(())?;
        envelope(&reply.body)
            .ok()?
            .get("system_uptime")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(f64::round)
    }

    pub async fn filament_used(&self) -> Option<f64> {
        let status = self.query_status("print_stats").await?;
        status
            .get("print_stats")?
            .get("filament_used")?
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
    }

    /// The file name and filament length in millimetres of the print
    /// `print_stats` reports, from one read; `None` unless that read says the
    /// print is `complete`.
    pub async fn finished_filament(&self) -> Option<(String, f64)> {
        let status = self.query_status("print_stats").await?;
        let stats = status.get("print_stats")?;
        (stats.get("state")?.as_str()? == "complete").then_some(())?;
        let used = stats.get("filament_used")?.as_f64()?;
        Some((stats.get("filename")?.as_str()?.to_owned(), used))
    }

    pub async fn object_list(&self) -> Vec<String> {
        let Some(status) = self.query_status("exclude_object").await else {
            return Vec::new();
        };
        let objects = status
            .get("exclude_object")
            .and_then(|v| v.get("objects"))
            .and_then(Value::as_array);
        objects
            .into_iter()
            .flatten()
            .filter_map(|object| object.as_object()?.get("name")?.as_str())
            .map(js_trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    }

    async fn query_status(&self, object: &str) -> Option<Value> {
        let reply = self
            .fetch(&format!("/printer/objects/query?{object}"))
            .await
            .ok()?;
        reply.ok().then_some(())?;
        let status = envelope(&reply.body).ok()?.get("status")?.clone();
        status.is_object().then_some(status)
    }

    /// The webcam database rows; none when Moonraker has no webcam API.
    async fn webcams(&self) -> Result<Vec<Map<String, Value>>> {
        let reply = self.fetch("/server/webcams/list").await?;
        if reply.status == 404 {
            return Ok(Vec::new());
        }
        ensure!(
            reply.ok(),
            "Moonraker webcam list returned HTTP {}",
            reply.status
        );
        let result = envelope(&reply.body)?;
        let webcams = result.get("webcams").and_then(Value::as_array);
        Ok(webcams
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .cloned()
            .collect())
    }

    pub async fn cameras(&self) -> Result<Vec<Camera>> {
        let base = self.base()?.to_owned();
        Ok(self
            .webcams()
            .await?
            .iter()
            .enumerate()
            .filter_map(|(index, camera)| list_camera(&base, index, camera))
            .collect())
    }

    /// One GET with the API key whose body is not buffered. The head and the
    /// whole body share `deadline`, as fetch with `AbortSignal.timeout` does.
    async fn stream(&self, url: &str, deadline: Duration) -> Result<StreamReply> {
        let key = key_header(self.api_key.as_deref().map(str::trim));
        let headers: Vec<(&str, &str)> = key
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        self.guard
            .stream(url, &headers, deadline, BodyDeadline::Total)
            .await
    }

    /// Open one print file under the `gcodes` root for download, as Node's
    /// `files.open`. The reply comes back as is, body unread.
    pub async fn open_file(&self, path: &str) -> Result<StreamReply> {
        let relative = safe_storage_path(path).context(INVALID_PATH)?;
        let encoded: Vec<String> = format!("{GCODES_ROOT}/{relative}")
            .split('/')
            .map(encode_uri_component)
            .collect();
        let url = self.url(&format!("/server/files/{}", encoded.join("/")))?;
        self.stream(&url, DOWNLOAD_DEADLINE).await
    }

    /// Open one enabled camera's browser view, as Node's `cameras.open`: its
    /// MJPEG stream, else its snapshot. A cross-origin URL in either field
    /// refuses the camera. The reply comes back as is, body unread.
    pub async fn open_camera(&self, camera_id: &str) -> Result<StreamReply> {
        self.url("")?;
        let base = self.base()?.to_owned();
        let webcams = self.webcams().await?;
        let camera = webcams
            .iter()
            .enumerate()
            .find(|(index, camera)| list_camera_id(*index, camera) == camera_id)
            .map(|(_, camera)| camera)
            .filter(|camera| camera.get("enabled") != Some(&Value::Bool(false)))
            .context("Camera not found")?;
        ensure!(
            ["stream_url", "snapshot_url"]
                .iter()
                .all(|key| same_origin(&base, text(camera, key)).is_some()),
            "Moonraker advertised a cross-origin camera URL"
        );
        let (_, url) =
            camera_view(&base, camera).context("Camera has no browser-compatible view")?;
        self.stream(&url, CAMERA_VIEW_DEADLINE).await
    }

    /// Browse one directory of the `gcodes` root via `/server/files/directory`,
    /// which, unlike `/server/files/list`, also reports folders and non-G-code files.
    pub async fn browse(&self, path: &str) -> Result<StorageListing> {
        self.base()?;
        let stripped = path.trim_matches('/');
        let requested = if stripped.is_empty() {
            String::new()
        } else {
            safe_storage_path(stripped).context(INVALID_PATH)?
        };
        let provider = if requested.is_empty() {
            GCODES_ROOT.to_owned()
        } else {
            format!("{GCODES_ROOT}/{requested}")
        };
        let encoded: Vec<String> = provider.split('/').map(encode_uri_component).collect();
        let reply = self
            .fetch(&format!(
                "/server/files/directory?path={}&extended=true",
                encoded.join("/")
            ))
            .await?;
        ensure!(
            reply.ok(),
            "Moonraker directory listing returned HTTP {}",
            reply.status
        );
        let result = envelope(&reply.body)?;
        let rows = |key: &str| {
            result
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_object)
        };
        let prefix = if requested.is_empty() {
            String::new()
        } else {
            format!("{requested}/")
        };
        let mut entries = Vec::new();
        for row in rows("dirs") {
            let Some(name) = entry_name(row.get("dirname")) else {
                continue;
            };
            entries.push(StorageEntry::Directory {
                path: format!("{prefix}{name}"),
                name,
                modified_at: modified_at(row.get("modified"))?,
            });
        }
        for row in rows("files") {
            let Some(name) = entry_name(row.get("filename")) else {
                continue;
            };
            entries.push(StorageEntry::File {
                path: format!("{prefix}{name}"),
                name,
                size_bytes: row
                    .get("size")
                    .and_then(Value::as_f64)
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .map(|v| v.round() as u64),
                modified_at: modified_at(row.get("modified"))?,
            });
        }
        Ok(StorageListing {
            path: requested,
            entries,
        })
    }
}

pub enum UploadSource {
    Bytes(Vec<u8>),
    Path(PathBuf),
}

#[derive(Debug, PartialEq, Serialize)]
pub struct UploadResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<bool>,
    pub message: String,
    /// The printer may have accepted the upload, or started the print, without
    /// replying. Reconcile instead of resending. Not part of the Node JSON.
    #[serde(skip)]
    pub outcome_unknown: bool,
}

impl UploadResult {
    pub(crate) fn failed(message: String, outcome_unknown: bool) -> Self {
        Self {
            ok: false,
            remote_path: None,
            started: None,
            message,
            outcome_unknown,
        }
    }
}

impl Driver {
    /// Upload into `gcodes`, then POST `/printer/print/start` only when `start`
    /// is true and the upload returned 2xx. Each request is sent once.
    pub async fn upload_file(
        &self,
        source: UploadSource,
        filename: &str,
        start: bool,
    ) -> UploadResult {
        let replaced = filename.replace(['/', '\\'], "_");
        let name = match js_trim(&replaced) {
            "" => "print.gcode",
            name => name,
        }
        .to_owned();
        // Check the base before opening the source.
        let start_path = format!(
            "/printer/print/start?filename={}",
            encode_uri_component(&name)
        );
        let urls = self
            .url("/server/files/upload")
            .and_then(|upload| Ok((upload, self.url(&start_path)?)));
        let (upload, start_url) = match urls {
            Ok(urls) => urls,
            Err(error) => return UploadResult::failed(error.to_string(), false),
        };
        let body = match multipart(source, &name).await {
            Ok(body) => body,
            Err(error) => return UploadResult::failed(error.to_string(), false),
        };
        let api_key = self.api_key.as_deref().map(str::trim);
        let reply = match self
            .guard
            .post(&upload, api_key, Some(body), UPLOAD_DEADLINE)
            .await
        {
            Ok(reply) => reply,
            // A source that ended early cannot have produced a complete upload.
            Err(PostError::Unsent(error) | PostError::SourceFailed(error)) => {
                return UploadResult::failed(format!("{error:#}"), false);
            }
            Err(PostError::Unknown(error)) => {
                let message = format!("Upload outcome unknown: {error:#}");
                return UploadResult::failed(message, true);
            }
        };
        if !(200..300).contains(&reply.status) {
            let message = format!(
                "Upload failed (HTTP {}){}",
                reply.status,
                error_text(&reply.body)
            );
            return UploadResult::failed(message, false);
        }
        let (started, message, outcome_unknown) = if !start {
            (false, format!("Uploaded {name}"), false)
        } else {
            match self
                .guard
                .post(&start_url, api_key, None, START_DEADLINE)
                .await
            {
                Ok(reply) if (200..300).contains(&reply.status) => {
                    (true, format!("Uploaded and started {name}"), false)
                }
                Ok(reply) => (
                    false,
                    format!(
                        "Uploaded, but start failed (HTTP {}){}",
                        reply.status,
                        error_text(&reply.body)
                    ),
                    false,
                ),
                // Node reports a start transport error as a failed upload. The
                // file is on the printer, so this reports the upload as done.
                Err(PostError::Unsent(error) | PostError::SourceFailed(error)) => (
                    false,
                    format!("Uploaded, but start failed: {error:#}"),
                    false,
                ),
                Err(PostError::Unknown(error)) => (
                    false,
                    format!("Uploaded, but start outcome unknown: {error:#}"),
                    true,
                ),
            }
        };
        UploadResult {
            ok: true,
            remote_path: Some(name),
            started: Some(started),
            message,
            outcome_unknown,
        }
    }
}

/// The `multipart/form-data` body undici's `FormData` sends: the file part, then `root`.
async fn multipart(source: UploadSource, name: &str) -> Result<PostBody> {
    let state = RandomState::new();
    let boundary = format!(
        "----pp-moonraker-{:016x}{:016x}",
        state.hash_one(1u8),
        state.hash_one(2u8)
    );
    // WHATWG form encoding escapes LF, CR and `"` in filenames.
    let escaped = name
        .replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('"', "%22");
    let head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{escaped}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    );
    let tail = format!(
        "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"root\"\r\n\r\n{GCODES_ROOT}\r\n--{boundary}--\r\n"
    );
    let (file, file_length): (Pin<Box<dyn AsyncRead + Send>>, u64) = match source {
        UploadSource::Bytes(bytes) => {
            let length = bytes.len() as u64;
            (Box::pin(Cursor::new(bytes)), length)
        }
        UploadSource::Path(path) => {
            let file = tokio::fs::File::open(&path).await?;
            let length = file.metadata().await?.len();
            (Box::pin(file), length)
        }
    };
    let length = head.len() as u64 + file_length + tail.len() as u64;
    let reader = Cursor::new(head.into_bytes())
        .chain(file)
        .chain(Cursor::new(tail.into_bytes()));
    Ok(PostBody {
        content_type: format!("multipart/form-data; boundary={boundary}"),
        reader: Box::pin(reader),
        length,
    })
}

/// `: <first 200 UTF-16 units>` of an error reply, as Node formats it. Node
/// drops text over 64 KiB, and `TextDecoder` strips a leading BOM.
pub(crate) fn error_text(body: &[u8]) -> String {
    if body.len() > MAX_ERROR_TEXT_BYTES {
        return String::new();
    }
    let text = String::from_utf8_lossy(body);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut cut = String::new();
    let mut units = 0;
    for c in text.chars() {
        units += c.len_utf16();
        if units > 200 {
            // `slice(0, 200)` keeps the high half of a pair cut at unit 200. A
            // Rust string cannot hold a lone surrogate, so it becomes U+FFFD.
            if units == 201 && c.len_utf16() == 2 {
                cut.push('\u{fffd}');
            }
            break;
        }
        cut.push(c);
    }
    if cut.is_empty() {
        String::new()
    } else {
        format!(": {cut}")
    }
}

/// Moonraker wraps nearly every success in `{ "result": ... }`; anything else is empty.
fn envelope(body: &[u8]) -> Result<Value> {
    let body: Value = serde_json::from_slice(body)?;
    Ok(match body.get("result") {
        Some(result) if result.is_object() => result.clone(),
        _ => Value::Null,
    })
}

/// `String.prototype.trim`: Unicode White_Space without NEL, plus the BOM.
pub(crate) fn js_trim(value: &str) -> &str {
    value.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

fn text<'a>(row: &'a Map<String, Value>, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).map_or("", js_trim)
}

fn list_camera(base: &str, index: usize, camera: &Map<String, Value>) -> Option<Camera> {
    if camera.get("enabled") == Some(&Value::Bool(false)) {
        return None;
    }
    let (view, _url) = camera_view(base, camera)?;
    let nonempty = |value: &str| (!value.is_empty()).then(|| value.to_owned());
    Some(Camera {
        id: list_camera_id(index, camera),
        name: nonempty(text(camera, "name")).unwrap_or_else(|| format!("Camera {}", index + 1)),
        view,
        service: nonempty(text(camera, "service")),
        aspect_ratio: nonempty(text(camera, "aspect_ratio")),
    })
}

/// Node's `cameraId`: the trimmed `uid`, else `camera-{index}`.
fn list_camera_id(index: usize, camera: &Map<String, Value>) -> String {
    match text(camera, "uid") {
        "" => format!("camera-{index}"),
        uid => uid.to_owned(),
    }
}

/// Pick a browser-renderable view, or None. Moonraker replays whatever a camera
/// service wrote into its database, so a cross-origin value in either URL
/// discredits the whole entry instead of falling back to its sibling.
fn camera_view(base: &str, camera: &Map<String, Value>) -> Option<(CameraView, String)> {
    let stream_url = text(camera, "stream_url");
    let stream = same_origin(base, stream_url)?;
    let snapshot = same_origin(base, text(camera, "snapshot_url"))?;
    let service = text(camera, "service").to_lowercase();
    // MJPEG is the only continuous format proxied; `action=stream` is mjpg-streamer's.
    let mjpeg = service.contains("mjpeg")
        || service.contains("ustreamer")
        || stream_url.to_ascii_lowercase().contains("action=stream");
    match (stream, snapshot) {
        (Some(url), _) if mjpeg => Some((CameraView::Mjpeg, url)),
        (_, Some(url)) => Some((CameraView::Snapshot, url)),
        _ => None,
    }
}

/// `Some(None)` when nothing was advertised, `None` when the value does not
/// resolve onto the Moonraker origin.
fn same_origin(base: &str, advertised: &str) -> Option<Option<String>> {
    if advertised.is_empty() {
        return Some(None);
    }
    let origin = Url::parse(base).ok()?.origin();
    let resolved = Url::parse(&format!("{base}/"))
        .ok()?
        .join(advertised)
        .ok()?;
    (resolved.origin() == origin).then(|| Some(resolved.into()))
}

/// Host-reported paths are untrusted: reject backslashes, NULs and empty,
/// `.` or `..` segments. Leading and trailing slashes are trimmed.
pub(crate) fn safe_storage_path(raw: &str) -> Option<String> {
    if raw.contains(['\\', '\0']) {
        return None;
    }
    let normalized = raw.trim_matches('/');
    let bad = |segment: &str| segment.is_empty() || segment == "." || segment == "..";
    (!normalized.is_empty() && !normalized.split('/').any(bad)).then(|| normalized.to_owned())
}

/// A directory row names one segment; a separator in it is a provider surprise.
fn entry_name(raw: Option<&Value>) -> Option<String> {
    safe_storage_path(raw?.as_str()?).filter(|name| !name.contains('/'))
}

pub(crate) fn encode_uri_component(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Moonraker reports `modified: 0` when it has no timestamp; that stays absent.
/// Matches `new Date(seconds * 1000).toISOString()`: years past 9999 print as
/// `+YYYYYY`, and a time past the ECMAScript limit fails the whole listing.
fn modified_at(raw: Option<&Value>) -> Result<Option<String>> {
    let Some(seconds) = raw
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v > 0.0)
    else {
        return Ok(None);
    };
    iso_time(seconds * 1000.0).map(Some)
}

/// `new Date(millis).toISOString()`, including its "Invalid time value" error.
pub(crate) fn iso_time(millis: f64) -> Result<String> {
    let millis = millis.trunc();
    ensure!(millis.abs() <= 8.64e15, "Invalid time value");
    let millis = millis as i64;
    let (days, rest) = (millis.div_euclid(86_400_000), millis.rem_euclid(86_400_000));
    // Howard Hinnant's civil_from_days, for days since 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let year = match year {
        0..=9999 => format!("{year:04}"),
        _ if year < 0 => format!("-{:06}", -year),
        _ => format!("+{year:06}"),
    };
    Ok(format!(
        "{year}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_t_21_iso_timestamps_match_to_iso_string() {
        for (seconds, expected) in [
            (1e-3, Some("1970-01-01T00:00:00.001Z")),
            (951_782_400.0, Some("2000-02-29T00:00:00.000Z")),
            (1_727_784_000.123_9, Some("2024-10-01T12:00:00.123Z")),
            (253_402_300_799.999, Some("9999-12-31T23:59:59.999Z")),
            (253_402_300_800.0, Some("+010000-01-01T00:00:00.000Z")),
            (8.64e12, Some("+275760-09-13T00:00:00.000Z")),
            (0.0, None),
            (-1.0, None),
        ] {
            assert_eq!(
                modified_at(Some(&serde_json::json!(seconds)))
                    .unwrap()
                    .as_deref(),
                expected,
                "{seconds}"
            );
        }
        assert_eq!(modified_at(Some(&serde_json::json!("1"))).unwrap(), None);
        let error = modified_at(Some(&serde_json::json!(8.64e12 + 0.001))).unwrap_err();
        assert_eq!(error.to_string(), "Invalid time value");
    }

    #[test]
    fn ticket_t_21_error_text_cuts_at_200_utf16_units() {
        let a = "a".repeat(199);
        assert_eq!(error_text(format!("{a}bc").as_bytes()), format!(": {a}b"));
        assert_eq!(
            error_text(format!("{a}😀z").as_bytes()),
            format!(": {a}\u{fffd}")
        );
        assert_eq!(error_text(format!("{a}b😀").as_bytes()), format!(": {a}b"));
        assert_eq!(error_text("\u{feff}".as_bytes()), "");
        assert_eq!(error_text(&vec![b'x'; MAX_ERROR_TEXT_BYTES + 1]), "");
    }

    #[test]
    fn ticket_t_21_path_rules_match_storage_path_guard() {
        assert_eq!(safe_storage_path("/a/b/").as_deref(), Some("a/b"));
        for bad in ["a//b", "a/./b", "../a", "a\\b", "a\0b", "/", ""] {
            assert_eq!(safe_storage_path(bad), None, "{bad:?}");
        }
        assert_eq!(
            encode_uri_component("Plate & Lid #2 ü!~*'()"),
            "Plate%20%26%20Lid%20%232%20%C3%BC!~*'()"
        );
        assert_eq!(js_trim("\u{feff} x \u{85}"), "x \u{85}");
    }
}
