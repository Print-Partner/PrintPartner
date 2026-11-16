#![cfg(feature = "desktop_rebuild")]
//! GRE-1485 (GRE-411 part 1): a confirmed Checkoff burst records Spoolman
//! deduction intent in its own transaction. Runs on a copy of the
//! required-units fixture: `link-verify` holds units (1,1), (1,2), (3,0) and
//! (3,1) of Build 1.
use checkoff_apply::apply;
use checkoff_fixture::Fixture;
use pp_storage::{
    WriterOwner,
    auth::{self, Secret},
    checkoff_progress::Request,
    read_model::Credential,
    spoolman_deductions::{Deduction, Filter, GramsBasis, SkipReason, State, Target},
};
use serde_json::{Value, json};
use std::{sync::atomic::AtomicBool, time::Duration};

#[path = "common/checkoff_apply.rs"]
mod checkoff_apply;
#[path = "common/checkoff_fixture.rs"]
mod checkoff_fixture;
#[path = "common/js_json.rs"]
mod js_json;

const SPOOLMAN: &str = r#"[{"id":"sm-1","type":"spoolman","name":"Spoolman","config":{"base_url":"http://127.0.0.1:7912","enabled":true},"created_at":"2026-11-01T00:00:00.000Z","updated_at":"2026-11-01T00:00:00.000Z"}]"#;

/// Replaces the `default` installation's setting `key`.
fn put(fixture: &Fixture, key: &str, value: &str) {
    let sql = "REPLACE INTO app_settings (tenant_id, key, value) VALUES ('default', ?1, ?2)";
    fixture.sql().execute(sql, [key, value]).unwrap();
}

/// The Module on, Spoolman `sm-1`, Part 1 pinned to spool 7, and 40 g over
/// the link's 4 units.
fn prepared() -> (Fixture, WriterOwner) {
    let fixture = Fixture::new();
    let owner = fixture.open();
    put(&fixture, "feature_toggles", r#"{"spoolman":true}"#);
    put(&fixture, "integrations", SPOOLMAN);
    fixture
        .sql()
        .execute_batch(
            "UPDATE parts SET spoolman_spool_id = 'spoolman:sm-1:spool:7' WHERE id = 1;
             INSERT INTO print_link_grams (tenant_id, link_id, grams, units, basis, updated_at)
             VALUES ('default', 'link-verify', 40, 4, 'estimated', '2026-11-12T00:00:00.000Z');",
        )
        .unwrap();
    (fixture, owner)
}

fn confirm(part: i64, unit: i64) -> Value {
    json!({ "part_id": part, "unit_index": unit, "result": "confirmed" })
}

fn burst(owner: &WriterOwner, key: &str, decisions: Vec<Value>) -> Value {
    let body = json!({ "idempotency_key": key, "beds": [{ "link_id": "link-verify", "decisions": decisions }] });
    let response = apply(owner, Request::VerifyBatch { body });
    assert_eq!(response["status"], 200, "{response}");
    response
}

fn deductions(owner: &WriterOwner) -> Vec<Deduction> {
    let policy = auth::AuthPolicy {
        registration: auth::RegistrationPolicy::Open,
        first_user: auth::FirstUserTenant::NewUser,
        session_tenant: auth::SessionTenantPolicy::AccountTenant,
    };
    owner
        .spoolman_deductions(policy)
        .unwrap()
        .list(
            Credential::Session(Secret::new("required-unit-fixture-secret".into())),
            Filter::default(),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
}

/// `(state, reason, units, target)` of every row, newest first.
fn summary(rows: &[Deduction]) -> Vec<(State, Option<SkipReason>, u32, Option<Target>)> {
    rows.iter()
        .map(|row| (row.state, row.reason, row.units, row.target.clone()))
        .collect()
}

fn spool_seven(grams: f64) -> Option<Target> {
    Some(Target {
        integration_id: "sm-1".into(),
        spool_id: 7,
        grams,
        basis: GramsBasis::Estimated,
    })
}

#[test]
fn a_confirmed_burst_records_one_pending_deduction_per_pinned_spool() {
    let (_fixture, owner) = prepared();
    burst(&owner, "k1", vec![confirm(1, 1), confirm(1, 2)]);
    let rows = deductions(&owner);
    assert_eq!(
        summary(&rows),
        [(State::Pending, None, 2, spool_seven(20.0))]
    );
    assert_eq!(
        (rows[0].link_id.as_str(), rows[0].profile_id),
        ("link-verify", 1)
    );
}

#[test]
fn a_replayed_burst_adds_no_row_and_undo_removes_the_pending_one() {
    let (_fixture, owner) = prepared();
    let first = burst(&owner, "k1", vec![confirm(1, 1), confirm(1, 2)]);
    let replay = burst(&owner, "k1", vec![confirm(1, 1), confirm(1, 2)]);
    assert_eq!(replay["body"]["undo_token"], first["body"]["undo_token"]);
    assert_eq!(deductions(&owner).len(), 1);

    let token = &first["body"]["undo_token"];
    let undone = apply(
        &owner,
        Request::UndoBatch {
            body: json!({ "undo_token": token }),
        },
    );
    assert_eq!(undone["status"], 200, "{undone}");
    assert!(deductions(&owner).is_empty());

    // The undone units are free again: confirming them anew records them anew.
    burst(&owner, "k2", vec![confirm(1, 1), confirm(1, 2)]);
    assert_eq!(
        summary(&deductions(&owner)),
        [(State::Pending, None, 2, spool_seven(20.0))]
    );
}

#[test]
fn units_that_cannot_be_deducted_give_one_skipped_row_per_reason() {
    let (fixture, owner) = prepared();
    // Part 3 pins nothing; the burst still succeeds.
    burst(&owner, "k1", vec![confirm(1, 1), confirm(3, 0)]);
    assert_eq!(
        summary(&deductions(&owner)),
        [
            (State::Skipped, Some(SkipReason::NoPinnedSpool), 1, None),
            (State::Pending, None, 1, spool_seven(10.0)),
        ]
    );

    for (reason, setup) in [
        (
            SkipReason::IntegrationDisabled,
            SPOOLMAN.replace("\"enabled\":true", "\"enabled\":false"),
        ),
        (SkipReason::IntegrationMissing, "[]".to_owned()),
        (SkipReason::IntegrationsUnreadable, "{".to_owned()),
    ] {
        let (fixture, owner) = prepared();
        put(&fixture, "integrations", &setup);
        burst(&owner, "k1", vec![confirm(1, 1)]);
        assert_eq!(
            summary(&deductions(&owner)),
            [(State::Skipped, Some(reason), 1, None)]
        );
    }

    fixture
        .sql()
        .execute("DELETE FROM print_link_grams", [])
        .unwrap();
    burst(&owner, "k2", vec![confirm(1, 2)]);
    assert_eq!(
        summary(&deductions(&owner))[0],
        (State::Skipped, Some(SkipReason::NoGrams), 1, None)
    );
}

#[test]
fn an_unreadable_module_setting_skips_the_units_and_the_burst_succeeds() {
    let (fixture, owner) = prepared();
    put(&fixture, "feature_toggles", r#"{"spoolman":"yes"}"#);
    burst(&owner, "k1", vec![confirm(1, 1), confirm(1, 2)]);
    assert_eq!(
        summary(&deductions(&owner)),
        [(
            State::Skipped,
            Some(SkipReason::ModuleSettingUnreadable),
            2,
            None
        )]
    );
}

#[test]
fn with_the_module_off_the_burst_succeeds_and_writes_no_row() {
    let (fixture, owner) = prepared();
    put(&fixture, "feature_toggles", r#"{"spoolman":false}"#);
    burst(&owner, "k1", vec![confirm(1, 1), confirm(1, 2)]);
    assert!(deductions(&owner).is_empty());
    let rows: i64 = fixture
        .sql()
        .query_row("SELECT count(*) FROM spoolman_deductions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0);
}
