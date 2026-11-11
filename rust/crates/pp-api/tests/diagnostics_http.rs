#![cfg(feature = "desktop_rebuild")]
use pp_api::{
    admin::AdminHttpConfig,
    diagnostics::{LogRecord, LogRecords, diagnostics_router},
    serving::Serving,
};
use pp_storage::{Limits, WriterOwner};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Records(Mutex<Option<Vec<LogRecord>>>);
impl LogRecords for Records {
    fn records(&self) -> std::io::Result<Vec<LogRecord>> {
        self.0
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| std::io::Error::other("unreadable"))
    }
}

fn record(offset: u64, value: Value) -> LogRecord {
    LogRecord {
        file: "fixture".into(),
        retained_file: Arc::new(()),
        offset,
        value,
    }
}

async fn body(request: reqwest::RequestBuilder) -> Value {
    request.send().await.unwrap().json().await.unwrap()
}
async fn check(request: reqwest::RequestBuilder, expected: u16) {
    assert_eq!(request.send().await.unwrap().status(), expected);
}
async fn assert_reads(
    send: &impl Fn(reqwest::Method, &str) -> reqwest::RequestBuilder,
    client: &reqwest::Client,
    origin: &str,
) {
    for prefix in ["", "/api/v1"] {
        for path in [
            "/settings/logging/config",
            "/settings/logging/logs",
            "/settings/logging/stats",
            "/metrics",
        ] {
            let path = format!("{prefix}{path}");
            check(client.get(format!("{origin}{path}")), 401).await;
            let reply = send(reqwest::Method::GET, &path).send().await.unwrap();
            assert_eq!(reply.status(), 200);
            assert_eq!(reply.headers()["cache-control"], "private, no-store");
        }
    }
    let metrics = send(reqwest::Method::GET, "/api/v1/metrics")
        .send()
        .await
        .unwrap();
    assert!(
        metrics.headers()["content-type"]
            .to_str()
            .unwrap()
            .contains("version=0.0.4")
    );
    let metrics = metrics.text().await.unwrap();
    assert!(metrics.contains("core_log_records{severity=\"error\"} 1"));
    assert!(metrics.contains("app_info{"));
    assert!(!metrics.contains("read-fixture-secret"));
    let stats: Value = body(send(reqwest::Method::GET, "/settings/logging/stats")).await;
    assert_eq!(stats["totalLogs"], 2);
    assert_eq!(stats["avgDuration"], 15.0);
    assert_eq!(stats["errorCount"], 1);
    let filtered: Value = body(send(
        reqwest::Method::GET,
        "/settings/logging/logs?severity=error&method=POST&limit=1",
    ))
    .await;
    assert_eq!(filtered.as_array().unwrap().len(), 1);
}
async fn assert_config(send: &impl Fn(reqwest::Method, &str) -> reqwest::RequestBuilder) {
    let updated = body(
        send(reqwest::Method::POST, "/settings/logging/config")
            .json(&json!({"minSeverity":"error", "enableWorkflowTracking":false})),
    )
    .await;
    assert_eq!(updated["minSeverity"], "error");
    assert_eq!(
        body(send(
            reqwest::Method::GET,
            "/api/v1/settings/logging/config"
        ))
        .await,
        updated
    );
    assert_eq!(
        body(send(reqwest::Method::GET, "/settings/logging/logs")).await,
        json!([])
    );
    check(
        send(reqwest::Method::POST, "/api/v1/settings/logging/config")
            .json(&json!({"minSeverity":"info", "enableWorkflowTracking":true})),
        200,
    )
    .await;
    for body in [
        json!(["info", true]),
        json!(null),
        json!([]),
        json!({"minSeverity":"trace"}),
        json!({"maxLogs":1}),
        json!({"minSeverity":null}),
        json!({"enableWorkflowTracking":null}),
        json!({"enableWorkflowTracking":1}),
    ] {
        check(
            send(reqwest::Method::POST, "/settings/logging/config").json(&body),
            400,
        )
        .await;
    }
}
#[tokio::test]
async fn diagnostics_auth_aliases_clear_and_restart() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("print-partner.db"),
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    let owner = WriterOwner::open(directory.path(), Limits::default())
        .unwrap()
        .0;
    let source = Arc::new(Records(Mutex::new(Some(vec![
        record(
            1,
            json!({"time":1,"level":30,"message":"one","method":"GET","duration":10}),
        ),
        record(
            2,
            json!({"time":2,"level":50,"message":"two","method":"POST","duration":20}),
        ),
    ]))));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let router = |flag| {
        diagnostics_router(
            AdminHttpConfig::from_startup(&origin, "default", flag).unwrap(),
            owner.accepted_reads().unwrap(),
            source.clone(),
        )
    };
    let mut serving = Serving::start(listener, router(Some("1")));
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let send = |method, path: &str| {
        client
            .request(method, format!("{origin}{path}"))
            .header("origin", &origin)
            .header("cookie", "pp_session=read-fixture-secret")
    };
    assert_reads(&send, &client, &origin).await;
    assert_config(&send).await;
    check(
        client
            .delete(format!("{origin}/settings/logging/logs"))
            .header("cookie", "pp_session=read-fixture-secret"),
        403,
    )
    .await;
    let retained = Arc::downgrade(&source.0.lock().unwrap().as_ref().unwrap()[0].retained_file);
    check(send(reqwest::Method::DELETE, "/settings/logging/logs"), 200).await;
    let empty: Value = body(send(reqwest::Method::GET, "/api/v1/settings/logging/logs")).await;
    assert_eq!(empty, json!([]));
    source
        .0
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .push(record(3, json!({"time":3,"level":40,"message":"new"})));
    let new: Value = body(send(reqwest::Method::GET, "/settings/logging/logs")).await;
    assert_eq!(new.as_array().unwrap().len(), 1);
    *source.0.lock().unwrap() = None;
    assert!(retained.upgrade().is_some());
    check(send(reqwest::Method::GET, "/metrics"), 500).await;
    serving.stop().await.unwrap();
    assert!(retained.upgrade().is_none());
    *source.0.lock().unwrap() = Some(vec![record(
        3,
        json!({"time":3,"level":40,"message":"retained"}),
    )]);
    let listener = tokio::net::TcpListener::bind(origin.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    serving = Serving::start(listener, router(Some("1")));
    let restarted: Value = body(send(reqwest::Method::GET, "/settings/logging/logs")).await;
    assert_eq!(restarted.as_array().unwrap().len(), 1);
    serving.stop().await.unwrap();
    let listener = tokio::net::TcpListener::bind(origin.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    serving = Serving::start(listener, router(Some("0")));
    check(send(reqwest::Method::GET, "/metrics"), 404).await;
    serving.stop().await.unwrap();
    owner.shutdown().unwrap();
}
