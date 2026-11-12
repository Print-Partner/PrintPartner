#![cfg(feature = "desktop_rebuild")]
//! `POST /sources/{id}/upload-files` over real storage (GRE-428 PR 1), with
//! synthetic STL files only.

use pp_api::{
    catalog::{CatalogHttpConfig, SourceUploads, admit_upload_routes},
    problem::without_envelope,
};
use pp_core::{
    DesktopRebuild,
    rebuild_api::{desktop_policy, product_router},
    source_acquisition::SourceAcquisitions,
    source_uploads::{IMPORT_QUOTA, SourceUploadService},
};
use pp_storage::{Limits, WriterOwner};
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const BOUNDARY: &str = "pp-upload-boundary";

struct Server {
    origin: String,
    client: Client,
    cookie: String,
    serving: Option<pp_api::serving::Serving>,
    owner: Arc<Mutex<WriterOwner>>,
    imports: Arc<SourceAcquisitions>,
    data: PathBuf,
    _root: tempfile::TempDir,
}

impl Server {
    /// Serves the product routes plus the upload route over a new data
    /// directory, with `slots` import slots and a free-space `reserve`.
    async fn start(slots: usize, reserve: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = std::fs::canonicalize(root.path()).unwrap().join("data");
        let owner = WriterOwner::open(&data, Limits::default()).unwrap().0;
        let imports = Arc::new(
            SourceAcquisitions::new(&owner, desktop_policy(), IMPORT_QUOTA, slots).unwrap(),
        );
        assert_eq!(imports.recover_before_accepting().unwrap(), 0);
        let owner = Arc::new(Mutex::new(owner));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let uploads: Arc<dyn SourceUploads> = Arc::new(SourceUploadService::new(
            Arc::clone(&imports),
            data.clone(),
            reserve,
        ));
        let config =
            CatalogHttpConfig::from_startup(&origin, "default", desktop_policy(), Some("1"));
        let uploads = admit_upload_routes(config.unwrap(), Arc::clone(&owner), uploads);
        let rebuild = DesktopRebuild::from_value(Some("1"));
        let product = product_router(rebuild, Arc::clone(&owner), &origin, None).unwrap();
        let serving = pp_api::serving::Serving::start(listener, uploads.merge(product.unwrap()));
        let mut server = Self {
            client: Client::builder().no_proxy().build().unwrap(),
            cookie: String::new(),
            serving: Some(serving),
            origin,
            owner,
            imports,
            data,
            _root: root,
        };
        // The first account signs in; its session cookie authorizes the rest.
        let account = json!({"email": "owner@example.test", "password": "password-1234"});
        let signed_in = server
            .request(Method::POST, "/auth/register")
            .json(&account);
        let signed_in = signed_in.send().await.unwrap();
        assert_eq!(signed_in.status(), 200);
        let set_cookie = signed_in.headers()["set-cookie"].to_str().unwrap();
        server.cookie = set_cookie.split(';').next().unwrap().to_owned();
        server
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{path}", self.origin);
        let request = self
            .client
            .request(method, url)
            .header("Origin", &self.origin);
        if self.cookie.is_empty() {
            return request;
        }
        request.header("Cookie", &self.cookie)
    }

    async fn json(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self.request(method, path);
        if let Some(body) = body {
            request = request.json(&body);
        }
        reply(request).await
    }

    async fn source(&self) -> i64 {
        let (status, created) = self
            .json(
                Method::POST,
                "/api/v1/sources",
                Some(json!({"name": "Upload Source", "source_kind": "local"})),
            )
            .await;
        assert!(matches!(status, 200 | 201), "{status} {created}");
        created["id"].as_i64().unwrap()
    }

    async fn upload(&self, source: i64, files: &[(&str, &[u8])]) -> (u16, Value) {
        self.post_body(&format!("/api/v1/sources/{source}"), multipart(files))
            .await
    }

    async fn post_body(&self, source: &str, body: Vec<u8>) -> (u16, Value) {
        let content_type = format!("multipart/form-data; boundary={BOUNDARY}");
        let request = self.request(Method::POST, &format!("{source}/upload-files"));
        reply(request.header("Content-Type", content_type).body(body)).await
    }

    /// Polls the job until it is terminal and returns its last snapshot,
    /// checking every status it passes through.
    async fn finished(&self, job: &str) -> Value {
        for _ in 0..600 {
            let (status, body) = self
                .json(Method::GET, &format!("/api/v1/jobs/{job}"), None)
                .await;
            assert_eq!(status, 200, "{body}");
            match body["status"].as_str().unwrap() {
                "pending" | "running" => {}
                _ => return body,
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("job {job} did not finish");
    }

    /// Uploads `files` and waits until their import is done.
    async fn imported(&self, source: i64, files: &[(&str, &[u8])]) {
        let (status, accepted) = self.upload(source, files).await;
        assert_eq!(status, 202, "{accepted}");
        let job = self.finished(accepted["job_id"].as_str().unwrap()).await;
        assert_eq!(job["status"], "done", "{job}");
    }

    /// The statuses `/ws/jobs/{job}` sends until the stream ends.
    async fn streamed_statuses(&self, job: &str) -> Vec<String> {
        let host = self.origin.trim_start_matches("http://");
        let mut socket = tokio::net::TcpStream::connect(host).await.unwrap();
        let request = format!(
            "GET /ws/jobs/{job} HTTP/1.1\r\nHost: {host}\r\nOrigin: {}\r\nConnection: Upgrade\r\n\
             Upgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\nCookie: {}\r\n\r\n",
            self.origin, self.cookie
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(socket.read_u8().await.unwrap());
        }
        assert!(head.starts_with(b"HTTP/1.1 101"));
        let mut statuses = Vec::new();
        loop {
            // Server frames are unmasked; job snapshots fit a 16-bit length.
            let mut frame = [0u8; 2];
            let read = socket.read_exact(&mut frame);
            if tokio::time::timeout(Duration::from_secs(20), read)
                .await
                .is_err()
            {
                panic!("stream stalled after {statuses:?}");
            }
            let (opcode, mut length) = (frame[0] & 0x0f, usize::from(frame[1] & 0x7f));
            if length == 126 {
                length = usize::from(socket.read_u16().await.unwrap());
            }
            let mut payload = vec![0; length];
            socket.read_exact(&mut payload).await.unwrap();
            if opcode != 1 {
                return statuses;
            }
            let snapshot: Value = serde_json::from_slice(&payload).unwrap();
            statuses.push(snapshot["status"].as_str().unwrap().to_owned());
        }
    }

    /// Sends an upload's headers and `head` of its `length` body bytes over a
    /// raw connection, and returns once the import has staged its candidate.
    async fn held_upload(&self, source: i64, length: usize, head: &[u8]) -> TcpStream {
        let host = self.origin.trim_start_matches("http://");
        let mut held = TcpStream::connect(host).await.unwrap();
        let headers = format!(
            "POST /sources/{source}/upload-files HTTP/1.1\r\nHost: {host}\r\nOrigin: {}\r\n\
             Cookie: {}\r\nContent-Type: multipart/form-data; boundary={BOUNDARY}\r\n\
             Content-Length: {length}\r\nConnection: close\r\n\r\n",
            self.origin, self.cookie,
        );
        held.write_all(headers.as_bytes()).await.unwrap();
        held.write_all(head).await.unwrap();
        for _ in 0..400 {
            if self.staged() > 0 {
                return held;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("the held upload staged nothing");
    }

    fn staged(&self) -> usize {
        std::fs::read_dir(self.data.join("source-captures")).map_or(0, |entries| entries.count())
    }

    async fn revision(&self, source: i64) -> Value {
        let (status, body) = self
            .json(Method::GET, &format!("/api/v1/sources/{source}"), None)
            .await;
        assert_eq!(status, 200, "{body}");
        body["current_source_revision_id"].clone()
    }

    async fn job_count(&self) -> usize {
        let (status, body) = self.json(Method::GET, "/api/v1/jobs", None).await;
        assert_eq!(status, 200, "{body}");
        body["jobs"].as_array().unwrap().len()
    }

    async fn stop(mut self) {
        self.serving.take().unwrap().stop().await.unwrap();
        self.imports.shutdown(Duration::from_secs(10)).unwrap();
        drop(self.imports);
        Arc::try_unwrap(self.owner)
            .unwrap_or_else(|_| panic!("router still holds storage"))
            .into_inner()
            .unwrap()
            .shutdown()
            .unwrap();
    }
}

async fn reply(request: reqwest::RequestBuilder) -> (u16, Value) {
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let body = response.json::<Value>().await;
    (status, body.map_or(Value::Null, without_envelope))
}

fn part(name: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = format!(
        "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    )
    .into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
    body
}

fn multipart(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = files
        .iter()
        .flat_map(|(name, bytes)| part(name, bytes))
        .collect::<Vec<_>>();
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

/// Every published revision file below `repos`, as `{revision}/{path}`.
fn revision_files(data: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![data.join("repos")];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if let Some(rest) = path
                .to_string_lossy()
                .split_once("/revisions/")
                .map(|(_, rest)| rest.to_owned())
                && rest.ends_with(".stl")
            {
                found.push(rest);
            }
        }
    }
    found.sort();
    found
}

fn stl(name: &str) -> Vec<u8> {
    format!("solid {name}\nendsolid {name}\n").into_bytes()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_files_publishes_and_reuses_revisions() {
    let server = Server::start(2, 0).await;
    let source = server.source().await;
    assert_eq!(server.revision(source).await, Value::Null);

    let (a, b) = (stl("a"), stl("b"));
    let (status, accepted) = server
        .upload(source, &[("a.stl", &a), ("parts/b.stl", &b)])
        .await;
    assert_eq!(status, 202, "{accepted}");
    let job = accepted["job_id"].as_str().unwrap().to_owned();
    let streamed = server.streamed_statuses(&job).await;
    assert_eq!(
        streamed.last().map(String::as_str),
        Some("done"),
        "{streamed:?}"
    );
    assert!(
        streamed
            .iter()
            .all(|status| matches!(status.as_str(), "pending" | "running" | "done")),
        "{streamed:?}"
    );
    let done = server.finished(&job).await;
    assert_eq!(
        (done["status"].as_str(), done["kind"].as_str()),
        (Some("done"), Some("supplied-source-import")),
        "{done}"
    );
    let first = server.revision(source).await;
    assert!(first.is_i64(), "{first}");
    let first_files = revision_files(&server.data);
    assert_eq!(first_files.len(), 2, "{first_files:?}");
    assert!(first_files.iter().any(|path| path.ends_with("/a.stl")));
    assert!(
        first_files
            .iter()
            .any(|path| path.ends_with("/parts/b.stl"))
    );

    // Different files make a new revision; the earlier one stays on disk.
    let c = stl("c");
    server.imported(source, &[("c.stl", &c)]).await;
    let second = server.revision(source).await;
    assert_ne!(second, first);
    let files = revision_files(&server.data);
    assert!(
        first_files.iter().all(|path| files.contains(path)),
        "{files:?}"
    );

    // The same files again keep the revision.
    server.imported(source, &[("c.stl", &c)]).await;
    assert_eq!(server.revision(source).await, second);
    assert_eq!(revision_files(&server.data), files);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_refusals_create_no_job() {
    let server = Server::start(2, 0).await;
    let source = server.source().await;
    let a = stl("a");

    let no_file = {
        let mut body = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nhello\r\n"
        )
        .into_bytes();
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        body
    };
    let refusals = [
        (9_999_999, multipart(&[("a.stl", &a)]), 404),
        (source, no_file, 400),
        (source, multipart(&[("../a.stl", &a)]), 400),
        (source, multipart(&[("a.stl", &a), ("a.stl", &a)]), 400),
    ];
    for (target, body, expected) in refusals {
        // The root alias, answered with an RFC 9457 problem.
        let (status, problem) = server.post_body(&format!("/sources/{target}"), body).await;
        assert_eq!(status, expected, "{problem}");
        assert!(problem["detail"].is_string(), "{problem}");
    }
    assert_eq!(server.job_count().await, 0);
    assert_eq!(server.revision(source).await, Value::Null);
    assert!(revision_files(&server.data).is_empty());
    server.stop().await;

    // An upload that would leave less than the reserve free is refused
    // before any byte is staged.
    let server = Server::start(2, u64::MAX / 2).await;
    let source = server.source().await;
    let (status, problem) = server.upload(source, &[("a.stl", &a)]).await;
    assert_eq!(status, 507, "{problem}");
    assert_eq!(server.job_count().await, 0);
    assert_eq!(server.staged(), 0);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_while_slots_are_full_is_conflict() {
    let server = Server::start(1, 0).await;
    let source = server.source().await;
    let a = stl("a");
    let body = multipart(&[("a.stl", &a)]);
    let (head, tail) = body.split_at(body.len() / 2);
    // The held upload owns the only slot once its candidate exists.
    let mut held = server.held_upload(source, body.len(), head).await;
    let (status, problem) = server.upload(source, &[("b.stl", &stl("b"))]).await;
    assert_eq!(status, 409, "{problem}");
    assert_eq!(server.job_count().await, 0);

    held.write_all(tail).await.unwrap();
    let mut response = String::new();
    held.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 202"), "{response}");
    let accepted: Value =
        without_envelope(serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap()).unwrap());
    let done = server.finished(accepted["job_id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "done", "{done}");
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cut_off_upload_is_discarded() {
    let server = Server::start(1, 0).await;
    let source = server.source().await;
    let body = multipart(&[("a.stl", &stl("a"))]);
    // The client goes away halfway through the file.
    drop(
        server
            .held_upload(source, body.len(), &body[..body.len() / 2])
            .await,
    );
    for _ in 0..200 {
        if server.staged() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(server.staged(), 0);
    assert_eq!(server.job_count().await, 0);
    assert_eq!(server.revision(source).await, Value::Null);
    // The slot is free again once the discarded upload returns it.
    let mut answer = (0, Value::Null);
    for _ in 0..200 {
        answer = server.upload(source, &[("a.stl", &stl("a"))]).await;
        if answer.0 != 409 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(answer.0, 202, "{}", answer.1);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_import_names_its_reason() {
    let server = Server::start(2, 0).await;
    let source = server.source().await;
    // A file type the engine does not take yet (GRE-428 PR 2 keeps it).
    let (status, accepted) = server
        .upload(
            source,
            &[("a.stl", &stl("a")), ("photo.png", b"not a model")],
        )
        .await;
    assert_eq!(status, 202, "{accepted}");
    let failed = server.finished(accepted["job_id"].as_str().unwrap()).await;
    assert_eq!(failed["status"], "error", "{failed}");
    assert_eq!(
        failed["error"], "A file has a type the import does not accept",
        "{failed}"
    );
    assert_eq!(server.revision(source).await, Value::Null);
    server.stop().await;
}
