use super::{AcceptedHttpConfig, board_router};
use axum::{Router, http::StatusCode};
use pp_storage::{Limits, WriterOwner};
use pp_testkit::Installation;
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[path = "../../../tests/support/loopback.rs"]
mod loopback;

const PATH: &str = "/api/v2/board/notes";
const OWNER: &str = "pp_session=required-unit-fixture-secret";
const OTHER: &str = "pp_session=board-test-foreign";

fn mount(data: &Installation) -> (WriterOwner, Router) {
    let (owner, _) = WriterOwner::open(data.path(), Limits::default()).unwrap();
    let router = board_router(
        AcceptedHttpConfig::from_startup(loopback::ORIGIN, "default", Some("1")).unwrap(),
        owner.accepted_reads().unwrap().board(),
    );
    (owner, router)
}

async fn list(app: &Router, cookie: Option<&str>) -> (StatusCode, Value) {
    loopback::send(app, "GET", PATH, None, cookie).await
}

async fn create(app: &Router, cookie: Option<&str>, build: i64, body: &str) -> (StatusCode, Value) {
    let request = json!({"build_id": build, "body": body}).to_string();
    loopback::send(app, "POST", PATH, Some(&request), cookie).await
}

#[tokio::test]
async fn board_http_lists_owner_choices_beyond_the_plan_summary_cap() {
    let data = Installation::with_authenticated_fixture().unwrap();
    Connection::open(data.database_path())
        .unwrap()
        .execute_batch(
            "WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM ids WHERE n < 65)
             INSERT INTO build_profiles (name) SELECT printf('Imported %d', n) FROM ids;
             INSERT INTO build_profiles (tenant_id, name) VALUES ('foreign', 'Hidden');",
        )
        .unwrap();
    let (owner, app) = mount(&data);
    let path = "/api/v2/board/builds";
    assert_eq!(loopback::send(&app, "GET", path, None, None).await.0, 401);
    let (status, body) = loopback::send(&app, "GET", path, None, Some(OWNER)).await;
    assert_eq!(status, 200, "{body}");
    let builds = body["builds"].as_array().unwrap();
    assert!(builds.len() > 64);
    let build = builds
        .iter()
        .find(|build| build["name"] == "Imported 65")
        .unwrap();
    assert!(!builds.iter().any(|build| build["name"] == "Hidden"));
    assert_eq!(
        create(
            &app,
            Some(OWNER),
            build["id"].as_i64().unwrap(),
            "Review this Build"
        )
        .await
        .0,
        200
    );
    drop(app);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn board_http_proves_owner_and_serves_only_converted_posts_after_reopen() {
    let data = Installation::with_authenticated_fixture().unwrap();
    let db = Connection::open(data.database_path()).unwrap();
    db.execute_batch("INSERT INTO board_posts (id, author_user_id, caption, title, snapshot_json, created_at)
        SELECT 'legacy', user_id, 'Legacy caption', 'Legacy title', '{}', '2025-01-01T00:00:00.000Z' FROM sessions LIMIT 1;").unwrap();
    let (owner, app) = mount(&data);
    db.execute_batch(
        "INSERT INTO users (id, display_name, created_at)
        VALUES ('foreign', 'Other owner', '2025-01-01T00:00:00.000Z');",
    )
    .unwrap();
    db.execute("INSERT INTO sessions (id, user_id, expires_at) VALUES (?1, 'foreign', '2099-01-01T00:00:00.000Z')",
        [hex::encode(Sha256::digest(b"board-test-foreign"))]).unwrap();

    for credential in [None, Some("pp_session=board-test-invalid")] {
        assert_eq!(list(&app, credential).await.0, 401);
        assert_eq!(create(&app, credential, 1, "Not stored").await.0, 401);
    }
    assert_eq!(
        list(&app, Some(OWNER)).await,
        (StatusCode::OK, json!({"notes": [], "imported_posts": []}))
    );
    let (status, note) = create(&app, Some(OWNER), 1, " Check the fit.\nKeep the clips. ").await;
    assert_eq!(status, 200, "{note}");
    assert_eq!(note["build_id"], 1);
    assert_eq!(note["build_name"], "Fixture Build");
    assert_eq!(note["body"], "Check the fit.\nKeep the clips.");
    db.execute(
        "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'board_imported_posts', ?1)",
        [json!({"legacy": {"caption": "Legacy caption", "title": "Legacy title", "created_at": "2025-01-01T00:00:00.000Z"}}).to_string()],
    ).unwrap();
    assert_eq!(
        list(&app, Some(OTHER)).await,
        (StatusCode::OK, json!({"notes": [], "imported_posts": []}))
    );
    let refusal = create(&app, Some(OTHER), 1, "Foreign write").await;
    assert_eq!(refusal.0, 404);
    assert_eq!(refusal.1["code"], "board_build_not_found");
    drop(app);
    owner.shutdown().unwrap();
    let (owner, app) = mount(&data);
    assert_eq!(
        list(&app, Some(OWNER)).await,
        (
            StatusCode::OK,
            json!({"notes": [note], "imported_posts": [
                {"id": "legacy", "caption": "Legacy caption", "title": "Legacy title", "created_at": "2025-01-01T00:00:00.000Z"}
            ]})
        )
    );
    assert_eq!(
        db.query_row(
            "SELECT caption FROM board_posts WHERE id='legacy'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "Legacy caption"
    );
    drop(app);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn board_http_rejects_invalid_notes_and_caps_each_build() {
    let data = Installation::with_authenticated_fixture().unwrap();
    let (owner, app) = mount(&data);
    for body in ["  ", "invalid\u{0}", &"é".repeat(1001)] {
        let (status, problem) = create(&app, Some(OWNER), 1, body).await;
        assert_eq!(status, 400, "{problem}");
        assert_eq!(problem["code"], "invalid_board_note");
    }
    assert_eq!(
        list(&app, Some(OWNER)).await.1,
        json!({"notes": [], "imported_posts": []})
    );
    for n in 0..20 {
        assert_eq!(
            create(&app, Some(OWNER), 1, &format!("Note {n}")).await.0,
            200
        );
    }
    let (status, problem) = create(&app, Some(OWNER), 1, "Note 21").await;
    assert_eq!(status, 409);
    assert_eq!(problem["code"], "board_note_limit");
    assert_eq!(
        list(&app, Some(OWNER)).await.1["notes"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    drop(app);
    owner.shutdown().unwrap();
}
