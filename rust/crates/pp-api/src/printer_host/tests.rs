use super::*;
use pp_storage::checkoff_progress::print_file::CAPTURE_UNAVAILABLE;

/// Answers from fixed values and records every port call.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
    /// Snapshot-source reads so far; printer `moved` answers a new
    /// fingerprint on each.
    sources: Mutex<usize>,
}

fn answer(printer: &str) -> HostAnswer<Value> {
    match printer {
        "moon" | "moved" => HostAnswer::Ok(json!({"path": "", "entries": []})),
        "gone" => HostAnswer::NotFound,
        "manual" => HostAnswer::NotLinked,
        "off" => HostAnswer::Unavailable,
        "spool" => HostAnswer::Unsupported,
        "down" => HostAnswer::Failed("Moonraker directory listing returned HTTP 500".into()),
        _ => HostAnswer::Busy,
    }
}

/// The printer's answer with its success replaced by `ok`.
fn answer_with<T>(printer: &str, ok: impl FnOnce() -> T) -> HostAnswer<T> {
    match answer(printer) {
        HostAnswer::Ok(_) => HostAnswer::Ok(ok()),
        HostAnswer::NotFound => HostAnswer::NotFound,
        HostAnswer::NotLinked => HostAnswer::NotLinked,
        HostAnswer::Unavailable => HostAnswer::Unavailable,
        HostAnswer::Unsupported => HostAnswer::Unsupported,
        HostAnswer::Failed(detail) => HostAnswer::Failed(detail),
        HostAnswer::Busy => HostAnswer::Busy,
    }
}

/// A printer reply with `status` and a five-byte body.
fn reply(status: u16, content_type: &str) -> HostStream {
    HostStream {
        status,
        content_type: Some(content_type.into()),
        content_length: Some(5),
        body: Body::from("G28\n\n"),
    }
}

impl PrinterHostPort for Fake {
    fn capabilities(&self, printer: String) -> Answer<Capabilities> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("capabilities {printer}"));
        let answer = answer_with(&printer, || Capabilities {
            files: true,
            cameras: true,
            status: true,
        });
        Box::pin(async move { answer })
    }
    fn file(&self, printer: String, path: String) -> Answer<Option<HostFile>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("file {printer} {path}"));
        let stream = match path.as_str() {
            "plates/plate.gcode" => Some(reply(200, "application/octet-stream")),
            "plates/removed.gcode" => Some(reply(404, "application/json")),
            "plates/page.html" => Some(reply(200, "text/html")),
            "plates/broken.gcode" => Some(reply(500, "application/json")),
            _ => None,
        };
        let answer = answer_with(&printer, || {
            stream.map(|stream| HostFile {
                stream,
                name: path.rsplit('/').next().unwrap_or_default().to_owned(),
                identity: json!({"size_bytes": 5, "modified_at": "2026-11-07T00:00:00.000Z"}),
            })
        });
        Box::pin(async move { answer })
    }
    fn snapshot_source(&self, printer: String) -> Answer<Option<String>> {
        let mut reads = self.sources.lock().unwrap();
        *reads += 1;
        let source = match printer.as_str() {
            "moved" => format!("source-{reads}"),
            _ => "source".to_owned(),
        };
        Box::pin(async move { HostAnswer::Ok(Some(source)) })
    }
    fn build_exists(&self, profile_id: i64) -> Answer<bool> {
        Box::pin(async move { HostAnswer::Ok(profile_id == 1) })
    }
    fn camera_view(&self, printer: String, camera: String) -> Answer<Option<HostStream>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("camera_view {printer} {camera}"));
        let answer = answer_with(&printer, || match camera.as_str() {
            "0" => Some(reply(200, "image/jpeg")),
            "mjpeg" => Some(reply(200, "multipart/x-mixed-replace; boundary=frame")),
            "page" => Some(reply(200, "text/html")),
            "svg" => Some(reply(200, "image/svg+xml")),
            _ => None,
        });
        Box::pin(async move { answer })
    }
    fn files(&self, printer: String, path: String) -> Answer<Value> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("files {printer} {path}"));
        let answer = answer(&printer);
        Box::pin(async move { answer })
    }
    fn cameras(&self, printer: String) -> Answer<Value> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("cameras {printer}"));
        let answer = match answer(&printer) {
            HostAnswer::Ok(_) => HostAnswer::Ok(json!([{"id": "0", "name": "Bed"}])),
            other => other,
        };
        Box::pin(async move { answer })
    }
    fn test(&self, integration: String) -> Answer<Value> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("test {integration}"));
        let answer = match answer(&integration) {
            HostAnswer::Ok(_) => {
                HostAnswer::Ok(json!({"ok": true, "message": "Connected (klippy: ready)"}))
            }
            other => other,
        };
        Box::pin(async move { answer })
    }
}

fn app() -> (App, Arc<Fake>) {
    let fake = Arc::new(Fake::default());
    (
        App {
            port: fake.clone(),
            windows: Arc::default(),
            snapshots: Arc::default(),
        },
        fake,
    )
}

async fn body(response: Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn query(path: &str) -> Query<PathQuery> {
    Query(PathQuery {
        path: Some(path.into()),
    })
}

fn root() -> Query<PathQuery> {
    Query(PathQuery { path: None })
}

#[tokio::test]
async fn ticket_t_21_capabilities_map_like_node() {
    let (app, _) = app();
    let read = |id: &str| capabilities(State(app.clone()), Path(id.into()));
    assert_eq!(
        body(read("moon").await).await,
        (200, json!({"files": true, "cameras": true, "status": true}))
    );
    let none = json!({"files": false, "cameras": false, "status": false});
    for id in ["manual", "off", "spool"] {
        assert_eq!(body(read(id).await).await, (200, none.clone()), "{id}");
    }
    let (status, problem) = body(read("gone").await).await;
    assert_eq!(
        (status, &problem["detail"]),
        (404, &json!("Printer not found"))
    );
    assert_eq!(body(read("busy").await).await.0, 503);
}

#[tokio::test]
async fn ticket_t_21_files_validate_the_path_before_the_port() {
    let (app, fake) = app();
    let list = |id: &str, path: Query<PathQuery>| files(State(app.clone()), Path(id.into()), path);
    for bad in ["../etc", "a//b", "a/./b", "a\\b", "a\0b"] {
        let (status, problem) = body(list("moon", query(bad)).await).await;
        assert_eq!(status, 400, "{bad}");
        assert_eq!(
            problem["detail"],
            "path is not a valid printer storage path"
        );
    }
    assert!(
        fake.calls.lock().unwrap().is_empty(),
        "no invalid path reaches the port"
    );
    assert_eq!(body(list("moon", root()).await).await.0, 200);
    assert_eq!(
        body(list("moon", query("  /plates/a/  ")).await).await.0,
        200
    );
    assert_eq!(
        *fake.calls.lock().unwrap(),
        ["files moon ", "files moon plates/a"]
    );
    let expect = [
        ("gone", 404, "Printer not found"),
        ("manual", 409, "Printer is not linked to a host"),
        ("off", 503, "Printer host is unavailable"),
        ("spool", 501, "Printer host does not support file browsing"),
        ("down", 502, "Moonraker directory listing returned HTTP 500"),
    ];
    for (id, code, detail) in expect {
        let (status, problem) = body(list(id, root()).await).await;
        assert_eq!(
            (status, problem["detail"].as_str()),
            (code, Some(detail)),
            "{id}"
        );
    }
}

#[tokio::test]
async fn ticket_t_21_cameras_and_test_answer_like_node() {
    let (app, _) = app();
    let list = |id: &str| cameras(State(app.clone()), Path(id.into()));
    assert_eq!(
        body(list("moon").await).await,
        (200, json!({"cameras": [{"id": "0", "name": "Bed"}]}))
    );
    assert_eq!(
        body(list("spool").await).await,
        (200, json!({"cameras": []}))
    );
    assert_eq!(body(list("down").await).await.0, 502);
    assert_eq!(body(list("manual").await).await.0, 409);

    let probe = |id: &str| test(State(app.clone()), Path(id.into()));
    assert_eq!(
        body(probe("moon").await).await,
        (
            200,
            json!({"ok": true, "message": "Connected (klippy: ready)"})
        )
    );
    let (status, problem) = body(probe("gone").await).await;
    assert_eq!(
        (status, &problem["detail"]),
        (404, &json!("Integration not found"))
    );
}

#[tokio::test]
async fn ticket_t_21_connection_test_keeps_nodes_five_per_minute_budget() {
    let (app, fake) = app();
    for _ in 0..TEST_LIMIT {
        assert_eq!(
            body(test(State(app.clone()), Path("moon".into())).await)
                .await
                .0,
            200
        );
    }
    let throttled = test(State(app.clone()), Path("moon".into())).await;
    assert!(throttled.headers().contains_key(header::RETRY_AFTER));
    let (status, problem) = body(throttled).await;
    assert_eq!(status, 429);
    assert!(
        problem["detail"]
            .as_str()
            .unwrap()
            .starts_with("Rate limit exceeded")
    );
    assert_eq!(
        fake.calls.lock().unwrap().len(),
        TEST_LIMIT as usize,
        "a throttled test never reaches the printer"
    );
    // Other routes keep their own budgets.
    assert_eq!(
        body(cameras(State(app), Path("moon".into())).await).await.0,
        200
    );
}

#[tokio::test]
async fn ticket_t_21_root_and_api_v1_mounts_keep_separate_budgets() {
    use tower::ServiceExt;
    let fake = Arc::new(Fake::default());
    let router = routes(fake.clone(), Arc::default());
    let get = |uri: &str| {
        axum::extract::Request::get(uri)
            .body(axum::body::Body::empty())
            .unwrap()
    };
    for _ in 0..CAMERAS_LIMIT {
        let response = router
            .clone()
            .oneshot(get("/printers/moon/cameras"))
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
    let throttled = router
        .clone()
        .oneshot(get("/printers/moon/cameras"))
        .await
        .unwrap();
    assert_eq!(throttled.status(), 429);
    let aliased = router
        .clone()
        .oneshot(get("/api/v1/printers/moon/cameras"))
        .await
        .unwrap();
    assert_eq!(
        aliased.status(),
        200,
        "the /api/v1 alias has its own window"
    );
    assert_eq!(fake.calls.lock().unwrap().len(), CAMERAS_LIMIT as usize + 1);
}

fn content(path: Option<&str>, profile_id: Option<&str>) -> Query<ContentQuery> {
    Query(ContentQuery {
        path: path.map(Into::into),
        profile_id: profile_id.map(Into::into),
    })
}

/// Status, the three proxied headers and the body text.
async fn streamed(response: Response) -> (u16, [Option<String>; 3], String) {
    let status = response.status().as_u16();
    let header = |name| {
        response
            .headers()
            .get(name)
            .map(|value: &HeaderValue| value.to_str().unwrap().to_owned())
    };
    let headers = [
        header(header::CONTENT_TYPE),
        header(header::CONTENT_LENGTH),
        header(header::CACHE_CONTROL),
    ];
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

#[tokio::test]
async fn ticket_t_21_file_content_streams_a_listed_file_like_node() {
    let (app, fake) = app();
    let open = |id: &str, query| file_content(State(app.clone()), Path(id.into()), query);
    assert_eq!(
        streamed(open("moon", content(Some(" /plates/plate.gcode/ "), None)).await).await,
        (
            200,
            [
                Some("application/octet-stream".into()),
                Some("5".into()),
                Some("private, no-store".into())
            ],
            "G28\n\n".into()
        )
    );
    assert_eq!(
        fake.calls.lock().unwrap().drain(..).collect::<Vec<_>>(),
        ["file moon plates/plate.gcode"]
    );
    let refused = [
        (content(None, None), 400, "path is required"),
        (content(Some("  "), None), 400, "path is required"),
        (
            content(Some("../etc"), None),
            400,
            "path is not a valid printer storage path",
        ),
        (
            content(Some("a//b"), None),
            400,
            "path is not a valid printer storage path",
        ),
        (
            content(Some("plates/plate.gcode"), Some("Build")),
            400,
            "Choose an existing Build before opening a print file",
        ),
        (
            content(Some("plates/plate.gcode"), Some("0")),
            400,
            "Choose an existing Build before opening a print file",
        ),
        (
            content(Some("plates/plate.gcode"), Some("2")),
            400,
            "Choose an existing Build before opening a print file",
        ),
        (content(None, Some("1")), 400, "path is required"),
    ];
    for (query, status, detail) in refused {
        let (got, problem) = body(open("moon", query).await).await;
        assert_eq!((got, &problem["detail"]), (status, &json!(detail)));
    }
    assert!(fake.calls.lock().unwrap().is_empty());
    for (printer, path, status, detail) in [
        ("moon", "plates/other.gcode", 404, "Printer file not found"),
        (
            "moon",
            "plates/removed.gcode",
            404,
            "Printer resource not found",
        ),
        (
            "moon",
            "plates/broken.gcode",
            502,
            "Printer host returned HTTP 500",
        ),
        ("gone", "a.gcode", 404, "Printer not found"),
        ("manual", "a.gcode", 409, "Printer is not linked to a host"),
        ("off", "a.gcode", 503, "Printer host is unavailable"),
        (
            "spool",
            "a.gcode",
            501,
            "Printer host does not support file browsing",
        ),
        (
            "down",
            "a.gcode",
            502,
            "Moonraker directory listing returned HTTP 500",
        ),
        ("busy", "a.gcode", 503, "Printer settings are unavailable"),
    ] {
        let (got, problem) = body(open(printer, content(Some(path), None)).await).await;
        assert_eq!(
            (got, &problem["detail"]),
            (status, &json!(detail)),
            "{printer} {path}"
        );
    }
}

#[tokio::test]
async fn ticket_t_21_camera_view_streams_a_listed_camera_like_node() {
    let (app, fake) = app();
    let view = |id: &str, camera: Option<&str>| {
        camera_view(
            State(app.clone()),
            Path(id.into()),
            Query(CameraQuery {
                id: camera.map(Into::into),
            }),
        )
    };
    let (status, headers, text) = streamed(view("moon", Some(" 0 ")).await).await;
    assert_eq!(
        (status, headers[0].as_deref(), text.as_str()),
        (200, Some("image/jpeg"), "G28\n\n")
    );
    assert_eq!(headers[2].as_deref(), Some("private, no-store"));
    for camera in [None, Some(""), Some("  ")] {
        let (got, problem) = body(view("moon", camera).await).await;
        assert_eq!((got, &problem["detail"]), (400, &json!("id is required")));
    }
    for (printer, camera, status, detail) in [
        ("moon", "9", 404, "Printer camera not found"),
        ("spool", "0", 404, "Printer camera not found"),
        ("gone", "0", 404, "Printer not found"),
        ("manual", "0", 409, "Printer is not linked to a host"),
        (
            "down",
            "0",
            502,
            "Moonraker directory listing returned HTTP 500",
        ),
    ] {
        let (got, problem) = body(view(printer, Some(camera)).await).await;
        assert_eq!(
            (got, &problem["detail"]),
            (status, &json!(detail)),
            "{printer} {camera}"
        );
    }
    assert_eq!(fake.calls.lock().unwrap()[0], "camera_view moon 0");
}

#[tokio::test]
async fn ticket_t_21_streamed_routes_keep_nodes_twenty_per_minute_budgets() {
    let (app, fake) = app();
    for _ in 0..20 {
        let query = content(Some("plates/plate.gcode"), None);
        let response = file_content(State(app.clone()), Path("moon".into()), query).await;
        assert_eq!(response.status(), 200);
    }
    let query = content(Some("plates/plate.gcode"), None);
    let response = file_content(State(app.clone()), Path("moon".into()), query).await;
    assert_eq!(response.status(), 429);
    assert!(response.headers().contains_key(header::RETRY_AFTER));
    // The camera view has its own window.
    let query = Query(CameraQuery {
        id: Some("0".into()),
    });
    assert_eq!(
        camera_view(State(app), Path("moon".into()), query)
            .await
            .status(),
        200
    );
    assert_eq!(fake.calls.lock().unwrap().len(), 21);
}

/// The headers that keep a printer's reply from running as a page of the app.
async fn guarded(response: Response) -> [Option<String>; 4] {
    let header = |name| {
        response
            .headers()
            .get(name)
            .map(|value: &HeaderValue| value.to_str().unwrap().to_owned())
    };
    [
        header(header::CONTENT_TYPE),
        header(header::X_CONTENT_TYPE_OPTIONS),
        header(header::CONTENT_SECURITY_POLICY),
        header(header::CONTENT_DISPOSITION),
    ]
}

#[tokio::test]
async fn ticket_t_21_streamed_replies_never_run_as_pages_of_the_app() {
    let (app, _) = app();
    let sandbox = || Some("default-src 'none'; sandbox".to_owned());
    let nosniff = || Some("nosniff".to_owned());
    let file = file_content(
        State(app.clone()),
        Path("moon".into()),
        content(Some("plates/page.html"), None),
    )
    .await;
    assert_eq!(
        guarded(file).await,
        [
            Some("text/html".into()),
            nosniff(),
            sandbox(),
            Some("attachment".into())
        ]
    );
    for (camera, served) in [
        ("0", "image/jpeg"),
        ("mjpeg", "multipart/x-mixed-replace; boundary=frame"),
        ("page", "application/octet-stream"),
        ("svg", "application/octet-stream"),
    ] {
        let query = Query(CameraQuery {
            id: Some(camera.into()),
        });
        let view = camera_view(State(app.clone()), Path("moon".into()), query).await;
        assert_eq!(
            guarded(view).await,
            [Some(served.into()), nosniff(), sandbox(), None],
            "{camera}"
        );
    }
}

#[tokio::test]
async fn gre_1214_file_content_captures_a_build_snapshot_like_node() {
    use sha2::{Digest, Sha256};
    let (app, fake) = app();
    let open = |id: &str, query| file_content(State(app.clone()), Path(id.into()), query);
    let scope = |printer: &str| Scope {
        profile_id: 1,
        printer_id: printer.into(),
        remote_path: "plates/plate.gcode".into(),
        filename: "plate.gcode".into(),
    };
    let response = open("moon", content(Some("plates/plate.gcode"), Some(" 0x1 "))).await;
    let headers = response.headers().clone();
    let token = headers["x-printpartner-snapshot"]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        headers[header::ACCESS_CONTROL_EXPOSE_HEADERS],
        "X-PrintPartner-Snapshot"
    );
    assert!(
        app.snapshots
            .read(&token, &scope("moon"), Some("source"))
            .is_none(),
        "the token redeems only after the whole file"
    );
    assert_eq!(
        streamed(response).await,
        (
            200,
            [
                Some("application/octet-stream".into()),
                None,
                Some("private, no-store".into())
            ],
            "G28

"
            .into()
        )
    );
    let inspected = app
        .snapshots
        .read(&token, &scope("moon"), Some("source"))
        .unwrap();
    assert_eq!(
        inspected.identity,
        json!({
            "size_bytes": 5,
            "modified_at": "2026-11-07T00:00:00.000Z",
            "sha256": hex::encode(Sha256::digest(b"G28

")),
        })
    );
    assert!(
        app.snapshots
            .read(&token, &scope("moon"), Some("changed"))
            .is_none()
    );

    // One capture at a time: an unread body holds the store, and dropping it
    // releases the store without a redeemable token.
    let held = open("moon", content(Some("plates/plate.gcode"), Some("1"))).await;
    let dropped = held.headers()["x-printpartner-snapshot"]
        .to_str()
        .unwrap()
        .to_owned();
    let (status, problem) =
        body(open("moon", content(Some("plates/plate.gcode"), Some("1"))).await).await;
    assert_eq!(
        (status, &problem["detail"]),
        (502, &json!(CAPTURE_UNAVAILABLE))
    );
    drop(held);
    assert!(
        app.snapshots
            .read(&dropped, &scope("moon"), Some("source"))
            .is_none()
    );
    let reopened = open("moon", content(Some("plates/plate.gcode"), Some("1"))).await;
    assert_eq!(reopened.status(), 200);
    drop(reopened);

    // A host that changed while the file opened is not captured.
    let (status, problem) =
        body(open("moved", content(Some("plates/plate.gcode"), Some("1"))).await).await;
    assert_eq!(
        (status, &problem["detail"]),
        (502, &json!(CAPTURE_UNAVAILABLE))
    );
    // A printer error is answered as without a Build, and nothing is captured.
    let broken = open("moon", content(Some("plates/broken.gcode"), Some("1"))).await;
    assert!(broken.headers().get("x-printpartner-snapshot").is_none());
    let (status, problem) = body(broken).await;
    assert_eq!(
        (status, &problem["detail"]),
        (502, &json!("Printer host returned HTTP 500"))
    );
    assert_eq!(
        fake.calls.lock().unwrap().len(),
        6,
        "every request with a Build opened the file once"
    );
}
