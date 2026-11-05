#![cfg(feature = "desktop_rebuild")]
//! T-61-checkoff: physical-result verification and progress as one storage
//! transaction. Replays `fixtures/parity/printer-checkoff-verify/node.json`
//! through the public checkoff progress client on a copy of the same fixture,
//! then checks refusal, fault, restart and authentication behavior.
use pp_storage::{
    Limits, WriterOwner,
    auth::Secret,
    checkoff_progress::{Request, Response},
    read_model::Credential,
};
use rusqlite::Connection;
use serde_json::{Value, json};

#[path = "common/js_json.rs"]
mod js_json;
#[path = "common/json_rows.rs"]
mod json_rows;
use std::{collections::HashMap, path::PathBuf, sync::atomic::AtomicBool, time::Duration};

const NODE: &str = include_str!("../../../../fixtures/parity/printer-checkoff-verify/node.json");
const DATABASE: &[u8] = include_bytes!("fixtures/required-units/unchanged.db");
const WAIT: Duration = Duration::from_secs(5);

fn node() -> Value {
    js_json::parse(NODE)
}

/// The Node generator's normalizer: generated UUIDs become `<uuid:n>` in order of
/// first appearance, and ISO times outside the seeded year become `<now>`.
#[derive(Default)]
struct Normalizer(HashMap<String, String>);

impl Normalizer {
    fn uuid(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 36
            && b.iter().enumerate().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => *c == b'-',
                14 => *c == b'4',
                19 => b"89ab".contains(c),
                _ => matches!(c, b'0'..=b'9' | b'a'..=b'f'),
            })
    }

    fn iso(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 24
            && b.iter().enumerate().all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                10 => *c == b'T',
                13 | 16 => *c == b':',
                19 => *c == b'.',
                23 => *c == b'Z',
                _ => c.is_ascii_digit(),
            })
    }

    fn walk(&mut self, value: Value) -> Value {
        match value {
            Value::Array(items) => items.into_iter().map(|v| self.walk(v)).collect(),
            Value::Object(map) => {
                Value::Object(map.into_iter().map(|(k, v)| (k, self.walk(v))).collect())
            }
            Value::String(s) if Self::uuid(&s) => {
                let next = format!("<uuid:{}>", self.0.len());
                self.0.entry(s).or_insert(next).clone().into()
            }
            Value::String(s) if Self::iso(&s) && !s.starts_with("2020-") => "<now>".into(),
            other => other,
        }
    }
}

struct Fixture {
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pp-printer-checkoff-verify-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir_all(path.join("repos/1/revisions/fixture")).unwrap();
        std::fs::write(path.join("print-partner.db"), DATABASE).unwrap();
        let fixture = Self { path };
        for sql in node()["setup"].as_array().unwrap() {
            fixture.sql().execute_batch(sql.as_str().unwrap()).unwrap();
        }
        fixture
    }

    fn sql(&self) -> Connection {
        Connection::open(self.path.join("print-partner.db")).unwrap()
    }

    fn open(&self) -> WriterOwner {
        WriterOwner::open(&self.path, Limits::default()).unwrap().0
    }

    fn rows(&self, normalizer: &mut Normalizer) -> Value {
        let mut rows = json_rows::query_rows(&self.sql(), node()["queries"].as_object().unwrap());
        for link in rows["links"].as_array_mut().unwrap() {
            link["value"] = js_json::parse(link["value"].as_str().unwrap());
        }
        normalizer.walk(rows)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn session() -> Credential {
    Credential::Session(Secret::new("required-unit-fixture-secret".into()))
}

fn apply(
    owner: &WriterOwner,
    credential: Credential,
    op: &str,
    body: &Value,
) -> anyhow::Result<Response> {
    let request = match op {
        "verify" => Request::Verify { body: body.clone() },
        "dismiss" => Request::Dismiss { body: body.clone() },
        other => panic!("Unknown Node step {other}"),
    };
    owner
        .checkoff_progress()
        .unwrap()
        .apply(credential, request, &AtomicBool::new(false), WAIT)
}

/// Replays every Node step, reopening the writer where Node restarted.
fn replay() -> (Fixture, Value, Vec<Value>) {
    let expected = node();
    let fixture = Fixture::new();
    let mut normalizer = Normalizer::default();
    let before = fixture.rows(&mut normalizer);
    let reopened_at = expected["reopenedAt"].as_u64().unwrap() as usize;
    let mut owner = fixture.open();
    let mut steps = Vec::new();
    for (index, step) in expected["steps"].as_array().unwrap().iter().enumerate() {
        if index == reopened_at {
            owner.shutdown().unwrap();
            owner = fixture.open();
        }
        let response = apply(
            &owner,
            session(),
            step["op"].as_str().unwrap(),
            &step["body"],
        )
        .unwrap();
        let receipt = normalizer.walk(serde_json::to_value(response).unwrap());
        let rows = fixture.rows(&mut normalizer);
        steps.push(json!({ "receipt": receipt, "rows": rows }));
    }
    owner.shutdown().unwrap();
    (fixture, before, steps)
}

fn first_success(steps: &[Value]) -> usize {
    steps
        .iter()
        .position(|s| s["receipt"]["status"] == 200)
        .unwrap()
}

fn progress(rows: &Value) -> Vec<(i64, i64, i64)> {
    rows["progress"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["part_id"].as_i64().unwrap(),
                r["unit_index"].as_i64().unwrap(),
                r["completed"].as_i64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn ticket_t_61_checkoff_verify_and_dismiss_match_node_across_restart() {
    let expected = node();
    let (_fixture, before, observed) = replay();
    assert_eq!(before, expected["before"]);
    let steps = expected["steps"].as_array().unwrap();
    assert_eq!(observed.len(), steps.len());
    assert!(steps.len() >= 15);
    let burst = steps.len() - 1;
    for (index, step) in steps.iter().enumerate().take(burst) {
        assert_eq!(
            observed[index]["receipt"], step["receipt"],
            "step {index} receipt"
        );
        assert_eq!(observed[index]["rows"], step["rows"], "step {index} rows");
    }
    let statuses: Vec<u64> = steps
        .iter()
        .map(|s| s["receipt"]["status"].as_u64().unwrap())
        .collect();
    for status in [200, 400, 404, 409] {
        assert!(statuses.contains(&status), "Node corpus covers {status}");
    }
    assert!(
        steps
            .iter()
            .any(|s| s["op"] == "dismiss" && s["receipt"]["status"] == 200)
    );
    assert!(
        steps
            .iter()
            .any(|s| s["receipt"]["body"]["link"]["state"] == "verified")
    );
    assert!(
        steps
            .iter()
            .any(|s| s["body"].get("additional_decisions").is_some()
                && s["receipt"]["status"] == 200)
    );
    for step in &observed {
        assert_eq!(
            step["rows"]["units"], before["units"],
            "Required units never change"
        );
    }
}

#[test]
fn ticket_t_61_checkoff_refused_burst_writes_nothing_where_node_kept_half() {
    let expected = node();
    let (_fixture, _before, observed) = replay();
    let steps = expected["steps"].as_array().unwrap();
    let last = steps.len() - 1;
    assert_eq!(steps[last]["partialInNode"], true);
    assert_eq!(observed[last]["receipt"], steps[last]["receipt"]);
    assert_eq!(observed[last]["receipt"]["status"], 400);
    assert_eq!(
        observed[last]["rows"],
        observed[last - 1]["rows"],
        "Rust refusal writes nothing"
    );
    let node_before = progress(&steps[last - 1]["rows"]);
    let node_after = progress(&steps[last]["rows"]);
    let changed: Vec<_> = node_after
        .iter()
        .filter(|row| !node_before.contains(row))
        .collect();
    assert_eq!(
        changed,
        [&(2, 0, 1)],
        "Node committed Part 2 of the refused burst"
    );
    let mut rest = steps[last]["rows"].clone();
    rest["progress"] = steps[last - 1]["rows"]["progress"].clone();
    assert_eq!(
        rest, observed[last]["rows"],
        "progress is the only difference"
    );
}

#[test]
fn ticket_t_61_checkoff_duplicate_and_stale_file_results_count_once() {
    let (_fixture, _before, observed) = replay();
    let expected = node();
    let steps = expected["steps"].as_array().unwrap();
    let first = first_success(steps);
    assert_eq!(steps[first + 1]["body"], steps[first]["body"]);
    assert_eq!(
        observed[first + 1]["receipt"]["body"]["detail"],
        "All decisions must target pending units on this link"
    );
    assert_eq!(observed[first + 1]["rows"], observed[first]["rows"]);
    let stale = steps
        .iter()
        .position(|s| s["body"]["link_id"] == "link-stale-file")
        .unwrap();
    let receipt = &observed[stale]["receipt"]["body"];
    assert_eq!(
        receipt["units_confirmed"], 0,
        "the re-reported file confirms an already printed unit"
    );
    assert_eq!(
        receipt["outcomes"].as_array().unwrap().len(),
        1,
        "the outcome is still recorded"
    );
    assert_eq!(
        progress(&observed[stale]["rows"]),
        progress(&observed[first]["rows"])
    );
    let printed = progress(&observed[first]["rows"])
        .iter()
        .filter(|r| r.2 == 1)
        .count();
    assert_eq!(printed, 2);
}

#[test]
fn ticket_t_61_checkoff_monitor_states_and_other_tenants_never_mark() {
    let (_fixture, _before, observed) = replay();
    let expected = node();
    for (index, step) in expected["steps"].as_array().unwrap().iter().enumerate() {
        let id = step["body"]["link_id"].as_str().unwrap_or_default();
        if id == "link-watching" || id == "link-foreign" {
            assert_ne!(observed[index]["receipt"]["status"], 200, "{id}");
            assert_eq!(
                observed[index]["rows"],
                observed[index - 1]["rows"],
                "{id} writes nothing"
            );
        }
    }
    let fixture = Fixture::new();
    let mut normalizer = Normalizer::default();
    let before = fixture.rows(&mut normalizer);
    let owner = fixture.open();
    let body = json!({ "link_id": "link-verify", "decisions": [{ "part_id": 1, "unit_index": 1, "result": "confirmed" }] });
    let error = apply(
        &owner,
        Credential::Session(Secret::new("not-a-session".into())),
        "verify",
        &body,
    )
    .unwrap_err();
    assert!(!error.to_string().is_empty());
    owner.shutdown().unwrap();
    assert_eq!(
        fixture.rows(&mut Normalizer::default()),
        before,
        "an unauthenticated verify writes nothing"
    );
}

#[test]
fn ticket_t_61_checkoff_fault_after_first_write_leaves_no_partial_record() {
    let expected = node();
    let steps = expected["steps"].as_array().unwrap();
    let first = first_success(steps);
    let fixture = Fixture::new();
    let owner = fixture.open();
    for step in &steps[..first] {
        apply(
            &owner,
            session(),
            step["op"].as_str().unwrap(),
            &step["body"],
        )
        .unwrap();
    }
    let before = fixture.rows(&mut Normalizer::default());
    // The second outcome row fails after progress, the link, the job and the
    // first outcome were written inside the transaction.
    fixture
        .sql()
        .execute_batch(
            "CREATE TRIGGER injected_fault BEFORE INSERT ON print_job_parts WHEN NEW.part_id = 3
             BEGIN SELECT RAISE(ABORT, 'injected fault'); END;",
        )
        .unwrap();
    let failed = apply(&owner, session(), "verify", &steps[first]["body"]).unwrap();
    assert_eq!(
        serde_json::to_value(&failed).unwrap(),
        json!({ "status": 500, "body": { "detail": "Internal Server Error" } })
    );
    assert_eq!(
        fixture.rows(&mut Normalizer::default()),
        before,
        "no partial record"
    );
    owner.shutdown().unwrap();

    fixture
        .sql()
        .execute_batch("DROP TRIGGER injected_fault")
        .unwrap();
    let owner = fixture.open();
    let mut normalizer = Normalizer::default();
    let retried = apply(&owner, session(), "verify", &steps[first]["body"]).unwrap();
    assert_eq!(
        normalizer.walk(serde_json::to_value(retried).unwrap()),
        steps[first]["receipt"]
    );
    assert_eq!(
        fixture.rows(&mut normalizer),
        steps[first]["rows"],
        "the retry after restart matches Node"
    );
    owner.shutdown().unwrap();
}
