//! PrusaLink driver (T-22), ported from the Node adapter: HTTP Digest auth,
//! connection test, status with the `/api/v1/job` fallback, filament used,
//! upload, storage browse, the camera list, `object_list`, and streamed print
//! file downloads and camera snapshots (GRE-1044).
//!
//! Every request first sends a bodyless GET `/api/v1/status` for a fresh
//! Digest challenge, never a write. A GET answered 401 is retried once with
//! that reply's challenge, as in Node. Unlike Node, a PUT answered 401 is not
//! resent: the upload body goes out at most once.
//! Until GRE-359, HTTPS is refused and redirects are not followed. Buffered
//! replies are bounded by the guard's 2 MiB limit, except directory listings,
//! which allow 8 MiB as in Node. Downloads and snapshots stream; `object_list`
//! reads only the first 64 KiB of a print file whose server ignores `Range`.
use crate::digest;
use crate::moonraker_driver::{
    Camera, CameraView, StorageEntry, StorageListing, TestResult, UploadResult, UploadSource,
    encode_uri_component, error_text, iso_time, js_trim, safe_storage_path,
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use pp_api_types::printer_status::{PrinterHostState as State, PrinterHostStatus as Status};
use pp_net::outbound::{
    BodyDeadline, HttpGuard, HttpReply, MAX_BODY, MAX_LISTING_BODY, PostBody, PostError,
    StreamReply,
};
use serde_json::{Map, Value};
use std::{io::Cursor, pin::Pin, time::Duration};
use tokio::io::AsyncRead;
use url::Url;

const PROBE: &str = "/api/v1/status";
const READ_DEADLINE: Duration = Duration::from_secs(8);
const UPLOAD_DEADLINE: Duration = Duration::from_secs(120);
const STORAGE_DEADLINE: Duration = Duration::from_secs(10);
const LISTING_DEADLINE: Duration = Duration::from_secs(15);
const METADATA_DEADLINE: Duration = Duration::from_secs(15);
/// Node aborts a print-file download after 120 s without data.
pub const DOWNLOAD_IDLE: Duration = Duration::from_secs(120);
/// Node allows a camera snapshot 30 s.
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(30);
const ACCEPT_JSON: (&str, &str) = ("accept", "application/json");
const INVALID_PATH: &str = "Invalid PrusaLink storage path";
/// The print-file prefix `object_list` reads for `objects_info=`.
const OBJECTS_PREFIX: usize = 65_536;

pub struct Driver {
    guard: HttpGuard,
    /// The same guard with Node's 8 MiB directory listing limit.
    listings: HttpGuard,
    base_url: String,
    username: String,
    password: Option<String>,
    storage: Option<String>,
}

impl Driver {
    /// `storage` is the operator-pinned storage root; uploads default to `usb`.
    pub fn new(
        guard: HttpGuard,
        base_url: &str,
        username: &str,
        password: Option<&str>,
        storage: Option<&str>,
    ) -> Self {
        Self {
            listings: guard
                .with_body_limit(MAX_LISTING_BODY)
                .unwrap_or_else(|_| guard.clone()),
            guard,
            base_url: base_url.into(),
            username: js_trim(username).into(),
            password: password
                .map(js_trim)
                .filter(|p| !p.is_empty() && *p != "****")
                .map(Into::into),
            storage: storage.and_then(safe_storage_path),
        }
    }

    fn base(&self) -> Result<&str> {
        let base = self.base_url.trim().trim_end_matches('/');
        ensure!(!base.is_empty(), "base_url is required");
        ensure!(!base.contains(['?', '#']), "Invalid base_url");
        Ok(base)
    }

    fn password(&self) -> Result<&str> {
        self.password
            .as_deref()
            .ok_or_else(|| anyhow!("username and password are required"))
    }

    fn url(&self, path: &str) -> Result<Url> {
        checked(&format!("{}{path}", self.base()?))
    }

    /// The challenge from a bodyless GET of `/api/v1/status` on the same origin.
    async fn challenge(&self, url: &Url) -> Result<Option<Challenge>, PostError> {
        let probe = format!("{}{PROBE}", url.origin().ascii_serialization());
        let reply = self
            .guard
            .request("GET", &probe, &[], None, READ_DEADLINE)
            .await?;
        Ok(Challenge::from_reply(&reply))
    }

    fn authorization(&self, method: &str, url: &Url, challenge: &Challenge) -> Result<String> {
        let uri = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        };
        let request = digest::Request {
            username: &self.username,
            password: self.password()?,
            method,
            uri: &uri,
            nc: 1,
            cnonce: &digest::cnonce(),
        };
        digest::authorization(&request, &challenge.params)
    }

    async fn send(
        &self,
        method: &str,
        url: &Url,
        headers: &[(&str, &str)],
        body: Option<PostBody>,
        deadline: Duration,
    ) -> Result<HttpReply, PostError> {
        self.send_via(&self.guard, method, url, headers, body, deadline)
            .await
    }

    /// One Digest-authorized request. Only a GET is retried after a 401.
    /// Without a password the request goes out once, unauthenticated, as in Node.
    async fn send_via(
        &self,
        guard: &HttpGuard,
        method: &str,
        url: &Url,
        headers: &[(&str, &str)],
        body: Option<PostBody>,
        deadline: Duration,
    ) -> Result<HttpReply, PostError> {
        let unsent = PostError::Unsent;
        if self.password.is_none() {
            return guard
                .request(method, url.as_str(), headers, body, deadline)
                .await;
        }
        let challenge = self.challenge(url).await?;
        let authorization = challenge
            .as_ref()
            .map(|c| self.authorization(method, url, c))
            .transpose()
            .map_err(unsent)?;
        let mut all = headers.to_vec();
        if let Some(value) = &authorization {
            all.push(("authorization", value));
        }
        let reply = guard
            .request(method, url.as_str(), &all, body, deadline)
            .await?;
        if reply.status != 401 || method != "GET" {
            return Ok(reply);
        }
        let Some(retry) = Challenge::from_reply(&reply) else {
            return Ok(reply);
        };
        let value = self.authorization(method, url, &retry).map_err(unsent)?;
        let mut all = headers.to_vec();
        all.push(("authorization", &value));
        guard
            .request(method, url.as_str(), &all, None, deadline)
            .await
    }

    async fn get(&self, path: &str) -> Result<HttpReply> {
        let url = self.url(path)?;
        self.password()?;
        self.fetch(&url, &[], READ_DEADLINE).await
    }

    async fn fetch(
        &self,
        url: &Url,
        headers: &[(&str, &str)],
        deadline: Duration,
    ) -> Result<HttpReply> {
        self.send("GET", url, headers, None, deadline)
            .await
            .map_err(|error| anyhow!("{error}"))
    }

    /// One Digest-authorized GET whose reply body streams: the head within
    /// `deadline`, then the body under `body`. A 401 with a fresh challenge
    /// is retried once, as `send` does for buffered GETs. As with Node's one
    /// `AbortSignal`, `deadline` starts before the challenge probe and covers
    /// the probe, the retry and, under `BodyDeadline::Total`, the body.
    async fn stream(
        &self,
        url: &Url,
        headers: &[(&str, &str)],
        deadline: Duration,
        body: BodyDeadline,
    ) -> Result<StreamReply> {
        if self.password.is_none() {
            return self
                .guard
                .stream(url.as_str(), headers, deadline, body)
                .await;
        }
        let until = tokio::time::Instant::now() + deadline;
        let left = || {
            let left = until.saturating_duration_since(tokio::time::Instant::now());
            ensure!(!left.is_zero(), "HTTP deadline");
            Ok(left)
        };
        let challenge = tokio::time::timeout_at(until, self.challenge(url))
            .await
            .map_err(|_| anyhow!("HTTP deadline"))?
            .map_err(|error| anyhow!("{error}"))?;
        let mut all = headers.to_vec();
        let authorization = challenge
            .as_ref()
            .map(|c| self.authorization("GET", url, c))
            .transpose()?;
        if let Some(value) = &authorization {
            all.push(("authorization", value));
        }
        let reply = self.guard.stream(url.as_str(), &all, left()?, body).await?;
        let Some(retry) = Challenge::from_parts(reply.status, reply.www_authenticate.as_deref())
        else {
            return Ok(reply);
        };
        drop(reply);
        let value = self.authorization("GET", url, &retry)?;
        let mut all = headers.to_vec();
        all.push(("authorization", &value));
        self.guard.stream(url.as_str(), &all, left()?, body).await
    }

    /// Open one print file for download through the same-origin
    /// `refs.download` URL its metadata advertises, as Node's `files.open`.
    /// A metadata reply that is not 2xx comes back as is, body unread; so does
    /// the download reply. The download fails after `DOWNLOAD_IDLE` without data.
    pub async fn open_file(&self, path: &str) -> Result<StreamReply> {
        let base = self.base()?.to_owned();
        let relative = safe_storage_path(path).context("Invalid PrusaLink print-file path")?;
        let root = self.storage_root().await?;
        let encoded: Vec<_> = format!("{root}/{relative}")
            .split('/')
            .map(encode_uri_component)
            .collect();
        let url = self.url(&format!("/api/v1/files/{}", encoded.join("/")))?;
        let metadata = self
            .stream(&url, &[ACCEPT_JSON], METADATA_DEADLINE, BodyDeadline::Total)
            .await?;
        if !(200..300).contains(&metadata.status) {
            return Ok(metadata);
        }
        let body = metadata.body.prefix(MAX_BODY + 1).await?;
        ensure!(body.len() <= MAX_BODY, "HTTP body unavailable or oversized");
        let download = json(&body)?
            .get("refs")
            .and_then(Value::as_object)
            .and_then(|refs| refs.get("download"))
            .and_then(Value::as_str)
            .map(js_trim)
            .unwrap_or_default()
            .to_owned();
        ensure!(
            !download.is_empty(),
            "PrusaLink did not advertise a download URL"
        );
        let target = checked(&format!("{base}/"))?
            .join(&download)
            .map_err(|_| anyhow!("Invalid URL"))?;
        ensure!(
            target.origin() == checked(&base)?.origin(),
            "PrusaLink advertised a cross-origin download URL"
        );
        self.stream(
            &checked(target.as_str())?,
            &[],
            DOWNLOAD_IDLE,
            BodyDeadline::Idle(DOWNLOAD_IDLE),
        )
        .await
    }

    /// Open one connected camera's `/snap` image, as Node's `cameras.open`.
    /// The id must equal a listed `camera_id` exactly.
    /// The reply comes back as is, body unread.
    pub async fn open_camera(&self, camera_id: &str) -> Result<StreamReply> {
        self.base()?;
        // Node matches the stored id exactly, untrimmed.
        let listed = self.camera_rows().await?.iter().any(|camera| {
            camera.get("camera_id").and_then(Value::as_str) == Some(camera_id)
                && camera.get("connected") != Some(&Value::Bool(false))
        });
        ensure!(listed, "Camera not found");
        let url = self.url(&format!(
            "/api/v1/cameras/{}/snap",
            encode_uri_component(camera_id)
        ))?;
        self.stream(&url, &[], SNAPSHOT_DEADLINE, BodyDeadline::Total)
            .await
    }

    /// The pinned storage root, else the first storage the printer reports as
    /// available. Node does not assume `usb` here.
    async fn storage_root(&self) -> Result<String> {
        if let Some(root) = &self.storage {
            return Ok(root.clone());
        }
        let url = self.url("/api/v1/storage")?;
        let reply = self.fetch(&url, &[ACCEPT_JSON], STORAGE_DEADLINE).await?;
        ensure!(
            ok(&reply),
            "PrusaLink storage list returned HTTP {}",
            reply.status
        );
        let body = json(&reply.body)?;
        let storages = body.get("storage_list").and_then(Value::as_array);
        for storage in storages.into_iter().flatten().filter_map(Value::as_object) {
            if storage.get("available") == Some(&Value::Bool(false)) {
                continue;
            }
            if let Some(root) = storage
                .get("path")
                .and_then(Value::as_str)
                .and_then(safe_storage_path)
            {
                return Ok(root);
            }
        }
        bail!("PrusaLink reported no available storage")
    }

    /// One directory, relative to the storage root. Children whose name is not
    /// one safe segment, and entries that are neither folders nor print files,
    /// are left out.
    pub async fn browse(&self, path: &str) -> Result<StorageListing> {
        self.base()?;
        self.password()?;
        let relative = if path.is_empty() {
            String::new()
        } else {
            safe_storage_path(path).context(INVALID_PATH)?
        };
        let root = self.storage_root().await?;
        let provider = if relative.is_empty() {
            root
        } else {
            format!("{root}/{relative}")
        };
        let suffix = if provider.contains('/') { "" } else { "/" };
        let encoded: Vec<_> = provider.split('/').map(encode_uri_component).collect();
        let url = self.url(&format!("/api/v1/files/{}{suffix}", encoded.join("/")))?;
        let reply = self
            .send_via(
                &self.listings,
                "GET",
                &url,
                &[ACCEPT_JSON],
                None,
                LISTING_DEADLINE,
            )
            .await
            .map_err(|error| anyhow!("{error}"))?;
        ensure!(
            ok(&reply),
            "PrusaLink file listing returned HTTP {}",
            reply.status
        );
        let folder = json(&reply.body)?;
        let children = folder.get("children").and_then(Value::as_array);
        let mut entries = Vec::new();
        for child in children.into_iter().flatten().filter_map(Value::as_object) {
            let Some(name) = child
                .get("name")
                .and_then(Value::as_str)
                .and_then(safe_storage_path)
                .filter(|name| !name.contains('/'))
            else {
                continue;
            };
            let path = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            let modified_at = match child.get("m_timestamp").and_then(Value::as_f64) {
                Some(seconds) if seconds.is_finite() => Some(iso_time(seconds * 1000.0)?),
                _ => None,
            };
            let shown = match display_name(child) {
                "" => name.clone(),
                shown => shown.to_owned(),
            };
            match child.get("type").and_then(Value::as_str) {
                Some("FOLDER") => entries.push(StorageEntry::Directory {
                    path,
                    name: shown,
                    modified_at,
                }),
                Some("PRINT_FILE") => entries.push(StorageEntry::File {
                    path,
                    name: shown,
                    // Absent stays absent: unknown must not read as zero.
                    size_bytes: child
                        .get("size")
                        .and_then(Value::as_f64)
                        .filter(|v| v.is_finite() && *v >= 0.0)
                        .map(|v| v.round() as u64),
                    modified_at,
                }),
                _ => {}
            }
        }
        Ok(StorageListing {
            path: relative,
            entries,
        })
    }

    /// The camera list's object rows as stored. A 403, 404 or 503 means none.
    async fn camera_rows(&self) -> Result<Vec<Map<String, Value>>> {
        let url = self.url("/api/v1/cameras")?;
        let reply = self.fetch(&url, &[ACCEPT_JSON], STORAGE_DEADLINE).await?;
        if !ok(&reply) {
            if matches!(reply.status, 403 | 404 | 503) {
                return Ok(Vec::new());
            }
            bail!("PrusaLink camera list returned HTTP {}", reply.status);
        }
        let body: Value = serde_json::from_slice(&reply.body)?;
        Ok(body
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .cloned()
            .collect())
    }

    /// Connected cameras as snapshot views.
    pub async fn cameras(&self) -> Result<Vec<Camera>> {
        let rows = self.camera_rows().await?;
        Ok(rows
            .iter()
            .enumerate()
            .filter_map(|(index, camera)| {
                let id = js_trim(camera.get("camera_id").and_then(Value::as_str)?);
                if id.is_empty() || camera.get("connected") == Some(&Value::Bool(false)) {
                    return None;
                }
                let name = camera
                    .get("config")
                    .and_then(|c| c.get("name"))
                    .and_then(Value::as_str)
                    .map(js_trim)
                    .filter(|n| !n.is_empty())
                    .map_or_else(|| format!("Camera {}", index + 1), str::to_owned);
                Some(Camera {
                    id: id.to_owned(),
                    name,
                    view: CameraView::Snapshot,
                    service: Some("prusalink".into()),
                    aspect_ratio: None,
                })
            })
            .collect())
    }

    /// Object names from the active print file's `objects_info=` block. Any
    /// failure, or a job that is not printing, paused or finished, gives none.
    pub async fn object_list(&self) -> Vec<String> {
        self.objects().await.unwrap_or_default()
    }

    async fn objects(&self) -> Result<Vec<String>> {
        let base = self.base()?;
        let reply = self
            .fetch(&self.url("/api/v1/job")?, &[], READ_DEADLINE)
            .await?;
        if !ok(&reply) {
            return Ok(Vec::new());
        }
        let job = json(&reply.body)?;
        let state = job.get("state").and_then(Value::as_str).unwrap_or("");
        if !matches!(
            state.to_uppercase().as_str(),
            "PRINTING" | "PAUSED" | "FINISHED"
        ) {
            return Ok(Vec::new());
        }
        let download = job
            .get("refs")
            .and_then(Value::as_object)
            .and_then(|refs| refs.get("download"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if download.is_empty() {
            return Ok(Vec::new());
        }
        let url = if download.starts_with("http") {
            download.to_owned()
        } else {
            let slash = if download.starts_with('/') { "" } else { "/" };
            format!("{base}{slash}{download}")
        };
        let range = [("range", "bytes=0-65535")];
        let reply = self
            .stream(
                &checked(&url)?,
                &range,
                LISTING_DEADLINE,
                BodyDeadline::Total,
            )
            .await?;
        if !(200..300).contains(&reply.status) {
            return Ok(Vec::new());
        }
        // Some PrusaLink versions ignore Range, so cap the consumed prefix too.
        let prefix = reply.body.prefix(OBJECTS_PREFIX).await?;
        Ok(objects_info(&windows_1252(&prefix)).unwrap_or_default())
    }

    pub async fn test_connection(&self) -> TestResult {
        let result = async {
            self.base()?;
            self.password()?;
            let info = self.get("/api/v1/info").await?;
            if ok(&info) {
                let info = json(&info.body)?;
                let label = ["name", "hostname", "printer_model"]
                    .iter()
                    .find_map(|key| {
                        info.get(*key)
                            .and_then(Value::as_str)
                            .filter(|v| !v.is_empty())
                    })
                    .unwrap_or("PrusaLink");
                let state = self.read_status().await.ok().map(|s| state_name(&s.state));
                let state = state.map_or(String::new(), |s| format!(", state: {s}"));
                return Ok(format!("Connected ({label}{state})"));
            }
            // Older builds have no `/api/v1/info`.
            let status = self.read_status().await?;
            if status.state == State::Offline {
                let fallback = format!("PrusaLink returned HTTP {}", info.status);
                bail!("{}", status.message.unwrap_or(fallback));
            }
            let message = status
                .message
                .unwrap_or_else(|| state_name(&status.state).into());
            Ok(format!("Connected ({message})"))
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

    pub async fn status(&self) -> Status {
        self.read_status()
            .await
            .unwrap_or_else(|error| Status::offline(&error.to_string()))
    }

    async fn read_status(&self) -> Result<Status> {
        if let Err(error) = self.base().and_then(|_| self.password()) {
            return Ok(Status::offline(&error.to_string()));
        }
        let reply = self.get(PROBE).await?;
        if reply.status == 204 {
            let mut idle = Status::offline("Idle");
            idle.state = State::Idle;
            return Ok(idle);
        }
        if !ok(&reply) {
            return Ok(Status::offline(&format!(
                "PrusaLink returned HTTP {}",
                reply.status
            )));
        }
        let body = json(&reply.body)?;
        let printer = body.get("printer").and_then(Value::as_object);
        let job = body.get("job").and_then(Value::as_object);
        let raw_state = printer.and_then(|p| {
            p.get("state")
                .and_then(Value::as_str)
                .or_else(|| p.get("status").and_then(Value::as_str))
        });
        let state = map_state(raw_state.unwrap_or(""));
        let mut progress = job.and_then(|j| percent(j.get("progress")));
        let mut filename = job.and_then(|j| file_name(j.get("file")));
        // Node tests these with truthiness, so an empty name counts as none.
        let named =
            |name: &Option<String>| name.as_deref().filter(|n| !n.is_empty()).map(str::to_owned);
        let mut eta = job.and_then(|j| seconds(j.get("time_remaining")));
        let active = matches!(state, State::Printing | State::Paused);
        if active && let Some(meta) = self.job_meta().await {
            filename = named(&meta.filename).or(filename);
            progress = progress.or(meta.progress);
            eta = eta.or(meta.eta);
        }
        let message = match (&state, &named(&filename)) {
            (State::Printing, Some(name)) => Some(format!("Printing {name}")),
            (State::Complete, Some(name)) => Some(format!("Complete · {name}")),
            (State::Complete, None) => Some("Complete".into()),
            (State::Idle, _) => Some("Idle".into()),
            _ => raw_state.map(str::to_owned),
        };
        let number = |key: &str| printer.and_then(|p| p.get(key)).and_then(Value::as_f64);
        Ok(Status {
            progress: if active { progress } else { None },
            filename,
            message,
            eta_seconds: eta,
            ip_address: Url::parse(self.base()?)?.host_str().map(str::to_owned),
            uptime_seconds: None,
            nozzle_temperature_c: number("temp_nozzle"),
            nozzle_target_c: number("target_nozzle"),
            bed_temperature_c: number("temp_bed"),
            bed_target_c: number("target_bed"),
            state,
        })
    }

    async fn job_meta(&self) -> Option<JobMeta> {
        let reply = self.get("/api/v1/job").await.ok()?;
        ok(&reply).then_some(())?;
        let body = json(&reply.body).ok()?;
        Some(JobMeta {
            filename: file_name(body.get("file")),
            progress: percent(body.get("progress")),
            eta: seconds(body.get("time_remaining")),
        })
    }

    pub async fn filament_used(&self) -> Option<f64> {
        let reply = self.get("/api/v1/job").await.ok()?;
        ok(&reply).then_some(())?;
        json(&reply.body)
            .ok()?
            .get("consumed_material")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v >= 0.0)
    }

    /// The file name and `consumed_material` (millimetres) of the job
    /// `/api/v1/job` reports, from one read; `None` unless it is `FINISHED`.
    pub async fn finished_filament(&self) -> Option<(String, f64)> {
        let reply = self.get("/api/v1/job").await.ok()?;
        ok(&reply).then_some(())?;
        let job = json(&reply.body).ok()?;
        let state = job.get("state")?.as_str()?;
        state.eq_ignore_ascii_case("FINISHED").then_some(())?;
        let used = job.get("consumed_material")?.as_f64()?;
        Some((file_name(job.get("file"))?, used))
    }

    /// PUT the file into the pinned storage (default `usb`). The body is sent
    /// once; a 401 reply is reported, never answered with a resend.
    pub async fn upload_file(
        &self,
        source: UploadSource,
        filename: &str,
        start: bool,
    ) -> UploadResult {
        let replaced = filename.replace(['/', '\\'], "_");
        let name = match js_trim(&replaced) {
            "" => "print.gcode",
            dots if dots.chars().all(|c| c == '.') => "print.gcode",
            name => name,
        }
        .to_owned();
        let root = self.storage.as_deref().unwrap_or("usb");
        let remote_path = format!("{root}/{name}");
        let encoded: Vec<_> = remote_path.split('/').map(encode_uri_component).collect();
        let path = format!("/api/v1/files/{}", encoded.join("/"));
        let checked = self.url(&path).and_then(|url| self.password().map(|_| url));
        let url = match checked {
            Ok(url) => url,
            Err(error) => return UploadResult::failed(error.to_string(), false),
        };
        let body = match octet_stream(source).await {
            Ok(body) => body,
            Err(error) => return UploadResult::failed(error.to_string(), false),
        };
        let headers = [
            ("print-after-upload", if start { "?1" } else { "?0" }),
            ("overwrite", "?1"),
        ];
        let reply = match self
            .send("PUT", &url, &headers, Some(body), UPLOAD_DEADLINE)
            .await
        {
            Ok(reply) => reply,
            Err(PostError::Unsent(error) | PostError::SourceFailed(error)) => {
                return UploadResult::failed(format!("{error:#}"), false);
            }
            Err(PostError::Unknown(error)) => {
                let message = format!("Upload outcome unknown: {error:#}");
                return UploadResult::failed(message, true);
            }
        };
        if reply.status == 401 {
            let reason = match Challenge::from_reply(&reply) {
                Some(c) if c.stale() => "the Digest nonce went stale; send again",
                _ => "PrusaLink rejected the username or password",
            };
            return UploadResult::failed(format!("Upload failed (HTTP 401): {reason}"), false);
        }
        if !ok(&reply) {
            let message = format!(
                "Upload failed (HTTP {}){}",
                reply.status,
                error_text(&reply.body)
            );
            return UploadResult::failed(message, false);
        }
        UploadResult {
            ok: true,
            remote_path: Some(remote_path),
            started: Some(start),
            message: if start {
                format!("Uploaded and started {name}")
            } else {
                format!("Uploaded {name}")
            },
            outcome_unknown: false,
        }
    }
}

/// `url` if it is HTTP; HTTPS waits for GRE-359.
fn checked(url: &str) -> Result<Url> {
    let url = Url::parse(url).map_err(|_| anyhow!("Invalid URL"))?;
    match url.scheme() {
        "http" => Ok(url),
        "https" => bail!("HTTPS PrusaLink hosts are not supported until GRE-359"),
        other => bail!("Unsupported URL protocol: {other}:"),
    }
}

/// A trimmed `display_name`, else the trimmed `name`, else empty.
fn display_name(row: &Map<String, Value>) -> &str {
    match row.get("display_name").and_then(Value::as_str).map(js_trim) {
        Some(shown) if !shown.is_empty() => shown,
        _ => row.get("name").and_then(Value::as_str).map_or("", js_trim),
    }
}

/// What Node's `TextDecoder("latin1")` yields: WHATWG maps that label to
/// windows-1252, which differs from Latin-1 in 0x80 to 0x9F.
fn windows_1252(bytes: &[u8]) -> String {
    const HIGH: [u32; 32] = [
        0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160,
        0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013,
        0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
    ];
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => char::from_u32(HIGH[usize::from(b - 0x80)]).unwrap_or('\u{fffd}'),
            _ => char::from(b),
        })
        .collect()
}

/// Names in the `objects_info={...}` JSON, found by counting braces as Node does.
fn objects_info(text: &str) -> Option<Vec<String>> {
    const MARKER: &str = "objects_info=";
    let start = text.find(MARKER)? + MARKER.len();
    let rest = &text[start..];
    rest.starts_with('{').then_some(())?;
    let mut depth = 0usize;
    let mut end = None;
    for (i, c) in rest.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let parsed: Value = serde_json::from_str(&rest[..=end?]).ok()?;
    let objects = parsed.as_object()?.get("objects")?.as_array()?;
    Some(
        objects
            .iter()
            .filter_map(|o| o.as_object()?.get("name")?.as_str())
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .collect(),
    )
}

struct Challenge {
    params: std::collections::BTreeMap<String, String>,
}

impl Challenge {
    /// A Digest challenge with a nonce from a 401 reply.
    fn from_reply(reply: &HttpReply) -> Option<Self> {
        Self::from_parts(reply.status, reply.www_authenticate.as_deref())
    }

    fn from_parts(status: u16, www_authenticate: Option<&str>) -> Option<Self> {
        if status != 401 {
            return None;
        }
        let params = digest::parse_www_authenticate(www_authenticate?);
        params
            .get("nonce")
            .is_some_and(|n| !n.is_empty())
            .then_some(Self { params })
    }

    fn stale(&self) -> bool {
        self.params
            .get("stale")
            .is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }
}

struct JobMeta {
    filename: Option<String>,
    progress: Option<f64>,
    eta: Option<f64>,
}

fn ok(reply: &HttpReply) -> bool {
    (200..300).contains(&reply.status)
}

fn json(body: &[u8]) -> Result<Map<String, Value>> {
    let value: Value = serde_json::from_slice(body)?;
    Ok(match value {
        Value::Object(map) => map,
        _ => Map::new(),
    })
}

fn map_state(raw: &str) -> State {
    match raw.to_uppercase().as_str() {
        "PRINTING" => State::Printing,
        "PAUSED" => State::Paused,
        "FINISHED" => State::Complete,
        // Buddy firmware reports READY between jobs; a cancel must not read as complete.
        "READY" | "IDLE" | "STOPPED" => State::Idle,
        "ATTENTION" | "ERROR" => State::Error,
        _ => State::Unknown,
    }
}

fn state_name(state: &State) -> &'static str {
    match state {
        State::Idle => "idle",
        State::Printing => "printing",
        State::Paused => "paused",
        State::Complete => "complete",
        State::Error => "error",
        State::Offline => "offline",
        State::Unknown => "unknown",
    }
}

fn percent(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .map(|v| v.clamp(0.0, 100.0).round())
}

fn seconds(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite() && *v >= 0.0)
}

/// `display_name`, then `name`, then `path`.
fn file_name(file: Option<&Value>) -> Option<String> {
    let file = file?.as_object()?;
    ["display_name", "name", "path"]
        .iter()
        .find_map(|key| file.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

async fn octet_stream(source: UploadSource) -> Result<PostBody> {
    let (reader, length): (Pin<Box<dyn AsyncRead + Send>>, u64) = match source {
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
    Ok(PostBody {
        content_type: "application/octet-stream".into(),
        reader,
        length,
    })
}
