#![cfg(feature = "desktop_rebuild")]
//! GRE-1090: one Checkoff burst over several beds is one transaction with an
//! idempotency key and an undo token. Runs on a copy of the required-units
//! fixture with the synthetic beds of the printer Checkoff verify parity fixture.
use pp_storage::{
    Limits, WriterOwner,
    auth::Secret,
    checkoff_progress::{Basis, Request, Response},
    read_model::Credential,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

#[path = "common/json_rows.rs"]
mod json_rows;

const NODE: &str = include_str!("../../../../fixtures/parity/printer-checkoff-verify/node.json");
const DATABASE: &[u8] = include_bytes!("fixtures/required-units/unchanged.db");

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pp-checkoff-bursts-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir_all(path.join("repos/1/revisions/fixture")).unwrap();
        std::fs::write(path.join("print-partner.db"), DATABASE).unwrap();
        let fixture = Self(path);
        let node: Value = serde_json::from_str(NODE).unwrap();
        for sql in node["setup"].as_array().unwrap() {
            fixture.sql().execute_batch(sql.as_str().unwrap()).unwrap();
        }
        fixture
    }

    fn sql(&self) -> Connection {
        Connection::open(self.0.join("print-partner.db")).unwrap()
    }

    fn open(&self) -> WriterOwner {
        WriterOwner::open(&self.0, Limits::default()).unwrap().0
    }

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

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn apply(owner: &WriterOwner, request: Request) -> Value {
    let response: Response = owner
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
    let dismissed = apply(&owner, Request::Dismiss { body: json!({ "link_id": "link-host-failed" }) });
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
