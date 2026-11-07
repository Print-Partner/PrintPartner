#![cfg(feature = "desktop_rebuild")]
//! GRE-421 part 2: Node's PATCH /plans/{id} and POST /plans/{id}/archive
//! golden (fixtures/parity/build-header) replayed through build_router on
//! disposable copies of accepted-plate-node.db: every status, body and changed
//! table matches Node except the declared corrections.
use axum::Router;
use loopback::ORIGIN;
use pp_api::builds::{BuildHttpConfig, build_router};
use pp_storage::{Limits, WriterOwner};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[path = "support/loopback.rs"]
mod loopback;
#[path = "support/node_view.rs"]
mod node_view;

#[derive(Deserialize)]
struct Golden {
    generated: String,
    phases: Vec<Phase>,
}

#[derive(Deserialize)]
struct Phase {
    name: String,
    seed: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    method: String,
    url: String,
    body: Option<Value>,
    payload: Option<String>,
    node: Exchange,
    changed: Vec<String>,
}

#[derive(Deserialize)]
struct Exchange {
    status: u16,
    body: Value,
}

const FIXTURE_TIMES: [&str; 2] = ["2026-10-02T21:06:51.794Z", "2026-10-03T00:00:00.000Z"];

/// The golden writes a time set during the request as "<now>".
fn stamp(value: Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(stamp).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, entry)| {
                    let now = (key == "archived_at" || key == "last_used_at")
                        && entry
                            .as_str()
                            .is_some_and(|time| !FIXTURE_TIMES.contains(&time));
                    let entry = if now { json!("<now>") } else { stamp(entry) };
                    (key, entry)
                })
                .collect(),
        ),
        other => other,
    }
}

/// Every row of every table, by table name.
fn tables(root: &Path) -> Vec<(String, String)> {
    let connection = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
    let names: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM \"{name}\""))
                .unwrap();
            let columns = statement.column_count();
            let rows: Vec<Vec<rusqlite::types::Value>> = statement
                .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            (name, format!("{rows:?}"))
        })
        .collect()
}

fn open(root: &Path, seed: &str) -> (WriterOwner, Router) {
    std::fs::create_dir_all(root.join("repos/1/revisions/accepted")).unwrap();
    std::fs::write(
        root.join("print-partner.db"),
        include_bytes!("../../pp-storage/tests/fixtures/accepted-plate-node.db"),
    )
    .unwrap();
    {
        let connection = rusqlite::Connection::open(root.join("print-partner.db")).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .unwrap();
        let triggers: Vec<String> = connection
            .prepare("SELECT name FROM sqlite_schema WHERE type='trigger'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for trigger in triggers {
            connection
                .execute_batch(&format!("DROP TRIGGER \"{trigger}\""))
                .unwrap();
        }
        connection.execute_batch(seed).unwrap();
    }
    let owner = WriterOwner::open(root, Limits::default()).unwrap().0;
    let policy = pp_storage::auth::AuthPolicy {
        registration: pp_storage::auth::RegistrationPolicy::Open,
        session_tenant: pp_storage::auth::SessionTenantPolicy::AccountTenant,
        first_user: pp_storage::auth::FirstUserTenant::NewUser,
    };
    let config = BuildHttpConfig::from_startup(ORIGIN, "default", Some("1")).unwrap();
    let router = build_router(config, owner.build_graph_with_policy(policy).unwrap());
    (owner, router)
}

/// Where Rust deliberately answers differently, and why. Each writes nothing.
fn correction(id: &str) -> Option<(u16, Value)> {
    let taken = json!({"code": "duplicate_build_name", "name": "Taken"});
    match id {
        // The Build rename rule Accept shares (GRE-1207): 409, not Node's 400
        // "Profile already exists" or, beside an archive, its 500.
        "rename_taken" | "rename_taken_with_archive" => Some((409, taken)),
        // Node answers 500 when the rename fails beside an archive.
        "blank_name_with_archive" => Some((400, json!({"detail": "Profile name is required"}))),
        // Rust does not expose Fastify's parser codes or messages, and Node
        // reads `body.archived` from a `null` body and answers 500.
        "not_json" | "archive_not_json" | "null_body" => {
            Some((400, json!({"detail": "Request is invalid"})))
        }
        _ => None,
    }
}

#[tokio::test]
async fn build_header_patch_and_archive_match_node() {
    let golden: Golden = serde_json::from_str(include_str!(
        "../../../../fixtures/parity/build-header/node.json"
    ))
    .unwrap();
    assert!(golden.generated.contains("build-header/generate.mjs"));
    let (mut matched, mut corrected) = (0, 0);
    for phase in &golden.phases {
        let dir = tempfile::tempdir().unwrap();
        let (owner, router) = open(dir.path(), &phase.seed);
        for case in &phase.cases {
            let before = tables(dir.path());
            let body = match (&case.payload, &case.body) {
                (Some(payload), _) => payload.clone(),
                (None, Some(body)) => body.to_string(),
                (None, None) => String::new(),
            };
            let (status, actual) = loopback::send(
                &router,
                &case.method,
                &case.url,
                &body,
                Some("pp_session=read-fixture-secret"),
            )
            .await;
            let after = tables(dir.path());
            let changed: Vec<String> = before
                .iter()
                .zip(&after)
                .filter(|(old, new)| old != new)
                .map(|(old, _)| old.0.clone())
                .collect();
            let actual = (status.as_u16(), stamp(node_view::node_view(actual)));
            let label = format!("{}/{}", phase.name, case.id);
            match correction(&case.id) {
                Some(expected) => {
                    assert_eq!(actual, expected, "{label}: corrected answer");
                    assert!(
                        changed.is_empty(),
                        "{label}: a correction wrote {changed:?}"
                    );
                    corrected += 1;
                }
                None => {
                    assert_eq!(
                        actual,
                        (case.node.status, case.node.body.clone()),
                        "{label}"
                    );
                    assert_eq!(changed, case.changed, "{label}: changed tables");
                    matched += 1;
                }
            }
        }
        drop(router);
        owner.shutdown().unwrap();
    }
    assert_eq!((matched, corrected), (47, 6));
}
