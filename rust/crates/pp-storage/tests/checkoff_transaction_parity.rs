#![cfg(feature = "desktop_rebuild")]
//! Replays `fixtures/parity/checkoff-transactions/node.json` through the public
//! checkoff progress client and compares each receipt with the Required-unit
//! and progress rows Node recorded.
use pp_storage::{
    Limits, WriterOwner,
    auth::Secret,
    checkoff_progress::{Basis, Import, Request, Response},
    read_model::Credential,
};
use rusqlite::Connection;
use serde_json::{Value, json};

#[path = "common/json_rows.rs"]
mod json_rows;
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

const NODE: &str = include_str!("../../../../fixtures/parity/checkoff-transactions/node.json");
const WAIT: Duration = Duration::from_secs(5);

fn node() -> Value {
    serde_json::from_str(NODE).unwrap()
}

fn case(name: &str) -> Value {
    node()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap()
        .clone()
}

fn session(database: &str) -> &'static str {
    match database {
        "rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db" => {
            "required-unit-fixture-secret"
        }
        other => panic!("Unknown Node fixture {other}"),
    }
}

fn bytes(database: &str) -> &'static [u8] {
    match database {
        "rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db" => {
            include_bytes!("fixtures/required-units/unchanged.db")
        }
        other => panic!("Unknown Node fixture {other}"),
    }
}

struct Fixture {
    path: PathBuf,
    session: &'static str,
}

impl Fixture {
    fn new(case: &Value) -> Self {
        let database = case["database"].as_str().unwrap();
        let path = std::env::temp_dir().join(format!(
            "pp-checkoff-transactions-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir_all(&path).unwrap();
        if database.ends_with("required-units/unchanged.db") {
            std::fs::create_dir_all(path.join("repos/1/revisions/fixture")).unwrap();
        }
        std::fs::write(path.join("print-partner.db"), bytes(database)).unwrap();
        Self {
            path,
            session: session(database),
        }
    }

    fn sql(&self) -> Connection {
        Connection::open(self.path.join("print-partner.db")).unwrap()
    }

    fn open(&self) -> WriterOwner {
        WriterOwner::open(&self.path, Limits::default()).unwrap().0
    }

    fn credential(&self) -> Credential {
        Credential::Session(Secret::new(self.session.into()))
    }

    fn read(&self) -> Value {
        json_rows::query_rows(&self.sql(), node()["queries"].as_object().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn response(owner: &WriterOwner, fixture: &Fixture, step: &Value) -> Response {
    let cancelled = AtomicBool::new(false);
    let request = &step["request"];
    match step["op"].as_str().unwrap() {
        "complete" => {
            let expected: Basis = serde_json::from_value(request["expected"].clone()).unwrap();
            owner
                .checkoff_progress()
                .unwrap()
                .apply(
                    fixture.credential(),
                    Request::Completion {
                        expected,
                        token: request["token"].as_str().unwrap().to_owned(),
                        completed: request["completed"].as_bool().unwrap(),
                    },
                    &cancelled,
                    WAIT,
                )
                .unwrap()
        }
        "import" => match Import::parse(request.clone()) {
            Ok(import) => owner
                .checkoff_progress()
                .unwrap()
                .apply(
                    fixture.credential(),
                    Request::Import(import),
                    &cancelled,
                    WAIT,
                )
                .unwrap(),
            Err(response) => response,
        },
        other => panic!("Unknown Node step {other}"),
    }
}

/// Runs Node's recorded requests and returns the fixture with the observed case.
fn replay(name: &str) -> (Fixture, Value, Value) {
    let expected = case(name);
    let fixture = Fixture::new(&expected);
    for sql in expected["setup"].as_array().unwrap() {
        fixture.sql().execute_batch(sql.as_str().unwrap()).unwrap();
    }
    let before = fixture.read();
    let reopened_at = expected["reopenedAt"].as_u64().unwrap() as usize;
    let mut owner = fixture.open();
    let mut steps = Vec::new();
    for (index, step) in expected["steps"].as_array().unwrap().iter().enumerate() {
        if index == reopened_at {
            owner.shutdown().unwrap();
            owner = fixture.open();
        }
        let receipt = serde_json::to_value(response(&owner, &fixture, step)).unwrap();
        let mut rows = fixture.read();
        rows.as_object_mut()
            .unwrap()
            .insert("receipts".into(), json!([receipt.clone()]));
        steps.push(json!({
            "op": step["op"],
            "request": step["request"],
            "receipt": receipt,
            "rows": rows,
        }));
    }
    owner.shutdown().unwrap();
    let observed = json!({
        "name": name,
        "database": expected["database"],
        "setup": expected["setup"],
        "reopenedAt": reopened_at,
        "before": before,
        "steps": steps,
    });
    (fixture, observed, expected)
}

fn assert_matches_node(observed: &Value, expected: &Value) {
    assert_eq!(observed["before"], expected["before"]);
    let steps = expected["steps"].as_array().unwrap();
    assert_eq!(observed["steps"].as_array().unwrap().len(), steps.len());
    for (index, step) in steps.iter().enumerate() {
        assert_eq!(&observed["steps"][index], step, "step {index}");
    }
    assert_eq!(observed, expected);
}

fn progress_len(step: &Value) -> usize {
    step["rows"]["progress"].as_array().unwrap().len()
}

fn replay_keeps_one_result(expected: &Value) {
    let steps = expected["steps"].as_array().unwrap();
    let width = progress_len(&steps[0]);
    assert!(width > 0);
    for (index, step) in steps.iter().enumerate() {
        assert_eq!(progress_len(step), width, "step {index} progress rows");
        assert_eq!(step["rows"]["units"], expected["before"]["units"]);
        assert_eq!(step["rows"]["receipts"].as_array().unwrap().len(), 1);
        assert_eq!(step["receipt"], step["rows"]["receipts"][0]);
        if index > 0 && step["request"] == steps[index - 1]["request"] {
            assert_eq!(
                step["receipt"],
                steps[index - 1]["receipt"],
                "step {index} replay"
            );
            assert_eq!(
                step["rows"]["progress"],
                steps[index - 1]["rows"]["progress"]
            );
        }
    }
    let reopened = expected["reopenedAt"].as_u64().unwrap() as usize;
    assert_eq!(steps[reopened]["receipt"], steps[reopened - 1]["receipt"]);
    assert_eq!(
        steps[reopened]["rows"]["progress"],
        steps[reopened - 1]["rows"]["progress"]
    );
}

fn refusals_keep_rows(expected: &Value) {
    let steps = expected["steps"].as_array().unwrap();
    for (index, step) in steps.iter().enumerate().skip(1) {
        if step["receipt"]["status"] == 200 {
            continue;
        }
        assert_eq!(
            step["rows"]["progress"],
            steps[index - 1]["rows"]["progress"],
            "step {index} refusal"
        );
        assert_ne!(step["receipt"]["status"], 200);
    }
    assert!(steps.iter().any(|step| step["receipt"]["status"] == 400));
    assert!(steps.iter().any(|step| step["receipt"]["status"] == 409));
}

#[test]
fn recorded_physical_result_replays_and_refuses_like_node() {
    let (fixture, observed, expected) = replay("recorded-physical-result");
    assert_matches_node(&observed, &expected);
    replay_keeps_one_result(&expected);
    refusals_keep_rows(&expected);
    assert!(
        expected["before"]["progress"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| {
                row["part_id"] == 1
                    && row["unit_index"] == 0
                    && row["completed"] == 1
                    && row["assembled"] == 1
            })
    );
    assert_eq!(
        expected["steps"][0]["receipt"],
        json!({
            "status": 200,
            "body": {
                "part_id": 1,
                "printed_count": 2,
                "print_units": [true, true, false],
                "assembled_units": [true, false, false],
                "missing": true
            }
        })
    );
    assert_eq!(
        expected["steps"][0]["rows"]["progress"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["part_id"] == 1)
            .map(|row| (row["completed"].as_i64(), row["assembled"].as_i64()))
            .collect::<Vec<_>>(),
        [(Some(1), Some(1)), (Some(1), Some(0)), (Some(0), Some(0))]
    );

    let mut duplicated = observed.clone();
    let row = duplicated["steps"][0]["rows"]["progress"][0].clone();
    duplicated["steps"][0]["rows"]["progress"]
        .as_array_mut()
        .unwrap()
        .push(row);
    assert_ne!(duplicated, expected);
    let mut extra_receipt = observed.clone();
    let receipt = extra_receipt["steps"][1]["receipt"].clone();
    extra_receipt["steps"][1]["rows"]["receipts"]
        .as_array_mut()
        .unwrap()
        .push(receipt);
    assert_ne!(extra_receipt, expected);
    let mut changed_status = observed;
    changed_status["steps"][0]["rows"]["progress"][0]["completed"] = json!(0);
    assert_ne!(changed_status, expected);

    let duplicate = fixture.sql().execute(
        "INSERT INTO print_progress (tenant_id, part_id, unit_index, completed, assembled)
         SELECT tenant_id, part_id, unit_index, completed, assembled FROM print_progress LIMIT 1",
        [],
    );
    assert!(
        duplicate.unwrap_err().to_string().contains("UNIQUE"),
        "duplicate progress write must fail"
    );
    let changed = fixture
        .sql()
        .execute("UPDATE required_units SET object_name = object_name", []);
    assert!(
        changed
            .unwrap_err()
            .to_string()
            .contains("Required unit is immutable"),
        "changing a Required unit must fail"
    );
}
