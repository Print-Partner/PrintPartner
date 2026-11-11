#![cfg(feature = "desktop_rebuild")]
use pp_api::{
    admin::AdminHttpConfig,
    diagnostics::{LogRecords, diagnostics_router},
    serving::Serving,
};
use pp_storage::{Limits, WriterOwner};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Records(Mutex<Option<Vec<Value>>>);
impl LogRecords for Records {
    fn records(&self) -> std::io::Result<Vec<Value>> {
        self.0
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| std::io::Error::other("unreadable"))
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
        json!({"time":1,"level":30,"message":"one","method":"GET","duration":10}),
        json!({"time":2,"level":50,"message":"two","method":"POST","duration":20}),
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
    for prefix in ["", "/api/v1"] {
        for path in [
            "/settings/logging/config",
            "/settings/logging/logs",
            "/settings/logging/stats",
            "/metrics",
        ] {
            let path = format!("{prefix}{path}");
            assert_eq!(
                client
                    .get(format!("{origin}{path}"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                401
            );
            let reply = send(reqwest::Method::GET, &path).send().await.unwrap();
            assert_eq!(reply.status(), 200);
            assert_eq!(reply.headers()["cache-control"], "private, no-store");
        }
    }
    let stats: Value = send(reqwest::Method::GET, "/settings/logging/stats")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["totalLogs"], 2);
    assert_eq!(stats["avgDuration"], 15.0);
    assert_eq!(stats["errorCount"], 1);
    let filtered: Value = send(
        reqwest::Method::GET,
        "/settings/logging/logs?severity=error&method=POST&limit=1",
    )
    .send()
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(filtered.as_array().unwrap().len(), 1);
    for body in [
        json!({"minSeverity":"trace"}),
        json!({"maxLogs":1}),
        json!({"enableWorkflowTracking":1}),
    ] {
        assert_eq!(
            send(reqwest::Method::POST, "/settings/logging/config")
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
    assert_eq!(
        client
            .delete(format!("{origin}/settings/logging/logs"))
            .header("cookie", "pp_session=read-fixture-secret")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        send(reqwest::Method::DELETE, "/settings/logging/logs")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let empty: Value = send(reqwest::Method::GET, "/api/v1/settings/logging/logs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty, json!([]));
    source
        .0
        .lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .push(json!({"time":3,"level":40,"message":"new"}));
    let new: Value = send(reqwest::Method::GET, "/settings/logging/logs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(new.as_array().unwrap().len(), 1);
    *source.0.lock().unwrap() = None;
    assert_eq!(
        send(reqwest::Method::GET, "/metrics")
            .send()
            .await
            .unwrap()
            .status(),
        500
    );
    serving.stop().await.unwrap();
    *source.0.lock().unwrap() = Some(vec![json!({"time":3,"level":40,"message":"retained"})]);
    let listener = tokio::net::TcpListener::bind(origin.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    serving = Serving::start(listener, router(Some("1")));
    let restarted: Value = send(reqwest::Method::GET, "/settings/logging/logs")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(restarted.as_array().unwrap().len(), 1);
    serving.stop().await.unwrap();
    let listener = tokio::net::TcpListener::bind(origin.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    serving = Serving::start(listener, router(Some("0")));
    assert_eq!(
        send(reqwest::Method::GET, "/metrics")
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    serving.stop().await.unwrap();
    owner.shutdown().unwrap();
}
