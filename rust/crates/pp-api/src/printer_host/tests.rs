use super::*;

/// Answers from fixed values and records every port call.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<String>>,
}

fn answer(printer: &str) -> HostAnswer<Value> {
    match printer {
        "moon" => HostAnswer::Ok(json!({"path": "", "entries": []})),
        "gone" => HostAnswer::NotFound,
        "manual" => HostAnswer::NotLinked,
        "off" => HostAnswer::Unavailable,
        "spool" => HostAnswer::Unsupported,
        "down" => HostAnswer::Failed("Moonraker directory listing returned HTTP 500".into()),
        _ => HostAnswer::Busy,
    }
}

impl PrinterHostPort for Fake {
    fn capabilities(&self, printer: String) -> Answer<Capabilities> {
        self.calls.lock().unwrap().push(format!("capabilities {printer}"));
        let answer = match answer(&printer) {
            HostAnswer::Ok(_) => HostAnswer::Ok(Capabilities {
                files: true,
                cameras: true,
                status: true,
            }),
            HostAnswer::NotFound => HostAnswer::NotFound,
            HostAnswer::NotLinked => HostAnswer::NotLinked,
            HostAnswer::Unavailable => HostAnswer::Unavailable,
            HostAnswer::Unsupported => HostAnswer::Unsupported,
            HostAnswer::Failed(detail) => HostAnswer::Failed(detail),
            HostAnswer::Busy => HostAnswer::Busy,
        };
        Box::pin(async move { answer })
    }
    fn files(&self, printer: String, path: String) -> Answer<Value> {
        self.calls.lock().unwrap().push(format!("files {printer} {path}"));
        let answer = answer(&printer);
        Box::pin(async move { answer })
    }
    fn cameras(&self, printer: String) -> Answer<Value> {
        self.calls.lock().unwrap().push(format!("cameras {printer}"));
        let answer = match answer(&printer) {
            HostAnswer::Ok(_) => HostAnswer::Ok(json!([{"id": "0", "name": "Bed"}])),
            other => other,
        };
        Box::pin(async move { answer })
    }
    fn test(&self, integration: String) -> Answer<Value> {
        self.calls.lock().unwrap().push(format!("test {integration}"));
        let answer = match answer(&integration) {
            HostAnswer::Ok(_) => HostAnswer::Ok(json!({"ok": true, "message": "Connected (klippy: ready)"})),
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
        },
        fake,
    )
}

async fn body(response: Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn query(path: &str) -> Option<Query<PathQuery>> {
    Some(Query(PathQuery {
        path: Some(path.into()),
    }))
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
    assert_eq!((status, &problem["detail"]), (404, &json!("Printer not found")));
    assert_eq!(body(read("busy").await).await.0, 503);
}

#[tokio::test]
async fn ticket_t_21_files_validate_the_path_before_the_port() {
    let (app, fake) = app();
    let list = |id: &str, path: Option<Query<PathQuery>>| files(State(app.clone()), Path(id.into()), path);
    for bad in ["../etc", "a//b", "a/./b", "a\\b", "a\0b"] {
        let (status, problem) = body(list("moon", query(bad)).await).await;
        assert_eq!(status, 400, "{bad}");
        assert_eq!(problem["detail"], "path is not a valid printer storage path");
    }
    assert!(fake.calls.lock().unwrap().is_empty(), "no invalid path reaches the port");
    assert_eq!(body(list("moon", None).await).await.0, 200);
    assert_eq!(body(list("moon", query("  /plates/a/  ")).await).await.0, 200);
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
        let (status, problem) = body(list(id, None).await).await;
        assert_eq!((status, problem["detail"].as_str()), (code, Some(detail)), "{id}");
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
    assert_eq!(body(list("spool").await).await, (200, json!({"cameras": []})));
    assert_eq!(body(list("down").await).await.0, 502);
    assert_eq!(body(list("manual").await).await.0, 409);

    let probe = |id: &str| test(State(app.clone()), Path(id.into()));
    assert_eq!(
        body(probe("moon").await).await,
        (200, json!({"ok": true, "message": "Connected (klippy: ready)"}))
    );
    let (status, problem) = body(probe("gone").await).await;
    assert_eq!((status, &problem["detail"]), (404, &json!("Integration not found")));
}

#[tokio::test]
async fn ticket_t_21_connection_test_keeps_nodes_five_per_minute_budget() {
    let (app, fake) = app();
    for _ in 0..TEST_LIMIT {
        assert_eq!(body(test(State(app.clone()), Path("moon".into())).await).await.0, 200);
    }
    let throttled = test(State(app.clone()), Path("moon".into())).await;
    assert!(throttled.headers().contains_key(header::RETRY_AFTER));
    let (status, problem) = body(throttled).await;
    assert_eq!(status, 429);
    assert!(problem["detail"].as_str().unwrap().starts_with("Rate limit exceeded"));
    assert_eq!(
        fake.calls.lock().unwrap().len(),
        TEST_LIMIT as usize,
        "a throttled test never reaches the printer"
    );
    // Other routes keep their own budgets.
    assert_eq!(body(cameras(State(app), Path("moon".into())).await).await.0, 200);
}
