#![cfg(feature = "desktop_rebuild")]
//! Replays `fixtures/parity/required-unit-decisions/node.json` through the
//! public working-draft and reconciliation clients and compares every result
//! and every recorded row with Node's after the same normalization.
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    read_model::Credential,
    required_units::ReconciliationCommand,
    working_drafts::PositiveId,
};
use rusqlite::Connection;
use serde_json::{Value, json};

#[path = "common/json_rows.rs"]
mod json_rows;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const NODE: &str = include_str!("../../../../fixtures/parity/required-unit-decisions/node.json");
const WAIT: Duration = Duration::from_secs(5);
const RECONCILIATION_KEYS: [&str; 2] = [
    "reconciliation_id",
    "current_required_unit_reconciliation_id",
];

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

fn id(value: u64) -> PositiveId {
    PositiveId::new(value).unwrap()
}

fn policy() -> auth::AuthPolicy {
    auth::AuthPolicy {
        registration: auth::RegistrationPolicy::Open,
        session_tenant: auth::SessionTenantPolicy::AccountTenant,
        first_user: auth::FirstUserTenant::NewUser,
    }
}

fn is_token(value: &str) -> bool {
    value.len() == 36
        && value.starts_with("ppu_")
        && value[4..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The generator's rule: tokens and reconciliation IDs across one case become
/// labels numbered by sorted value.
fn normalize(value: &Value) -> Value {
    fn collect(node: &Value, key: &str, tokens: &mut BTreeSet<String>, ids: &mut BTreeSet<i64>) {
        match node {
            Value::Array(items) => items.iter().for_each(|i| collect(i, key, tokens, ids)),
            Value::Object(map) => map.iter().for_each(|(k, v)| collect(v, k, tokens, ids)),
            Value::String(s) if is_token(s) => {
                tokens.insert(s.clone());
            }
            Value::Number(n) if RECONCILIATION_KEYS.contains(&key) => {
                ids.insert(n.as_i64().unwrap());
            }
            _ => {}
        }
    }
    fn rewrite(
        node: &Value,
        key: &str,
        tokens: &BTreeMap<&str, String>,
        ids: &BTreeMap<i64, String>,
    ) -> Value {
        match node {
            Value::Array(items) => items.iter().map(|i| rewrite(i, key, tokens, ids)).collect(),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), rewrite(v, k, tokens, ids)))
                    .collect(),
            ),
            Value::String(s) if tokens.contains_key(s.as_str()) => json!(tokens[s.as_str()]),
            Value::Number(n) if RECONCILIATION_KEYS.contains(&key) => {
                json!(ids[&n.as_i64().unwrap()])
            }
            other => other.clone(),
        }
    }
    let (mut tokens, mut ids) = (BTreeSet::new(), BTreeSet::new());
    collect(value, "", &mut tokens, &mut ids);
    let tokens = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| (t.as_str(), format!("unit#{i}")))
        .collect();
    let ids = ids
        .iter()
        .enumerate()
        .map(|(i, n)| (*n, format!("reconciliation#{i}")))
        .collect();
    rewrite(value, "", &tokens, &ids)
}

struct Fixture {
    path: PathBuf,
    session: &'static str,
}

impl Fixture {
    fn new(case: &Value) -> Self {
        let (bytes, session): (&[u8], _) = match case["database"].as_str().unwrap() {
            "rust/crates/pp-storage/tests/fixtures/accepted-plate-node.db" => (
                include_bytes!("fixtures/accepted-plate-node.db"),
                "read-fixture-secret",
            ),
            "rust/crates/pp-storage/tests/fixtures/required-units/unsafe.db" => (
                include_bytes!("fixtures/required-units/unsafe.db"),
                "required-unit-fixture-secret",
            ),
            other => panic!("Unknown Node fixture {other}"),
        };
        let path = std::env::temp_dir().join(format!(
            "pp-required-unit-decisions-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("print-partner.db"), bytes).unwrap();
        Self { path, session }
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

    fn read(&self, group: &str) -> Value {
        json_rows::query_rows(&self.sql(), node()["queries"][group].as_object().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Runs Node's recorded requests through the Rust clients and returns the
/// fixture with the observed case, normalized as Node's is.
fn replay(name: &str) -> (Fixture, Value, Value) {
    let expected = case(name);
    let f = Fixture::new(&expected);
    for sql in expected["setup"].as_array().unwrap() {
        f.sql().execute_batch(sql.as_str().unwrap()).unwrap();
    }
    let history = f.read("history");
    let reopened_at = expected["reopenedAt"].as_u64().unwrap() as usize;
    let mut owner = f.open();
    let mut steps = Vec::new();
    for (index, step) in expected["steps"].as_array().unwrap().iter().enumerate() {
        if index == reopened_at {
            owner.shutdown().unwrap();
            owner = f.open();
        }
        let request = step["request"].clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let result = match step["op"].as_str().unwrap() {
            "edit" => serde_json::to_value(
                owner
                    .working_drafts_with_policy(policy())
                    .unwrap()
                    .edit(
                        f.credential(),
                        id(1),
                        id(2),
                        serde_json::from_value(request.clone()).unwrap(),
                        cancelled,
                        WAIT,
                    )
                    .unwrap(),
            ),
            "reconcile" => {
                let command = ReconciliationCommand::new(
                    id(1),
                    id(2),
                    serde_json::from_value(request.clone()).unwrap(),
                    step["key"].as_str().unwrap(),
                    f.credential(),
                )
                .unwrap();
                serde_json::to_value(
                    owner
                        .required_units_with_policy(policy())
                        .unwrap()
                        .reconcile(command, cancelled, WAIT)
                        .unwrap(),
                )
            }
            other => panic!("Unknown Node step {other}"),
        }
        .unwrap();
        steps.push(json!({
            "op": step["op"], "key": step["key"], "request": request,
            "result": result, "state": f.read("state"),
        }));
    }
    owner.shutdown().unwrap();
    assert_eq!(f.read("history"), history, "{name} keeps history");
    let observed = json!({
        "name": name, "database": expected["database"], "setup": expected["setup"],
        "reopenedAt": reopened_at, "history": history, "steps": steps,
    });
    (f, observed, expected)
}

fn assert_matches_node(observed: &Value, expected: &Value) {
    let actual = normalize(observed);
    assert_eq!(actual["history"], expected["history"]);
    let steps = expected["steps"].as_array().unwrap();
    assert_eq!(actual["steps"].as_array().unwrap().len(), steps.len());
    for (index, step) in steps.iter().enumerate() {
        assert_eq!(actual["steps"][index], *step, "step {index}");
    }
    assert_eq!(actual, *expected);
}

/// `[draft Part, unit index, kind, token]` for the draft's selected
/// reconciliation in one normalized step.
fn selected(step: &Value) -> Vec<Value> {
    let current = &step["state"]["drafts"][1]["current_required_unit_reconciliation_id"];
    step["state"]["assignments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["reconciliation_id"] == *current)
        .map(|a| {
            json!([
                a["target_draft_part_id"],
                a["unit_index"],
                a["kind"],
                a["required_unit_token"]
            ])
        })
        .collect()
}

fn included(step: &Value) -> Vec<Value> {
    step["result"]["workspace"]["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .map(|p| json!([p["draft_part_id"], p["included"], p["quantity_effective"]]))
                .collect()
        })
        .unwrap_or_default()
}

fn swap_first_reused_tokens(case: &mut Value) {
    let step = case["steps"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|s| {
            s["state"]["assignments"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| a["kind"] == "reuse")
                .count()
                >= 2
        })
        .unwrap();
    let rows = step["state"]["assignments"].as_array_mut().unwrap();
    let reused: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i]["kind"] == "reuse")
        .take(2)
        .collect();
    let first = rows[reused[0]]["required_unit_token"].clone();
    rows[reused[0]]["required_unit_token"] = rows[reused[1]]["required_unit_token"].clone();
    rows[reused[1]]["required_unit_token"] = first;
}

#[test]
fn excluded_restored_and_split_parts_keep_their_plate_tokens_like_node() {
    let (f, observed, expected) = replay("plate-exclude-restore-split");
    assert_matches_node(&observed, &expected);
    let history = &expected["history"];
    assert_eq!(
        history["plateUnits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| &u["required_unit_token"])
            .collect::<Vec<_>>(),
        [&json!("unit#0")]
    );
    assert_eq!(
        history["mapping"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| json!([m["revision_part_id"], m["required_unit_token"]]))
            .collect::<Vec<_>>(),
        [json!([1, "unit#1"]), json!([2, "unit#0"])]
    );
    assert_eq!(
        history["progress"],
        json!([
            {"part_id":1,"unit_index":0,"completed":1,"assembled":1},
            {"part_id":2,"unit_index":0,"completed":1,"assembled":1}
        ])
    );
    let steps = expected["steps"].as_array().unwrap();
    let both = [
        json!([3, 0, "reuse", "unit#1"]),
        json!([4, 0, "reuse", "unit#0"]),
    ];
    let restored = [json!([3, true, 1]), json!([4, true, 1])];
    let kinds: Vec<_> = steps.iter().map(|s| s["result"]["kind"].clone()).collect();
    assert_eq!(
        kinds,
        [
            "ready",
            "ready",
            "ready",
            "ready",
            "draft_changed",
            "draft_changed",
            "ready",
            "ready"
        ]
    );
    assert_eq!(
        included(&steps[0]),
        [json!([3, false, 1]), json!([4, true, 1])]
    );
    assert_eq!(selected(&steps[0]), both);
    assert_eq!(included(&steps[1]), restored);
    assert_eq!(selected(&steps[1]), both);
    assert_eq!(
        included(&steps[2]),
        [json!([3, true, 1]), json!([4, false, 1])]
    );
    assert_eq!(selected(&steps[2]), both);
    assert_eq!(
        included(&steps[3]),
        [json!([3, true, 1]), json!([4, false, 3])]
    );
    assert_eq!(
        selected(&steps[3]),
        [
            both[0].clone(),
            both[1].clone(),
            json!([4, 1, "create", null]),
            json!([4, 2, "create", null])
        ]
    );
    // Restoring the step-1 planning state replays its automatic selection key.
    assert_eq!(selected(&steps[4]), Vec::<Value>::new());
    assert_eq!(expected["reopenedAt"], 5);
    assert_eq!(selected(&steps[5]), Vec::<Value>::new());
    for step in &steps[6..] {
        assert_eq!(included(step), restored);
        assert_eq!(selected(step), both);
    }
    assert_eq!(
        steps[6]["state"]["drafts"][1]["current_required_unit_reconciliation_id"],
        steps[7]["state"]["drafts"][1]["current_required_unit_reconciliation_id"]
    );

    let mut remapped = observed.clone();
    swap_first_reused_tokens(&mut remapped);
    assert_ne!(normalize(&remapped), expected);
    let error = f
        .sql()
        .execute_batch("DELETE FROM accepted_plate_units")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Accepted Plate unit is immutable"),
        "{error}"
    );
}

#[test]
fn accepted_prior_completion_retains_the_completed_unit_like_node() {
    let (f, observed, expected) = replay("unsafe-retain-completed-split");
    assert_matches_node(&observed, &expected);
    let history = &expected["history"];
    let completed: Vec<_> = history["progress"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["completed"] == 1)
        .map(|p| json!([p["part_id"], p["unit_index"]]))
        .collect();
    assert_eq!(completed, [json!([1, 1])]);
    assert_eq!(
        history["mapping"][1],
        json!({"revision_id":1,"revision_part_id":1,"unit_index":1,"required_unit_token":"unit#1"})
    );
    let steps = expected["steps"].as_array().unwrap();
    let conflict = json!({"kind":"unresolved","conflicts":[{"kind":"unsafe_predecessor","target_draft_part_id":4,"predecessor_revision_part_id":1}]});
    let others = [
        json!([5, 0, "reuse", "unit#3"]),
        json!([6, 0, "reuse", "unit#4"]),
        json!([6, 1, "reuse", "unit#5"]),
    ];
    assert_eq!(steps[0]["result"]["workspace"]["reconciliation"], conflict);
    assert_eq!(
        selected(&steps[1]),
        [
            vec![
                json!([4, 0, "reuse", "unit#1"]),
                json!([4, 1, "create", null]),
                json!([4, 2, "create", null])
            ],
            others.to_vec()
        ]
        .concat()
    );
    assert_eq!(steps[2]["result"]["workspace"]["reconciliation"], conflict);
    assert_eq!(included(&steps[2])[0], json!([4, true, 1]));
    let retained = [vec![json!([4, 0, "reuse", "unit#1"])], others.to_vec()].concat();
    assert_eq!(selected(&steps[3]), retained);
    assert_eq!(expected["reopenedAt"], 4);
    assert_eq!(steps[4]["result"], steps[3]["result"]);
    assert_eq!(selected(&steps[4]), retained);
    assert_eq!(
        selected(&steps[5]),
        [vec![json!([4, 0, "create", null])], others.to_vec()].concat()
    );
    assert_eq!(steps[6]["result"], json!({"kind":"draft_changed"}));
    assert_eq!(selected(&steps[6]), selected(&steps[5]));

    let mut remapped = observed.clone();
    swap_first_reused_tokens(&mut remapped);
    assert_ne!(normalize(&remapped), expected);
    f.sql()
        .execute_batch("UPDATE print_progress SET completed=0 WHERE part_id=1 AND unit_index=1")
        .unwrap();
    let mut lost = observed;
    lost["history"] = f.read("history");
    assert_ne!(normalize(&lost), expected);
}
