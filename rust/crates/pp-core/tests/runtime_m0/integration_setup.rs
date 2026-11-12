//! Daily-loop step 3 on a Rust-only runtime: an integration added through
//! `POST /api/v2/integrations` passes its connection test and reports live
//! status with no restart, still does after a restart on the same data
//! directory, and is gone everywhere after `DELETE`. The printer is the
//! recorded loopback Moonraker; nothing is uploaded or started.
use super::printer_host::recorded;
use super::watcher_proof::{proof_data, run_in_child, shut_down, signed_in, start_rust_only};
use super::{request, temporary};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::time::Duration;

const SECRET: &str = "created-key-canary";

#[tokio::test]
async fn integration_setup_rust_only_runtime_creates_polls_and_deletes() {
    let data = temporary("integration-setup");
    run_in_child("integration_setup::integration_setup_in_child", &data).await;
    let _ = std::fs::remove_dir_all(data);
}

/// One request, with its status, parsed body and the body's text.
async fn call(
    origin: &str,
    cookie: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value, String) {
    let (code, _, bytes) = request(origin, path, method, Some(cookie), None, None, body).await;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (code, serde_json::from_str(&text).unwrap_or_default(), text)
}

/// Polls the status route until the recorded print shows.
async fn printing(origin: &str, cookie: &str, id: &str) {
    let path = format!("/api/v1/integrations/{id}/status");
    let seen = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (code, body, _) = call(origin, cookie, "GET", &path, None).await;
            if code == StatusCode::OK && body["state"] == "printing" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    seen.await
        .expect("the runtime never polled the created integration");
}

#[tokio::test]
#[ignore = "runs in an isolated subprocess with explicit environment"]
async fn integration_setup_in_child() {
    let data = proof_data();
    let (printer, _) = recorded();
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let moonraker = json!({"kind": "moonraker", "name": "Voron",
        "base_url": printer.url(), "api_key": SECRET});
    let created = "/api/v2/integrations";
    for (method, path) in [("POST", created), ("DELETE", "/api/v2/integrations/any")] {
        let body = (method == "POST").then(|| moonraker.clone());
        let (code, _, _) = call(&origin, &desktop, method, path, body).await;
        assert_eq!(code, StatusCode::UNAUTHORIZED, "{method} {path}");
    }
    let cookie = signed_in(&origin, &desktop, 1).await;
    let post = |body: Value| call(&origin, &cookie, "POST", created, Some(body));

    // A refused request names its field and stores nothing.
    let mut refused = moonraker.clone();
    refused["base_url"] = json!("http://user:pw@printer.invalid");
    let (code, problem, _) = post(refused).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{problem}");
    assert_eq!(problem["errors"][0]["pointer"], "/base_url", "{problem}");
    let mut bambu = moonraker.clone();
    bambu["kind"] = json!("bambu");
    let (code, problem, _) = post(bambu).await;
    assert_eq!(
        (code, &problem["errors"][0]["pointer"]),
        (StatusCode::BAD_REQUEST, &json!("/kind")),
        "{problem}"
    );

    let (code, view, text) = post(moonraker).await;
    assert_eq!(code, StatusCode::OK, "{text}");
    assert!(!text.contains(SECRET), "{text}");
    let id = view["id"].as_str().expect("the new id").to_owned();
    assert_eq!(
        view,
        json!({"id": id, "kind": "moonraker", "name": "Voron", "enabled": true,
            "base_url": printer.url(), "username": null,
            "has_api_key": true, "has_password": false})
    );
    let test = format!("/api/v1/integrations/{id}/test");
    let remove = format!("/api/v2/integrations/{id}");

    // A printer that links the new host shows it. Printer writes are a later
    // change, so the fleet row is stored directly.
    let fleet = json!([{"id": "p-1", "name": "Voron", "integration_id": id}]).to_string();
    rusqlite::Connection::open(data.join("print-partner.db"))
        .unwrap()
        .execute(
            "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','printer.fleet',?1)",
            [&fleet],
        )
        .unwrap();
    let (code, farm, _) = call(&origin, &cookie, "GET", "/api/v2/printers", None).await;
    assert_eq!(code, StatusCode::OK, "{farm}");
    assert_eq!(
        farm["printers"][0]["integration"],
        json!({"id": id, "kind": "moonraker"}),
        "{farm}"
    );

    // A Spoolman integration created here is one the default setting takes.
    let spoolman = json!({"kind": "spoolman", "name": "Spools", "base_url": "http://127.0.0.1:9"});
    let (code, spool, _) = post(spoolman).await;
    assert_eq!(code, StatusCode::OK, "{spool}");
    let default = "/api/v1/settings/spoolman-default";
    let chosen = json!({"integration_id": spool["id"]});
    let (code, body, _) = call(&origin, &cookie, "PUT", default, Some(chosen.clone())).await;
    assert_eq!((code, &body), (StatusCode::OK, &chosen));

    // With no restart the new host passes its test and is polled; after a
    // restart on the same data directory it still does.
    let (code, body, _) = call(&origin, &cookie, "POST", &test, None).await;
    assert_eq!(
        (code, &body["ok"]),
        (StatusCode::OK, &json!(true)),
        "{body}"
    );
    printing(&origin, &cookie, &id).await;
    shut_down(runtime).await;
    let (runtime, origin, desktop) = start_rust_only(&data).await;
    let cookie = signed_in(&origin, &desktop, 2).await;
    let (code, body, _) = call(&origin, &cookie, "POST", &test, None).await;
    assert_eq!(
        (code, &body["ok"]),
        (StatusCode::OK, &json!(true)),
        "{body}"
    );
    printing(&origin, &cookie, &id).await;

    // Delete answers what it removed. Then the test and status routes do not
    // know the id, the printer hears nothing more, and the default is clear.
    let (code, removed, text) = call(&origin, &cookie, "DELETE", &remove, None).await;
    assert_eq!((code, &removed), (StatusCode::OK, &view), "{text}");
    assert!(!text.contains(SECRET), "{text}");
    let (code, body, _) = call(&origin, &cookie, "POST", &test, None).await;
    assert_eq!(code, StatusCode::NOT_FOUND, "{body}");
    // The watchers re-read the stored hosts about once a second.
    let status = format!("/api/v1/integrations/{id}/status");
    let dropped = tokio::time::timeout(Duration::from_secs(20), async {
        while call(&origin, &cookie, "GET", &status, None).await.0 != StatusCode::NOT_FOUND {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    dropped.await.expect("the deleted host kept its status");
    let (code, problem, _) = call(&origin, &cookie, "DELETE", &remove, None).await;
    assert_eq!(
        (code, &problem["code"]),
        (StatusCode::NOT_FOUND, &json!("integration_not_found"))
    );
    let (_, farm, _) = call(&origin, &cookie, "GET", "/api/v2/printers", None).await;
    assert_eq!(
        (
            &farm["printers"][0]["id"],
            &farm["printers"][0]["integration"]
        ),
        (&json!("p-1"), &Value::Null),
        "{farm}"
    );
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let heard = printer.request_lines().len();
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(
        printer.request_lines().len(),
        heard,
        "a deleted host is polled"
    );
    let spool_path = format!("/api/v2/integrations/{}", spool["id"].as_str().unwrap());
    let (code, _, _) = call(&origin, &cookie, "DELETE", &spool_path, None).await;
    assert_eq!(code, StatusCode::OK);
    let (code, body, _) = call(&origin, &cookie, "GET", default, None).await;
    assert_eq!(
        (code, &body),
        (StatusCode::OK, &json!({"integration_id": null}))
    );
    shut_down(runtime).await;
    for line in printer.request_lines() {
        assert!(line.starts_with("GET "), "the printer saw {line}");
    }
}
