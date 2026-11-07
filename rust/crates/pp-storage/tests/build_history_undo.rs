#![cfg(feature = "desktop_rebuild")]
//! GRE-1259: History lists manual Checkoff bursts beside printer bursts, each
//! with the undo state its own undo would answer now, and
//! `POST /plans/{id}/history/{entry_id}/undo` runs that undo. Runs on a copy of
//! the required-units fixture with the verify parity fixture's links.
use checkoff_apply::apply;
use checkoff_fixture::Fixture;
use pp_storage::{
    WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    build_graph::PositiveId,
    checkoff_progress::Request,
    read_model::{
        Credential,
        history::{Outcome, Request as HistoryRequest},
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "common/accepted_basis.rs"]
mod accepted_basis;
#[path = "common/checkoff_apply.rs"]
mod checkoff_apply;
#[path = "common/checkoff_fixture.rs"]
mod checkoff_fixture;
#[path = "common/js_json.rs"]
mod js_json;
#[path = "common/json_rows.rs"]
mod json_rows;

const SECRET: &str = "required-unit-fixture-secret";

fn credential() -> Credential {
    Credential::Session(Secret::new(SECRET.into()))
}

/// The Build's checkoff bursts as History serves them, newest first.
fn bursts(owner: &WriterOwner) -> Vec<Value> {
    let policy = AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    };
    let request = HistoryRequest {
        build: Some(PositiveId::new(1).unwrap()),
        cursor: None,
        limit: 200,
    };
    let read = owner.build_history(policy).unwrap().read(
        credential(),
        request,
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(5),
    );
    let Outcome::Page(page) = read.unwrap() else {
        panic!("Build 1 missing")
    };
    page.entries
        .iter()
        .map(|entry| serde_json::to_value(entry).unwrap())
        .filter(|entry| entry["type"] == "checkoff_burst")
        .collect()
}

fn undo(owner: &WriterOwner, entry: &str) -> Value {
    apply(
        owner,
        Request::UndoHistoryEntry {
            profile_id: 1,
            entry_id: entry.into(),
        },
    )
}

/// A manual burst on Build 1 setting `printed` on unit `unit` of Part 1.
fn manual(owner: &WriterOwner, key: &str, unit: u8, printed: bool) -> String {
    let body = json!({ "idempotency_key": key, "expected": accepted_basis::basis(owner), "units": [
        { "token": format!("ppu_{unit:032x}"), "printed": printed },
    ] });
    let done = apply(
        owner,
        Request::ManualBurst {
            profile_id: 1,
            body,
        },
    );
    assert_eq!(done["status"], 200, "{done}");
    format!(
        "checkoff_burst:manual-{}",
        hex::encode(Sha256::digest(key.as_bytes()))
    )
}

/// A verify-batch burst confirming Part 2 unit 0 on `link-burst`; returns its
/// History id, the job its outcome wrote.
fn printer(fixture: &Fixture, owner: &WriterOwner) -> String {
    let body = json!({ "idempotency_key": "printer", "beds": [{ "link_id": "link-burst",
        "decisions": [{ "part_id": 2, "unit_index": 0, "result": "confirmed" }] }] });
    let done = apply(owner, Request::VerifyBatch { body });
    assert_eq!(done["status"], 200, "{done}");
    let job: String = fixture
        .sql()
        .query_row(
            "SELECT id FROM print_jobs WHERE link_id='link-burst'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    format!("checkoff_burst:{job}")
}

/// Opens storage and records a printer burst, then a manual one: the printer
/// burst re-saves every stored link, so it goes first. Returns their History ids.
fn two_bursts(fixture: &Fixture) -> (WriterOwner, String, String) {
    let owner = fixture.open();
    let printer_id = printer(fixture, &owner);
    let manual_id = manual(&owner, "fill", 3, true);
    (owner, printer_id, manual_id)
}

fn find<'a>(entries: &'a [Value], id: &str) -> &'a Value {
    entries
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("{id} not in {entries:#?}"))
}

fn stored(fixture: &Fixture) -> Value {
    let queries = json!({
        "settings": "SELECT tenant_id, key, value FROM app_settings ORDER BY tenant_id, key",
        "progress": "SELECT * FROM print_progress ORDER BY rowid",
        "jobs": "SELECT * FROM print_jobs ORDER BY rowid",
        "outcomes": "SELECT * FROM print_job_parts ORDER BY rowid",
        "events": "SELECT * FROM domain_events ORDER BY rowid",
    });
    json_rows::query_rows(&fixture.sql(), queries.as_object().unwrap())
}

#[test]
fn build_history_lists_manual_and_printer_bursts_with_live_undo() {
    let fixture = Fixture::new();
    let (owner, printer_id, manual_id) = two_bursts(&fixture);
    let before = stored(&fixture);
    let listed = bursts(&owner);
    assert_eq!(stored(&fixture), before, "the read writes nothing");
    let entry = find(&listed, &manual_id);
    assert_eq!(entry["summary"], "Checked off 2 units", "{entry}");
    assert_eq!(
        entry["link"],
        json!({"kind": "checkoff_burst", "id": manual_id.trim_start_matches("checkoff_burst:")})
    );
    assert_eq!(
        entry["undo"],
        json!({"undoable": true}),
        "reason and detail are omitted"
    );
    let entry = find(&listed, &printer_id);
    assert_eq!(
        entry["summary"], "Checked off 1 unit from plate-f.gcode",
        "{entry}"
    );
    assert_eq!(entry["undo"], json!({"undoable": true}));

    let later = manual(&owner, "clear", 2, false);
    let changed = "Checkoff changed since this burst: a later change on its Parts depends on it";
    let listed = bursts(&owner);
    assert_eq!(
        find(&listed, &manual_id)["undo"],
        json!({"undoable": false, "reason": "state_changed", "detail": changed})
    );
    assert_eq!(find(&listed, &later)["summary"], "Unchecked 2 units");
    assert_eq!(find(&listed, &later)["undo"], json!({"undoable": true}));

    fixture
        .sql()
        .execute(
            "UPDATE build_profiles SET archived_at='2026-11-06T00:00:00.000Z' WHERE id=1",
            [],
        )
        .unwrap();
    let archived = json!({"undoable": false, "reason": "archived",
        "detail": "Archived Plan Progress cannot be changed"});
    let listed = bursts(&owner);
    assert_eq!(find(&listed, &later)["undo"], archived);
    assert_eq!(find(&listed, &printer_id)["undo"], archived);
    owner.shutdown().unwrap();
}

#[test]
fn build_history_undo_runs_the_burst_undo_and_reports_already_undone() {
    let fixture = Fixture::new();
    let (owner, printer_id, manual_id) = two_bursts(&fixture);
    let events = |fixture: &Fixture| -> i64 {
        fixture
            .sql()
            .query_row(
                "SELECT count(*) FROM domain_events WHERE event_type='checkoff.progress_changed'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    };
    let before = events(&fixture);
    let undone = undo(&owner, &manual_id);
    assert_eq!(undone["status"], 200, "{undone}");
    assert!(undone["body"]["undone_at"].as_str().unwrap().ends_with('Z'));
    let already = json!({"undoable": false, "reason": "already_undone",
        "detail": "This Checkoff burst was already undone"});
    assert_eq!(undone["body"]["entry"]["id"], manual_id.as_str());
    assert_eq!(undone["body"]["entry"]["undo"], already);
    assert!(
        events(&fixture) > before,
        "the undo records the Part change"
    );
    assert_eq!(find(&bursts(&owner), &manual_id)["undo"], already);
    assert_eq!(
        undo(&owner, &manual_id),
        json!({"status": 409, "body": {"detail": "This Checkoff burst was already undone"}})
    );

    let undone = undo(&owner, &printer_id);
    assert_eq!(undone["status"], 200, "{undone}");
    assert_eq!(
        undone["body"]["entry"],
        Value::Null,
        "the undo deleted the job"
    );
    assert!(
        bursts(&owner).iter().all(|entry| entry["id"] != printer_id),
        "a printer burst leaves History with its job"
    );
    let log = apply(
        &owner,
        Request::FarmLog {
            cursor: None,
            limit: None,
        },
    );
    assert_eq!(
        log["body"]["entries"][0]["kind"], "checkoff_burst_undo",
        "the farm log records the undo as the farm log route would"
    );
    owner.shutdown().unwrap();
}

#[test]
fn build_history_burst_without_a_kept_receipt_has_no_undo() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let printer_id = printer(&fixture, &owner);
    fixture
        .sql()
        .execute(
            "UPDATE app_settings SET value='[]' WHERE key='printer.checkoff_bursts'",
            [],
        )
        .unwrap();
    let none = "No undo is recorded for this Checkoff burst";
    assert_eq!(
        find(&bursts(&owner), &printer_id)["undo"],
        json!({"undoable": false, "reason": "no_undo_token", "detail": none})
    );
    let before = stored(&fixture);
    assert_eq!(
        undo(&owner, &printer_id),
        json!({"status": 409, "body": {"detail": none}})
    );
    assert_eq!(stored(&fixture), before, "a refusal writes nothing");
    owner.shutdown().unwrap();
}

#[test]
fn build_history_printer_undo_reverses_every_bed_of_its_burst() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let bed = |link: &str, part: i64| {
        json!({ "link_id": link,
        "decisions": [{ "part_id": part, "unit_index": 0, "result": "confirmed" }] })
    };
    let body = json!({ "idempotency_key": "two-beds",
        "beds": [bed("link-burst", 2), bed("link-verify", 3)] });
    let done = apply(&owner, Request::VerifyBatch { body });
    assert_eq!(done["status"], 200, "{done}");
    let jobs: Vec<String> = {
        let db = fixture.sql();
        let mut statement = db.prepare("SELECT 'checkoff_burst:' || id FROM print_jobs WHERE link_id IN ('link-burst','link-verify')").unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    assert_eq!(jobs.len(), 2, "one job per bed");
    let listed = bursts(&owner);
    for id in &jobs {
        assert_eq!(
            find(&listed, id)["undo"],
            json!({"undoable": true, "scope": "Undo reverses the whole burst: 2 beds on Build 1"})
        );
    }
    let undone = undo(&owner, &jobs[0]);
    assert_eq!(undone["status"], 200, "{undone}");
    let listed = bursts(&owner);
    assert!(
        jobs.iter()
            .all(|id| listed.iter().all(|entry| entry["id"] != id.as_str())),
        "one undo removes both beds' entries"
    );
    owner.shutdown().unwrap();
}
