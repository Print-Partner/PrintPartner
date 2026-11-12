#![cfg(feature = "desktop_rebuild")]
use pp_api::builds::{BuildHttpConfig, build_router};
use pp_storage::{Limits, WriterOwner};
use pp_testkit::{Installation, build_experiments as fixtures};
use rusqlite::Connection;
use serde_json::{Value, json};
#[path = "support/loopback.rs"]
mod loopback;

#[path = "support/auth_policy.rs"]
mod auth_policy;
use auth_policy::policy;

fn open() -> (Installation, WriterOwner, axum::Router) {
    let data = Installation::with_authenticated_fixture().unwrap();
    let owner = WriterOwner::open(data.path(), Limits::default()).unwrap().0;
    let router = router(&owner, Some("true"));
    (data, owner, router)
}

fn router(owner: &WriterOwner, flag: Option<&str>) -> axum::Router {
    build_router(
        BuildHttpConfig::from_startup(loopback::ORIGIN, "default", flag).unwrap(),
        owner.build_graph_with_policy(policy()).unwrap(),
    )
    .merge(pp_api::drafts::draft_router(
        pp_api::drafts::DraftHttpConfig::from_startup(loopback::ORIGIN, "default", flag).unwrap(),
        owner
            .working_drafts_with_observations(
                policy(),
                std::sync::Arc::new(owner.source_reads().unwrap()),
                Default::default(),
            )
            .unwrap(),
        owner
            .rebuild_auth_with_policy(policy(), Default::default())
            .unwrap(),
    ))
}

async fn send(router: &axum::Router, method: &str, path: &str, body: Option<&str>) -> (u16, Value) {
    let (status, body) = loopback::send(
        router,
        method,
        path,
        body,
        Some("pp_session=required-unit-fixture-secret"),
    )
    .await;
    (status.as_u16(), pp_api::problem::without_envelope(body))
}

fn bytes(data: &Installation) -> Vec<(String, Vec<u8>)> {
    ["print-partner.db", "print-partner.db-wal"]
        .into_iter()
        .map(|name| {
            (
                name.to_owned(),
                std::fs::read(data.path().join(name)).unwrap_or_default(),
            )
        })
        .collect()
}

#[tokio::test]
async fn duplicate_is_detached_and_parent_delete_writes_no_database_bytes() {
    let (data, owner, app) = open();
    let db = Connection::open(data.database_path()).unwrap();
    let baseline = fixtures::prepare_parent(&db).unwrap();
    let copied = send(
        &app,
        "POST",
        "/api/v2/plans/1/duplicate",
        Some(r#"{"name":"Alternative"}"#),
    )
    .await;
    assert_eq!(copied.0, 201, "{copied:?}");
    let build = copied.1["build_id"].as_i64().unwrap();
    let draft = copied.1["draft_id"].as_i64().unwrap();
    assert_eq!(copied.1["parent_build_id"], 1);
    fixtures::assert_detached(&db, build, draft, &baseline).unwrap();
    let before = bytes(&data);
    let refused = send(&app, "DELETE", "/api/v2/plans/1", None).await;
    assert_eq!(refused.0, 409);
    assert_eq!(refused.1["code"], "build_has_experiments");
    assert_eq!(
        refused.1["builds"],
        json!([{"build_id":build,"name":"Alternative"}])
    );
    assert_eq!(
        bytes(&data),
        before,
        "refusal preserves database and WAL bytes"
    );
    drop(app);
    drop(db);
    verify_restart_and_delete(&data, owner, build).await;
}

async fn verify_restart_and_delete(data: &Installation, owner: WriterOwner, build: i64) {
    owner.shutdown().unwrap();
    let owner = WriterOwner::open(data.path(), Limits::default()).unwrap().0;
    let db = Connection::open(data.database_path()).unwrap();
    fixtures::assert_restricted_and_archive(&db, build).unwrap();
    let app = router(&owner, Some("true"));
    assert_eq!(fixtures::manifest_count(&db, build).unwrap(), 1);
    assert_eq!(send(&app, "DELETE", "/plans/1", None).await.0, 409);
    assert_eq!(
        send(&app, "DELETE", &format!("/plans/{build}"), None)
            .await
            .0,
        204
    );
    assert_eq!(fixtures::manifest_count(&db, build).unwrap(), 0);
    assert_eq!(fixtures::manifest_count(&db, 1).unwrap(), 1);
    assert_eq!(send(&app, "DELETE", "/plans/1", None).await.0, 204);
    assert_eq!(fixtures::manifest_count(&db, 1).unwrap(), 0);
    drop(app);
    drop(db);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn duplicate_refuses_invalid_names_missing_builds_and_flag_off_without_writes() {
    let (data, owner, app) = open();
    let before = bytes(&data);
    for (path, body, status) in [
        ("/plans/1/duplicate", r#"{"name":""}"#, 400),
        (
            "/plans/1/duplicate",
            r#"{"name":"Legacy","clear_checkoff":true}"#,
            400,
        ),
        ("/plans/999/duplicate", r#"{"name":"Missing"}"#, 404),
        ("/api/v1/plans/1/duplicate", r#"{"name":"Legacy"}"#, 409),
        (
            "/plans/1/duplicate",
            r#"{"name":"X","parent_build_id":999}"#,
            400,
        ),
    ] {
        assert_eq!(
            send(&app, "POST", path, Some(body)).await.0,
            status,
            "{path}"
        );
    }
    let off = router(&owner, None);
    assert_eq!(
        send(
            &off,
            "POST",
            "/plans/1/duplicate",
            Some(r#"{"name":"Off"}"#)
        )
        .await
        .0,
        404
    );
    let unauth = loopback::send(
        &app,
        "POST",
        "/plans/1/duplicate",
        Some(r#"{"name":"No auth"}"#),
        None,
    )
    .await;
    assert_eq!(unauth.0.as_u16(), 401);
    assert_eq!(bytes(&data), before);
    drop(off);
    drop(app);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn experiment_accept_preview_refuses_order_authority_before_writing() {
    let (data, owner, app) = open();
    let db = Connection::open(data.database_path()).unwrap();
    fixtures::prepare_order_claim(&db).unwrap();
    let before = bytes(&data);
    let answer = send(&app, "GET", "/plans/1/drafts/2/accept-preview", None).await;
    assert_eq!(answer.0, 409, "{answer:?}");
    assert_eq!(answer.1["code"], "experiment_order_requires_promotion");
    assert_eq!(bytes(&data), before);
    drop(app);
    drop(db);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn duplicate_rolls_back_build_and_ancestry_when_draft_insert_fails() {
    let (data, owner, app) = open();
    let db = Connection::open(data.database_path()).unwrap();
    fixtures::prepare_parent(&db).unwrap();
    db.execute_batch("CREATE TRIGGER fail_experiment_draft BEFORE INSERT ON plan_drafts WHEN NEW.created_by='system:experiment' BEGIN SELECT RAISE(ABORT,'injected draft failure'); END;").unwrap();
    let before = bytes(&data);
    let answer = send(
        &app,
        "POST",
        "/plans/1/duplicate",
        Some(r#"{"name":"Failed copy"}"#),
    )
    .await;
    assert_eq!(answer.0, 500, "{answer:?}");
    assert_eq!(bytes(&data), before);
    assert_eq!(
        db.query_row::<i64, _, _>("SELECT count(*) FROM build_experiments", [], |r| r.get(0))
            .unwrap(),
        0
    );
    drop(app);
    drop(db);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn duplicate_refuses_a_taken_name_without_writes() {
    let (data, owner, app) = open();
    let body = Some(r#"{"name":"Taken"}"#);
    assert_eq!(send(&app, "POST", "/plans/1/duplicate", body).await.0, 201);
    let before = bytes(&data);
    let refused = send(&app, "POST", "/plans/1/duplicate", body).await;
    assert_eq!(refused.0, 409);
    assert_eq!(refused.1["code"], "duplicate_build_name");
    assert_eq!(bytes(&data), before);
    drop(app);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn duplicate_without_an_accepted_revision_has_no_draft() {
    let (data, owner, app) = open();
    let db = Connection::open(data.database_path()).unwrap();
    fixtures::unaccepted_parent(&db).unwrap();
    let copied = send(
        &app,
        "POST",
        "/plans/99/duplicate",
        Some(r#"{"name":"Empty experiment"}"#),
    )
    .await;
    assert_eq!(copied.0, 201, "{copied:?}");
    assert_eq!(copied.1["parent_build_id"], 99);
    assert!(copied.1["draft_id"].is_null());
    drop(db);
    drop(app);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn experiment_accept_refuses_an_order_and_accepts_without_one() {
    for order in [Some("PO-9"), None] {
        let (data, owner, app) = open();
        let db = Connection::open(data.database_path()).unwrap();
        fixtures::prepare_order_claim(&db).unwrap();
        fixtures::set_experiment_order(&db, order).unwrap();
        let workspace = send(&app, "GET", "/plans/1/drafts/2", None).await;
        assert_eq!(workspace.0, 200);
        let draft = &workspace.1["draft"];
        let body = json!({"expected_snapshot_digest": draft["snapshot_digest"],
            "expected_lifecycle_version": draft["lifecycle_version"],
            "expected_base": draft["base"], "expected_header_version":1,
            "remap_checkoff_links":false});
        let before = bytes(&data);
        let (status, answer) = loopback::send_with_headers(
            &app,
            "POST",
            "/api/v2/plans/1/drafts/2/apply",
            Some("application/json"),
            serde_json::to_vec(&body).unwrap(),
            Some("pp_session=required-unit-fixture-secret"),
            &[("idempotency-key", "experiment-accept")],
        )
        .await;
        let answer = pp_api::problem::without_envelope(answer);
        if order.is_some() {
            assert_eq!(status.as_u16(), 409, "{answer:?}");
            assert_eq!(answer["code"], "experiment_order_requires_promotion");
            assert_eq!(bytes(&data), before);
        } else {
            assert_eq!(status.as_u16(), 200, "{answer:?}");
            assert_eq!(answer["draft_id"], 2);
            assert_eq!(answer["plan_version"], 2);
            fixtures::assert_experiment_accepted(&db, answer["revision_id"].as_i64().unwrap())
                .unwrap();
        }
        drop(db);
        drop(app);
        owner.shutdown().unwrap();
    }
}
