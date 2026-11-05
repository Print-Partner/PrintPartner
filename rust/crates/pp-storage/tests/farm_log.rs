#![cfg(feature = "desktop_rebuild")]
//! GRE-1126: the farm log on disposable on-disk SQLite. Bursts and undos
//! append durable entries in their own transactions, a replay appends none,
//! sends never offer undo, and pages are stable across a restart.
use checkoff_fixture::Fixture;
use pp_storage::{WriterOwner, auth::Secret, checkoff_progress::Request, read_model::Credential};
use serde_json::{Value, json};
use std::{sync::atomic::AtomicBool, time::Duration};

#[path = "common/checkoff_fixture.rs"]
mod checkoff_fixture;
#[path = "common/js_json.rs"]
mod js_json;

fn apply(owner: &WriterOwner, request: Request) -> Value {
    let response = owner
        .checkoff_progress()
        .unwrap()
        .apply(
            Credential::Session(Secret::new("required-unit-fixture-secret".into())),
            request,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    serde_json::to_value(response).unwrap()
}

fn page(owner: &WriterOwner, cursor: Option<&str>, limit: Option<&str>) -> Value {
    apply(
        owner,
        Request::FarmLog {
            cursor: cursor.map(str::to_owned),
            limit: limit.map(str::to_owned),
        },
    )
}

fn entries(owner: &WriterOwner) -> Vec<Value> {
    let page = page(owner, None, None);
    assert_eq!(page["status"], 200, "{page}");
    page["body"]["entries"].as_array().unwrap().clone()
}

fn confirm(part: i64, unit: i64) -> Value {
    json!({ "part_id": part, "unit_index": unit, "result": "confirmed" })
}

fn burst(owner: &WriterOwner, key: &str, bed: Value) -> Value {
    let done = apply(
        owner,
        Request::VerifyBatch {
            body: json!({ "idempotency_key": key, "beds": [bed] }),
        },
    );
    assert_eq!(done["status"], 200, "{done}");
    done
}

fn undo_entry(owner: &WriterOwner, id: &str) -> Value {
    apply(owner, Request::UndoFarmLogEntry { id: id.into() })
}

fn seed_sends(fixture: &Fixture) {
    let queue = json!([
        {"id": "send-1", "filename": "plate.gcode", "artifact_path": "/disposable/plate.gcode",
         "printer_id": "printer-1", "profile_id": 1, "state": "done",
         "created_at": "2020-01-01T00:00:00.000Z"},
        {"id": "send-2", "filename": "plate.gcode", "artifact_path": "/disposable/plate.gcode",
         "printer_id": "printer-1", "host_name": " Bench MK4 ", "profile_id": 1, "state": "cancelled",
         "created_at": "2020-01-01T00:01:00.000Z"}
    ]);
    seed_queue(fixture, &queue.to_string());
}

fn seed_queue(fixture: &Fixture, raw: &str) {
    fixture
        .sql()
        .execute(
            "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'printer.send_queue', ?1)
             ON CONFLICT (tenant_id, key) DO UPDATE SET value=excluded.value",
            [raw],
        )
        .unwrap();
}

fn progress_events(fixture: &Fixture) -> i64 {
    fixture
        .sql()
        .query_row(
            "SELECT count(*) FROM domain_events WHERE event_type='checkoff.progress_changed'
             AND payload_json='{\"part_id\":2}'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn farm_log_reads_an_unparseable_send_queue_as_empty() {
    let fixture = Fixture::new();
    seed_queue(&fixture, "not json");
    let owner = fixture.open();
    let bed = json!({ "link_id": "link-burst", "decisions": [confirm(2, 0)] });
    burst(&owner, "malformed-queue", bed);
    assert_eq!(
        entries(&owner)
            .iter()
            .map(|e| e["id"].clone())
            .collect::<Vec<_>>(),
        vec![json!("checkoff_burst:1")],
        "bursts still list when the send queue does not parse"
    );
    assert_eq!(undo_entry(&owner, "send:send-1")["status"], 404);
}

#[test]
fn farm_log_sends_follow_their_state_and_refuse_undo() {
    let fixture = Fixture::new();
    seed_sends(&fixture);
    let owner = fixture.open();
    assert_eq!(
        entries(&owner)
            .iter()
            .map(|e| (e["summary"].clone(), e["send_state"].clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                json!("Cancelled send of plate.gcode to Bench MK4"),
                json!("cancelled")
            ),
            (json!("Sent plate.gcode to printer-1"), json!("done")),
        ],
        "a send's summary follows its state"
    );
    assert_eq!(
        undo_entry(&owner, "send:send-1"),
        json!({"status": 409, "body": {"detail": "A printer command cannot be undone"}})
    );
}

#[test]
fn farm_log_records_bursts_and_undo_once_and_survives_restart() {
    let fixture = Fixture::new();
    seed_sends(&fixture);
    let owner = fixture.open();
    assert_eq!(
        entries(&owner)
            .iter()
            .map(|e| (e["id"].clone(), e["undo"].clone()))
            .collect::<Vec<_>>(),
        vec![
            (
                json!("send:send-2"),
                json!({"undoable": false, "reason": "printer_command", "detail": "A printer command cannot be undone"})
            ),
            (
                json!("send:send-1"),
                json!({"undoable": false, "reason": "printer_command", "detail": "A printer command cannot be undone"})
            ),
        ],
        "dispatch and cancel are listed without undo"
    );

    let bed = json!({ "link_id": "link-burst", "decisions": [confirm(2, 0)] });
    let done = burst(&owner, "k1", bed.clone());
    assert_eq!(
        burst(&owner, "k1", bed),
        done,
        "a replay returns the receipt"
    );
    let logged = entries(&owner);
    assert_eq!(logged.len(), 3, "the replay appended no entry: {logged:#?}");
    let first = &logged[0];
    assert_eq!(first["id"], "checkoff_burst:1");
    assert_eq!(first["kind"], "checkoff_burst");
    assert_eq!(first["units_confirmed"], 1);
    assert_eq!(first["build_ids"], json!([1]));
    assert_eq!(
        first["undo"],
        json!({"undoable": true, "reason": null, "detail": null})
    );

    owner.shutdown().unwrap();
    let owner = fixture.open();
    assert_eq!(
        entries(&owner),
        logged,
        "the log reads the same after a restart"
    );

    let before = progress_events(&fixture);
    let undone = undo_entry(&owner, "checkoff_burst:1");
    assert_eq!(undone["status"], 200, "{undone}");
    assert_eq!(
        progress_events(&fixture),
        before + 1,
        "the undo by entry records the Part change in the domain event log"
    );
    assert_eq!(undone["body"]["id"], "checkoff_burst_undo:2");
    assert_eq!(undone["body"]["undoes"], "checkoff_burst:1");
    assert_eq!(undone["body"]["undo"]["reason"], "undo_entry");
    owner.shutdown().unwrap();
    let owner = fixture.open();
    let after = entries(&owner);
    assert_eq!(after.len(), 4, "the undo appended one durable entry");
    assert_eq!(after[0]["id"], "checkoff_burst_undo:2");
    assert_eq!(
        after[1]["undo"],
        json!({"undoable": false, "reason": "already_undone", "detail": "This Checkoff burst was already undone"})
    );
    assert_eq!(
        undo_entry(&owner, "checkoff_burst:1"),
        json!({"status": 409, "body": {"detail": "This Checkoff burst was already undone"}})
    );
    assert_eq!(
        undo_entry(&owner, "checkoff_burst_undo:2"),
        json!({"status": 409, "body": {"detail": "An undo cannot be undone"}})
    );
    for unknown in [
        "checkoff_burst:99",
        "checkoff_burst_undo:99",
        "send:missing",
    ] {
        assert_eq!(
            undo_entry(&owner, unknown)["status"],
            404,
            "{unknown} is not in the log"
        );
    }
    assert_eq!(entries(&owner).len(), 4, "refusals append nothing");
}

#[test]
fn farm_log_refuses_undo_when_touched_state_changed() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    burst(
        &owner,
        "k",
        json!({ "link_id": "link-verify", "decisions": [confirm(3, 0)] }),
    );
    let later = apply(
        &owner,
        Request::Verify {
            body: json!({ "link_id": "link-burst", "decisions": [confirm(3, 1)] }),
        },
    );
    assert_eq!(later["status"], 200, "{later}");
    let reason = "Checkoff changed since this burst: a later verify or progress change on its Parts depends on it";
    assert_eq!(
        entries(&owner)[0]["undo"],
        json!({"undoable": false, "reason": "state_changed", "detail": reason})
    );
    assert_eq!(
        undo_entry(&owner, "checkoff_burst:1"),
        json!({"status": 409, "body": {"detail": reason}})
    );
    assert_eq!(entries(&owner).len(), 1, "the refusal appended nothing");
}

#[test]
fn farm_log_pages_newest_first_with_a_stable_cursor() {
    let fixture = Fixture::new();
    seed_sends(&fixture);
    let owner = fixture.open();
    burst(
        &owner,
        "k",
        json!({ "link_id": "link-burst", "decisions": [confirm(2, 0)] }),
    );
    let all: Vec<Value> = entries(&owner).iter().map(|e| e["id"].clone()).collect();
    let mut walked = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = page(&owner, cursor.as_deref(), Some("1"));
        assert_eq!(page["status"], 200, "{page}");
        walked.extend(
            page["body"]["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["id"].clone()),
        );
        match page["body"]["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(walked, all);
    for (cursor, limit, detail) in [
        (None, Some("0"), "limit must be 1 to 200"),
        (None, Some("201"), "limit must be 1 to 200"),
        (Some("zz"), None, "cursor is invalid"),
    ] {
        assert_eq!(
            page(&owner, cursor, limit),
            json!({"status": 400, "body": {"detail": detail}})
        );
    }
}
