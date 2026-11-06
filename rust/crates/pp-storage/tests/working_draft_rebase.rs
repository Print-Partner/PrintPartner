#![cfg(feature = "desktop_rebuild")]
//! Replays `fixtures/working-draft-rebase-node.mjs`. Regenerate it with
//! `node --import ./web/node_modules/tsx/dist/loader.mjs --conditions=development
//! rust/crates/pp-storage/tests/fixtures/working-draft-rebase-node.mjs web/apps/server`.
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    read_model::Credential,
    working_drafts::{
        Failure, PositiveId, RebaseArgs, RebaseRequest, RebaseWorkspaceOutcome,
        observation::{
            ArtifactRead, CheckoutManifestUse, DocumentRead, DraftReads, InventoryPath,
            OwnedCatalogReadRequest, OwnedCheckoutReadRequest, OwnedStlReadRequest,
            PreparationBudget, PreparationLimits, ReadFailure, ReadResult, Resource, StlInventory,
            StlRootRead,
        },
    },
};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

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
    include_str!("fixtures/working-draft-rebase-node.jsonl")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|case| case["name"] == name)
        .unwrap()
}

/// Serves `files` from every STL root. The first scan runs `between`, as Node
/// runs it after preparation, and can cancel.
struct Reads {
    files: Vec<String>,
    database: PathBuf,
    between: Mutex<Option<String>>,
    cancel: Option<Arc<AtomicBool>>,
    delay: Duration,
}
struct Root(Arc<Reads>);
struct Inventory(Vec<InventoryPath>);
struct Shared(Arc<Reads>);
impl DraftReads for Shared {
    fn resolve_stl_root(
        &self,
        _: &OwnedStlReadRequest,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlRootRead>> {
        budget.check()?;
        Ok(Box::new(Root(self.0.clone())))
    }
    fn read_checkout_manifest(
        &self,
        _: &OwnedCheckoutReadRequest,
        _: CheckoutManifestUse,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        budget.check()?;
        Ok(DocumentRead::Missing)
    }
    fn read_catalog_document(
        &self,
        _: &OwnedCatalogReadRequest,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        budget.check()?;
        Ok(DocumentRead::Missing)
    }
}
impl StlRootRead for Root {
    fn logical_resolved_path(&self) -> Option<&str> {
        Some("/logical/root")
    }
    fn scan_stls(
        &mut self,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlInventory>> {
        budget.check()?;
        if let Some(sql) = self.0.between.lock().unwrap().take() {
            Connection::open(&self.0.database)
                .unwrap()
                .execute_batch(&sql)
                .unwrap();
        }
        if let Some(cancel) = &self.0.cancel {
            cancel.store(true, Ordering::Release);
        }
        std::thread::sleep(self.0.delay);
        let entries = self
            .0
            .files
            .iter()
            .enumerate()
            .map(|(traversal_ordinal, path)| {
                budget.entry(0, path.len())?;
                Ok(InventoryPath {
                    physical_relative_path: path.clone(),
                    traversal_ordinal,
                })
            })
            .collect::<ReadResult<_>>()?;
        Ok(Box::new(Inventory(entries)))
    }
}
impl StlInventory for Inventory {
    fn entries(&self) -> &[InventoryPath] {
        &self.0
    }
    fn hash_tracked_winner(
        &mut self,
        index: usize,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<ArtifactRead> {
        let bytes = self.0[index].physical_relative_path.as_bytes();
        budget.artifact(bytes.len() as u64, bytes.len())?;
        Ok(ArtifactRead {
            byte_count: bytes.len() as u64,
            byte_sha256: hex::encode(Sha256::digest(bytes)),
        })
    }
}

struct Fixture {
    path: PathBuf,
}
impl Fixture {
    fn new(case: &Value) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pp-draft-rebase-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let revision = path.join("repos/1/revisions/fixture");
        std::fs::create_dir_all(&revision).unwrap();
        std::fs::write(
            path.join("print-partner.db"),
            include_bytes!("fixtures/required-units/unchanged.db"),
        )
        .unwrap();
        for file in case["files"].as_array().unwrap() {
            let file = file.as_str().unwrap();
            std::fs::write(revision.join(file), file).unwrap();
        }
        let fixture = Self { path };
        for sql in case["setup"].as_array().unwrap() {
            fixture.sql().execute_batch(sql.as_str().unwrap()).unwrap();
        }
        fixture
    }
    fn database(&self) -> PathBuf {
        self.path.join("print-partner.db")
    }
    fn sql(&self) -> Connection {
        Connection::open(self.database()).unwrap()
    }
    fn reads(
        &self,
        files: &Value,
        between: &Value,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Arc<Shared> {
        Arc::new(Shared(Arc::new(Reads {
            files: serde_json::from_value(files.clone()).unwrap(),
            database: self.database(),
            between: Mutex::new(between.as_str().map(str::to_owned)),
            cancel,
            delay: Duration::ZERO,
        })))
    }
    fn rows(&self, sql: &str) -> Value {
        let connection = self.sql();
        let mut statement = connection.prepare(sql).unwrap();
        let columns: Vec<_> = statement
            .column_names()
            .into_iter()
            .map(str::to_owned)
            .collect();
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
            "drafts": self.rows("SELECT id,tenant_id,profile_id,base_revision_id,base_plan_version,state,lifecycle_version,rebased_from_draft_id,rebased_from_lifecycle_version,rebased_from_snapshot_digest,digest_format,snapshot_digest,created_by,idempotency_key,created_at GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z' AS created_at_iso,current_required_unit_reconciliation_id,consumed_revision_id,consumed_at FROM plan_drafts ORDER BY id"),
            "inputs": self.rows("SELECT * FROM plan_draft_inputs ORDER BY id"),
            "parts": self.rows("SELECT * FROM plan_draft_parts ORDER BY id"),
        })
    }
    fn open(&self) -> WriterOwner {
        WriterOwner::open(&self.path, Limits::default()).unwrap().0
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn rebase(
    owner: &WriterOwner,
    reads: Arc<Shared>,
    call: &Value,
    limits: PreparationLimits,
    cancelled: Arc<AtomicBool>,
) -> anyhow::Result<Value> {
    let client = owner.working_drafts_with_observations(policy(), reads, limits)?;
    let request: RebaseRequest = serde_json::from_value(call["request"].clone())?;
    let outcome = client.rebase(
        session(),
        id(call["profile"].as_u64().unwrap()),
        RebaseArgs {
            draft_id: id(call["draft"].as_u64().unwrap()),
            idempotency_key: call["key"].as_str().unwrap(),
            request,
        },
        cancelled,
        Duration::from_secs(5),
    )?;
    let mut outcome: Value = serde_json::from_slice(&outcome.into_json_body()?.into_bytes())?;
    if let Some(created_at) = outcome["draft"]["createdAt"].as_str() {
        let iso = created_at.len() == 24
            && created_at
                .bytes()
                .zip(b"0000-00-00T00:00:00.000Z")
                .all(|(c, p)| {
                    if *p == b'0' {
                        c.is_ascii_digit()
                    } else {
                        c == *p
                    }
                });
        outcome["draft"]["createdAt"] = json!(iso);
    }
    Ok(outcome)
}

fn compare_node(name: &str) {
    let case = node(name);
    let f = Fixture::new(&case);
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
        let actual = match call["op"].as_str().unwrap() {
            "edit" => owner
                .working_drafts_with_policy(policy())
                .unwrap()
                .edit(
                    session(),
                    id(call["profile"].as_u64().unwrap()),
                    id(call["draft"].as_u64().unwrap()),
                    serde_json::from_value(call["request"].clone()).unwrap(),
                    Arc::new(AtomicBool::new(false)),
                    Duration::from_secs(5),
                )
                .map(|outcome| serde_json::to_value(outcome).unwrap()),
            "rebase" => rebase(
                &owner,
                f.reads(&case["files"], &call["between"], None),
                call,
                PreparationLimits::default(),
                Arc::new(AtomicBool::new(false)),
            ),
            other => panic!("Unknown Node call {other}"),
        };
        match expected["error"].as_str() {
            Some(message) => assert_eq!(
                actual.unwrap_err().to_string(),
                message,
                "{name} call {index}"
            ),
            None => assert_eq!(&actual.unwrap(), expected, "{name} call {index}"),
        }
        owner.shutdown().unwrap();
    }
    let rows = f.recorded_rows();
    for table in ["drafts", "inputs", "parts"] {
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
    an_unchanged_base_and_inputs_is_base_unchanged,
    "base-unchanged"
);
node_case!(
    the_source_state_version_digest_and_ids_are_checked,
    "source-checks"
);
node_case!(out_of_range_lifecycle_versions_are_errors, "version-bounds");
node_case!(a_profile_without_a_baseline_is_refused, "baseline-required");
node_case!(
    open_draft_edits_carry_and_replay_by_key_and_generation,
    "inputs-moved-open-edits"
);
node_case!(
    a_decision_the_new_base_also_changed_conflicts,
    "revised-base"
);
node_case!(decisions_merge_onto_a_revised_base, "revised-base-merges");
node_case!(
    a_new_part_the_revised_base_excludes_conflicts,
    "revised-base-new-part"
);
node_case!(
    a_draft_without_decisions_takes_the_revised_base,
    "revised-base-agrees"
);
node_case!(
    an_abandoned_draft_rebases_onto_a_moved_base,
    "base-moved-abandoned"
);
node_case!(
    a_recompute_key_is_an_idempotency_conflict,
    "key-of-a-recompute"
);
node_case!(
    an_abandoned_draft_rebases_onto_moved_inputs,
    "inputs-moved-abandoned"
);
node_case!(
    a_base_moved_after_preparation_is_base_changed,
    "base-moved-after-prepare"
);
node_case!(
    inputs_moved_after_preparation_are_refused,
    "inputs-moved-after-prepare"
);
node_case!(
    a_source_abandoned_after_preparation_is_not_abandoned,
    "source-abandoned-after-prepare"
);
node_case!(a_layer_shared_by_two_sources_conflicts, "shared-layer");
node_case!(a_duplicated_part_key_is_ambiguous, "duplicate-key");
node_case!(
    a_renamed_part_matches_by_tracked_artifact,
    "artifact-rename"
);
node_case!(two_decisions_on_one_target_collide, "artifact-collision");
node_case!(a_profile_without_layers_is_no_layers, "no-layers");
node_case!(a_profile_without_stls_is_no_stls, "no-stls");

#[test]
fn cancellation_during_preparation_writes_nothing() {
    let case = node("base-moved-abandoned");
    let f = Fixture::new(&case);
    let before = f.recorded_rows();
    let owner = f.open();
    let cancelled = Arc::new(AtomicBool::new(false));
    let reads = f.reads(&case["files"], &Value::Null, Some(cancelled.clone()));
    let error = rebase(
        &owner,
        reads,
        &case["calls"][0],
        PreparationLimits::default(),
        cancelled,
    )
    .unwrap_err();
    owner.shutdown().unwrap();
    assert!(
        matches!(error.downcast_ref(), Some(ReadFailure::Cancelled)),
        "{error:?}"
    );
    assert_eq!(f.recorded_rows(), before);
}

/// The writer's five-second admission wait starts after preparation, so a
/// slow Source scan does not refuse an uncontended write.
#[test]
fn a_preparation_slower_than_the_busy_wait_still_writes() {
    let case = node("base-moved-abandoned");
    let f = Fixture::new(&case);
    let owner = f.open();
    let reads = Arc::new(Shared(Arc::new(Reads {
        files: serde_json::from_value(case["files"].clone()).unwrap(),
        database: f.database(),
        between: Mutex::new(None),
        cancel: None,
        delay: Duration::from_millis(5_500),
    })));
    let outcome = rebase(
        &owner,
        reads,
        &case["calls"][0],
        PreparationLimits::default(),
        Arc::new(AtomicBool::new(false)),
    );
    owner.shutdown().unwrap();
    assert_eq!(outcome.unwrap()["kind"], "rebased");
}

/// Loading the source draft spends the preparation budget, so an oversized
/// source is refused before anything is written.
#[test]
fn an_oversized_source_draft_exceeds_the_comparison_budget() {
    let case = node("base-moved-after-prepare");
    let f = Fixture::new(&case);
    f.sql()
        .execute(
            "UPDATE plan_draft_parts SET notes=? WHERE draft_id=2",
            ["n".repeat(60_000)],
        )
        .unwrap();
    let before = f.recorded_rows();
    let owner = f.open();
    let limits = PreparationLimits {
        comparison_work: 8_192,
        ..PreparationLimits::default()
    };
    let reads = f.reads(&case["files"], &Value::Null, None);
    let error = rebase(
        &owner,
        reads,
        &case["calls"][0],
        limits,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap_err();
    owner.shutdown().unwrap();
    assert!(
        matches!(
            error.downcast_ref(),
            Some(ReadFailure::LimitExceeded(Resource::ComparisonWork))
        ),
        "{error:?}"
    );
    assert_eq!(f.recorded_rows(), before);
}

/// Another Plan's draft is not found without charging the budget for it.
#[test]
fn another_plans_draft_is_not_found_before_it_is_charged() {
    let case = node("base-unchanged");
    let f = Fixture::new(&case);
    f.sql()
        .execute_batch(
            "INSERT INTO build_profiles(id,tenant_id,name) VALUES(3,'default','New Build')",
        )
        .unwrap();
    let owner = f.open();
    let limits = PreparationLimits {
        comparison_work: 1,
        ..PreparationLimits::default()
    };
    let reads = f.reads(&case["files"], &Value::Null, None);
    let mut call = case["calls"][0].clone();
    call["profile"] = json!(3);
    let outcome = rebase(
        &owner,
        reads,
        &call,
        limits,
        Arc::new(AtomicBool::new(false)),
    );
    owner.shutdown().unwrap();
    assert_eq!(outcome.unwrap(), json!({"kind": "not_found"}));
}

#[test]
fn a_cancelled_request_is_refused_before_reading() {
    let case = node("base-unchanged");
    let f = Fixture::new(&case);
    let owner = f.open();
    let reads = f.reads(&case["files"], &Value::Null, None);
    let error = rebase(
        &owner,
        reads,
        &case["calls"][0],
        PreparationLimits::default(),
        Arc::new(AtomicBool::new(true)),
    )
    .unwrap_err();
    owner.shutdown().unwrap();
    assert!(
        matches!(error.downcast_ref(), Some(Failure::Cancelled)),
        "{error:?}"
    );
}

#[test]
fn a_client_without_observations_cannot_rebase() {
    let case = node("base-unchanged");
    let f = Fixture::new(&case);
    let owner = f.open();
    let call = &case["calls"][0];
    let error = owner
        .working_drafts_with_policy(policy())
        .unwrap()
        .rebase(
            session(),
            id(1),
            RebaseArgs {
                draft_id: id(2),
                idempotency_key: "first",
                request: serde_json::from_value(call["request"].clone()).unwrap(),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap_err();
    owner.shutdown().unwrap();
    assert_eq!(
        error.to_string(),
        "Working-draft observations are not configured"
    );
}

#[test]
fn the_body_parses_as_node_parses_it() {
    let digest = "a".repeat(64);
    let parse = |body: Value| serde_json::from_value::<RebaseRequest>(body).is_ok();
    assert!(parse(
        json!({"expected_source_lifecycle_version": 0, "expected_source_snapshot_digest": digest})
    ));
    assert!(parse(
        json!({"expected_source_state": "open", "expected_source_lifecycle_version": 0, "expected_source_snapshot_digest": digest})
    ));
    for body in [
        json!({"expected_source_state": null, "expected_source_lifecycle_version": 0, "expected_source_snapshot_digest": digest}),
        json!({"expected_source_state": "consumed", "expected_source_lifecycle_version": 0, "expected_source_snapshot_digest": digest}),
        json!({"expected_source_lifecycle_version": -1, "expected_source_snapshot_digest": digest}),
        json!({"expected_source_lifecycle_version": 0, "expected_source_snapshot_digest": "A".repeat(64)}),
        json!({"expected_source_lifecycle_version": 0, "expected_source_snapshot_digest": digest, "extra": 1}),
    ] {
        assert!(!parse(body.clone()), "{body}");
    }
}

#[test]
fn ticket_t_61_publication_rebase_and_select_selects_the_rebased_draft_once_per_key() {
    let case = node("base-moved-abandoned");
    let f = Fixture::new(&case);
    let call = &case["calls"][0];
    let owner = f.open();
    let client = owner
        .working_drafts_with_observations(
            policy(),
            f.reads(&case["files"], &Value::Null, None),
            PreparationLimits::default(),
        )
        .unwrap();
    let run = || {
        let outcome = client
            .rebase_and_select(
                session(),
                id(1),
                RebaseArgs {
                    draft_id: id(2),
                    idempotency_key: "first",
                    request: serde_json::from_value(call["request"].clone()).unwrap(),
                },
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap();
        let RebaseWorkspaceOutcome::Workspace(outcome) = outcome else {
            panic!("rebase was refused: {outcome:?}");
        };
        serde_json::to_value(outcome).unwrap()
    };
    let selected = run();
    assert_eq!(selected["kind"], "ready", "{selected}");
    assert_eq!(selected["workspace"]["draft"]["draft_id"], 3);
    let events = || f.rows("SELECT event_type,payload_json FROM domain_events ORDER BY seq");
    let draft = |id: u8| json!({"event_type":"plan.draft_changed","payload_json":format!("{{\"draft_id\":{id}}}")});
    assert_eq!(events(), json!([draft(3), draft(3)]));
    let selection = f.rows("SELECT id,current_required_unit_reconciliation_id,snapshot_digest FROM plan_drafts WHERE id=3");
    assert!(!selection[0]["current_required_unit_reconciliation_id"].is_null());
    assert_eq!(
        selection[0]["snapshot_digest"],
        selected["workspace"]["draft"]["snapshot_digest"]
    );
    assert_eq!(run(), selected);
    assert_eq!(f.rows("SELECT id,current_required_unit_reconciliation_id,snapshot_digest FROM plan_drafts WHERE id=3"), selection);
    assert_eq!(events(), json!([draft(3), draft(3)]));
    owner.shutdown().unwrap();
}

/// GRE-1197: a rebased draft carries its source draft's header row.
#[test]
fn rebase_carries_the_source_draft_header() {
    let case = node("base-moved-abandoned");
    let f = Fixture::new(&case);
    let call = &case["calls"][0];
    let owner = f.open();
    f.sql().execute("INSERT INTO plan_draft_headers (draft_id,tenant_id,name,order_number,customer,due_date,header_version,updated_at) VALUES (2,'default','Renamed','PO-7','Acme','2026-12-01',3,'2026-11-06T00:00:00.000Z')", []).unwrap();
    let outcome = rebase(
        &owner,
        f.reads(&case["files"], &Value::Null, None),
        call,
        PreparationLimits::default(),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    assert_eq!(outcome["kind"], "rebased", "{outcome}");
    let headers = f.rows(
        "SELECT draft_id,tenant_id,name,order_number,customer,due_date,header_version,updated_at FROM plan_draft_headers ORDER BY draft_id",
    );
    let rows = headers.as_array().unwrap();
    assert_eq!(rows.len(), 2, "{headers}");
    assert_eq!(rows[1]["draft_id"], 3);
    let mut carried = rows[1].clone();
    carried["draft_id"] = json!(2);
    assert_eq!(carried, rows[0]);
    owner.shutdown().unwrap();
}
