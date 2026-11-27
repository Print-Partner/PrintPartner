#![cfg(feature = "desktop_rebuild")]
//! GRE-1485 (GRE-411 part 1): a confirmed Checkoff burst records Spoolman
//! deduction intent in its own transaction. Runs on a copy of the
//! required-units fixture: `link-verify` holds units (1,1), (1,2), (3,0) and
//! (3,1) of Build 1.
use checkoff_apply::apply;
use checkoff_fixture::Fixture;
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    checkoff_progress::Request,
    read_model::Credential,
    spoolman_deductions::{
        Answer, Claimed, Deduction, DeductionError, DeductionRecord, Failure, Filter, GramsBasis,
        Outcome, SkipReason, SpoolmanDeductionsClient, State, Target,
    },
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

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

fn client(owner: &WriterOwner) -> SpoolmanDeductionsClient {
    owner
        .spoolman_deductions(auth::AuthPolicy {
            registration: auth::RegistrationPolicy::Open,
            first_user: auth::FirstUserTenant::NewUser,
            session_tenant: auth::SessionTenantPolicy::AccountTenant,
        })
        .unwrap()
}

fn credential() -> Credential {
    Credential::Session(Secret::new("required-unit-fixture-secret".into()))
}

fn claim(
    owner: &WriterOwner,
    id: i64,
    grams_before: Option<f64>,
) -> Result<Option<Claimed>, DeductionError> {
    client(owner).claim(
        credential(),
        id,
        grams_before,
        &AtomicBool::new(false),
        Duration::from_secs(5),
    )
}

fn finish(owner: &WriterOwner, id: i64, end: Outcome) -> Result<bool, DeductionError> {
    client(owner).finish(
        credential(),
        id,
        end,
        &AtomicBool::new(false),
        Duration::from_secs(5),
    )
}

fn reconcile(owner: &WriterOwner, id: i64, answer: Answer) -> Result<Deduction, DeductionError> {
    client(owner).reconcile(
        credential(),
        id,
        answer,
        &AtomicBool::new(false),
        Duration::from_secs(5),
    )
}

fn record(owner: &WriterOwner, id: i64) -> Option<DeductionRecord> {
    client(owner)
        .get(
            credential(),
            id,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
}

fn conflict(owner: &WriterOwner, id: i64, answer: Answer) {
    assert!(
        matches!(
            reconcile(owner, id, answer).unwrap_err(),
            DeductionError::StateConflict
        ),
        "{answer:?}"
    );
}

fn pending_id(owner: &WriterOwner) -> i64 {
    deductions(owner)
        .iter()
        .find(|row| row.state == State::Pending)
        .unwrap()
        .id
}

/// A second connection reads the committed row. Claim returns only after its
/// write commits.
fn stored(fixture: &Fixture, id: i64) -> (String, Option<String>, Option<i64>, Option<f64>) {
    fixture
        .sql()
        .query_row(
            "SELECT state, failure, failure_status, spool_grams_before
             FROM spoolman_deductions WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
}

#[test]
fn a_claim_commits_only_a_pending_row_and_keeps_its_before_value() {
    let (fixture, owner) = prepared();
    burst(&owner, "k1", vec![confirm(1, 1), confirm(3, 0)]);
    let rows = deductions(&owner);
    let pending = rows.iter().find(|row| row.state == State::Pending).unwrap();
    let skipped = rows.iter().find(|row| row.state == State::Skipped).unwrap();
    assert!(claim(&owner, skipped.id, Some(1.0)).unwrap().is_none());
    assert_eq!(
        claim(&owner, pending.id, Some(f64::NAN))
            .unwrap_err()
            .to_string(),
        "spool grams before a deduction must be a finite number"
    );
    assert_eq!(
        stored(&fixture, pending.id),
        ("pending".into(), None, None, None)
    );

    let claimed = claim(&owner, pending.id, Some(12.5)).unwrap().unwrap();
    assert_eq!(
        (
            claimed.id,
            claimed.integration_id.as_str(),
            claimed.spool_id,
            claimed.grams
        ),
        (pending.id, "sm-1", 7, 10.0)
    );
    assert_eq!(claimed.spool_grams_before, Some(12.5));
    assert_eq!(
        stored(&fixture, pending.id),
        ("sending".into(), None, None, Some(12.5))
    );
    assert!(claim(&owner, pending.id, Some(99.0)).unwrap().is_none());
    assert_eq!(
        stored(&fixture, pending.id),
        ("sending".into(), None, None, Some(12.5))
    );
    assert_eq!(stored(&fixture, skipped.id).0, "skipped");
}

#[test]
fn a_finish_applies_only_while_the_row_is_sending() {
    let (fixture, owner) = prepared();
    fixture
        .sql()
        .execute(
            "UPDATE parts SET spoolman_spool_id = 'spoolman:sm-1:spool:7' WHERE id = 3",
            [],
        )
        .unwrap();
    for (key, unit) in [
        ("k1", confirm(1, 1)),
        ("k2", confirm(1, 2)),
        ("k3", confirm(3, 0)),
        ("k4", confirm(3, 1)),
    ] {
        burst(&owner, key, vec![unit]);
    }
    let ids: Vec<i64> = deductions(&owner).iter().rev().map(|row| row.id).collect();
    assert_eq!(ids.len(), 4);
    assert!(!finish(&owner, ids[0], Outcome::Applied).unwrap());
    assert_eq!(stored(&fixture, ids[0]).0, "pending");

    claim(&owner, ids[0], Some(3.5)).unwrap().unwrap();
    assert_eq!(
        finish(&owner, ids[0], Outcome::Refused(99))
            .unwrap_err()
            .to_string(),
        "a refused deduction needs an HTTP status from 100 to 599"
    );
    assert_eq!(
        finish(&owner, ids[0], Outcome::Refused(600))
            .unwrap_err()
            .to_string(),
        "a refused deduction needs an HTTP status from 100 to 599"
    );
    assert_eq!(stored(&fixture, ids[0]).0, "sending");
    assert!(finish(&owner, ids[0], Outcome::Refused(422)).unwrap());
    assert_eq!(
        stored(&fixture, ids[0]),
        (
            "failed".into(),
            Some("refused".into()),
            Some(422),
            Some(3.5)
        )
    );
    assert!(!finish(&owner, ids[0], Outcome::Applied).unwrap());
    assert!(claim(&owner, ids[0], Some(1.0)).unwrap().is_none());

    let ends = [
        (ids[1], Outcome::Applied, "applied", None),
        (ids[2], Outcome::ReplyLost, "unknown", Some("reply_lost")),
        (ids[3], Outcome::Unreachable, "failed", Some("unreachable")),
    ];
    for (id, end, state, failure) in ends {
        claim(&owner, id, None).unwrap().unwrap();
        assert!(finish(&owner, id, end).unwrap());
        assert_eq!(
            stored(&fixture, id),
            (state.into(), failure.map(str::to_owned), None, None)
        );
        assert!(!finish(&owner, id, Outcome::Applied).unwrap());
        assert!(claim(&owner, id, None).unwrap().is_none());
    }

    for (burst_id, end, failure) in [
        ("k5", Outcome::IntegrationMissing, "integration_missing"),
        ("k6", Outcome::IntegrationDisabled, "integration_disabled"),
    ] {
        fixture
            .sql()
            .execute(
                "INSERT INTO spoolman_deductions
                 (tenant_id,burst,link_id,profile_id,state,units,integration_id,spool_id,grams,grams_basis,created_at,updated_at)
                 VALUES ('default',?1,'link-verify',1,'pending',1,'sm-1',7,1.0,'estimated','t','t')",
                [burst_id],
            )
            .unwrap();
        let id = pending_id(&owner);
        claim(&owner, id, None).unwrap().unwrap();
        assert!(finish(&owner, id, end).unwrap());
        assert_eq!(
            stored(&fixture, id),
            ("failed".into(), Some(failure.into()), None, None)
        );
    }
}

#[test]
fn reconcile_is_the_only_return_to_pending() {
    let (fixture, owner) = prepared();
    burst(&owner, "k1", vec![confirm(1, 1)]);
    burst(&owner, "k2", vec![confirm(1, 2)]);
    burst(&owner, "k3", vec![confirm(3, 0)]);
    let pending_ids: Vec<i64> = deductions(&owner)
        .iter()
        .filter(|row| row.state == State::Pending)
        .map(|row| row.id)
        .collect();
    assert_eq!(pending_ids.len(), 2);
    let (unknown, failed) = (pending_ids[0], pending_ids[1]);
    let skipped = deductions(&owner)
        .iter()
        .find(|row| row.state == State::Skipped)
        .unwrap()
        .id;

    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);
    client(&owner).on_burst(Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    }));

    let pending = stored(&fixture, unknown);
    for answer in [Answer::Deducted, Answer::Retry, Answer::Dismiss] {
        conflict(&owner, unknown, answer);
        assert_eq!(stored(&fixture, unknown), pending);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);

    claim(&owner, unknown, Some(12.5)).unwrap().unwrap();
    let sending = stored(&fixture, unknown);
    for answer in [Answer::Deducted, Answer::Retry, Answer::Dismiss] {
        conflict(&owner, unknown, answer);
    }
    assert_eq!(stored(&fixture, unknown), sending);
    assert!(finish(&owner, unknown, Outcome::ReplyLost).unwrap());
    assert!(claim(&owner, unknown, Some(1.0)).unwrap().is_none());
    assert!(!finish(&owner, unknown, Outcome::Applied).unwrap());

    let retried = reconcile(&owner, unknown, Answer::Retry).unwrap();
    assert_eq!(retried.state, State::Pending);
    assert_eq!(retried.failure, None);
    assert_eq!(
        stored(&fixture, unknown),
        ("pending".into(), None, None, Some(12.5))
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    conflict(&owner, unknown, Answer::Retry);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    claim(&owner, unknown, Some(9.0)).unwrap().unwrap();
    assert!(finish(&owner, unknown, Outcome::ReplyLost).unwrap());
    let applied = reconcile(&owner, unknown, Answer::Deducted).unwrap();
    assert_eq!(applied.state, State::Applied);
    assert_eq!(applied.failure, None);
    assert_eq!(
        stored(&fixture, unknown),
        ("applied".into(), None, None, Some(9.0))
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    for answer in [Answer::Deducted, Answer::Retry, Answer::Dismiss] {
        conflict(&owner, unknown, answer);
    }
    assert_eq!(stored(&fixture, unknown).0, "applied");

    let found = record(&owner, unknown).unwrap();
    assert_eq!(found.deduction.state, State::Applied);
    assert_eq!(found.deduction.spool_grams_before, Some(9.0));
    assert_eq!(found.integration.unwrap().id, "sm-1");
    put(
        &fixture,
        "integrations",
        &SPOOLMAN.replace("\"enabled\":true", "\"enabled\":false"),
    );
    assert!(record(&owner, unknown).unwrap().integration.is_none());
    put(&fixture, "integrations", SPOOLMAN);

    claim(&owner, failed, Some(3.0)).unwrap().unwrap();
    assert!(finish(&owner, failed, Outcome::Refused(422)).unwrap());
    conflict(&owner, failed, Answer::Deducted);
    assert_eq!(
        stored(&fixture, failed),
        (
            "failed".into(),
            Some("refused".into()),
            Some(422),
            Some(3.0)
        )
    );
    let dismissed = reconcile(&owner, failed, Answer::Dismiss).unwrap();
    assert_eq!(dismissed.state, State::Dismissed);
    assert_eq!(dismissed.failure, Some(Failure::Refused));
    assert_eq!(dismissed.failure_status, Some(422));
    assert_eq!(
        stored(&fixture, failed),
        (
            "dismissed".into(),
            Some("refused".into()),
            Some(422),
            Some(3.0)
        )
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    for answer in [Answer::Deducted, Answer::Retry, Answer::Dismiss] {
        conflict(&owner, failed, answer);
    }

    fixture
        .sql()
        .execute(
            "INSERT INTO spoolman_deductions
             (tenant_id,burst,link_id,profile_id,state,units,integration_id,spool_id,grams,grams_basis,created_at,updated_at)
             VALUES ('default','k4','link-verify',1,'pending',1,'sm-1',7,1.0,'estimated','t','t')",
            [],
        )
        .unwrap();
    let again = pending_id(&owner);
    claim(&owner, again, Some(4.0)).unwrap().unwrap();
    assert!(finish(&owner, again, Outcome::Refused(409)).unwrap());
    let retried_failure = reconcile(&owner, again, Answer::Retry).unwrap();
    assert_eq!(retried_failure.state, State::Pending);
    assert_eq!(retried_failure.failure, None);
    assert_eq!(
        stored(&fixture, again),
        ("pending".into(), None, None, Some(4.0))
    );
    assert_eq!(hits.load(Ordering::SeqCst), 2);

    let skipped_row = stored(&fixture, skipped);
    for answer in [Answer::Deducted, Answer::Retry, Answer::Dismiss] {
        conflict(&owner, skipped, answer);
    }
    assert_eq!(stored(&fixture, skipped), skipped_row);
    assert!(record(&owner, skipped).unwrap().integration.is_none());
    assert!(matches!(
        reconcile(&owner, 0, Answer::Dismiss).unwrap_err(),
        DeductionError::NotFound
    ));
    assert!(record(&owner, 0).is_none());
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[test]
fn a_restart_marks_a_sending_row_unknown_and_does_not_send_it() {
    let (fixture, owner) = prepared();
    burst(&owner, "k1", vec![confirm(1, 1)]);
    let sending = pending_id(&owner);
    claim(&owner, sending, Some(80.0)).unwrap().unwrap();
    burst(&owner, "k2", vec![confirm(1, 2)]);
    let still_pending = pending_id(&owner);
    drop(owner);

    let owner = WriterOwner::open(&fixture.path, Limits::default())
        .unwrap()
        .0;
    assert_eq!(
        stored(&fixture, sending),
        (
            "unknown".into(),
            Some("interrupted".into()),
            None,
            Some(80.0)
        )
    );
    assert_eq!(
        stored(&fixture, still_pending),
        ("pending".into(), None, None, None)
    );
    assert!(claim(&owner, sending, Some(1.0)).unwrap().is_none());
    assert_eq!(stored(&fixture, sending).0, "unknown");
    let claimed = claim(&owner, still_pending, Some(1.0)).unwrap().unwrap();
    assert_eq!(claimed.id, still_pending);
}

#[test]
fn undo_removes_pending_and_skipped_rows_and_leaves_a_sending_claim() {
    let (fixture, owner) = prepared();
    let first = burst(&owner, "k1", vec![confirm(1, 1), confirm(3, 0)]);
    let pending = pending_id(&owner);
    claim(&owner, pending, Some(4.0)).unwrap().unwrap();
    let undone = apply(
        &owner,
        Request::UndoBatch {
            body: json!({ "undo_token": &first["body"]["undo_token"] }),
        },
    );
    assert_eq!(undone["status"], 200, "{undone}");
    let rows = deductions(&owner);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, State::Sending);
    assert_eq!(rows[0].spool_grams_before, Some(4.0));
    let claims: i64 = fixture
        .sql()
        .query_row(
            "SELECT count(*) FROM spoolman_deduction_units WHERE deduction_id=?1",
            [pending],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claims, 1);
    burst(&owner, "k2", vec![confirm(1, 1)]);
    assert_eq!(deductions(&owner).len(), 1);
}

fn version(fixture: &Fixture) -> String {
    fixture
        .sql()
        .query_row(
            "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

fn column_present(fixture: &Fixture, name: &str) -> bool {
    fixture
        .sql()
        .query_row(
            "SELECT count(*) FROM pragma_table_info('spoolman_deductions') WHERE name=?1",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

#[test]
fn schema_50_keeps_a_step_49_database_and_a_failed_step_leaves_it_there() {
    let (fixture, owner) = prepared();
    burst(&owner, "k1", vec![confirm(1, 1), confirm(3, 0)]);
    drop(owner);
    let columns = "id,tenant_id,burst,link_id,profile_id,state,reason,units,integration_id,spool_id,grams,grams_basis,created_at,updated_at";
    let read = |connection: &rusqlite::Connection| -> Vec<String> {
        let mut statement = connection
            .prepare(&format!(
                "SELECT {columns} FROM spoolman_deductions ORDER BY id"
            ))
            .unwrap();
        let width = statement.column_count();
        statement
            .query_map([], |row| {
                let values = (0..width)
                    .map(|index| row.get::<_, rusqlite::types::Value>(index))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(format!("{values:?}"))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let connection = fixture.sql();
    connection
        .execute_batch(&format!(
            "CREATE TABLE spoolman_deductions_kept AS SELECT {columns} FROM spoolman_deductions;
             DROP TABLE spoolman_deductions;
             {schema}
             INSERT INTO spoolman_deductions ({columns}) SELECT {columns} FROM spoolman_deductions_kept;
             DROP TABLE spoolman_deductions_kept;
             DROP TABLE plan_revision_spoolman_colors;
             DROP TABLE pending_actions;
             UPDATE app_settings SET value='49' WHERE tenant_id='default' AND key='schema_version';
             CREATE TRIGGER reject_schema50 BEFORE UPDATE OF value ON app_settings
             WHEN NEW.tenant_id = 'default' AND NEW.key = 'schema_version' AND NEW.value = '50'
             BEGIN SELECT RAISE(ABORT, 'schema50 stamp rejected'); END;",
            schema = include_str!("../src/spoolman_deductions/schema.sql"),
        ))
        .unwrap();
    let rows = read(&connection);
    assert_eq!(rows.len(), 2);
    drop(connection);
    assert!(!column_present(&fixture, "failure"));

    let Err(error) = WriterOwner::open(&fixture.path, Limits::default()) else {
        panic!("step 50 committed past the stamp trigger");
    };
    assert!(
        format!("{error:#}").contains("schema50 stamp rejected"),
        "{error:#}"
    );
    assert_eq!(version(&fixture), "49");
    assert!(!column_present(&fixture, "failure"));
    assert_eq!(read(&fixture.sql()), rows);
    assert!(fixture.path.join("backups/pre-schema50.db").is_file());

    fixture
        .sql()
        .execute_batch("DROP TRIGGER reject_schema50")
        .unwrap();
    let (owner, ready) = WriterOwner::open(&fixture.path, Limits::default()).unwrap();
    assert_eq!((ready.version, ready.previous_version), (52, 49));
    drop(owner);
    assert_eq!(version(&fixture), "52");
    assert!(column_present(&fixture, "failure"));
    assert!(column_present(&fixture, "failure_status"));
    assert!(column_present(&fixture, "spool_grams_before"));
    assert_eq!(read(&fixture.sql()), rows);
    let blanks: (i64, i64, i64) = fixture
        .sql()
        .query_row(
            "SELECT count(failure), count(failure_status), count(spool_grams_before)
             FROM spoolman_deductions",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(blanks, (0, 0, 0));
}
