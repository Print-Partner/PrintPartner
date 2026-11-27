use super::{
    reads::{confirm, session, tool},
    *,
};
use rusqlite::Connection;

async fn setup() -> (McpServer, Vec<(String, String)>, Connection) {
    let server = McpServer::seeded("").await;
    std::fs::create_dir_all(server._directory.join("repos/1/revisions/fixture")).unwrap();
    let headers = session(&server, &format!("Bearer {}", server.create_key().await.1)).await;
    let db = Connection::open(server._directory.join("print-partner.db")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    (server, headers, db)
}

fn count(db: &Connection, query: &str) -> i64 {
    db.query_row(query, [], |row| row.get(0)).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_restore_keeps_accepted_identity_and_restores_provenance_as_a_draft() {
    let (server, headers, db) = setup().await;
    let call = |name, args| tool(&server, &headers, name, args);
    let (proposal, failed) = call(
        "create_plan_snapshot",
        json!({"plan_id":1,"name":"Before changes"}),
    )
    .await;
    assert!(!failed, "{proposal}");
    assert_eq!(count(&db, "SELECT count(*) FROM plan_snapshots"), 0);
    // A different MCP session can confirm the persisted proposal.
    let other = session(&server, &format!("Bearer {}", server.create_key().await.1)).await;
    let (saved, failed) = confirm(&server, &other, &proposal).await;
    assert!(!failed, "{saved}");
    assert_eq!(saved["result"]["name"], "Before changes");
    let (replay, failed) = confirm(&server, &other, &proposal).await;
    assert!(failed, "{replay}");
    assert_eq!(replay["code"], "already_applied");
    assert_eq!(count(&db, "SELECT count(*) FROM plan_snapshots"), 1);
    let snapshot = &saved["result"]["snapshot_id"];

    let args = json!({"plan_id":1,"snapshot_id":snapshot});
    let (blocked, failed) = call("propose_restore_snapshot", args.clone()).await;
    assert!(failed, "{blocked}");
    assert_eq!(blocked["code"], "draft_changed");
    db.execute_batch(
        "UPDATE plan_drafts SET state='abandoned',lifecycle_version=lifecycle_version+1 WHERE id=2;
                      UPDATE build_profiles SET name='Renamed later' WHERE id=1;",
    )
    .unwrap();
    let (restore, failed) = call("propose_restore_snapshot", args).await;
    assert!(!failed, "{restore}");
    assert_eq!(
        count(&db, "SELECT count(*) FROM plan_drafts WHERE state='open'"),
        0
    );
    let (restored, failed) = confirm(&server, &other, &restore).await;
    assert!(!failed, "{restored}");
    assert_eq!(restored["result"]["draft"]["base"]["revision_id"], 1);
    let draft = restored["result"]["draft"]["draft_id"].as_i64().unwrap();
    let state: (i64, String, String, i64, i64) = db.query_row(
        "SELECT b.accepted_plan_revision_id,b.name,h.name,i.source_revision_id,p.base_revision_part_id
           FROM build_profiles b
           JOIN plan_drafts d ON d.profile_id=b.id
           JOIN plan_draft_headers h ON h.draft_id=d.id
           JOIN plan_draft_inputs i ON i.draft_id=d.id
           JOIN plan_draft_parts p ON p.draft_id=d.id
          WHERE d.id=?1 AND p.part_key='bracket.stl'",
        [draft], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).unwrap();
    assert_eq!(
        state,
        (1, "Renamed later".into(), "Fixture Build".into(), 1, 1)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_is_detached_and_archive_retains_history() {
    let (server, headers, db) = setup().await;
    let call = |name, args| tool(&server, &headers, name, args);
    let (duplicate, failed) =
        call("duplicate_plan", json!({"plan_id":1,"name":"Experiment"})).await;
    assert!(!failed, "{duplicate}");
    assert_eq!(count(&db, "SELECT count(*) FROM build_experiments"), 0);
    let (copy, failed) = confirm(&server, &headers, &duplicate).await;
    assert!(!failed, "{copy}");
    let experiment = copy["result"]["build_id"].as_i64().unwrap();
    let (parent, accepted, detached, source, progress): (i64, Option<i64>, Option<i64>, i64, i64) = db.query_row(
        "SELECT e.parent_build_id,b.accepted_plan_revision_id,p.base_revision_part_id,i.source_revision_id,
                (SELECT COUNT(*) FROM print_progress g JOIN parts x ON x.id=g.part_id WHERE x.profile_id=b.id)
           FROM build_experiments e JOIN build_profiles b ON e.build_id=b.id
           JOIN plan_drafts d ON d.profile_id=b.id JOIN plan_draft_parts p ON p.draft_id=d.id
           JOIN plan_draft_inputs i ON i.draft_id=d.id
          WHERE b.id=?1 AND p.part_key='bracket.stl'",
        [experiment], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    ).unwrap();
    assert_eq!(
        (parent, accepted, detached, source, progress),
        (1, None, None, 1, 0)
    );

    let (remaining, failed) = call("archive_plan", json!({"plan_id":1})).await;
    assert!(failed, "{remaining}");
    assert_eq!(remaining["code"], "print_remaining");
    db.execute_batch("UPDATE print_progress SET completed=1")
        .unwrap();
    let (archive, failed) = call("archive_plan", json!({"plan_id":1})).await;
    assert!(!failed, "{archive}");
    db.execute_batch("UPDATE print_progress SET completed=0 WHERE id=1")
        .unwrap();
    let (remaining, failed) = confirm(&server, &headers, &archive).await;
    assert!(failed, "{remaining}");
    assert_eq!(remaining["code"], "print_remaining");
    db.execute_batch("UPDATE print_progress SET completed=1")
        .unwrap();
    let (archived, failed) = confirm(&server, &headers, &archive).await;
    assert!(!failed, "{archived}");
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM build_profiles WHERE id=1 AND archived_at IS NOT NULL AND accepted_plan_revision_id=1"
        ),
        1
    );
    assert_eq!(
        count(
            &db,
            "SELECT count(*) FROM plan_revisions WHERE id=1 AND profile_id=1"
        ),
        1
    );
    assert_eq!(count(&db, "SELECT count(*) FROM build_experiments"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_history_proposals_are_tenant_scoped_and_refuse_a_moved_base() {
    let (server, headers, db) = setup().await;
    let call = |name, args| tool(&server, &headers, name, args);
    for (name, fields) in [
        ("create_plan_snapshot", json!({})),
        ("propose_restore_snapshot", json!({"snapshot_id":1})),
        ("duplicate_plan", json!({"name":"Experiment"})),
        ("archive_plan", json!({})),
    ] {
        let mut absent = fields.clone();
        absent["plan_id"] = json!(99);
        let mut foreign = fields;
        foreign["plan_id"] = json!(2);
        let refusal = call(name, foreign.clone()).await;
        assert_eq!(refusal, call(name, absent).await);
        assert!(refusal.1, "{name}");
        assert_eq!(refusal.0["code"], "plan_not_found");
        assert_eq!(
            server.unkeyed_call_status(&headers, name, foreign).await,
            401
        );
    }
    let (snapshot, failed) = call("create_plan_snapshot", json!({"plan_id":1})).await;
    assert!(!failed, "{snapshot}");
    db.execute_batch("UPDATE build_profiles SET accepted_plan_version=2 WHERE id=1")
        .unwrap();
    let (stale, failed) = confirm(&server, &headers, &snapshot).await;
    assert!(failed, "{stale}");
    assert_eq!(stale["code"], "base_changed");
    assert_eq!(count(&db, "SELECT count(*) FROM plan_snapshots"), 0);
}
