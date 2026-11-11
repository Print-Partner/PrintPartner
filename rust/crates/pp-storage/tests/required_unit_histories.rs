//! T-61 (required units).
#![cfg(feature = "desktop_rebuild")]
//! Replays the seeded Node histories in
//! `fixtures/parity/required-unit-histories/node.json` through the public
//! working-draft and reconciliation clients. Every step must match Node's
//! outcome, Part view, conflicts, selected assignments and the digest of the
//! full result and state rows. The same histories then check that surviving
//! Required-unit IDs keep their predecessor's tokens and that refused requests
//! leave every decision and assignment row as it was.
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    read_model::Credential,
    required_units::ReconciliationCommand,
    working_drafts::PositiveId,
};
use rusqlite::Connection;

#[path = "common/json_rows.rs"]
mod json_rows;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

const NODE: &str = include_str!("../../../../fixtures/parity/required-unit-histories/node.json");
const WAIT: Duration = Duration::from_secs(5);
const RECONCILIATION_KEYS: [&str; 2] = [
    "reconciliation_id",
    "current_required_unit_reconciliation_id",
];

static CORPUS: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(NODE).unwrap());

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

/// The generator's rule: tokens and reconciliation IDs across one history
/// become labels numbered by sorted value.
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

/// The generator's `canonical`: JSON with object keys sorted at every level.
fn canonical(value: &Value) -> String {
    match value {
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            let fields: Vec<_> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::from(k.as_str()), canonical(&map[k])))
                .collect();
            format!("{{{}}}", fields.join(","))
        }
        scalar => scalar.to_string(),
    }
}

/// The generator's `summary` plus the step digest.
fn summary(step: &Value) -> Value {
    let (result, state) = (&step["result"], &step["state"]);
    let mut out = Map::new();
    for key in ["op", "key", "request"] {
        out.insert(key.into(), step[key].clone());
    }
    if let Some(error) = result.get("error") {
        out.insert("error".into(), error.clone());
    } else {
        let rows = |table: &str| state[table].as_array().unwrap().clone();
        let draft = rows("drafts").into_iter().find(|d| d["id"] == 2).unwrap();
        let current = &draft["current_required_unit_reconciliation_id"];
        out.insert("kind".into(), result["kind"].clone());
        out.insert(
            "reconciliation".into(),
            result
                .pointer("/workspace/reconciliation")
                .cloned()
                .unwrap_or(Value::Null),
        );
        let parts = rows("parts")
            .iter()
            .filter(|p| p["draft_id"] == 2)
            .map(|p| {
                json!([
                    p["id"],
                    p["base_revision_part_id"],
                    p["included"],
                    p["quantity_effective"]
                ])
            })
            .collect();
        out.insert("parts".into(), parts);
        let decisions = rows("decisions")
            .iter()
            .filter(|d| d["reconciliation_id"] == *current)
            .map(|d| {
                json!([
                    d["target_draft_part_id"],
                    d["kind"],
                    d["predecessor_revision_part_id"]
                ])
            })
            .collect();
        out.insert("decisions".into(), decisions);
        let selection = rows("assignments")
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
            .collect();
        out.insert("selection".into(), selection);
    }
    let digest = Sha256::digest(canonical(&json!({"result": result, "state": state})));
    out.insert("digest".into(), json!(hex::encode(digest)));
    Value::Object(out)
}

struct Fixture {
    path: PathBuf,
    session: &'static str,
}

impl Fixture {
    fn new(database: &str) -> Self {
        let (bytes, session): (&[u8], _) = match database {
            "rust/crates/pp-storage/tests/fixtures/accepted-plate-node.db" => (
                include_bytes!("fixtures/accepted-plate-node.db"),
                "read-fixture-secret",
            ),
            "rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db" => (
                include_bytes!("fixtures/required-units/unchanged.db"),
                "required-unit-fixture-secret",
            ),
            "rust/crates/pp-storage/tests/fixtures/required-units/shrink.db" => (
                include_bytes!("fixtures/required-units/shrink.db"),
                "required-unit-fixture-secret",
            ),
            "rust/crates/pp-storage/tests/fixtures/required-units/unsafe.db" => (
                include_bytes!("fixtures/required-units/unsafe.db"),
                "required-unit-fixture-secret",
            ),
            other => panic!("Unknown Node fixture {other}"),
        };
        let path = std::env::temp_dir().join(format!(
            "pp-required-unit-histories-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir_all(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
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
        json_rows::query_rows(&self.sql(), CORPUS["queries"][group].as_object().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn edit(owner: &WriterOwner, f: &Fixture, request: &Value) -> anyhow::Result<Value> {
    let outcome = owner.working_drafts_with_policy(policy())?.edit(
        f.credential(),
        id(1),
        id(2),
        serde_json::from_value(request.clone())?,
        Arc::new(AtomicBool::new(false)),
        WAIT,
    )?;
    Ok(serde_json::to_value(outcome)?)
}

fn reconcile(
    owner: &WriterOwner,
    f: &Fixture,
    key: &str,
    request: &Value,
) -> anyhow::Result<Value> {
    let command = ReconciliationCommand::new(
        id(1),
        id(2),
        serde_json::from_value(request.clone())?,
        key,
        f.credential(),
    )?;
    let outcome = owner.required_units_with_policy(policy())?.reconcile(
        command,
        Arc::new(AtomicBool::new(false)),
        WAIT,
    )?;
    Ok(serde_json::to_value(outcome)?)
}

/// Runs one Node history's literal requests and returns the normalized
/// `{history, initial, steps}` the Rust clients produced.
fn replay(expected: &Value) -> Value {
    let f = Fixture::new(expected["database"].as_str().unwrap());
    for sql in expected["setup"].as_array().unwrap() {
        f.sql().execute_batch(sql.as_str().unwrap()).unwrap();
    }
    let history = f.read("history");
    let initial = f.read("state");
    let reopened_at = expected["reopenedAt"].as_u64().unwrap() as usize;
    let mut owner = f.open();
    let mut steps = Vec::new();
    for (index, step) in expected["steps"].as_array().unwrap().iter().enumerate() {
        if index == reopened_at {
            owner.shutdown().unwrap();
            owner = f.open();
        }
        let request = &step["request"];
        let result = match step["op"].as_str().unwrap() {
            "edit" => edit(&owner, &f, request),
            "reconcile" => reconcile(&owner, &f, step["key"].as_str().unwrap(), request),
            other => panic!("Unknown Node step {other}"),
        }
        .unwrap_or_else(|error| json!({"error": error.root_cause().to_string()}));
        steps.push(json!({
            "op": step["op"], "key": step["key"], "request": request,
            "result": result, "state": f.read("state"),
        }));
    }
    owner.shutdown().unwrap();
    assert_eq!(
        f.read("history"),
        history,
        "{} keeps history",
        expected["name"]
    );
    normalize(&json!({"history": history, "initial": initial, "steps": steps}))
}

/// Replays the histories on up to four threads, since each owner start takes
/// most of a history's time. Results keep the corpus order.
fn replay_all(histories: &[Value]) -> Vec<Value> {
    let next = AtomicUsize::new(0);
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let mut done: Vec<(usize, Value)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(history) = histories.get(index) else {
                            return mine;
                        };
                        mine.push((index, replay(history)));
                    }
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });
    done.sort_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, observed)| observed).collect()
}

fn rows<'a>(value: &'a Value, table: &str) -> &'a Vec<Value> {
    value[table].as_array().unwrap()
}

/// Returns each step whose Rust summary differs from Node's.
fn differences(expected: &Value, observed: &Value) -> Vec<String> {
    let name = &expected["name"];
    let mut out = Vec::new();
    if observed["history"] != expected["history"] {
        out.push(format!("{name} history"));
    }
    let steps = expected["steps"].as_array().unwrap();
    let actual = observed["steps"].as_array().unwrap();
    assert_eq!(actual.len(), steps.len(), "{name} step count");
    for (index, (node, rust)) in steps.iter().zip(actual).enumerate() {
        if summary(rust) != *node {
            out.push(format!(
                "{name} step {index}: node {node}\nrust {}\nfull {rust}",
                summary(rust)
            ));
        }
    }
    out
}

/// A refused request leaves every draft, Part, reconciliation, decision and
/// assignment row as the previous step left it. Returns the refused count.
fn assert_refusals_roll_back(name: &Value, observed: &Value) -> usize {
    let mut refused = 0;
    let mut previous = &observed["initial"];
    for (index, step) in observed["steps"].as_array().unwrap().iter().enumerate() {
        if step["result"].get("error").is_some() {
            refused += 1;
            assert_eq!(
                step["state"], *previous,
                "{name} step {index} left rows behind"
            );
        }
        previous = &step["state"];
    }
    refused
}

/// In every ready selection, a reused token belongs to the target Part's
/// predecessor and appears once, and a Part whose inclusion, quantity and
/// decision did not change since the last ready selection keeps the same
/// units. Returns the reused-unit and unchanged-Part counts.
fn assert_surviving_units(name: &Value, observed: &Value) -> (usize, usize) {
    let (mut reused, mut stable) = (0, 0);
    let history = &observed["history"];
    // Every token belongs to the one tenant Profile and one revision unit.
    assert!(rows(history, "units").iter().all(|u| u["profile_id"] == 1));
    let owner: BTreeMap<&str, &Value> = rows(history, "mapping")
        .iter()
        .map(|m| {
            (
                m["required_unit_token"].as_str().unwrap(),
                &m["revision_part_id"],
            )
        })
        .collect();
    let mut last: BTreeMap<i64, (Value, Value, Value, Vec<Value>)> = BTreeMap::new();
    for (index, step) in observed["steps"].as_array().unwrap().iter().enumerate() {
        let state = &step["state"];
        let draft = rows(state, "drafts").iter().find(|d| d["id"] == 2).unwrap();
        let current = &draft["current_required_unit_reconciliation_id"];
        let ready = rows(state, "reconciliations")
            .iter()
            .any(|r| r["reconciliation_id"] == *current && r["result_kind"] == "ready");
        if !ready {
            continue;
        }
        let mut tokens = BTreeSet::new();
        for part in rows(state, "parts").iter().filter(|p| p["draft_id"] == 2) {
            let part_id = part["id"].as_i64().unwrap();
            let decision = rows(state, "decisions")
                .iter()
                .find(|d| {
                    d["reconciliation_id"] == *current && d["target_draft_part_id"] == part["id"]
                })
                .map(|d| json!([d["kind"], d["predecessor_revision_part_id"]]))
                .unwrap_or(Value::Null);
            let predecessor = match decision[0].as_str() {
                Some("accept_prior_completion") => &decision[1],
                _ => &part["base_revision_part_id"],
            };
            let units: Vec<Value> = rows(state, "assignments")
                .iter()
                .filter(|a| {
                    a["reconciliation_id"] == *current && a["target_draft_part_id"] == part["id"]
                })
                .map(|a| json!([a["unit_index"], a["kind"], a["required_unit_token"]]))
                .collect();
            for unit in units.iter().filter(|u| u[1] == "reuse") {
                reused += 1;
                let token = unit[2].as_str().unwrap();
                assert!(
                    tokens.insert(token.to_owned()),
                    "{name} step {index} reuses {token} twice"
                );
                assert_eq!(
                    owner.get(token).copied(),
                    Some(predecessor),
                    "{name} step {index} Part {part_id} took {token} from another predecessor"
                );
            }
            let now = (
                part["included"].clone(),
                part["quantity_effective"].clone(),
                decision,
                units,
            );
            if let Some(before) = last.get(&part_id)
                && (&before.0, &before.1, &before.2) == (&now.0, &now.1, &now.2)
            {
                stable += 1;
                assert_eq!(
                    before.3, now.3,
                    "{name} step {index} Part {part_id} lost its units"
                );
            }
            last.insert(part_id, now);
        }
    }
    (reused, stable)
}

/// One replay of every history carries the parity diff and the history
/// properties, so the corpus replays once.
#[test]
fn ticket_t_61_required_units_histories_match_node_and_keep_surviving_units() {
    let histories = CORPUS["histories"].as_array().unwrap();
    let (mut diffs, mut steps, mut refused, mut reused, mut stable) = (Vec::new(), 0, 0, 0, 0);
    for (expected, observed) in histories.iter().zip(replay_all(histories)) {
        diffs.extend(differences(expected, &observed));
        steps += expected["steps"].as_array().unwrap().len();
        refused += assert_refusals_roll_back(&expected["name"], &observed);
        let (r, s) = assert_surviving_units(&expected["name"], &observed);
        (reused, stable) = (reused + r, stable + s);
    }
    eprintln!(
        "{} histories, {steps} steps, {} differences; {refused} refused requests rolled back; \
         {reused} reused units kept their predecessor; {stable} unchanged Parts kept their units",
        histories.len(),
        diffs.len()
    );
    assert!(diffs.is_empty(), "{}", diffs.join("\n\n"));
    assert!(refused > 0 && reused > 0 && stable > 0);
}

#[test]
fn ticket_t_61_required_units_corpus_covers_every_history_operation() {
    let mut seen = BTreeMap::<String, usize>::new();
    for expected in CORPUS["histories"].as_array().unwrap() {
        if !expected["setup"].as_array().unwrap().is_empty() {
            *seen.entry("completed setup".into()).or_default() += 1;
        }
        for step in expected["steps"].as_array().unwrap() {
            let decisions = step["request"]["decisions"].as_array().unwrap();
            let mut label = |l: String| *seen.entry(l).or_default() += 1;
            label(match step.get("error") {
                Some(error) => format!("error {}", error.as_str().unwrap()),
                None => format!("{} {}", step["op"].as_str().unwrap(), step["kind"]),
            });
            for decision in decisions {
                label(format!("decision {}", decision["kind"].as_str().unwrap()));
            }
            if step["op"] == "reconcile" && decisions.len() > 1 {
                label("multi-decision reconcile".into());
            }
            if step["selection"]
                .as_array()
                .is_some_and(|s| s.iter().any(|a| a[2] == "create"))
            {
                label("selection creates units".into());
            }
        }
    }
    for required in [
        "completed setup",
        "decision set_included",
        "decision set_quantity_override",
        "decision accept_prior_completion",
        "decision replace",
        "decision select_exact_predecessor",
        "multi-decision reconcile",
        "selection creates units",
        "edit \"ready\"",
        "reconcile \"ready\"",
        "reconcile \"draft_changed\"",
        "reconcile \"idempotency_conflict\"",
        "error Accepted prior completion predecessor is not the known predecessor",
        "error Exact predecessor selection is not applicable to an unsafe predecessor",
        "error Required-unit reconciliation decision is not applicable",
        "error Required-unit reconciliation decision target is missing",
    ] {
        assert!(
            seen.get(required).copied().unwrap_or(0) > 0,
            "{required} in {seen:?}"
        );
    }
    eprintln!("{seen:#?}");
}

/// A second account gets its own tenant and cannot reach, replay or change
/// the first tenant's draft, reconciliations or Required units.
#[test]
fn ticket_t_61_required_units_second_tenant_cannot_touch_the_first() {
    let f = Fixture::new("rust/crates/pp-storage/tests/fixtures/required-units/unsafe.db");
    let owner = f.open();
    let auth = owner.auth(auth::FirstUserTenant::NewUser);
    let auth::Outcome::Session { token, .. } = auth
        .submit(
            auth::Request::Register {
                email: "second@example.test".into(),
                display_name: "Second".into(),
                password: Secret::new("second-tenant-password".into()),
            },
            Arc::new(AtomicBool::new(false)),
            WAIT,
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    else {
        panic!("second account was not registered")
    };
    let digest = rows(&f.read("state"), "drafts")[1]["snapshot_digest"].clone();
    let request = json!({"expected_snapshot_digest": digest, "decisions": [
        {"kind":"accept_prior_completion","target_draft_part_id":4,"predecessor_revision_part_id":1}
    ]});
    let first = reconcile(&owner, &f, "shared-key", &request).unwrap();
    assert_eq!(first["kind"], "ready");
    let before = (f.read("state"), f.read("history"));
    let second = || Credential::Session(Secret::new(token.expose().to_owned()));
    let client = owner.required_units_with_policy(policy()).unwrap();
    let command = ReconciliationCommand::new(
        id(1),
        id(2),
        serde_json::from_value(request.clone()).unwrap(),
        "shared-key",
        second(),
    )
    .unwrap();
    let outcome = client
        .reconcile(command, Arc::new(AtomicBool::new(false)), WAIT)
        .unwrap();
    assert_eq!(
        serde_json::to_value(outcome).unwrap(),
        json!({"kind":"profile_not_found"})
    );
    let edit = owner
        .working_drafts_with_policy(policy())
        .unwrap()
        .edit(
            second(),
            id(1),
            id(2),
            serde_json::from_value(json!({"expected_snapshot_digest": digest, "decisions": [
                {"kind":"set_included","draft_part_ids":[4],"value":false}
            ]}))
            .unwrap(),
            Arc::new(AtomicBool::new(false)),
            WAIT,
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(edit).unwrap(),
        json!({"kind":"profile_not_found"})
    );
    assert_eq!((f.read("state"), f.read("history")), before);
    assert_eq!(
        reconcile(&owner, &f, "shared-key", &request).unwrap(),
        first
    );
    owner.shutdown().unwrap();
}
