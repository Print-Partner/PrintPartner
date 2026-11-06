#![cfg(test)]

use super::*;
use serde_json::json;

struct Fixture {
    root: PathBuf,
    connection: Connection,
}

impl Fixture {
    fn new(sql: &str) -> Self {
        let root = std::env::temp_dir().join(format!("pp-capture-{}", rand::random::<u64>()));
        std::fs::create_dir_all(root.join("repos/1/revisions/accepted")).unwrap();
        let db = root.join("fixture.db");
        std::fs::write(
            &db,
            include_bytes!("../../tests/fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        let connection = Connection::open(&db).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        let triggers = connection
            .prepare("SELECT name FROM sqlite_master WHERE type='trigger'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        for trigger in triggers {
            connection
                .execute_batch(&format!("DROP TRIGGER \"{trigger}\";"))
                .unwrap();
        }
        connection
            .execute_batch(include_str!("../send_queue/schema.sql"))
            .unwrap();
        connection.execute_batch(sql).unwrap();
        Self { root, connection }
    }

    fn capture(&mut self, profile: i64, minimum: u64) -> Result<(Box<Snapshot>, CapturedContext)> {
        let repos = self.root.join("repos");
        let tx = self.connection.transaction().unwrap();
        capture_published(&tx, "default", profile, &repos, minimum)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Windows cannot delete a database file while a connection holds it.
        drop(std::mem::replace(
            &mut self.connection,
            Connection::open_in_memory().unwrap(),
        ));
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn capture_published_returns_snapshot_and_context_at_receipt_version() {
    let mut f = Fixture::new("");
    let (snapshot, context) = f.capture(1, 1).unwrap();
    assert_eq!((snapshot.revision_id, snapshot.plan_version), (1, 1));
    assert_eq!(
        context.profile_summary_v2["accepted_progress"],
        json!({"kind":"ready","total_units":1,"remaining_units":1})
    );
    assert_eq!(context.workflow["kind"], "ready");
}

#[test]
fn capture_published_refuses_snapshot_older_than_receipt() {
    let mut f = Fixture::new("");
    for minimum in [2, u64::MAX] {
        let error = f.capture(1, minimum).unwrap_err();
        assert_eq!(error.to_string(), "Accepted capture predates Save receipt");
    }
}

#[test]
fn capture_published_refuses_missing_build_and_unready_plan() {
    let mut f = Fixture::new("");
    let error = f.capture(2, 0).unwrap_err();
    assert_eq!(error.to_string(), "Published Build context missing");

    let mut f = Fixture::new("UPDATE build_profiles SET accepted_plan_revision_id=NULL");
    let error = f.capture(1, 0).unwrap_err();
    assert_eq!(
        error.to_string(),
        "Published accepted authority unavailable"
    );
}

#[test]
fn capture_published_propagates_accepted_integrity_failure() {
    let mut f = Fixture::new("UPDATE plan_revisions SET snapshot_digest='corrupt'");
    let error = f.capture(1, 0).unwrap_err();
    assert_eq!(
        error.downcast_ref::<graph::Integrity>().unwrap().code,
        "revision_digest"
    );
}

// Part 1 is excluded and part 2 is included, one unit each.
#[test]
fn accepted_progress_counts_included_incomplete_units() {
    let mut f = Fixture::new("UPDATE print_progress SET completed=1 WHERE part_id=2");
    let (_, context) = f.capture(1, 1).unwrap();
    assert_eq!(
        context.profile_summary_v2["accepted_progress"],
        json!({"kind":"ready","total_units":1,"remaining_units":0})
    );
    let profile = &context.profile_summary_v1["profile"];
    assert_eq!(
        (&profile["total_units"], &profile["remaining_units"]),
        (&json!(1), &json!(0))
    );

    let mut f = Fixture::new("UPDATE print_progress SET completed=1 WHERE part_id=1");
    let (_, context) = f.capture(1, 1).unwrap();
    assert_eq!(
        context.profile_summary_v2["accepted_progress"],
        json!({"kind":"ready","total_units":1,"remaining_units":1})
    );
}
