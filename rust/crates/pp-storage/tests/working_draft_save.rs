#![cfg(feature = "desktop_rebuild")]
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    read_model::Credential,
    working_drafts::{
        PositiveId,
        observation::{
            ArtifactRead, CheckoutManifestUse, DocumentRead, DraftReads, InventoryPath,
            OwnedCatalogReadRequest, OwnedCheckoutReadRequest, OwnedStlReadRequest,
            PreparationBudget, PreparationLimits, ReadFailure, ReadResult, Resource, StlInventory,
            StlRootRead,
        },
        save::{CommittedSaveCaptureFailure, Outcome, SaveCommand},
    },
};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        Arc,
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
struct Reads {
    files: Vec<String>,
    cancel: Option<Arc<AtomicBool>>,
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
        if let Some(cancel) = &self.0.cancel {
            cancel.store(true, Ordering::Release);
        }
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
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "pp-draft-save-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let repo = path.join("repos/1/revisions/fixture");
        std::fs::create_dir_all(&repo).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(
            path.join("print-partner.db"),
            include_bytes!("fixtures/required-units/unchanged.db"),
        )
        .unwrap();
        for file in ["bracket.stl", "excluded.stl", "gear.stl", "new.stl"] {
            std::fs::write(repo.join(file), file).unwrap();
        }
        Self { path }
    }
    fn sql(&self) -> Connection {
        Connection::open(self.path.join("print-partner.db")).unwrap()
    }
    fn open(&self) -> WriterOwner {
        WriterOwner::open(&self.path, Limits::default()).unwrap().0
    }
    fn reads(&self) -> Arc<Shared> {
        Arc::new(Shared(Arc::new(Reads {
            files: vec![
                "bracket.stl".into(),
                "excluded.stl".into(),
                "gear.stl".into(),
                "new.stl".into(),
            ],
            cancel: None,
        })))
    }
    fn request(&self, source: bool) -> Value {
        let expected = if source {
            self.sql().query_row("SELECT id,snapshot_digest,lifecycle_version FROM plan_drafts WHERE id=2", [], |r| Ok(json!({"draft_id":r.get::<_,i64>(0)?,"snapshot_digest":r.get::<_,String>(1)?,"lifecycle_version":r.get::<_,i64>(2)?,"state":"open","base":{"revision_id":1,"plan_version":1}}))).unwrap()
        } else {
            Value::Null
        };
        json!({"expected_base":{"revision_id":1,"plan_version":1},"expected_draft":expected,
            "remap_checkoff_links":true,"decisions":[{"kind":"set_quantity_override","target":{"part_key":"bracket.stl","relative_path":"bracket.stl","source_layer":"base:Fixture Source"},"value":4}]})
    }
    fn save(&self, owner: &WriterOwner, request: Value, key: &str) -> anyhow::Result<Outcome> {
        self.save_with(
            owner,
            request,
            key,
            self.reads(),
            PreparationLimits::default(),
            Arc::new(AtomicBool::new(false)),
        )
    }
    fn save_with(
        &self,
        owner: &WriterOwner,
        request: Value,
        key: &str,
        reads: Arc<Shared>,
        limits: PreparationLimits,
        cancel: Arc<AtomicBool>,
    ) -> anyhow::Result<Outcome> {
        owner
            .working_drafts_with_observations(policy(), reads, limits)?
            .save(
                SaveCommand::new(
                    session(),
                    id(1),
                    serde_json::from_value(request)?,
                    key.into(),
                )?,
                cancel,
                Duration::from_secs(1),
            )
    }
    fn state(&self) -> Vec<String> {
        let sql = self.sql();
        let mut q = sql.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
        let tables = q
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        tables
            .into_iter()
            .map(|t| {
                let mut q = sql
                    .prepare(&format!("SELECT * FROM \"{t}\" ORDER BY rowid"))
                    .unwrap();
                let n = q.column_count();
                format!(
                    "{t}:{:?}",
                    q.query_map([], |r| (0..n)
                        .map(|i| r.get::<_, rusqlite::types::Value>(i))
                        .collect::<rusqlite::Result<Vec<_>>>())
                        .unwrap()
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .unwrap()
                )
            })
            .collect()
    }
    fn close_source(&self) {
        self.sql().execute("UPDATE plan_drafts SET state='abandoned',lifecycle_version=lifecycle_version+1 WHERE id=2", []).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
fn saved(outcome: Outcome) -> Value {
    let v: Value = serde_json::from_slice(&outcome.into_json_body().unwrap().into_bytes()).unwrap();
    assert_eq!(v["kind"], "saved", "{v}");
    v
}
fn refused(f: &Fixture, owner: &WriterOwner, request: Value, key: &str, reason: &str) {
    let before = f.state();
    let v: Value = serde_json::from_slice(
        &f.save(owner, request, key)
            .unwrap()
            .into_json_body()
            .unwrap()
            .into_bytes(),
    )
    .unwrap();
    assert_eq!(v["reason"]["kind"], reason, "{v}");
    assert_eq!(before, f.state(), "refusal changed database");
}
#[test]
fn save_publishes_closes_source_and_replays_without_observation() {
    let f = Fixture::new();
    std::fs::write(
        f.path.join("print-partner.db"),
        include_bytes!("fixtures/required-units/unsafe.db"),
    )
    .unwrap();
    let owner = f.open();
    let digest = f.request(true)["expected_draft"]["snapshot_digest"].clone();
    let selection = owner.required_units_with_policy(policy()).unwrap().reconcile(
        pp_storage::required_units::ReconciliationCommand::new(id(1), id(2),
            serde_json::from_value(json!({"expected_snapshot_digest":digest,"decisions":[{"kind":"replace","target_draft_part_id":4}]})).unwrap(),
            "source-selection", session()).unwrap(), Arc::new(AtomicBool::new(false)), Duration::from_secs(1)).unwrap();
    assert!(matches!(
        selection,
        pp_storage::required_units::Outcome::Ready { .. }
    ));
    let req = f.request(true);
    let first = saved(f.save(&owner, req.clone(), "\u{feff} save \u{a0}").unwrap());
    assert_eq!(first["closed_draft_ids"], json!([2]));
    assert_eq!(first["receipt"]["plan_version"], 2);
    assert_eq!(first["authority"]["snapshot"]["planVersion"], 2);
    assert_eq!(
        f.sql()
            .query_row(
                "SELECT state,lifecycle_version FROM plan_drafts WHERE id=2",
                [],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            )
            .unwrap(),
        ("abandoned".into(), 1)
    );
    assert_eq!(f.sql().query_row("SELECT quantity_override FROM plan_revision_parts WHERE revision_id=(SELECT accepted_plan_revision_id FROM build_profiles WHERE id=1) AND part_key='bracket.stl'",[],|r|r.get::<_,i64>(0)).unwrap(),4);
    assert_eq!(f.sql().query_row("SELECT count(*) FROM plan_draft_required_unit_decisions WHERE reconciliation_id=(SELECT current_required_unit_reconciliation_id FROM plan_drafts WHERE id=3) AND kind='replace' AND target_draft_part_id<>4", [], |r|r.get::<_,i64>(0)).unwrap(),1);
    let before = f.state();
    let limits = PreparationLimits {
        entries: 0,
        comparison_work: 0,
        ..PreparationLimits::default()
    };
    let replay = saved(
        f.save_with(
            &owner,
            req.clone(),
            "save",
            f.reads(),
            limits,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap(),
    );
    assert_eq!(replay["receipt"], first["receipt"]);
    assert_eq!(before, f.state());
    let mut conflict = req.clone();
    conflict["remap_checkoff_links"] = json!(false);
    refused(&f, &owner, conflict, "save", "idempotency_conflict");
    refused(&f, &owner, req, "different-key", "base_changed");
    owner.shutdown().unwrap();
}
#[test]
fn save_without_source_prepares_from_accepted_and_captures_current_authority_on_replay() {
    let f = Fixture::new();
    f.close_source();
    let owner = f.open();
    let req = f.request(false);
    let first = saved(f.save(&owner, req.clone(), "first").unwrap());
    assert_eq!(first["closed_draft_ids"], json!([]));
    let mut next = req.clone();
    next["expected_base"] = json!({"revision_id":first["receipt"]["revision_id"],"plan_version":2});
    next["decisions"][0]["value"] = json!(5);
    let second = saved(f.save(&owner, next, "next").unwrap());
    assert_eq!(second["receipt"]["plan_version"], 3);
    let replay = saved(f.save(&owner, req, "first").unwrap());
    assert_eq!(replay["receipt"], first["receipt"]);
    assert_eq!(replay["authority"]["snapshot"]["planVersion"], 3);
    owner.shutdown().unwrap();
}
#[test]
fn stale_missing_and_changed_inputs_refuse_without_writes() {
    let f = Fixture::new();
    let owner = f.open();
    refused(&f, &owner, f.request(false), "unobserved", "draft_changed");
    let mut req = f.request(true);
    req["expected_draft"]["snapshot_digest"] = json!("f".repeat(64));
    refused(&f, &owner, req, "digest", "draft_changed");
    let mut req = f.request(true);
    req["expected_draft"]["draft_id"] = json!(999);
    refused(&f, &owner, req, "missing", "draft_changed");
    let mut req = f.request(true);
    req["expected_base"]["plan_version"] = json!(2);
    req["expected_draft"]["base"]["plan_version"] = json!(2);
    refused(&f, &owner, req, "stale", "base_changed");
    f.sql()
        .execute(
            "UPDATE source_revisions SET manifest_digest=? WHERE id=1",
            ["b".repeat(64)],
        )
        .unwrap();
    refused(&f, &owner, f.request(true), "inputs", "inputs_changed");
    owner.shutdown().unwrap();
}
#[test]
fn choice_fallback_and_reset_match_node() {
    for (key, path) in [
        ("missing-key", "/BRACKET.STL/"),
        ("/BRACKET.STL/", "missing-path"),
    ] {
        let f = Fixture::new();
        let owner = f.open();
        let mut req = f.request(true);
        req["decisions"][0]["target"]["part_key"] = json!(key);
        req["decisions"][0]["target"]["relative_path"] = json!(path);
        req["decisions"][0]["value"] = Value::Null;
        saved(f.save(&owner, req, "reset").unwrap());
        assert_eq!(f.sql().query_row("SELECT quantity_override,quantity_effective FROM plan_revision_parts WHERE revision_id=(SELECT accepted_plan_revision_id FROM build_profiles WHERE id=1) AND part_key='bracket.stl'",[],|r|Ok((r.get::<_,Option<i64>>(0)?,r.get::<_,i64>(1)?))).unwrap(),(None,1));
        owner.shutdown().unwrap();
    }
}
#[test]
fn unresolved_and_colliding_choice_targets_refuse_without_writes() {
    let f = Fixture::new();
    let owner = f.open();
    let mut req = f.request(true);
    req["decisions"][0]["target"]["part_key"] = json!("missing");
    req["decisions"][0]["target"]["relative_path"] = json!("missing");
    refused(&f, &owner, req, "missing", "part_not_found");
    let mut req = f.request(true);
    let mut alias = req["decisions"][0].clone();
    alias["target"]["part_key"] = json!("alias");
    req["decisions"].as_array_mut().unwrap().push(alias);
    refused(&f, &owner, req, "collision", "part_ambiguous");
    owner.shutdown().unwrap();
}
#[test]
fn publication_failure_rolls_back_snapshot_reconciliation_and_source_closure() {
    let f = Fixture::new();
    let owner = f.open();
    f.sql().execute_batch("CREATE TRIGGER reject_save BEFORE INSERT ON plan_revisions BEGIN SELECT RAISE(ABORT,'save injection'); END").unwrap();
    let before = f.state();
    let error = f.save(&owner, f.request(true), "failure").unwrap_err();
    assert!(error.to_string().contains("save injection"), "{error:#}");
    assert_eq!(before, f.state());
    f.sql().execute_batch("DROP TRIGGER reject_save; CREATE TRIGGER refuse_close BEFORE UPDATE OF state ON plan_drafts WHEN OLD.id=2 AND NEW.state='abandoned' BEGIN SELECT RAISE(IGNORE); END").unwrap();
    refused(
        &f,
        &owner,
        f.request(true),
        "refused-closure",
        "draft_changed",
    );
    f.sql().execute_batch("DROP TRIGGER refuse_close").unwrap();
    saved(f.save(&owner, f.request(true), "failure").unwrap());
    owner.shutdown().unwrap();
}
#[test]
fn post_commit_capture_failure_retains_receipt_and_is_retryable() {
    let f = Fixture::new();
    let owner = f.open();
    let req = f.request(true);
    std::fs::remove_dir_all(f.path.join("repos/1/revisions/fixture")).unwrap();
    let error = f.save(&owner, req.clone(), "committed").unwrap_err();
    let failure = error
        .downcast_ref::<CommittedSaveCaptureFailure>()
        .expect("committed receipt");
    assert_eq!(failure.receipt.plan_version().get(), 2);
    assert_eq!(failure.closed_draft_ids, vec![id(2)]);
    assert_eq!(
        f.sql()
            .query_row(
                "SELECT accepted_plan_version FROM build_profiles WHERE id=1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    let repo = f.path.join("repos/1/revisions/fixture");
    std::fs::create_dir_all(&repo).unwrap();
    for file in ["bracket.stl", "excluded.stl", "gear.stl", "new.stl"] {
        std::fs::write(repo.join(file), file).unwrap();
    }
    let retry = saved(f.save(&owner, req, "committed").unwrap());
    assert_eq!(
        retry["receipt"],
        serde_json::to_value(&failure.receipt).unwrap()
    );
    owner.shutdown().unwrap();
}
#[test]
fn source_budget_and_cancellation_leave_database_unchanged() {
    let f = Fixture::new();
    let owner = f.open();
    let before = f.state();
    let limits = PreparationLimits {
        visits: 0,
        ..PreparationLimits::default()
    };
    let error = f
        .save_with(
            &owner,
            f.request(true),
            "limited",
            f.reads(),
            limits,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<ReadFailure>(),
            Some(ReadFailure::LimitExceeded(Resource::Visits))
        ),
        "{error:#}"
    );
    assert_eq!(before, f.state());
    let limits = PreparationLimits {
        comparison_work: 0,
        ..PreparationLimits::default()
    };
    let error = f
        .save_with(
            &owner,
            f.request(true),
            "work",
            f.reads(),
            limits,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<ReadFailure>(),
            Some(ReadFailure::LimitExceeded(Resource::ComparisonWork))
        ),
        "{error:#}"
    );
    assert_eq!(before, f.state());
    let reads = Arc::new(Shared(Arc::new(Reads {
        files: vec!["bracket.stl".into()],
        cancel: Some(Arc::new(AtomicBool::new(false))),
    })));
    let cancel = reads.0.cancel.as_ref().unwrap().clone();
    f.close_source();
    let before = f.state();
    let error = f
        .save_with(
            &owner,
            f.request(false),
            "cancel",
            reads,
            PreparationLimits::default(),
            cancel,
        )
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<ReadFailure>(),
            Some(ReadFailure::Cancelled)
        ),
        "{error:#}"
    );
    assert_eq!(before, f.state());
    owner.shutdown().unwrap();
}

/// GRE-1197: Save writes a new draft from the observed one, which carries the
/// observed draft's header row.
#[test]
fn save_carries_the_source_draft_header() {
    let f = Fixture::new();
    let owner = f.open();
    f.sql().execute("INSERT INTO plan_draft_headers (draft_id,tenant_id,name,order_number,customer,due_date,header_version,updated_at) VALUES (2,'default','Renamed','PO-7','Acme','2026-12-01',3,'2026-11-06T00:00:00.000Z')", []).unwrap();
    let first = saved(f.save(&owner, f.request(true), "header").unwrap());
    assert_eq!(first["closed_draft_ids"], json!([2]));
    let header = |draft: i64| {
        f.sql()
            .query_row(
                "SELECT tenant_id,name,order_number,customer,due_date,header_version,updated_at FROM plan_draft_headers WHERE draft_id=?1",
                [draft],
                |row| {
                    (0..7)
                        .map(|index| row.get::<_, rusqlite::types::Value>(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                },
            )
            .unwrap()
    };
    let created: i64 = f
        .sql()
        .query_row("SELECT max(id) FROM plan_drafts", [], |row| row.get(0))
        .unwrap();
    assert!(created > 2);
    assert_eq!(header(created), header(2));
    owner.shutdown().unwrap();
}
