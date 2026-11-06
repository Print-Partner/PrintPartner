#![cfg(feature = "desktop_rebuild")]
//! GRE-816: abandon replays its lifecycle without selecting Required units,
//! and a cancelled follow-on selection keeps the edit that already committed.
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    read_model::Credential,
    working_drafts::{
        Failure, PositiveId, WorkingDraftClient,
        observation::{
            ArtifactRead, CheckoutManifestUse, DocumentRead, DraftReads, InventoryPath,
            OwnedCatalogReadRequest, OwnedCheckoutReadRequest, OwnedStlReadRequest,
            PreparationBudget, PreparationLimits, ReadResult, StlInventory, StlRootRead,
        },
        save::SaveCommand,
    },
};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const FILES: [&str; 4] = ["bracket.stl", "excluded.stl", "gear.stl", "new.stl"];
const WAIT: Duration = Duration::from_secs(2);

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
fn live() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// Serves the fixture repo's four STLs, hashing each path as its bytes.
struct Reads;
struct Root;
struct Inventory(Vec<InventoryPath>);
impl DraftReads for Reads {
    fn resolve_stl_root(
        &self,
        _: &OwnedStlReadRequest,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlRootRead>> {
        budget.check()?;
        Ok(Box::new(Root))
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
        let entries = FILES
            .iter()
            .enumerate()
            .map(|(traversal_ordinal, path)| {
                budget.entry(0, path.len())?;
                Ok(InventoryPath {
                    physical_relative_path: (*path).into(),
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
            "pp-draft-lifecycle-{}",
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
        for file in FILES {
            std::fs::write(repo.join(file), file).unwrap();
        }
        Self { path }
    }
    fn sql(&self) -> Connection {
        Connection::open(self.path.join("print-partner.db")).unwrap()
    }
    fn open(&self) -> (WriterOwner, WorkingDraftClient) {
        let owner = WriterOwner::open(&self.path, Limits::default()).unwrap().0;
        let client = owner
            .working_drafts_with_observations(
                policy(),
                Arc::new(Reads),
                PreparationLimits::default(),
            )
            .unwrap();
        (owner, client)
    }
    /// Every table's rows, for exact before/after comparison.
    fn tables(&self) -> BTreeMap<String, String> {
        let sql = self.sql();
        let names = sql
            .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        names
            .into_iter()
            .map(|table| {
                let mut q = sql
                    .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
                    .unwrap();
                let n = q.column_count();
                let rows = q
                    .query_map([], |r| {
                        (0..n)
                            .map(|i| r.get::<_, rusqlite::types::Value>(i))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                (table, format!("{rows:?}"))
            })
            .collect()
    }
    fn changed(&self, before: &BTreeMap<String, String>) -> Vec<String> {
        let after = self.tables();
        assert_eq!(
            before.keys().collect::<Vec<_>>(),
            after.keys().collect::<Vec<_>>()
        );
        after
            .into_iter()
            .filter(|(table, rows)| before[table] != *rows)
            .map(|(table, _)| table)
            .collect()
    }
    fn draft(&self, draft_id: i64) -> (String, i64, Option<i64>, String) {
        self.sql().query_row("SELECT state,lifecycle_version,current_required_unit_reconciliation_id,snapshot_digest FROM plan_drafts WHERE id=?", [draft_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).unwrap()
    }
    fn parts(&self, draft_id: i64) -> Vec<(i64, i64, Option<i64>, i64)> {
        self.sql()
            .prepare("SELECT id,included,quantity_override,quantity_effective FROM plan_draft_parts WHERE draft_id=? ORDER BY id")
            .unwrap()
            .query_map([draft_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn abandon(client: &WorkingDraftClient, expected: u64) -> Value {
    let request = serde_json::from_value(json!({"expected_lifecycle_version":expected})).unwrap();
    serde_json::to_value(
        client
            .abandon(session(), id(1), id(2), request, live(), WAIT)
            .unwrap(),
    )
    .unwrap()
}
fn save(client: &WorkingDraftClient, expected_draft: Value, key: &str) -> Value {
    let request = json!({"expected_base":{"revision_id":1,"plan_version":1},"expected_draft":expected_draft,
        "remap_checkoff_links":true,"decisions":[{"kind":"set_quantity_override","target":{"part_key":"bracket.stl","relative_path":"bracket.stl","source_layer":"base:Fixture Source"},"value":4}]});
    let command = SaveCommand::new(
        session(),
        id(1),
        serde_json::from_value(request).unwrap(),
        key.into(),
    )
    .unwrap();
    let outcome = client.save(command, live(), WAIT).unwrap();
    serde_json::from_slice(&outcome.into_json_body().unwrap().into_bytes()).unwrap()
}

#[test]
fn ticket_t_61_publication_service_abandon_replay_preserves_lifecycle() {
    let f = Fixture::new();
    let (_, _, _, digest) = f.draft(2);
    let open = json!({"draft_id":2,"state":"open","lifecycle_version":0,"snapshot_digest":digest,"base":{"revision_id":1,"plan_version":1}});
    let (owner, client) = f.open();

    // Abandon closes the draft, records one `plan.draft_changed` event and
    // starts no Required-unit selection.
    let before = f.tables();
    let committed = abandon(&client, 0);
    let mut abandoned = open.clone();
    abandoned["state"] = json!("abandoned");
    abandoned["lifecycle_version"] = json!(1);
    assert_eq!(committed, json!({"kind":"ready","draft":abandoned}));
    assert_eq!(f.changed(&before), ["domain_events", "plan_drafts"]);
    assert_eq!(f.draft(2), ("abandoned".into(), 1, None, digest.clone()));

    // Replays return the committed lifecycle; later versions are not open.
    let before = f.tables();
    assert_eq!(abandon(&client, 0), committed);
    let not_open = abandon(&client, 1);
    assert_eq!(not_open["kind"], "not_open", "{not_open}");
    assert_eq!(not_open["workspace"]["draft"], abandoned);
    assert_eq!(abandon(&client, 2)["kind"], "draft_changed");
    assert!(f.changed(&before).is_empty(), "abandon replay wrote rows");

    // Save from the stale open identity refuses without writes.
    let refused = save(&client, open, "stale-source");
    assert_eq!(refused["reason"]["kind"], "draft_changed", "{refused}");
    assert!(f.changed(&before).is_empty(), "refused Save wrote rows");

    // Save without a source publishes and leaves the abandoned draft alone.
    let saved = save(&client, Value::Null, "after-abandon");
    assert_eq!(saved["kind"], "saved", "{saved}");
    assert_eq!(saved["closed_draft_ids"], json!([]));
    assert_eq!(saved["receipt"]["plan_version"], 2);
    assert_eq!(f.draft(2), ("abandoned".into(), 1, None, digest));
    let before = f.tables();
    assert_eq!(
        save(&client, Value::Null, "after-abandon")["receipt"],
        saved["receipt"]
    );
    assert_eq!(abandon(&client, 0), committed);
    assert!(
        f.changed(&before).is_empty(),
        "replay after Save wrote rows"
    );
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_61_publication_cancelled_follow_on_preserves_committed_edit() {
    let f = Fixture::new();
    let (_, _, _, opened) = f.draft(2);
    let edit = json!({"expected_snapshot_digest":opened,"decisions":[
        {"kind":"set_included","draft_part_ids":[5],"value":true},
        {"kind":"set_quantity_override","draft_part_ids":[6],"value":3}]});
    let edited = vec![(4, 1, Some(3), 3), (5, 1, Some(1), 1), (6, 1, Some(3), 3)];
    // Slow the follow-on selection's first insert so cancellation lands after
    // the edit has committed and while selection holds the write lock.
    f.sql().execute_batch("CREATE TRIGGER slow_select AFTER INSERT ON plan_draft_required_unit_reconciliations BEGIN SELECT sum(n) FROM (WITH RECURSIVE n(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM n WHERE n<1000000) SELECT n FROM n); END").unwrap();
    let (owner, client) = f.open();
    let cancelled = live();
    let worker = {
        let (cancelled, request) = (
            cancelled.clone(),
            serde_json::from_value(edit.clone()).unwrap(),
        );
        std::thread::spawn(move || client.edit(session(), id(1), id(2), request, cancelled, WAIT))
    };
    let probe = f.sql();
    probe.busy_timeout(Duration::ZERO).unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        if f.parts(2) == edited {
            match probe.execute_batch("BEGIN IMMEDIATE") {
                Ok(()) => probe.execute_batch("ROLLBACK").unwrap(),
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::DatabaseBusy =>
                {
                    break;
                }
                Err(e) => panic!("{e}"),
            }
        }
        assert!(Instant::now() < deadline, "selection never started");
        std::thread::sleep(Duration::from_millis(1));
    }
    let committed = f.tables();
    cancelled.store(true, Ordering::Release);
    let error = worker.join().unwrap().unwrap_err();
    assert!(
        matches!(error.downcast_ref(), Some(Failure::Cancelled)),
        "{error:?}"
    );
    owner.shutdown().unwrap();
    f.sql().execute_batch("DROP TRIGGER slow_select").unwrap();

    // The selection rolled back; the edit stays committed and unselected.
    assert!(
        f.changed(&committed).is_empty(),
        "cancelled selection wrote"
    );
    let (state, lifecycle, selection, digest) = f.draft(2);
    assert_eq!((state.as_str(), lifecycle, selection), ("open", 0, None));
    assert_ne!(digest, opened);

    // After a restart, retrying the same edit only selects units.
    let (owner, client) = f.open();
    let request = serde_json::from_value(edit).unwrap();
    let retried = serde_json::to_value(
        client
            .edit(session(), id(1), id(2), request, live(), WAIT)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(retried["kind"], "ready", "{retried}");
    // A plan-draft-v2 selection folds into the snapshot digest.
    let (state, lifecycle, selection, selected) = f.draft(2);
    assert_eq!((state.as_str(), lifecycle, selection), ("open", 0, Some(2)));
    assert_ne!(selected, digest);
    assert_eq!(
        retried["workspace"]["draft"],
        json!({"draft_id":2,"state":"open","lifecycle_version":0,"snapshot_digest":selected,"base":{"revision_id":1,"plan_version":1}})
    );
    assert_eq!(f.parts(2), edited);
    assert_eq!(
        f.changed(&committed),
        [
            "domain_events",
            "plan_draft_required_unit_assignments",
            "plan_draft_required_unit_reconciliations",
            "plan_drafts"
        ]
    );
    owner.shutdown().unwrap();
}
