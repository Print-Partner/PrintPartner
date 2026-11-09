#![cfg(feature = "desktop_rebuild")]
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    read_model::Credential,
    required_units::ReconciliationCommand,
    working_drafts::{AbandonRequest, EditRequest, Failure, PositiveId},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const OPENED: &str = "ae11e670337e3e5eab736b29b4e294edbea8b564661cf15972625783f8930ee3";

fn session() -> Credential {
    Credential::Session(Secret::new("required-unit-fixture-secret".into()))
}
fn policy() -> auth::AuthPolicy {
    auth::AuthPolicy {
        registration: auth::RegistrationPolicy::Open,
        session_tenant: auth::SessionTenantPolicy::AccountTenant,
        first_user: auth::FirstUserTenant::NewUser,
    }
}
fn id(value: u64) -> PositiveId {
    PositiveId::new(value).unwrap()
}
fn node(name: &str) -> Value {
    include_str!("fixtures/working-draft-writes-node.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|case| case["name"] == name)
        .unwrap()
}
fn updated_body() -> Value {
    json!({
        "expected_snapshot_digest": OPENED,
        "decisions": [
            {"kind":"set_included","draft_part_ids":[5],"value":true},
            {"kind":"set_quantity_override","draft_part_ids":[4],"value":null},
        ],
    })
}
fn updated() -> EditRequest {
    serde_json::from_value(updated_body()).unwrap()
}

struct Fixture {
    path: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pp-draft-writes-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("print-partner.db"),
            include_bytes!("fixtures/required-units/unchanged.db"),
        )
        .unwrap();
        Self { path }
    }
    fn sql(&self) -> Connection {
        Connection::open(self.path.join("print-partner.db")).unwrap()
    }
    fn open(&self) -> WriterOwner {
        WriterOwner::open(&self.path, Limits::default()).unwrap().0
    }
    fn rows(&self, sql: &str) -> Value {
        let connection = self.sql();
        let mut statement = connection.prepare(sql).unwrap();
        let columns = statement
            .column_names()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let rows = statement
            .query_map([], |row| {
                Ok(Value::Object(
                    columns
                        .iter()
                        .enumerate()
                        .map(|(i, column)| {
                            let value = match row.get_ref(i).unwrap() {
                                rusqlite::types::ValueRef::Null => Value::Null,
                                rusqlite::types::ValueRef::Integer(n) => json!(n),
                                value => json!(value.as_str().unwrap()),
                            };
                            (column.clone(), value)
                        })
                        .collect(),
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        Value::from(rows)
    }
    fn recorded_rows(&self) -> Value {
        json!({
            "drafts": self.rows("SELECT id,state,lifecycle_version,digest_format,current_required_unit_reconciliation_id,snapshot_digest FROM plan_drafts ORDER BY id"),
            "parts": self.rows("SELECT id,draft_id,included,quantity_override,quantity_effective FROM plan_draft_parts ORDER BY id"),
            "reconciliations": self.rows("SELECT id,draft_id,actor_id,idempotency_key,planning_digest,result_kind,reconciliation_digest,payload_digest FROM plan_draft_required_unit_reconciliations ORDER BY id"),
            "assignments": self.rows("SELECT reconciliation_id,target_draft_part_id,unit_index,kind,required_unit_token FROM plan_draft_required_unit_assignments ORDER BY reconciliation_id,target_draft_part_id,unit_index"),
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn compare_node(name: &str) {
    compare_node_errors(name, &[]);
}

fn compare_node_errors(name: &str, error_replacements: &[(&str, &str)]) {
    let case = node(name);
    let f = Fixture::new();
    for sql in case["setup"].as_array().unwrap() {
        f.sql().execute_batch(sql.as_str().unwrap()).unwrap();
    }
    if case["select"] == true {
        let owner = f.open();
        let request =
            serde_json::from_value(json!({"expected_snapshot_digest":OPENED, "decisions":[]}))
                .unwrap();
        let command =
            ReconciliationCommand::new(id(1), id(2), request, "core-selection", session()).unwrap();
        owner
            .required_units_with_policy(policy())
            .unwrap()
            .reconcile(
                command,
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(2),
            )
            .unwrap();
        owner.shutdown().unwrap();
    }
    let calls = case["calls"].as_array().unwrap();
    let results = case["results"].as_array().unwrap();
    assert_eq!(calls.len(), results.len());
    for (index, (call, expected)) in calls.iter().zip(results).enumerate() {
        if call["op"] == "sql" {
            f.sql()
                .execute_batch(call["sql"].as_str().unwrap())
                .unwrap();
            continue;
        }
        let owner = f.open();
        let client = owner.working_drafts_with_policy(policy()).unwrap();
        let profile = id(call["profile"].as_u64().unwrap());
        let draft = id(call["draft"].as_u64().unwrap());
        let cancelled = Arc::new(AtomicBool::new(false));
        let wait = Duration::from_secs(2);
        let request = call["request"].clone();
        let actual = match call["op"].as_str().unwrap() {
            "edit" => client
                .edit(
                    session(),
                    profile,
                    draft,
                    serde_json::from_value(request).unwrap(),
                    cancelled,
                    wait,
                )
                .map(|outcome| serde_json::to_value(outcome).unwrap()),
            "abandon" => client
                .abandon(
                    session(),
                    profile,
                    draft,
                    serde_json::from_value(request).unwrap(),
                    cancelled,
                    wait,
                )
                .map(|outcome| serde_json::to_value(outcome).unwrap()),
            other => panic!("Unknown Node call {other}"),
        };
        match expected["error"].as_str() {
            Some(message) => {
                let message = error_replacements
                    .iter()
                    .find(|(from, _)| *from == message)
                    .map(|(_, to)| *to)
                    .unwrap_or(message);
                assert_eq!(
                    actual.unwrap_err().to_string(),
                    message,
                    "{name} call {index}"
                );
            }
            None => assert_eq!(&actual.unwrap(), expected, "{name} call {index}"),
        }
        owner.shutdown().unwrap();
    }
    let rows = f.recorded_rows();
    for table in ["drafts", "parts", "reconciliations", "assignments"] {
        assert_eq!(rows[table], case[table], "{name} {table}");
    }
}
macro_rules! node_case {
    ($test:ident, $name:literal) => {
        #[test]
        fn $test() {
            compare_node($name);
        }
    };
}
node_case!(
    edit_and_its_retry_select_units_like_node,
    "edit-updated-and-retried"
);
node_case!(
    unchanged_edit_with_a_stale_digest_selects_like_node,
    "edit-unchanged-with-stale-digest"
);
node_case!(
    stale_digest_edit_is_draft_changed_without_writes,
    "edit-conflict"
);
node_case!(
    missing_profile_draft_and_parts_are_not_found,
    "edit-missing"
);
node_case!(edit_of_a_consumed_draft_is_not_open, "edit-consumed");
node_case!(
    edit_after_the_accepted_base_moves_is_base_changed,
    "edit-base-changed"
);
node_case!(
    edit_without_an_accepted_baseline_is_refused,
    "edit-baseline-required"
);
node_case!(
    failed_selection_errors_and_its_retry_selects,
    "edit-selection-fails-then-retried"
);
#[test]
fn failed_selection_keeps_the_edit_after_the_base_changes() {
    // The fixture sets one quantity to 10_000, above the assignment cap, so
    // selection refuses before the injected insert trigger.
    compare_node_errors(
        "edit-selection-fails-then-base-changes",
        &[(
            "injected selection write failure",
            "Reconciliation row limit exceeded",
        )],
    );
}
node_case!(
    clearing_to_an_unselectable_inferred_quantity_keeps_the_edit,
    "edit-clears-to-an-unselectable-inferred-quantity"
);
node_case!(
    edit_of_a_selected_draft_replaces_its_selection,
    "edit-selected"
);
node_case!(
    abandon_retry_and_later_requests_match_node,
    "abandon-retried-then-not-open"
);
node_case!(
    abandon_with_a_stale_lifecycle_is_draft_changed,
    "abandon-conflict"
);
node_case!(abandon_of_a_consumed_draft_is_not_open, "abandon-consumed");
node_case!(
    abandon_checks_profile_then_version_then_draft,
    "abandon-missing"
);

#[test]
fn requests_accept_what_node_contracts_accept() {
    let contract = node("contract");
    for probe in contract["edit"].as_array().unwrap() {
        let body = probe["body"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<EditRequest>(body).is_ok(),
            probe["accepted"] == true,
            "{body}"
        );
    }
    for probe in contract["batchLimit"].as_array().unwrap() {
        let decisions = (1..=probe["count"].as_u64().unwrap())
            .map(|part| json!({"kind":"set_included","draft_part_ids":[part],"value":true}))
            .collect::<Vec<_>>();
        let body = json!({"expected_snapshot_digest":OPENED, "decisions":decisions});
        assert_eq!(
            serde_json::from_value::<EditRequest>(body).is_ok(),
            probe["accepted"] == true,
            "{} decisions",
            probe["count"]
        );
    }
    for probe in contract["abandon"].as_array().unwrap() {
        let body = probe["body"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<AbandonRequest>(body).is_ok(),
            probe["accepted"] == true,
            "{body}"
        );
    }
}

fn cancel_while_writing(f: &Fixture, slow_event: &str, edit_committed: bool) -> Value {
    f.sql().execute_batch(&format!("CREATE TRIGGER slow_write AFTER {slow_event} BEGIN SELECT sum(n) FROM (WITH RECURSIVE n(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM n WHERE n<1000000) SELECT n FROM n); END")).unwrap();
    let request = updated_body();
    let owner = f.open();
    let client = owner.working_drafts_with_policy(policy()).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker = {
        let cancelled = cancelled.clone();
        std::thread::spawn(move || {
            client.edit(
                session(),
                id(1),
                id(2),
                serde_json::from_value(request).unwrap(),
                cancelled,
                Duration::from_secs(2),
            )
        })
    };
    let probe = f.sql();
    probe.busy_timeout(Duration::ZERO).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let committed = f.rows(
            "SELECT id,included,quantity_override FROM plan_draft_parts WHERE id IN (4,5) ORDER BY id",
        ) == json!([
            {"id":4,"included":1,"quantity_override":null},
            {"id":5,"included":1,"quantity_override":1}
        ]);
        if committed == edit_committed {
            match probe.execute_batch("BEGIN IMMEDIATE") {
                Ok(()) => probe.execute_batch("ROLLBACK").unwrap(),
                Err(rusqlite::Error::SqliteFailure(error, _))
                    if error.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    break;
                }
                Err(error) => panic!("{error}"),
            }
        }
        assert!(Instant::now() < deadline, "edit never reached {slow_event}");
        std::thread::sleep(Duration::from_millis(1));
    }
    cancelled.store(true, Ordering::Release);
    let error = worker.join().unwrap().unwrap_err();
    assert!(
        matches!(error.downcast_ref(), Some(Failure::Cancelled)),
        "{error:?}"
    );
    owner.shutdown().unwrap();
    f.recorded_rows()
}

#[test]
fn cancellation_before_the_edit_commits_rolls_it_back() {
    let f = Fixture::new();
    let before = f.recorded_rows();
    assert_eq!(
        cancel_while_writing(&f, "UPDATE ON plan_draft_parts", false),
        before
    );
}

#[test]
fn cancellation_during_selection_keeps_the_committed_edit() {
    let f = Fixture::new();
    let rows = cancel_while_writing(
        &f,
        "INSERT ON plan_draft_required_unit_reconciliations",
        true,
    );
    assert_eq!(
        rows["parts"],
        json!([
            {"id":1,"draft_id":1,"included":1,"quantity_override":3,"quantity_effective":3},
            {"id":2,"draft_id":1,"included":0,"quantity_override":1,"quantity_effective":1},
            {"id":3,"draft_id":1,"included":1,"quantity_override":2,"quantity_effective":2},
            {"id":4,"draft_id":2,"included":1,"quantity_override":null,"quantity_effective":1},
            {"id":5,"draft_id":2,"included":1,"quantity_override":1,"quantity_effective":1},
            {"id":6,"draft_id":2,"included":1,"quantity_override":2,"quantity_effective":2}
        ])
    );
    assert_eq!(rows["reconciliations"].as_array().unwrap().len(), 1);
    assert_eq!(rows["assignments"].as_array().unwrap().len(), 6);
    assert!(rows["drafts"][1]["current_required_unit_reconciliation_id"].is_null());
}

#[test]
fn cancelled_unauthenticated_and_stopped_writes_change_nothing() {
    let f = Fixture::new();
    let before = f.recorded_rows();
    let owner = f.open();
    let client = owner.working_drafts_with_policy(policy()).unwrap();
    let abandon = |credential, cancelled| {
        client.abandon(
            credential,
            id(1),
            id(2),
            serde_json::from_value(json!({"expected_lifecycle_version":0})).unwrap(),
            Arc::new(AtomicBool::new(cancelled)),
            Duration::from_secs(2),
        )
    };
    let error = client
        .edit(
            session(),
            id(1),
            id(2),
            updated(),
            Arc::new(AtomicBool::new(true)),
            Duration::ZERO,
        )
        .unwrap_err();
    assert!(matches!(error.downcast_ref(), Some(Failure::Cancelled)));
    let error = abandon(session(), true).unwrap_err();
    assert!(matches!(error.downcast_ref(), Some(Failure::Cancelled)));
    let stranger = || Credential::Session(Secret::new("not-a-session".into()));
    assert_eq!(
        abandon(stranger(), false).unwrap_err().to_string(),
        "Authentication required"
    );
    let error = client
        .edit(
            stranger(),
            id(1),
            id(2),
            updated(),
            Arc::new(AtomicBool::new(false)),
            Duration::ZERO,
        )
        .unwrap_err();
    assert_eq!(error.to_string(), "Authentication required");
    owner.shutdown().unwrap();
    let error = abandon(session(), false).unwrap_err();
    assert!(matches!(error.downcast_ref(), Some(Failure::Stopped)));
    assert_eq!(f.recorded_rows(), before);
}
