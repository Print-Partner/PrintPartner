#![cfg(feature = "desktop_rebuild")]
//! GRE-1259 through the mounted routes: History lists a manual Checkoff burst
//! with its undo state, `POST /plans/{id}/history/{entry_id}/undo` undoes it
//! once, and every other case answers the undo's own refusal, on a disposable
//! copy of a real SQLite fixture. With the flag off nothing is mounted.
use axum::{Router, http::StatusCode};
use checkoff_http::{Fixture, post, send};
use pp_api::{accepted::accepted_router, checkoff::checkoff_router, history::history_router};
use pp_storage::{
    WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[path = "support/checkoff_http.rs"]
mod checkoff_http;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;

fn mount(fixture: &Fixture, flag: &str) -> (WriterOwner, Router) {
    fixture.open_with(flag, |config, owner| {
        let policy = AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        };
        checkoff_router(config.clone(), owner.checkoff_progress().unwrap())
            .merge(accepted_router(
                config.clone(),
                owner.accepted_reads().unwrap(),
            ))
            .merge(history_router(config, owner.build_history(policy).unwrap()))
    })
}

/// History's entries for Build 1.
async fn history(router: &Router) -> Vec<Value> {
    let (status, page) = send(router, "GET", "/api/v2/plans/1/history", "", true).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    page["entries"].as_array().unwrap().clone()
}

/// Records a manual burst setting `printed` on Part 1's unit token `unit`;
/// returns its History entry.
async fn manual(router: &Router, key: &str, unit: u8, printed: bool) -> Value {
    let (_, units) = send(router, "GET", "/api/v2/plans/1/checkoff/units", "", true).await;
    let body = json!({ "idempotency_key": key, "expected": units["basis"],
        "units": [{ "token": format!("ppu_{unit:032x}"), "printed": printed }] });
    let (status, done) = post(
        router,
        "/api/v2/plans/1/checkoff/bursts",
        &body.to_string(),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{done}");
    let id = format!(
        "checkoff_burst:manual-{}",
        hex::encode(Sha256::digest(key.as_bytes()))
    );
    let entries = history(router).await;
    let entry = entries.into_iter().find(|entry| entry["id"] == id.as_str());
    let entry = entry.unwrap();
    assert!(
        !entry
            .to_string()
            .contains(done["undo_token"].as_str().unwrap()),
        "History never shows the undo token"
    );
    entry
}

async fn undo(router: &Router, build: &str, entry: &str) -> (StatusCode, Value) {
    let path = format!("/api/v2/plans/{build}/history/{entry}/undo");
    post(router, &path, "", true).await
}

fn conflict(detail: &str) -> (StatusCode, Value) {
    (StatusCode::CONFLICT, json!(detail))
}

#[tokio::test]
async fn build_history_undo_http_undoes_a_manual_burst_once() {
    let fixture = Fixture::new();
    let (owner, router) = mount(&fixture, "1");
    let entry = manual(&router, "fill", 3, true).await;
    assert_eq!(entry["summary"], "Checked off 2 units");
    assert_eq!(entry["undo"], json!({"undoable": true}));
    let id = entry["id"].as_str().unwrap();
    let path = format!("/api/v2/plans/1/history/{id}/undo");
    let (status, _) = post(&router, &path, "", false).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, undone) = undo(&router, "1", &id.replace(':', "%3A")).await;
    assert_eq!(status, StatusCode::OK, "{undone}");
    let already = "This Checkoff burst was already undone";
    let mut expected = entry.clone();
    expected["undo"] = json!({"undoable": false, "reason": "already_undone", "detail": already});
    assert_eq!(undone["entry"], expected, "the entry stays, undone");
    assert!(undone["undone_at"].is_string());
    assert!(history(&router).await.contains(&expected));
    let (status, again) = post(&router, &format!("/plans/1/history/{id}/undo"), "", true).await;
    assert_eq!((status, again["detail"].clone()), conflict(already));
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn build_history_undo_http_answers_each_refusal() {
    let fixture = Fixture::new();
    let (owner, router) = mount(&fixture, "true");
    let first = manual(&router, "fill", 3, true).await;
    let later = manual(&router, "clear", 2, false).await;
    let changed = "Checkoff changed since this burst: a later change on its Parts depends on it";
    let first_id = first["id"].as_str().unwrap();
    let (status, body) = undo(&router, "1", first_id).await;
    assert_eq!((status, body["detail"].clone()), conflict(changed));

    let sql = fixture.sql();
    sql.execute_batch(
        "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'printer.send_queue',
           '[{\"id\":\"send-1\",\"filename\":\"plate.gcode\",\"artifact_path\":\"/disposable/plate.gcode\",
             \"printer_id\":\"printer-1\",\"profile_id\":1,\"state\":\"done\",\"created_at\":\"2020-01-01T00:00:00.000Z\"}]');
         INSERT INTO plan_decisions (id,tenant_id,profile_id,created_at,actor,kind,label,summary)
           VALUES (90,'default',1,'2020-01-01T00:00:00.000Z','user','user_note','','Note');
         INSERT INTO build_profiles (id,tenant_id,name) VALUES (5,'foreign','GRE-1259 other tenant Build');
         INSERT INTO print_jobs (id,tenant_id,profile_id,filename,at,link_id)
           VALUES ('node-job','default',1,'old.gcode','2020-01-01T00:00:00.000Z','link-old');
         INSERT INTO print_job_parts (id,tenant_id,job_id,at,profile_id,part_id,unit_index,result)
           VALUES ('node-part','default','node-job','2020-01-01T00:00:00.000Z',1,1,0,'confirmed');",
    )
    .unwrap();
    let (status, body) = undo(&router, "1", "send:send-1").await;
    assert_eq!(
        (status, body["detail"].clone()),
        conflict("A printer command cannot be undone")
    );
    let (status, body) = undo(&router, "1", "checkoff_burst:node-job").await;
    assert_eq!(
        (status, body["detail"].clone()),
        conflict("No undo is recorded for this Checkoff burst")
    );
    let (status, body) = undo(&router, "1", "decision:90").await;
    assert_eq!(
        (status, body["detail"].clone()),
        conflict("This History entry cannot be undone")
    );
    for (build, entry, detail) in [
        (
            "1",
            "checkoff_burst:manual-missing",
            "History entry not found",
        ),
        ("1", "send:send-2", "History entry not found"),
        ("999", first_id, "Profile not found"),
        ("abc", first_id, "Profile not found"),
        ("5", first_id, "Profile not found"),
    ] {
        let (status, body) = undo(&router, build, entry).await;
        assert_eq!(
            (status, body["detail"].clone()),
            (StatusCode::NOT_FOUND, json!(detail)),
            "{build} {entry}"
        );
    }

    sql.execute(
        "UPDATE build_profiles SET archived_at='2026-11-06T00:00:00.000Z' WHERE id=1",
        [],
    )
    .unwrap();
    let archived = "Archived Plan Progress cannot be changed";
    let (status, body) = undo(&router, "1", later["id"].as_str().unwrap()).await;
    assert_eq!((status, body["detail"].clone()), conflict(archived));
    let listed = history(&router).await;
    let listed = listed
        .iter()
        .find(|entry| entry["id"] == later["id"])
        .unwrap();
    assert_eq!(
        listed["undo"],
        json!({"undoable": false, "reason": "archived", "detail": archived})
    );
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn build_history_undo_http_is_not_mounted_with_the_flag_off() {
    let fixture = Fixture::new();
    let (owner, router) = mount(&fixture, "0");
    let (status, _) = undo(&router, "1", "checkoff_burst:manual-x").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    owner.shutdown().unwrap();
}
