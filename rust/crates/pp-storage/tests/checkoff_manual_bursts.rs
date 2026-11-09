#![cfg(feature = "desktop_rebuild")]
//! GRE-1191: a manual Checkoff burst sets printed and assembled state on one
//! Build's Required units in one transaction with an idempotency key and an
//! undo token, with no printer link. Runs on a copy of the required-units
//! fixture: Build 1 owns Part 1 (tokens 1-3, unit 1 printed and assembled),
//! Part 2 (token 4, excluded) and Part 3 (tokens 5-6).
use checkoff_apply::apply;
use checkoff_fixture::Fixture;
use pp_storage::checkoff_progress::{Basis, Request};
use serde_json::{Value, json};

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
#[path = "common/progress_rows.rs"]
mod progress_rows;

fn token(n: u8) -> String {
    format!("ppu_{n:032x}")
}

impl Fixture {
    fn basis(&self, owner: &pp_storage::WriterOwner) -> Basis {
        accepted_basis::basis(owner)
    }

    fn progress(&self) -> Vec<String> {
        progress_rows::progress(&self.sql())
    }

    /// Every row a burst must not write: drafts, Plans, Required units,
    /// printer links, jobs, outcomes, sends and every setting but its receipts.
    fn untouched(&self) -> Value {
        let queries = json!({
            "drafts": "SELECT * FROM plan_drafts ORDER BY rowid",
            "revisions": "SELECT * FROM plan_revisions ORDER BY rowid",
            "profiles": "SELECT * FROM build_profiles ORDER BY rowid",
            "units": "SELECT * FROM required_units ORDER BY token",
            "jobs": "SELECT * FROM print_jobs ORDER BY rowid",
            "outcomes": "SELECT * FROM print_job_parts ORDER BY rowid",
            "settings": "SELECT tenant_id, key, value FROM app_settings WHERE key NOT LIKE 'checkoff.manual_bursts%' ORDER BY tenant_id, key",
        });
        json_rows::query_rows(&self.sql(), queries.as_object().unwrap())
    }
}

fn burst(owner: &pp_storage::WriterOwner, profile: i64, body: Value) -> Value {
    apply(
        owner,
        Request::ManualBurst {
            profile_id: profile,
            body,
        },
    )
}

fn undo(owner: &pp_storage::WriterOwner, profile: i64, token: &Value) -> Value {
    apply(
        owner,
        Request::UndoManualBurst {
            profile_id: profile,
            body: json!({ "undo_token": token }),
        },
    )
}

/// Part 1 filled through unit 3 (one press of F), unit 1 of Part 3 printed
/// and assembled.
fn fill(key: &str, basis: &Basis) -> Value {
    json!({ "idempotency_key": key, "expected": basis, "units": [
        { "token": token(3), "printed": true },
        { "token": token(5), "printed": true, "assembled": true },
    ] })
}

#[test]
fn manual_burst_records_one_transaction_or_nothing() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let basis = fixture.basis(&owner);
    let before = fixture.progress();
    let untouched = fixture.untouched();

    fixture
        .sql()
        .execute_batch(
            "CREATE TRIGGER injected_fault BEFORE UPDATE ON print_progress WHEN NEW.part_id = 3
             BEGIN SELECT RAISE(ABORT, 'injected fault'); END;",
        )
        .unwrap();
    let failed = burst(&owner, 1, fill("k1", &basis));
    assert_eq!(
        failed,
        json!({ "status": 500, "body": { "detail": "Internal Server Error" } })
    );
    assert_eq!(
        fixture.progress(),
        before,
        "Part 1 is not kept when Part 3 fails"
    );
    fixture
        .sql()
        .execute_batch("DROP TRIGGER injected_fault")
        .unwrap();

    let done = burst(&owner, 1, fill("k1", &basis));
    assert_eq!(done["status"], 200, "{done}");
    let body = &done["body"];
    assert!(body["undo_token"].as_str().unwrap().starts_with("ppm_"));
    assert_eq!(
        (
            &body["printed_added"],
            &body["assembled_added"],
            &body["printed_removed"]
        ),
        (&json!(3), &json!(1), &json!(0)),
        "Part 1 unit 1 was already printed and assembled"
    );
    assert_eq!(
        body["parts"],
        json!([
            { "part_id": 1, "quantity": 3, "printed_count": 3, "assembled_count": 1 },
            { "part_id": 3, "quantity": 2, "printed_count": 1, "assembled_count": 1 },
        ])
    );
    assert_eq!(
        fixture.progress(),
        [
            "1:0:1:1", "1:1:1:0", "1:2:1:0", "2:0:0:0", "3:0:1:1", "3:1:0:0"
        ]
    );
    assert_eq!(
        fixture.untouched(),
        untouched,
        "no draft, Required unit, link, job, outcome, send or stock row changes"
    );
    let events: Vec<String> = fixture
        .sql()
        .prepare("SELECT payload_json FROM domain_events WHERE event_type='checkoff.progress_changed' ORDER BY rowid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        events,
        [r#"{"part_id":1}"#, r#"{"part_id":3}"#],
        "one progress event per touched Part, in the burst's transaction"
    );
    owner.shutdown().unwrap();
}

#[test]
fn manual_burst_replays_after_restart_and_refuses_a_changed_body() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let basis = fixture.basis(&owner);
    let first = burst(&owner, 1, fill("phone-retry", &basis));
    assert_eq!(first["status"], 200);
    let rows = fixture.progress();
    assert_eq!(burst(&owner, 1, fill("phone-retry", &basis)), first);
    owner.shutdown().unwrap();
    let owner = fixture.open();
    assert_eq!(
        burst(&owner, 1, fill("phone-retry", &basis)),
        first,
        "the receipt survives a restart"
    );
    assert_eq!(fixture.progress(), rows, "replays write nothing");
    let mut changed = fill("phone-retry", &basis);
    changed["units"][0]["token"] = json!(token(2));
    assert_eq!(
        burst(&owner, 1, changed),
        json!({ "status": 409, "body": { "detail": "idempotency_key was already used for a different request" } })
    );
    assert_eq!(fixture.progress(), rows);
    owner.shutdown().unwrap();
}

#[test]
fn manual_burst_refuses_what_would_break_the_plan() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let basis = fixture.basis(&owner);
    let before = fixture.progress();
    let detail = |response: Value| {
        (
            response["status"].clone(),
            response["body"]["detail"].clone(),
        )
    };

    let mut stale = basis.clone();
    stale.plan_version += 1;
    assert_eq!(
        detail(burst(&owner, 1, fill("stale", &stale))),
        (json!(409), json!("Accepted Plan changed; reload and retry"))
    );
    assert_eq!(
        detail(burst(&owner, 2, fill("other-build", &basis))),
        (json!(404), json!("Plan not found"))
    );
    let contradiction = json!({ "idempotency_key": "c", "expected": basis, "units": [
        { "token": token(3), "printed": true }, { "token": token(2), "printed": false },
    ] });
    assert_eq!(
        detail(burst(&owner, 1, contradiction)),
        (
            json!(400),
            json!(
                "Part 1: the burst prints unit 3 and clears unit 2, but printed units fill from the left"
            )
        )
    );
    let unprinted = json!({ "idempotency_key": "u", "expected": basis, "units": [
        { "token": token(1), "printed": true }, { "token": token(2), "assembled": true },
    ] });
    assert_eq!(
        detail(burst(&owner, 1, unprinted)),
        (
            json!(400),
            json!("units[1]: an unprinted unit cannot be assembled")
        )
    );
    for (key, unit) in [("excluded", token(4)), ("unknown", token(99))] {
        let body = json!({ "idempotency_key": key, "expected": basis, "units": [{ "token": unit, "printed": true }] });
        assert_eq!(
            detail(burst(&owner, 1, body)),
            (
                json!(404),
                json!("units[0]: Required unit not found on this Build's Accepted Plan")
            )
        );
    }
    for (body, message) in [
        (
            json!({ "idempotency_key": "r", "expected": basis, "units": [{ "token": token(1), "printed": true }, { "token": token(1), "assembled": false }] }),
            "units[1] repeats a Required unit",
        ),
        (
            json!({ "idempotency_key": "n", "expected": basis, "units": [{ "token": token(1) }] }),
            "units[0] sets neither printed nor assembled",
        ),
        (
            json!({ "idempotency_key": "x", "expected": basis, "units": [{ "token": token(1), "printed": true }], "count": 3 }),
            "Unknown field count",
        ),
        (
            json!({ "idempotency_key": " ", "expected": basis, "units": [{ "token": token(1), "printed": true }] }),
            "idempotency_key must be 1 to 200 characters",
        ),
    ] {
        assert_eq!(detail(burst(&owner, 1, body)), (json!(400), json!(message)));
    }
    assert_eq!(fixture.progress(), before, "a refused burst writes nothing");

    fixture
        .sql()
        .execute(
            "UPDATE build_profiles SET archived_at='2026-11-06T00:00:00.000Z' WHERE id=1",
            [],
        )
        .unwrap();
    assert_eq!(
        detail(burst(&owner, 1, fill("archived", &basis))),
        (
            json!(409),
            json!("Archived Plan Progress cannot be changed")
        )
    );
    owner.shutdown().unwrap();
}

#[test]
fn manual_burst_undo_restores_the_prior_state_or_refuses_with_a_reason() {
    let fixture = Fixture::new();
    let owner = fixture.open();
    let basis = fixture.basis(&owner);
    let before = fixture.progress();
    let done = burst(&owner, 1, fill("undo-me", &basis));
    let token_value = done["body"]["undo_token"].clone();
    owner.shutdown().unwrap();

    let owner = fixture.open();
    assert_eq!(
        undo(&owner, 2, &token_value)["status"],
        404,
        "another Build's path"
    );
    let undone = undo(&owner, 1, &token_value);
    assert_eq!(undone["status"], 200, "{undone}");
    assert_eq!(undone["body"]["undo_token"], token_value);
    assert_eq!(
        fixture.progress(),
        before,
        "undo restores the exact prior progress"
    );
    assert_eq!(
        undo(&owner, 1, &token_value)["body"]["detail"],
        "This Checkoff burst was already undone"
    );
    assert_eq!(
        burst(&owner, 1, fill("undo-me", &basis))["body"]["detail"],
        "This Checkoff burst was undone; send the change again as a new burst",
        "a late retry does not count the burst again"
    );
    assert_eq!(fixture.progress(), before);
    assert_eq!(undo(&owner, 1, &json!("ppm_missing"))["status"], 404);

    // A later change on the burst's Part blocks undo with its reason.
    let second = burst(&owner, 1, fill("second", &basis));
    let later = apply(
        &owner,
        Request::Assembly {
            expected: basis.clone(),
            token: token(2),
            assembled: true,
        },
    );
    assert_eq!(later["status"], 200, "{later}");
    let after_later = fixture.progress();
    let refused = undo(&owner, 1, &second["body"]["undo_token"]);
    assert_eq!(
        (refused["status"].clone(), refused["body"]["detail"].clone()),
        (
            json!(409),
            json!("Checkoff changed since this burst: a later change on its Parts depends on it")
        )
    );
    assert_eq!(
        fixture.progress(),
        after_later,
        "a refused undo writes nothing"
    );

    // Clearing a printed unit is a burst too, and undo puts it back.
    let clear = burst(
        &owner,
        1,
        json!({ "idempotency_key": "clear", "expected": basis, "units": [{ "token": token(2), "printed": false }] }),
    );
    assert_eq!(clear["body"]["printed_removed"], 2, "{clear}");
    assert_eq!(
        fixture.progress(),
        [
            "1:0:1:1", "1:1:0:0", "1:2:0:0", "2:0:0:0", "3:0:1:1", "3:1:0:0"
        ],
        "clearing unit 2 clears unit 3 and the assembly of unit 2"
    );
    assert_eq!(undo(&owner, 1, &clear["body"]["undo_token"])["status"], 200);
    assert_eq!(fixture.progress(), after_later);
    owner.shutdown().unwrap();
}
