#![cfg(feature = "desktop_rebuild")]

use pp_storage::{
    Limits, WriterOwner,
    auth::{
        AuthPolicy, AuthorityFailure, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy,
    },
    catalog::CreateSource,
    jobs::{
        ClaimedAttempt, Credential, JobKind, PersistentState, ServerWorkerClient, WorkerAdmission,
    },
    uploads::{Admission, File, Input, Operation, OwnedInput, Phase, State, Target},
};
use rusqlite::{Connection, OpenFlags, types::Value};
use std::{collections::BTreeMap, path::PathBuf, sync::atomic::AtomicBool, time::Duration};

const RESERVED: u64 = 20 * 1024 * 1024;

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

struct TemporaryDirectory(PathBuf);

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let result = std::fs::remove_dir_all(&self.0);
        if !std::thread::panicking() {
            result.expect("remove disposable import database");
        }
    }
}

struct Fixture {
    worker: ServerWorkerClient,
    owner: WriterOwner,
    directory: TemporaryDirectory,
}

impl Fixture {
    fn new(worker_policy: Option<AuthPolicy>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-import-worker-policy-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir(&root).unwrap();
        let directory = TemporaryDirectory(root);
        let (owner, _) = WriterOwner::open(&directory.0, Limits::default()).unwrap();
        let admission = WorkerAdmission {
            kinds: vec![(JobKind::SuppliedSourceImport, 1)],
            total: 1,
            per_resource: 1,
            lease_seconds: 60,
        };
        let worker = match worker_policy {
            Some(policy) => owner.job_worker_with_policy(policy, admission),
            None => owner.job_worker(admission),
        }
        .unwrap();
        Self {
            worker,
            owner,
            directory,
        }
    }

    fn admit_and_claim(&self) -> (Operation, ClaimedAttempt) {
        let imports = self.owner.imports(policy(), 64 * 1024 * 1024).unwrap();
        let operation = imports
            .admit(
                Credential::PhysicalOwner(self.owner.job_physical_owner()),
                Admission {
                    key: "worker-policy".into(),
                    target: Target::Create {
                        metadata: Box::new(CreateSource {
                            name: "Disposable import".into(),
                            source_kind: Some("local".into()),
                            source_type: Some("local".into()),
                            ..Default::default()
                        }),
                    },
                    input: Input::Files {
                        paths: vec!["part.stl".into()],
                    },
                    reserved_bytes: RESERVED,
                    max_input_bytes: 1024,
                    max_prepared_bytes: 2048,
                },
                vec![File {
                    path: "part.stl".into(),
                    size: 3,
                    sha256: "a".repeat(64),
                    kind: "input".into(),
                }],
                0,
                imports.accounting_epoch(),
                &AtomicBool::new(false),
            )
            .unwrap();
        let claim = self
            .worker
            .claim_import(&operation.job_id)
            .unwrap()
            .expect("physical owner import is claimable");
        assert_eq!(claim.job.job_id, operation.job_id);
        assert_eq!(claim.job.state, PersistentState::Running);
        assert_eq!(claim.job.generation, 1);
        assert!(
            claim.source_work.is_some(),
            "claim retains its Source lease"
        );
        assert_eq!(operation.state, State::Admitted);
        assert_eq!(operation.observation_cursor, Some(0));
        (operation, claim)
    }

    fn connection(&self) -> Connection {
        Connection::open_with_flags(
            self.directory.0.join("print-partner.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
    }

    fn snapshot(&self) -> DatabaseSnapshot {
        let mut connection = self.connection();
        let transaction = connection.transaction().unwrap();
        let tables = transaction
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        DatabaseSnapshot(
            tables
                .into_iter()
                .map(|name| {
                    let quoted = name.replace('"', "\"\"");
                    let mut statement = transaction
                        .prepare(&format!("SELECT * FROM \"{quoted}\""))
                        .unwrap();
                    let columns = statement
                        .column_names()
                        .map(str::to_owned)
                        .collect::<Vec<_>>();
                    let order = (1..=columns.len())
                        .map(|index| index.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    statement = transaction
                        .prepare(&format!("SELECT * FROM \"{quoted}\" ORDER BY {order}"))
                        .unwrap();
                    let rows = statement
                        .query_map([], |row| {
                            (0..columns.len())
                                .map(|index| row.get(index))
                                .collect::<rusqlite::Result<Vec<Value>>>()
                        })
                        .unwrap()
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .unwrap();
                    (name, TableSnapshot { columns, rows })
                })
                .collect(),
        )
    }
}

#[derive(Debug, PartialEq)]
struct DatabaseSnapshot(BTreeMap<String, TableSnapshot>);

#[derive(Debug, PartialEq)]
struct TableSnapshot {
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

impl TableSnapshot {
    fn column(&self, name: &str) -> usize {
        self.columns
            .iter()
            .position(|column| column == name)
            .unwrap()
    }
}

fn owned_input(operation: &Operation) -> OwnedInput {
    OwnedInput {
        locator: format!(".pp-imports/{}", operation.job_id),
        digest: operation.requested_digest.clone(),
        files: operation.requested_files.clone(),
    }
}

#[test]
fn policyless_worker_claims_physical_import_but_refuses_phase_without_writes() {
    let fixture = Fixture::new(None);
    let (operation, claim) = fixture.admit_and_claim();
    let phase = Phase::Owned(owned_input(&operation));
    let source_work = claim.source_work.as_ref().unwrap();
    let before = fixture.snapshot();
    let error = fixture
        .worker
        .import_phase(&claim.lease, source_work, phase)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AuthorityFailure>(),
        Some(&AuthorityFailure::PolicyRequired)
    );
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn policy_worker_persists_owned_input_and_fenced_observation() {
    let fixture = Fixture::new(Some(policy()));
    let (operation, claim) = fixture.admit_and_claim();
    let owned = owned_input(&operation);
    let source_work = claim.source_work.as_ref().unwrap();
    let connection = fixture.connection();
    let fence: String = connection
        .query_row(
            "SELECT json_extract(document,'$._attempt_fence') FROM durable_jobs WHERE id=?1",
            [&operation.job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!fence.is_empty());
    let earliest: String = connection
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut expected = fixture.snapshot();
    let result = fixture
        .worker
        .import_phase(&claim.lease, source_work, Phase::Owned(owned.clone()))
        .unwrap();
    let latest: String = connection
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ','now')", [], |row| {
            row.get(0)
        })
        .unwrap();
    let actual = fixture.snapshot();

    let mut expected_operation = operation.clone();
    expected_operation.state = State::OwnedInputReady;
    expected_operation.owned = Some(owned);
    expected_operation.observation_cursor = Some(1);
    assert_eq!(result, expected_operation);
    let journal = expected.0.get_mut("source_import_operations").unwrap();
    assert_eq!(journal.rows.len(), 1);
    let state = journal.column("state");
    let document = journal.column("document");
    journal.rows[0][state] = Value::Text("owned_input_ready".into());
    journal.rows[0][document] = Value::Text(serde_json::to_string(&expected_operation).unwrap());

    let observations = &actual.0["source_revision_observations"];
    assert_eq!(observations.rows.len(), 1);
    let timestamp = &observations.rows[0][observations.column("created_at")];
    let Value::Text(created_at) = timestamp else {
        panic!("observation timestamp must be text")
    };
    assert!(&earliest <= created_at && created_at <= &latest);
    let expected_observations = expected.0.get_mut("source_revision_observations").unwrap();
    assert!(expected_observations.rows.is_empty());
    assert_eq!(
        expected_observations.columns,
        [
            "tenant_id",
            "operation_key",
            "cursor",
            "phase",
            "job_id",
            "attempt_generation",
            "attempt_fence",
            "revision_id",
            "receipt_activated",
            "cancel_requested",
            "created_at"
        ]
    );
    expected_observations.rows.push(vec![
        Value::Text(operation.tenant),
        Value::Text(operation.key),
        Value::Integer(1),
        Value::Text("owned".into()),
        Value::Text(operation.job_id),
        Value::Integer(1),
        Value::Text(fence),
        Value::Null,
        Value::Null,
        Value::Integer(0),
        timestamp.clone(),
    ]);
    assert_eq!(actual, expected);
}

/// A worker sharing the import owner's admission claims by its own kind, so
/// it never takes a requeued import past admission; the owner still claims it
/// by kind (GRE-428).
#[test]
fn kind_claim_never_takes_a_requeued_import() {
    let fixture = Fixture::new(Some(policy()));
    let (operation, claim) = fixture.admit_and_claim();
    let source_work = claim.source_work.as_ref().unwrap();
    let owned = Phase::Owned(owned_input(&operation));
    fixture
        .worker
        .import_phase(&claim.lease, source_work, owned)
        .unwrap();
    drop(claim);
    let Fixture {
        worker,
        owner,
        directory,
    } = fixture;
    drop(worker);
    owner.shutdown().unwrap();

    // Opening storage requeues the stopped attempt.
    let (owner, _) = WriterOwner::open(&directory.0, Limits::default()).unwrap();
    let shared = WorkerAdmission {
        kinds: vec![
            (JobKind::PrinterUpload, 4),
            (JobKind::SuppliedSourceImport, 1),
        ],
        total: 5,
        per_resource: 1,
        lease_seconds: 60,
    };
    let worker = owner.job_worker_with_policy(policy(), shared).unwrap();
    let sends = worker
        .claim_pending_kind(
            JobKind::PrinterUpload,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(sends.receive().unwrap().is_none());
    let imported = worker.claim_kind(JobKind::SuppliedSourceImport).unwrap();
    assert_eq!(imported.expect("owner claims").job.job_id, operation.job_id);
    drop(worker);
    owner.shutdown().unwrap();
}
