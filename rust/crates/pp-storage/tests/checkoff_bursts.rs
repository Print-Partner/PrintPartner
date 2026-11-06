#![cfg(feature = "desktop_rebuild")]
//! GRE-1090: one Checkoff burst over several beds is one transaction with an
//! idempotency key and an undo token. Runs on a copy of the required-units
//! fixture with the synthetic beds of the printer Checkoff verify parity fixture.
use checkoff_apply::apply;
use checkoff_fixture::Fixture;
use pp_storage::{
    WriterOwner,
    checkoff_progress::{Basis, Request},
};
use serde_json::{Value, json};

#[path = "common/checkoff_apply.rs"]
mod checkoff_apply;
#[path = "common/checkoff_fixture.rs"]
mod checkoff_fixture;
#[path = "common/js_json.rs"]
mod js_json;
#[path = "common/json_rows.rs"]
mod json_rows;

impl Fixture {
    /// Every row a burst can write, except its own receipt.
    fn checkoff(&self) -> Value {
        let queries = json!({
            "progress": "SELECT part_id, unit_index, completed, assembled FROM print_progress ORDER BY part_id, unit_index",
            "units": "SELECT token, profile_id, object_name FROM required_units ORDER BY token",
            "jobs": "SELECT * FROM print_jobs ORDER BY rowid",
            "parts": "SELECT * FROM print_job_parts ORDER BY rowid",
            "links": "SELECT tenant_id, value FROM app_settings WHERE key = 'printer.checkoff_links' ORDER BY tenant_id",
        });
        json_rows::query_rows(&self.sql(), queries.as_object().unwrap())
    }
}

fn batch(owner: &WriterOwner, body: Value) -> Value {
    apply(owner, Request::VerifyBatch { body })
}

fn undo(owner: &WriterOwner, token: &Value) -> Value {
    apply(
        owner,
        Request::UndoBatch {
            body: json!({ "undo_token": token }),
        },
    )
}

fn confirm(part: i64, unit: i64) -> Value {
    json!({ "part_id": part, "unit_index": unit, "result": "confirmed" })
}

/// Two beds: the plate on `link-verify` and Part 2 on `link-burst`.
fn two_beds(key: &str) -> Value {
    json!({ "idempotency_key": key, "beds": [
        { "link_id": "link-verify", "decisions": [confirm(1, 1), confirm(1, 2)] },
        { "link_id": "link-burst", "decisions": [confirm(2, 0), { "part_id": 3, "unit_index": 1, "result": "rejected", "reason": "warping" }] },
    ] })
}

fn printed(rows: &Value) -> Vec<(i64, i64)> {
    rows["progress"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["completed"] == 1)
        .map(|r| {
            (
                r["part_id"].as_i64().unwrap(),
                r["unit_index"].as_i64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn verify_batch_commits_every_bed_or_none() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let before = fixture.checkoff();
    let refused = batch(
        &owner,
        json!({ "idempotency_key": "k1", "beds": [
        { "link_id": "link-verify", "decisions": [confirm(1, 1)] },
        { "link_id": "link-burst", "decisions": [confirm(2, 0), confirm(3, 1)] },
    ] }),
    );
    assert_eq!(refused["status"], 400);
    assert_eq!(
        refused["body"]["detail"],
        "beds[1]: Confirm must include lower incomplete units first (Progress fills from the left)"
    );
    assert_eq!(fixture.checkoff(), before, "the first bed is not kept");

    fixture
        .sql()
        .execute_batch(
            "CREATE TRIGGER injected_fault BEFORE INSERT ON print_job_parts WHEN NEW.part_id = 2
             BEGIN SELECT RAISE(ABORT, 'injected fault'); END;",
        )
        .unwrap();
    let failed = batch(&owner, two_beds("k1"));
    assert_eq!(
        failed,
        json!({ "status": 500, "body": { "detail": "Internal Server Error" } })
    );
    assert_eq!(
        fixture.checkoff(),
        before,
        "a fault in the second bed leaves no record of the first"
    );
    fixture
        .sql()
        .execute_batch("DROP TRIGGER injected_fault")
        .unwrap();

    let done = batch(&owner, two_beds("k1"));
    assert_eq!(done["status"], 200, "{done}");
    assert_eq!(done["body"]["units_confirmed"], 3);
    assert_eq!(done["body"]["units_rejected"], 1);
    assert_eq!(done["body"]["beds"].as_array().unwrap().len(), 2);
    let after = fixture.checkoff();
    assert_eq!(printed(&after), [(1, 0), (1, 1), (1, 2), (2, 0)]);
    assert_eq!(after["jobs"].as_array().unwrap().len(), 2);
    assert_eq!(after["parts"].as_array().unwrap().len(), 4);
    owner.shutdown().unwrap();
}

#[test]
fn verify_batch_replays_the_receipt_and_refuses_a_changed_body() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let first = batch(&owner, two_beds("phone-retry"));
    assert_eq!(first["status"], 200);
    let rows = fixture.checkoff();
    // The beds are no longer awaiting verify, yet the original retry still
    // gets its receipt instead of a stale refusal.
    assert_eq!(batch(&owner, two_beds("phone-retry")), first);
    let mut reordered = two_beds("phone-retry");
    reordered = json!({ "beds": reordered["beds"], "idempotency_key": "phone-retry" });
    assert_eq!(
        batch(&owner, reordered),
        first,
        "key order does not change the request"
    );
    owner.shutdown().unwrap();
    let owner = fixture.open();
    assert_eq!(
        batch(&owner, two_beds("phone-retry")),
        first,
        "the receipt survives a restart"
    );
    assert_eq!(fixture.checkoff(), rows, "replays write nothing");
    let mut changed = two_beds("phone-retry");
    changed["beds"][0]["decisions"] = json!([confirm(1, 1)]);
    assert_eq!(
        batch(&owner, changed),
        json!({ "status": 409, "body": { "detail": "idempotency_key was already used for a different request" } })
    );
    assert_eq!(fixture.checkoff(), rows);
    owner.shutdown().unwrap();
}

#[test]
fn verify_batch_undo_restores_the_prior_state_or_refuses_with_a_reason() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    // Any link write stores the links as Node normalizes them; start from that.
    let dismissed = apply(
        &owner,
        Request::Dismiss {
            body: json!({ "link_id": "link-host-failed" }),
        },
    );
    assert_eq!(dismissed["status"], 200);
    let before = fixture.checkoff();
    let done = batch(&owner, two_beds("undo-me"));
    let token = done["body"]["undo_token"].clone();
    assert!(token.as_str().unwrap().starts_with("ppb_"));
    owner.shutdown().unwrap();
    let owner = fixture.open();
    let undone = undo(&owner, &token);
    assert_eq!(undone["status"], 200, "{undone}");
    assert_eq!(
        fixture.checkoff(),
        before,
        "undo restores the exact prior Checkoff state"
    );
    assert_eq!(
        undo(&owner, &token)["body"]["detail"],
        "This Checkoff burst was already undone"
    );
    assert_eq!(
        batch(&owner, two_beds("undo-me")),
        done,
        "a late retry does not apply the burst again"
    );
    assert_eq!(fixture.checkoff(), before);
    assert_eq!(undo(&owner, &json!("ppb_missing"))["status"], 404);

    let link_changed = batch(
        &owner,
        json!({ "idempotency_key": "a", "beds": [
        { "link_id": "link-verify", "decisions": [confirm(1, 1)] },
    ] }),
    );
    let verified = apply(
        &owner,
        Request::Verify {
            body: json!({ "link_id": "link-verify", "decisions": [confirm(1, 2)] }),
        },
    );
    assert_eq!(verified["status"], 200);
    let rows = fixture.checkoff();
    assert_eq!(
        undo(&owner, &link_changed["body"]["undo_token"])["body"]["detail"],
        "Checkoff link link-verify changed since this burst"
    );
    assert_eq!(fixture.checkoff(), rows, "a refused undo writes nothing");

    let progress_changed = batch(
        &owner,
        json!({ "idempotency_key": "b", "beds": [
        { "link_id": "link-burst", "decisions": [confirm(2, 0)] },
    ] }),
    );
    fixture
        .sql()
        .execute(
            "UPDATE print_progress SET assembled=1 WHERE part_id=2 AND unit_index=0",
            [],
        )
        .unwrap();
    assert_eq!(
        undo(&owner, &progress_changed["body"]["undo_token"]),
        json!({ "status": 409, "body": { "detail": "Progress for Part 2 unit 0 changed since this burst" } })
    );
    owner.shutdown().unwrap();
}

#[test]
fn verify_batch_undo_refuses_after_a_later_checkoff_on_another_link() {
    let later: [(&str, Value, Request); 2] = [
        (
            "a second link records the printed unit",
            json!([{ "link_id": "link-verify", "decisions": [confirm(1, 1)] }]),
            Request::Verify {
                body: json!({ "link_id": "link-stale-file", "decisions": [confirm(1, 1)] }),
            },
        ),
        (
            "a later unit fills from the burst's unit",
            json!([{ "link_id": "link-verify", "decisions": [confirm(3, 0)] }]),
            Request::Verify {
                body: json!({ "link_id": "link-burst", "decisions": [confirm(3, 1)] }),
            },
        ),
    ];
    for (case, beds, request) in later {
        let fixture = Fixture::new();
        let owner = fixture.open();
        let done = batch(&owner, json!({ "idempotency_key": "k", "beds": beds }));
        assert_eq!(done["status"], 200, "{case}: {done}");
        let applied = apply(&owner, request);
        assert_eq!(applied["status"], 200, "{case}: {applied}");
        let rows = fixture.checkoff();
        assert_eq!(
            undo(&owner, &done["body"]["undo_token"]),
            json!({ "status": 409, "body": { "detail": "Checkoff changed since this burst: a later verify or progress change on its Parts depends on it" } }),
            "{case}"
        );
        assert_eq!(
            fixture.checkoff(),
            rows,
            "{case}: a refused undo writes nothing"
        );
        owner.shutdown().unwrap();
    }
}

#[test]
fn verify_batch_undo_ignores_later_activity_that_does_not_touch_its_parts() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let before = fixture.checkoff();
    let done = batch(
        &owner,
        json!({ "idempotency_key": "k", "beds": [{ "link_id": "link-burst", "decisions": [confirm(2, 0)] }] }),
    );
    assert_eq!(done["status"], 200, "{done}");
    let dismissed = apply(
        &owner,
        Request::Dismiss {
            body: json!({ "link_id": "link-host-failed" }),
        },
    );
    assert_eq!(dismissed["status"], 200);
    fixture
        .sql()
        .execute(
            "INSERT INTO print_jobs (id, tenant_id, profile_id, filename, status, at) VALUES ('unrelated-send', 'default', 1, 'other.gcode', 'printing', '2020-01-03T00:00:00.000Z')",
            [],
        )
        .unwrap();
    let undone = undo(&owner, &done["body"]["undo_token"]);
    assert_eq!(undone["status"], 200, "{undone}");
    let after = fixture.checkoff();
    assert_eq!(
        after["progress"], before["progress"],
        "Part 2 unit 0 is pending again"
    );
    assert_eq!(
        after["parts"], before["parts"],
        "the burst's outcome is gone"
    );
    owner.shutdown().unwrap();
}

#[test]
fn verify_batch_undo_refuses_on_an_archived_build() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let done = batch(&owner, two_beds("archive-later"));
    assert_eq!(done["status"], 200);
    fixture
        .sql()
        .execute(
            "UPDATE build_profiles SET archived_at='2026-11-05T00:00:00.000Z' WHERE id=1",
            [],
        )
        .unwrap();
    let rows = fixture.checkoff();
    let undone = undo(&owner, &done["body"]["undo_token"]);
    assert_eq!(
        undone["body"]["detail"], "Archived Plan Progress cannot be changed",
        "{undone}"
    );
    assert_eq!(
        fixture.checkoff(),
        rows,
        "undo writes nothing on an archived Build"
    );
    owner.shutdown().unwrap();
}

#[test]
fn verify_batch_counts_units_other_beds_confirm_toward_left_fill() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let done = batch(
        &owner,
        json!({ "idempotency_key": "f-part-3", "beds": [
        { "link_id": "link-burst", "decisions": [confirm(3, 1)] },
        { "link_id": "link-verify", "decisions": [confirm(3, 0)] },
    ] }),
    );
    assert_eq!(done["status"], 200, "{done}");
    assert!(printed(&fixture.checkoff()).ends_with(&[(3, 0), (3, 1)]));
    owner.shutdown().unwrap();
}

#[test]
fn verify_batch_refuses_undo_of_a_receipt_without_a_state_or_unit_list() {
    for missing in ["state", "units"] {
        let fixture = Fixture::new();
        let owner = fixture.open();
        let done = batch(&owner, two_beds("old-format"));
        assert_eq!(done["status"], 200);
        owner.shutdown().unwrap();
        let connection = fixture.sql();
        let raw: String = connection
            .query_row(
                "SELECT value FROM app_settings WHERE key = 'printer.checkoff_bursts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let mut receipts: Value = serde_json::from_str(&raw).unwrap();
        receipts[0]["undo"].as_object_mut().unwrap().remove(missing);
        connection
            .execute(
                "UPDATE app_settings SET value=?1 WHERE key = 'printer.checkoff_bursts'",
                [receipts.to_string()],
            )
            .unwrap();
        let owner = fixture.open();
        let rows = fixture.checkoff();
        assert_eq!(
            undo(&owner, &done["body"]["undo_token"]),
            json!({ "status": 409, "body": { "detail": "This Checkoff burst predates the undo state check and cannot be undone" } })
        );
        assert_eq!(fixture.checkoff(), rows);
        owner.shutdown().unwrap();
    }
}

#[test]
fn verify_batch_refuses_a_key_whose_receipt_was_trimmed() {
    let fixture = Fixture::new();
    let old: Vec<Value> = (0..100)
        .map(|i| json!({ "idempotency_key": format!("old-{i}"), "request_hash": "", "undo_token": format!("ppb_old{i}"), "response": {}, "undo": {} }))
        .collect();
    fixture
        .sql()
        .execute(
            "INSERT INTO app_settings (tenant_id, key, value) VALUES ('default', 'printer.checkoff_bursts', ?1)",
            [serde_json::to_string(&old).unwrap()],
        )
        .unwrap();
    let owner = fixture.open();
    assert_eq!(batch(&owner, two_beds("new"))["status"], 200);
    let rows = fixture.checkoff();
    let mut replay = two_beds("old-0");
    replay["beds"] = json!([{ "link_id": "link-stale-file", "decisions": [confirm(1, 1)] }]);
    assert_eq!(
        batch(&owner, replay),
        json!({ "status": 409, "body": { "detail": "idempotency_key belongs to a Checkoff burst too old to replay" } })
    );
    assert_eq!(fixture.checkoff(), rows, "a retired key writes nothing");
    assert_eq!(
        batch(&owner, two_beds("old-1"))["status"],
        409,
        "old-1 is still a receipt with another hash"
    );
    owner.shutdown().unwrap();
}

#[test]
fn verify_batch_never_exceeds_required_counts_or_changes_required_units() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let before = fixture.checkoff();
    let beds = json!({ "idempotency_key": "stale-file", "beds": [
        { "link_id": "link-verify", "decisions": [confirm(1, 1), confirm(1, 2)] },
        { "link_id": "link-stale-file", "decisions": [confirm(1, 1)] },
    ] });
    let done = batch(&owner, beds);
    assert_eq!(done["status"], 200);
    assert_eq!(
        done["body"]["units_confirmed"], 2,
        "the re-reported file counts nothing twice"
    );
    let after = fixture.checkoff();
    assert_eq!(after["units"], before["units"]);
    assert_eq!(
        after["progress"].as_array().unwrap().len(),
        before["progress"].as_array().unwrap().len()
    );
    let part_one: Vec<_> = printed(&after)
        .into_iter()
        .filter(|(p, _)| *p == 1)
        .collect();
    assert_eq!(
        part_one.len(),
        3,
        "Part 1 requires three units and has three printed"
    );
    let basis: Basis = serde_json::from_value(json!({
        "profile_id": 1, "plan_version": 1, "plan_revision_id": 1,
        "plan_revision_digest": "eb2dd43bcbf42d909a0b5bdcbeb9b0010951a905f7c5cb01d4b4def1b9eca54f",
        "required_unit_mapping_digest": "8c446659d41d31efe3d95931c9f898b0d5442a41104474671bdccab4779e3a41",
    }))
    .unwrap();
    let over = apply(
        &owner,
        Request::Completion {
            expected: basis,
            token: "ppu_00000000000000000000000000000003".into(),
            completed: true,
        },
    );
    assert_eq!(over["body"]["printed_count"], 3);
    owner.shutdown().unwrap();
}

#[test]
fn verify_batch_refuses_malformed_bursts_before_any_write() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let before = fixture.checkoff();
    let bed = json!({ "link_id": "link-verify", "decisions": [confirm(1, 1)] });
    for (body, detail) in [
        (
            json!({ "beds": [bed] }),
            "idempotency_key must be 1 to 200 characters",
        ),
        (
            json!({ "idempotency_key": "x".repeat(201), "beds": [bed] }),
            "idempotency_key must be 1 to 200 characters",
        ),
        (
            json!({ "idempotency_key": "k", "beds": [] }),
            "beds must list 1 to 50 beds",
        ),
        (
            json!({ "idempotency_key": "k", "beds": vec![bed.clone(); 51] }),
            "beds must list 1 to 50 beds",
        ),
        (
            json!({ "idempotency_key": "k", "beds": [bed, bed] }),
            "beds[1] repeats a link",
        ),
        (
            json!({ "idempotency_key": "k", "beds": [{ "decisions": [] }] }),
            "beds[0].link_id is required",
        ),
        (
            json!({ "idempotency_key": "k", "beds": [{ "link_id": [], "decisions": [] }] }),
            "beds[0].link_id is required",
        ),
        (
            json!({ "idempotency_key": "k", "beds": [bed, { "link_id": [" link-verify "], "decisions": [] }] }),
            "beds[1] repeats a link",
        ),
    ] {
        assert_eq!(
            batch(&owner, body),
            json!({ "status": 400, "body": { "detail": detail } })
        );
    }
    assert_eq!(
        undo(&owner, &json!(""))["body"]["detail"],
        "undo_token is required"
    );
    assert_eq!(fixture.checkoff(), before);
    let coerced = batch(
        &owner,
        json!({ "idempotency_key": "array-id", "beds": [{ "link_id": [" link-verify "], "decisions": [confirm(1, 1)] }] }),
    );
    assert_eq!(
        coerced["status"], 200,
        "link_id is read like String(link_id).trim(): {coerced}"
    );
    owner.shutdown().unwrap();
}

/// VBT-06: storage, which runs the transaction, cannot reach a printer or
/// Spoolman: it has no network or driver dependency.
#[test]
fn verify_batch_transaction_has_no_printer_or_spoolman_path() {
    let manifest = include_str!("../Cargo.toml");
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .unwrap()
        .split("\n[")
        .next()
        .unwrap();
    for forbidden in ["pp-drivers", "pp-net", "reqwest", "hyper", "tokio"] {
        assert!(
            !dependencies.contains(forbidden),
            "pp-storage depends on {forbidden}"
        );
    }
}

fn progress_events(fixture: &Fixture) -> i64 {
    fixture
        .sql()
        .query_row(
            "SELECT count(*) FROM domain_events WHERE event_type='checkoff.progress_changed'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn verify_batch_and_undo_append_progress_events_and_a_replay_does_not() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let start = progress_events(&fixture);
    let done = batch(&owner, two_beds("events"));
    assert_eq!(done["status"], 200, "{done}");
    let applied = progress_events(&fixture);
    assert!(applied > start, "confirmed units commit an event");
    assert_eq!(batch(&owner, two_beds("events")), done);
    assert_eq!(
        progress_events(&fixture),
        applied,
        "a replay writes no event"
    );
    assert_eq!(undo(&owner, &done["body"]["undo_token"])["status"], 200);
    assert!(progress_events(&fixture) > applied, "undo commits an event");
}
