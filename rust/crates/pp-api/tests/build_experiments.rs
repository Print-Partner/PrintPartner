#![cfg(feature = "desktop_rebuild")]
use pp_api::builds::{BuildHttpConfig, build_router};
use pp_storage::{Limits, WriterOwner};
use pp_testkit::{Fixture, Installation};
use rusqlite::Connection;
use serde_json::{Value, json};
#[path = "support/loopback.rs"]
mod loopback;

#[path = "support/auth_policy.rs"]
mod auth_policy;
use auth_policy::policy;

fn open() -> (Installation, WriterOwner, axum::Router) {
    let data = Installation::create(Fixture::Empty).unwrap();
    std::fs::write(
        data.database_path(),
        include_bytes!("../../pp-storage/tests/fixtures/required-units/unchanged.db"),
    )
    .unwrap();
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
    db.execute(
        "UPDATE build_profiles SET order_number='PO-7' WHERE id=1",
        [],
    )
    .unwrap();
    let parent: (i64, i64) = db
        .query_row(
            "SELECT accepted_plan_revision_id,accepted_plan_version FROM build_profiles WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let original = db.query_row::<String,_,_>("SELECT group_concat(required_unit_token,',') FROM plan_revision_required_units WHERE revision_id=?1",[parent.0],|r|r.get(0)).unwrap();
    db.execute("UPDATE parts SET filament_color_id='new-filament',filament_custom_hex='#112233',spoolman_spool_id='spool-42' WHERE profile_id=1",[]).unwrap();
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
    let header: (Option<String>,Option<i64>,i64,Option<String>) = db.query_row("SELECT order_number,accepted_plan_revision_id,accepted_plan_version,archived_at FROM build_profiles WHERE id=?1", [build], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
    assert_eq!(header, (None, None, 0, None));
    let count = |sql: &str, id: i64| db.query_row::<i64, _, _>(sql, [id], |r| r.get(0)).unwrap();
    assert_eq!(
        count("SELECT count(*) FROM parts WHERE profile_id=?1", build),
        0
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM plan_revisions WHERE profile_id=?1",
            build
        ),
        0
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM plan_draft_parts WHERE draft_id=?1 AND base_revision_part_id IS NOT NULL",
            draft
        ),
        0
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM plan_draft_parts WHERE draft_id=?1",
            draft
        ),
        count(
            "SELECT count(*) FROM plan_revision_parts WHERE revision_id=?1",
            parent.0
        )
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM profile_layers WHERE profile_id=?1",
            build
        ),
        count("SELECT count(*) FROM profile_layers WHERE profile_id=?1", 1)
    );
    assert_eq!(
        count(
            "SELECT count(*) FROM build_experiments WHERE parent_build_id=?1",
            1
        ),
        1
    );
    assert_eq!(db.query_row::<String,_,_>("SELECT group_concat(required_unit_token,',') FROM plan_revision_required_units WHERE revision_id=?1",[parent.0],|r|r.get(0)).unwrap(),original);
    assert_eq!(db.query_row::<i64,_,_>("SELECT count(*) FROM (SELECT part_key,quantity_effective,included,notes,role_override FROM plan_revision_parts WHERE revision_id=?1 EXCEPT SELECT part_key,quantity_effective,included,notes,role_override FROM plan_draft_parts WHERE draft_id=?2)",rusqlite::params![parent.0,draft],|r|r.get(0)).unwrap(),0);
    let assignment: (String,String,String) = db.query_row("SELECT filament_color_id,filament_custom_hex,spoolman_spool_id FROM plan_draft_parts WHERE draft_id=?1 LIMIT 1",[draft],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(
        assignment,
        ("new-filament".into(), "#112233".into(), "spool-42".into())
    );
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
    // The FK also protects ancestry from a caller outside the HTTP adapter.
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert!(
        db.execute("DELETE FROM build_profiles WHERE id=1", [])
            .is_err()
    );
    assert_eq!(
        db.query_row::<i64, _, _>(
            "SELECT parent_build_id FROM build_experiments WHERE build_id=?1",
            [build],
            |r| r.get(0)
        )
        .unwrap(),
        1
    );
    let app = router(&owner, Some("true"));
    // Archived experiments still preserve their parent's ancestry.
    db.execute(
        "UPDATE build_profiles SET archived_at='2026-11-10' WHERE id=?1",
        [build],
    )
    .unwrap();
    assert_eq!(send(&app, "DELETE", "/plans/1", None).await.0, 409);
    assert_eq!(
        send(&app, "DELETE", &format!("/plans/{build}"), None)
            .await
            .0,
        204
    );
    assert_eq!(send(&app, "DELETE", "/plans/1", None).await.0, 204);
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
    db.execute_batch("INSERT INTO build_profiles(id,tenant_id,name) VALUES(99,'default','Parent'); INSERT INTO build_experiments(build_id,parent_build_id) VALUES(1,99); INSERT INTO plan_draft_headers(draft_id,tenant_id,name,order_number,header_version,updated_at) VALUES(2,'default','Experiment','PO-9',1,'2026-11-10');").unwrap();
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
