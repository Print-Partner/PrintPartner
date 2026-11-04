#![cfg(feature = "desktop_rebuild")]

use pp_storage::{
    Limits, WriterOwner,
    auth::Secret,
    catalog::{CatalogFailure, Credentials, Deletion, Outcome, Request, SourceCatalogClient},
};
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{os::unix::fs::DirBuilderExt, path::PathBuf};

#[path = "common/process.rs"]
mod child_process;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Transcript {
    generated: String,
    seed_sql: Vec<String>,
    initial: Value,
    steps: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    name: String,
    actor: Actor,
    request: Value,
    reopen: bool,
    outcome: Value,
    snapshot: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Actor {
    Owner,
    Foreign,
}

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "pp-catalog-metadata-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .unwrap();
        for id in [7, 8, 9] {
            std::fs::create_dir_all(directory.join("local").join(id.to_string())).unwrap();
        }
        Self(directory)
    }

    fn open(&self) -> WriterOwner {
        WriterOwner::open(
            &self.0,
            Limits {
                queued_writes: 2,
                readers: 1,
            },
        )
        .unwrap()
        .0
    }

    fn seed(&self, statements: &[String]) {
        let db = Connection::open(self.0.join("print-partner.db")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        for sql in statements {
            db.execute_batch(
                &sql.replace("@DATA_DIR@", &self.0.to_str().unwrap().replace('\'', "''")),
            )
            .unwrap();
        }
    }

    fn normalize(&self, value: &mut Value) {
        match value {
            Value::Array(items) => {
                for item in items {
                    self.normalize(item);
                }
            }
            Value::Object(fields) => {
                if let Some(path) = fields.get_mut("local_path")
                    && !path.is_null()
                {
                    let actual = PathBuf::from(path.as_str().unwrap());
                    let relative = actual.strip_prefix(&self.0).unwrap();
                    assert!(actual.is_dir(), "local path must exist: {actual:?}");
                    assert!(
                        ["local/7", "local/8", "local/9"].contains(&relative.to_str().unwrap())
                    );
                    *path = json!(format!("@DATA_DIR@/{}", relative.display()));
                }
                for item in fields.values_mut() {
                    self.normalize(item);
                }
            }
            _ => {}
        }
    }

    fn snapshot(&self) -> Value {
        let db = Connection::open_with_flags(
            self.0.join("print-partner.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let mut snapshot = json!({
            "projects": rows(&db, "SELECT * FROM projects ORDER BY id"),
            "settings": rows(&db, "SELECT * FROM app_settings ORDER BY tenant_id,key"),
            "activity": rows(&db, "SELECT * FROM app_events ORDER BY id"),
        });
        for row in snapshot["projects"].as_array_mut().unwrap() {
            row["metadata_json"] =
                serde_json::from_str(row["metadata_json"].as_str().unwrap()).unwrap();
        }
        for row in snapshot["settings"].as_array_mut().unwrap() {
            if row["key"] == "source_categories" || row["key"] == "stl_naming_defaults" {
                row["value"] = serde_json::from_str(row["value"].as_str().unwrap()).unwrap();
            }
        }
        for row in snapshot["activity"].as_array_mut().unwrap() {
            row["payload_json"] =
                serde_json::from_str(row["payload_json"].as_str().unwrap()).unwrap();
        }
        self.normalize(&mut snapshot);
        snapshot
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rows(db: &Connection, sql: &str) -> Vec<Value> {
    let mut statement = db.prepare(sql).unwrap();
    let names: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    statement
        .query_map([], |row| {
            let mut fields = Map::new();
            for (i, name) in names.iter().enumerate() {
                let value = match row.get_ref(i)? {
                    ValueRef::Null => Value::Null,
                    ValueRef::Integer(value) => json!(value),
                    ValueRef::Real(value) => json!(value),
                    ValueRef::Text(value) => json!(std::str::from_utf8(value).unwrap()),
                    ValueRef::Blob(_) => panic!("unexpected blob in catalog fixture"),
                };
                fields.insert(name.clone(), value);
            }
            Ok(Value::Object(fields))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn outcome(client: &SourceCatalogClient, request: Request) -> Value {
    enum Transport {
        Json,
        Naming,
        GlobalNaming,
        Delete(i64),
        Activity,
    }
    let transport = match &request {
        Request::GetNaming { .. } | Request::SaveNaming { .. } => Transport::Naming,
        Request::GetGlobalNaming {} | Request::SaveGlobalNaming { .. } => Transport::GlobalNaming,
        Request::Delete { id } => Transport::Delete(*id),
        Request::SourceActivity { .. } => Transport::Activity,
        Request::Get { .. }
        | Request::Update { .. }
        | Request::GetCategoriesWithTree { .. }
        | Request::SaveCategoriesWithTree { .. }
        | Request::ApplyNumericCategory { .. } => Transport::Json,
        _ => panic!("unsupported fixture command"),
    };
    match (transport, client.execute(request)) {
        (Transport::Delete(id), Ok(Outcome::Deletion(Deletion::Deleted { source_id }))) => {
            assert_eq!(source_id, id);
            json!({"status": 204, "body": null})
        }
        (Transport::Delete(_), result) => panic!("unexpected deletion outcome: {result:?}"),
        (Transport::Naming, Err(error))
            if matches!(
                error.downcast_ref::<CatalogFailure>(),
                Some(CatalogFailure::NotFound)
            ) =>
        {
            json!({"status": 404, "body": {"code": "source_not_found", "detail": error.to_string()}})
        }
        (Transport::Activity, Ok(Outcome::Activity(events))) => {
            json!({"status": 200, "body": {"events": events}})
        }
        (Transport::Activity, result) => panic!("unexpected activity outcome: {result:?}"),
        (Transport::GlobalNaming, Ok(Outcome::Data(profile))) => {
            json!({"status": 200, "body": {"profile": profile}})
        }
        (Transport::GlobalNaming, result) => panic!("unexpected global naming outcome: {result:?}"),
        (_, Ok(result)) => json!({"status": 200, "body": serde_json::to_value(result).unwrap()}),
        (_, Err(error))
            if matches!(
                error.downcast_ref::<CatalogFailure>(),
                Some(CatalogFailure::Conflict)
            ) =>
        {
            json!({"status": 409, "body": {"code": "catalog_metadata_conflict", "detail": error.to_string()}})
        }
        (_, Err(error)) => panic!("unmapped catalog error: {error:?}"),
    }
}

#[test]
fn node_catalog_metadata_mutations_match_public_client_and_reopen() {
    let transcript: Transcript = serde_json::from_str(include_str!(
        "../../../../fixtures/parity/catalog-metadata-mutations/node.json"
    ))
    .unwrap();
    assert_eq!(
        transcript.generated,
        "fixtures/parity/catalog-metadata-mutations/generate.mjs"
    );
    assert_eq!(transcript.steps.len(), 33);
    let fixture = Fixture::new();
    let mut owner = fixture.open();
    fixture.seed(&transcript.seed_sql);
    assert_eq!(
        fixture.snapshot(),
        transcript.initial,
        "initial persistent rows"
    );
    for step in transcript.steps {
        if step.reopen {
            owner.shutdown().unwrap();
            owner = fixture.open();
        }
        let client = match step.actor {
            Actor::Owner => owner.local_source_catalog(),
            Actor::Foreign => owner.source_catalog(Credentials::Session(Secret::new(
                "catalog-metadata-foreign-session".into(),
            ))),
        };
        let request: Request = serde_json::from_value(step.request.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            step.request,
            "{}: public request round trip",
            step.name
        );
        let mut actual = outcome(&client, request);
        fixture.normalize(&mut actual);
        assert_eq!(
            actual, step.outcome,
            "{}: complete command outcome",
            step.name
        );
        assert_eq!(
            fixture.snapshot(),
            step.snapshot,
            "{}: complete persistent rows",
            step.name
        );
    }
    owner.shutdown().unwrap();
}

fn rebuild_runtime_enabled() -> bool {
    matches!(
        std::env::var("PP_DESKTOP_REBUILD").ok().as_deref(),
        Some("1" | "true")
    )
}

fn seeded_owner() -> (Fixture, WriterOwner) {
    let transcript: Transcript = serde_json::from_str(include_str!(
        "../../../../fixtures/parity/catalog-metadata-mutations/node.json"
    ))
    .unwrap();
    let fixture = Fixture::new();
    let owner = fixture.open();
    fixture.seed(&transcript.seed_sql);
    (fixture, owner)
}

#[test]
fn catalog_metadata_token_is_ignored_when_runtime_flag_is_off() {
    assert!(
        !rebuild_runtime_enabled(),
        "PP_DESKTOP_REBUILD must be off for the tokenless catalog build"
    );
    let (fixture, owner) = seeded_owner();
    let client = owner.local_source_catalog();
    let before = fixture.snapshot();
    let request = Request::Update {
        id: 7,
        patch: pp_storage::catalog::SourcePatch {
            metadata: Some(
                serde_json::from_value(json!({"custom_name": "Flag off rename"})).unwrap(),
            ),
            metadata_version: Some(4),
            ..Default::default()
        },
    };
    let updated = client.execute(request).unwrap();
    let body = serde_json::to_value(updated).unwrap();
    assert_eq!(body["metadata"]["custom_name"], "Flag off rename");
    assert!(body.get("catalog_metadata_version").is_none());
    let after = fixture.snapshot();
    assert_ne!(after, before);
    assert!(after["projects"].as_array().unwrap().iter().all(|row| {
        row["metadata_json"]
            .get("catalog_metadata_version")
            .is_none()
    }));
    owner.shutdown().unwrap();
}

#[test]
fn catalog_metadata_stale_snapshot_is_refused() {
    if !rebuild_runtime_enabled() {
        let output = child_process::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "catalog_metadata_stale_snapshot_is_refused",
                "--test-threads=1",
            ])
            .env("PP_DESKTOP_REBUILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stale token child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (fixture, owner) = seeded_owner();
    let client = owner.local_source_catalog();
    let before = fixture.snapshot();
    let refused = client.execute(Request::Update {
        id: 7,
        patch: pp_storage::catalog::SourcePatch {
            metadata: Some(
                serde_json::from_value(json!({"custom_name": "Should not land"})).unwrap(),
            ),
            metadata_version: Some(4),
            ..Default::default()
        },
    });
    let error = refused.expect_err("stale catalog metadata token");
    assert!(
        matches!(
            error.downcast_ref::<CatalogFailure>(),
            Some(CatalogFailure::Conflict)
        ),
        "{error:?}"
    );
    assert_eq!(
        error.to_string(),
        "Catalog metadata changed; retry from the current version"
    );
    assert_eq!(fixture.snapshot(), before, "stale token must not write");
    owner.shutdown().unwrap();
}

#[test]
fn catalog_metadata_stale_fixture_matches_node() {
    if !rebuild_runtime_enabled() {
        let output = child_process::command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "catalog_metadata_stale_fixture_matches_node",
                "--test-threads=1",
            ])
            .env("PP_DESKTOP_REBUILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stale fixture child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let transcript: Transcript = serde_json::from_str(include_str!(
        "../../../../fixtures/parity/catalog-metadata-mutations/stale.json"
    ))
    .unwrap();
    assert_eq!(
        transcript.generated,
        "fixtures/parity/catalog-metadata-mutations/generate.mjs"
    );
    assert_eq!(transcript.steps.len(), 6);
    let fixture = Fixture::new();
    let mut owner = fixture.open();
    fixture.seed(&transcript.seed_sql);
    assert_eq!(
        fixture.snapshot(),
        transcript.initial,
        "initial persistent rows"
    );
    for step in transcript.steps {
        if step.reopen {
            owner.shutdown().unwrap();
            owner = fixture.open();
        }
        let client = owner.local_source_catalog();
        assert!(matches!(step.actor, Actor::Owner));
        let request: Request = serde_json::from_value(step.request.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            step.request,
            "{}: public request round trip",
            step.name
        );
        let mut actual = outcome(&client, request);
        fixture.normalize(&mut actual);
        assert_eq!(
            actual, step.outcome,
            "{}: complete command outcome",
            step.name
        );
        assert_eq!(
            fixture.snapshot(),
            step.snapshot,
            "{}: complete persistent rows",
            step.name
        );
    }
    owner.shutdown().unwrap();
}
