#![cfg(feature = "desktop_rebuild")]
//! Supplied-import phases over the upload journal. Node writes supplied files in
//! one call and has no journal, attempts or Source observations, so these cases
//! port the reference `source_import_operation.rs` storage assertions.
//! Fail and Cleanup retries adopt only a matching last attempt observation,
//! after Source proof and authority checks. A generic claim of a recovered
//! import records one `authority_refused` Source observation and does not
//! record another when the claim is retried. Two complete imports of one
//! revision key share that revision and keep each import's attempt bindings.
//! An existing revision that is not complete is refused without changing the
//! revision or its attempt. Run these regressions with
//! `cargo test -p pp-storage --features desktop_rebuild --test import_phases`.
use pp_storage::{
    Limits, WriterOwner,
    auth::{
        self, AuthPolicy, AuthorityFailure, FirstUserTenant, RegistrationPolicy, Secret,
        SessionTenantPolicy,
    },
    catalog::{
        CreateSource, Credentials, Deletion, Outcome as CatalogOutcome, Request, SourceWorkLease,
    },
    jobs::{
        ClaimedAttempt, Credential, JobKind, Outcome, ServerWorkerClient, UserOperation,
        WorkerAdmission,
    },
    uploads::{
        Admission, Artifact, File, ImportClient, IncompleteRevisionKey, Input, Operation,
        OwnedInput, Phase, Postprocessing, SourceAuthorityRevision, State, Target,
    },
};
use rusqlite::{Connection, params};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const RESERVED: u64 = 3_506_438_144;
const QUOTA: u64 = 16 * 1024 * 1024 * 1024;

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

struct Fixture {
    root: PathBuf,
    owner: WriterOwner,
    imports: ImportClient,
    worker: ServerWorkerClient,
}

impl Fixture {
    fn open() -> Self {
        Self::open_with_admission(1)
    }

    fn open_with_admission(per_resource: usize) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-import-phases-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        Self::with_admission(root, owner, per_resource)
    }

    fn with(root: PathBuf, owner: WriterOwner) -> Self {
        Self::with_admission(root, owner, 1)
    }

    fn with_admission(root: PathBuf, owner: WriterOwner, per_resource: usize) -> Self {
        let imports = owner.imports(policy(), QUOTA).unwrap();
        let admission = WorkerAdmission {
            kinds: JobKind::ALL.into_iter().map(|kind| (kind, 2)).collect(),
            total: 4,
            per_resource,
            lease_seconds: 60,
        };
        let worker = owner.job_worker_with_policy(policy(), admission).unwrap();
        Self {
            root,
            owner,
            imports,
            worker,
        }
    }

    fn reopen(self) -> Self {
        let Self { root, owner, .. } = self;
        owner.shutdown().unwrap();
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        Self::with(root, owner)
    }

    fn physical(&self) -> Credential {
        Credential::PhysicalOwner(self.owner.job_physical_owner())
    }

    fn auth(&self, request: auth::Request) -> auth::Outcome {
        self.owner
            .auth_with_policy(policy())
            .unwrap()
            .submit(
                request,
                Arc::new(AtomicBool::new(false)),
                Duration::from_secs(5),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
    }

    fn register(&self) -> String {
        let auth::Outcome::Session { token, .. } = self.auth(auth::Request::Register {
            email: "importer@example.test".into(),
            display_name: "Importer".into(),
            password: Secret::new("long-test-password".into()),
        }) else {
            panic!("session expected")
        };
        token.expose().into()
    }

    fn login(&self) -> String {
        let auth::Outcome::Session { token, .. } = self.auth(auth::Request::Login {
            email: "importer@example.test".into(),
            password: Secret::new("long-test-password".into()),
        }) else {
            panic!("session expected")
        };
        token.expose().into()
    }

    fn logout(&self, token: &str) {
        self.auth(auth::Request::Logout {
            token: Secret::new(token.into()),
        });
    }

    fn admit(&self, credential: Credential, key: &str) -> Operation {
        self.try_admit(credential, key, RESERVED).unwrap()
    }

    fn try_admit(
        &self,
        credential: Credential,
        key: &str,
        reserved: u64,
    ) -> anyhow::Result<Operation> {
        let files = vec![file("part.stl", "input", 'a')];
        let request = Admission {
            key: key.into(),
            target: Target::Create {
                metadata: Box::new(CreateSource {
                    name: key.into(),
                    source_kind: Some("local".into()),
                    source_type: Some("local".into()),
                    ..Default::default()
                }),
            },
            input: Input::Files {
                paths: vec!["part.stl".into()],
            },
            reserved_bytes: reserved,
            max_input_bytes: 268_435_456,
            max_prepared_bytes: 1_073_741_824,
        };
        let epoch = self.imports.accounting_epoch();
        let cancelled = AtomicBool::new(false);
        self.imports
            .admit(credential, request, files, 0, epoch, &cancelled)
    }

    fn claim(&self, op: &Operation) -> ClaimedAttempt {
        self.worker.claim_import(&op.job_id).unwrap().unwrap()
    }

    fn phase(&self, claim: &ClaimedAttempt, phase: Phase) -> anyhow::Result<Operation> {
        self.worker
            .import_phase(&claim.lease, claim.source_work.as_ref().unwrap(), phase)
    }

    fn cancel(&self, token: &str, op: &Operation) -> pp_storage::jobs::JobRecord {
        let Outcome::Job(job, _) = self
            .owner
            .jobs(policy())
            .unwrap()
            .submit(
                Credential::Session(Secret::new(token.into())),
                UserOperation::Cancel {
                    job_id: op.job_id.clone(),
                },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
            .receive()
            .unwrap()
        else {
            panic!("job expected")
        };
        job
    }

    fn delete_source(&self, token: &str, op: &Operation) -> Deletion {
        let catalog = self
            .owner
            .source_catalog_with_policy(Credentials::Session(Secret::new(token.into())), policy())
            .unwrap();
        let CatalogOutcome::Deletion(deletion) = catalog
            .execute(Request::Delete { id: op.source_id })
            .unwrap()
        else {
            panic!("deletion expected")
        };
        deletion
    }

    /// The import's quota row as (reserved, retained, settled).
    fn quota(&self, op: &Operation) -> (i64, i64, bool) {
        self.connection()
            .query_row(
                "SELECT reserved_bytes,retained_bytes,settled FROM source_import_quota WHERE operation_key=?1",
                [&op.key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    fn document(&self, op: &Operation) -> String {
        self.connection()
            .query_row(
                "SELECT document FROM source_import_operations WHERE operation_key=?1",
                [&op.key],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn connection(&self) -> Connection {
        Connection::open(self.root.join("print-partner.db")).unwrap()
    }

    fn snapshot(&self) -> Vec<(String, Vec<Vec<String>>)> {
        let connection = self.connection();
        let tables = connection
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        tables
            .into_iter()
            .map(|table| {
                let mut statement = connection
                    .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
                    .unwrap();
                let width = statement.column_count();
                let mut rows = statement
                    .query_map([], |row| {
                        (0..width)
                            .map(|index| {
                                row.get::<_, rusqlite::types::Value>(index)
                                    .map(|value| format!("{value:?}"))
                            })
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .unwrap();
                rows.sort();
                (table, rows)
            })
            .collect()
    }

    fn rejected_phase(
        &self,
        claim: &ClaimedAttempt,
        source_work: &SourceWorkLease,
        phase: Phase,
        expected: &str,
    ) {
        let before = self.snapshot();
        assert_eq!(
            self.worker
                .import_phase(&claim.lease, source_work, phase)
                .unwrap_err()
                .to_string(),
            expected
        );
        assert_eq!(self.snapshot(), before);
    }

    fn assert_retry(&self, claim: &ClaimedAttempt, phase: Phase, expected: &Operation) {
        let before = self.snapshot();
        let epoch = self.imports.accounting_epoch();
        for _ in 0..2 {
            assert_eq!(self.phase(claim, phase.clone()).unwrap(), *expected);
            assert_eq!(self.snapshot(), before);
            assert_eq!(self.imports.accounting_epoch(), epoch);
        }
    }

    fn job(&self, op: &Operation) -> (String, Option<String>, i64) {
        self.connection()
            .query_row(
                "SELECT state,json_extract(document,'$._authority_refusal.phase'),json_extract(document,'$.generation') FROM durable_jobs WHERE id=?1",
                [&op.job_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    /// Each observation as (cursor, phase, generation, fence, revision,
    /// activated, cancel).
    fn observations(&self, op: &Operation) -> Vec<Observation> {
        self.connection()
            .prepare("SELECT cursor,phase,attempt_generation,attempt_fence,revision_id,receipt_activated,cancel_requested FROM source_revision_observations WHERE operation_key=?1 ORDER BY cursor")
            .unwrap()
            .query_map([&op.key], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn refusal_events(&self, op: &Operation) -> i64 {
        self.connection()
            .query_row(
                "SELECT COUNT(*) FROM durable_job_history WHERE job_id=?1 AND event='authority_refused'",
                [&op.job_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn preclaim_rows(&self) -> i64 {
        self.connection()
            .query_row("SELECT COUNT(*) FROM source_preclaim_refusals", [], |row| {
                row.get(0)
            })
            .unwrap()
    }
}

type Observation = (
    i64,
    String,
    i64,
    Option<String>,
    Option<i64>,
    Option<bool>,
    bool,
);

fn file(path: &str, kind: &str, byte: char) -> File {
    File {
        path: path.into(),
        size: 3,
        sha256: byte.to_string().repeat(64),
        kind: kind.into(),
    }
}

fn fail() -> Phase {
    Phase::Fail("Import failed".into())
}

fn owned(op: &Operation) -> Phase {
    Phase::Owned(OwnedInput {
        locator: format!(".pp-imports/{}", op.job_id),
        digest: op.requested_digest.clone(),
        files: op.requested_files.clone(),
    })
}

fn admit_files(
    fixture: &Fixture,
    credential: Credential,
    key: &str,
    target: Target,
    files: Vec<File>,
) -> Operation {
    let paths = files.iter().map(|file| file.path.clone()).collect();
    fixture
        .imports
        .admit(
            credential,
            Admission {
                key: key.into(),
                target,
                input: Input::Files { paths },
                reserved_bytes: RESERVED,
                max_input_bytes: 268_435_456,
                max_prepared_bytes: 1_073_741_824,
            },
            files,
            0,
            fixture.imports.accounting_epoch(),
            &AtomicBool::new(false),
        )
        .unwrap()
}

fn activate_import(fixture: &Fixture, op: &Operation) -> Operation {
    let claim = fixture.claim(op);
    fixture.phase(&claim, owned(op)).unwrap();
    fixture.phase(&claim, published(op)).unwrap();
    let activated = fixture.phase(&claim, Phase::Activate).unwrap();
    fixture.phase(&claim, Phase::Cleanup).unwrap();
    activated
}

fn published(op: &Operation) -> Phase {
    let upstream_key = "c".repeat(64);
    Phase::Published(Artifact {
        tenant: op.tenant.clone(),
        source_id: op.source_id,
        locator: format!("{}/revisions/{upstream_key}", op.source_id),
        upstream_key,
        manifest_digest: "d".repeat(64),
        stored_bytes: 9,
        files: vec![
            file("parts/b.stl", "stl", 'e'),
            file("guide.pdf", "pdf", 'f'),
            file("README.md", "readme", '1'),
        ],
        suggested_rules: vec!["parts/".into()],
    })
}

fn authority(error: &anyhow::Error) -> Option<&AuthorityFailure> {
    error.downcast_ref::<AuthorityFailure>()
}

#[test]
fn exact_fail_retry_adopts_the_recorded_result_without_writes() {
    let fixture = Fixture::open();
    let op = fixture.admit(fixture.physical(), "fail-retry");
    let claim = fixture.claim(&op);
    let failed = fixture.phase(&claim, fail()).unwrap();
    assert_eq!(failed.state, State::Failed);
    assert_eq!(failed.observation_cursor, Some(1));
    fixture.assert_retry(&claim, fail(), &failed);
}

#[test]
fn exact_cleanup_retry_adopts_the_recorded_result_without_writes() {
    let fixture = Fixture::open();
    let op = fixture.admit(fixture.physical(), "cleanup-retry");
    let claim = fixture.claim(&op);
    fixture.phase(&claim, owned(&op)).unwrap();
    fixture.phase(&claim, published(&op)).unwrap();
    fixture.phase(&claim, Phase::Activate).unwrap();
    let settled = fixture.phase(&claim, Phase::Cleanup).unwrap();
    assert!(settled.cleanup_settled);
    assert_eq!(fixture.job(&op).0, "succeeded");
    fixture.assert_retry(&claim, Phase::Cleanup, &settled);
    for phase in [
        Phase::Read,
        owned(&op),
        published(&op),
        Phase::Activate,
        fail(),
    ] {
        fixture.rejected_phase(
            &claim,
            claim.source_work.as_ref().unwrap(),
            phase,
            "Stale attempt fence",
        );
    }
}

#[test]
fn failed_cleanup_retries_preserve_the_released_quota() {
    let fixture = Fixture::open();
    let op = fixture.admit(fixture.physical(), "failed-cleanup");
    let claim = fixture.claim(&op);
    fixture.phase(&claim, fail()).unwrap();
    let settled = fixture.phase(&claim, Phase::Cleanup).unwrap();
    assert_eq!(fixture.quota(&op), (0, 0, true));
    assert_eq!(fixture.job(&op).0, "failed");
    assert_eq!(fixture.observations(&op).len(), 2);
    fixture.assert_retry(&claim, Phase::Cleanup, &settled);
    fixture.rejected_phase(
        &claim,
        claim.source_work.as_ref().unwrap(),
        fail(),
        "Stale attempt fence",
    );
    fixture.reopen();
}

#[test]
fn a_recovered_fail_records_the_new_attempt_and_rejects_the_old_one() {
    let fixture = Fixture::open();
    let op = fixture.admit(fixture.physical(), "recovered-fail");
    let old = fixture.claim(&op);
    fixture.phase(&old, fail()).unwrap();
    let fixture = fixture.reopen();
    let claim = fixture.claim(&op);
    let failed = fixture.phase(&claim, fail()).unwrap();
    assert_eq!(failed.observation_cursor, Some(2));
    let observations = fixture.observations(&op);
    assert_ne!(observations[0].2, observations[1].2);
    assert_ne!(observations[0].3, observations[1].3);
    fixture.assert_retry(&claim, fail(), &failed);
    let settled = fixture.phase(&claim, Phase::Cleanup).unwrap();
    for phase in [fail(), Phase::Cleanup] {
        fixture.rejected_phase(
            &old,
            claim.source_work.as_ref().unwrap(),
            phase,
            "Foreign worker lease",
        );
    }
    fixture.assert_retry(&claim, Phase::Cleanup, &settled);
    fixture.reopen();
}

#[test]
fn a_previous_generation_cannot_adopt_the_current_attempts_cleanup() {
    let fixture = Fixture::open();
    let token = fixture.register();
    let op = fixture.admit(
        Credential::Session(Secret::new(token.clone())),
        "old-generation",
    );
    let mut old = fixture.claim(&op);
    fixture.phase(&old, fail()).unwrap();
    fixture.cancel(&token, &op);
    old.source_work.as_mut().unwrap().release().unwrap();
    let claim = fixture.claim(&op);
    let failed = fixture.phase(&claim, fail()).unwrap();
    assert_eq!(failed.state, State::Cancelled);
    let settled = fixture.phase(&claim, Phase::Cleanup).unwrap();
    for phase in [fail(), Phase::Cleanup] {
        fixture.rejected_phase(
            &old,
            claim.source_work.as_ref().unwrap(),
            phase,
            "Stale attempt fence",
        );
    }
    fixture.assert_retry(&claim, Phase::Cleanup, &settled);
}

#[test]
fn cleanup_retry_requires_a_live_source_proof() {
    let fixture = Fixture::open();
    let op = fixture.admit(fixture.physical(), "cleanup-proof");
    let mut claim = fixture.claim(&op);
    fixture.phase(&claim, fail()).unwrap();
    fixture.phase(&claim, Phase::Cleanup).unwrap();
    claim.source_work.as_mut().unwrap().release().unwrap();
    let replacement = fixture
        .owner
        .local_source_catalog()
        .begin_work(
            op.source_id,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    fixture.rejected_phase(
        &claim,
        claim.source_work.as_ref().unwrap(),
        Phase::Cleanup,
        "Lease already released",
    );
    drop(replacement);
}

#[test]
fn cleanup_retry_revalidates_authority_without_mutating_the_terminal_job() {
    let fixture = Fixture::open();
    let token = fixture.register();
    let op = fixture.admit(
        Credential::Session(Secret::new(token.clone())),
        "cleanup-authority",
    );
    let claim = fixture.claim(&op);
    fixture.phase(&claim, fail()).unwrap();
    fixture.phase(&claim, Phase::Cleanup).unwrap();
    fixture.logout(&token);
    let before = fixture.snapshot();
    let error = fixture.phase(&claim, Phase::Cleanup).unwrap_err();
    assert_eq!(
        authority(&error),
        Some(&AuthorityFailure::CredentialInvalid)
    );
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(fixture.job(&op).1, None);
}

#[test]
fn terminal_settlement_without_cleanup_cannot_be_adopted_as_a_cleanup_retry() {
    let fixture = Fixture::open();
    let token = fixture.register();
    let op = fixture.admit(
        Credential::Session(Secret::new(token.clone())),
        "refused-cleanup",
    );
    let claim = fixture.claim(&op);
    fixture.phase(&claim, fail()).unwrap();
    fixture.logout(&token);
    assert!(fixture.phase(&claim, Phase::Cleanup).is_err());
    fixture.rejected_phase(
        &claim,
        claim.source_work.as_ref().unwrap(),
        Phase::Cleanup,
        "Stale attempt fence",
    );
}

#[test]
fn phases_carry_an_admitted_import_to_a_settled_activation() {
    let fixture = Fixture::open();
    let op = fixture.admit(fixture.physical(), "activate");
    let lease = fixture.claim(&op);
    let epoch = fixture.imports.accounting_epoch();
    assert_eq!(fixture.phase(&lease, Phase::Read).unwrap(), op);
    assert_eq!(fixture.imports.accounting_epoch(), epoch);
    assert_eq!(
        fixture
            .phase(&lease, published(&op))
            .unwrap_err()
            .to_string(),
        "Input not ready"
    );
    assert_eq!(
        fixture
            .phase(&lease, Phase::Cleanup)
            .unwrap_err()
            .to_string(),
        "Import is not settled"
    );
    let ready = fixture.phase(&lease, owned(&op)).unwrap();
    assert_eq!(ready.state, State::OwnedInputReady);
    assert_eq!(fixture.imports.accounting_epoch(), epoch + 1);
    assert_eq!(
        fixture.phase(&lease, owned(&op)).unwrap_err().to_string(),
        "Input already captured"
    );
    assert_eq!(
        fixture.phase(&lease, published(&op)).unwrap().state,
        State::Published
    );
    let activated = fixture.phase(&lease, Phase::Activate).unwrap();
    let receipt = activated.receipt.clone().unwrap();
    assert_eq!(
        (&activated.state, receipt.activated, receipt.postprocessing),
        (
            &State::Activated,
            true,
            Postprocessing::DocumentMetadataIndexedPdfPending
        )
    );
    let settled = fixture.phase(&lease, Phase::Cleanup).unwrap();
    assert!(settled.cleanup_settled);
    assert_eq!(settled.observation_cursor, Some(4));
    assert_eq!(
        fixture.phase(&lease, Phase::Read).unwrap_err().to_string(),
        "Stale attempt fence"
    );

    let db = fixture.connection();
    let source: (i64, String, String, String) = db
        .query_row(
            "SELECT current_source_revision_id,last_commit_sha,imported_paths,json_extract(metadata_json,'$.remote_update_status') FROM projects WHERE id=?1",
            [op.source_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        source,
        (
            receipt.revision_id,
            "c".repeat(64),
            "[\"parts/\"]".into(),
            "up_to_date".into()
        )
    );
    let docs: Vec<(String, String, i64)> = db
        .prepare("SELECT path,extract_status,source_revision_id FROM source_docs WHERE project_id=?1 ORDER BY id")
        .unwrap()
        .query_map([op.source_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let id = receipt.revision_id;
    assert_eq!(
        docs,
        [
            ("README.md".into(), "na".into(), id),
            ("guide.pdf".into(), "pending".into(), id)
        ]
    );
    let quota: (i64, i64, bool, bool) = db
        .query_row(
            "SELECT q.reserved_bytes,q.retained_bytes,q.settled,a.activated FROM source_import_quota q, source_revision_attempts a WHERE q.operation_key=?1 AND a.operation_key=?1",
            [&op.key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(quota, (0, 9, true, true));
    assert_eq!(fixture.job(&op).0, "succeeded");
    let fence = Some(
        db.query_row(
            "SELECT attempt_fence FROM source_revision_attempts WHERE operation_key=?1",
            [&op.key],
            |row| row.get::<_, String>(0),
        )
        .unwrap(),
    );
    let observed = |cursor, phase: &str, receipt| {
        let (revision, activated) = receipt;
        (
            cursor,
            phase.into(),
            1,
            fence.clone(),
            revision,
            activated,
            false,
        )
    };
    assert_eq!(
        fixture.observations(&op),
        [
            observed(1, "owned", (None, None)),
            observed(2, "published", (None, None)),
            observed(3, "activated", (Some(id), Some(true))),
            observed(4, "cleanup", (Some(id), Some(true))),
        ]
    );
    drop(db);
    fixture.reopen();
}

#[test]
fn complete_imports_share_one_revision_and_keep_each_attempt() {
    let fixture = Fixture::open();
    let first = admit_files(
        &fixture,
        fixture.physical(),
        "first-complete",
        Target::Create {
            metadata: Box::new(CreateSource {
                name: "shared-revision".into(),
                source_kind: Some("local".into()),
                source_type: Some("local".into()),
                ..Default::default()
            }),
        },
        vec![file("part.stl", "input", 'a')],
    );
    let activated = activate_import(&fixture, &first);
    let revision_id = activated.receipt.unwrap().revision_id;
    let revision_before = revision_evidence(&fixture, revision_id);
    fixture
        .connection()
        .execute(
            "UPDATE projects SET source_configuration_version=source_configuration_version+1 WHERE id=?1",
            [first.source_id],
        )
        .unwrap();
    let second = admit_files(
        &fixture,
        fixture.physical(),
        "second-complete",
        Target::Existing {
            source_id: first.source_id,
        },
        vec![file("other.stl", "input", 'b')],
    );
    let again = activate_import(&fixture, &second);
    let receipt = again.receipt.unwrap();
    assert_eq!(receipt.revision_id, revision_id);
    assert_eq!(revision_evidence(&fixture, revision_id), revision_before);
    let first_binding = first.authority_revision.unwrap();
    let second_binding = second.authority_revision.unwrap();
    assert_ne!(
        (
            first_binding.source_configuration_version,
            first_binding.activation_observation_digest.clone(),
            first_binding.input_digest.clone(),
        ),
        (
            second_binding.source_configuration_version,
            second_binding.activation_observation_digest.clone(),
            second_binding.input_digest.clone(),
        )
    );
    assert_eq!(
        attempt_binding(&fixture, &first.key),
        binding_of(revision_id, &first_binding)
    );
    assert_eq!(
        attempt_binding(&fixture, &second.key),
        binding_of(revision_id, &second_binding)
    );
    assert_eq!(
        revision_binding(&fixture, revision_id),
        binding_of(revision_id, &first_binding)
    );
    let artifacts: i64 = fixture
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM source_revision_artifacts WHERE revision_id=?1",
            [revision_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(artifacts, 2);
}

#[test]
fn an_incomplete_revision_key_is_refused_without_replacing_evidence() {
    let fixture = Fixture::open();
    let host = fixture.admit(fixture.physical(), "incomplete-host");
    let upstream = "c".repeat(64);
    seed_incomplete(&fixture, &host, &upstream);
    let revision_before = revision_evidence_for_key(&fixture, host.source_id, &upstream);
    let attempt_before = attempt_evidence(&fixture, "seeded-incomplete");
    let op = admit_files(
        &fixture,
        fixture.physical(),
        "incomplete-reuse",
        Target::Existing {
            source_id: host.source_id,
        },
        vec![file("part.stl", "input", 'a')],
    );
    let claim = fixture.claim(&op);
    fixture.phase(&claim, owned(&op)).unwrap();
    fixture.phase(&claim, published(&op)).unwrap();
    let before = fixture.snapshot();
    let error = fixture.phase(&claim, Phase::Activate).unwrap_err();
    assert_eq!(
        error.downcast_ref::<IncompleteRevisionKey>(),
        Some(&IncompleteRevisionKey),
        "{error:#}"
    );
    assert_eq!(error.to_string(), "Incomplete revision key");
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(
        revision_evidence_for_key(&fixture, host.source_id, &upstream),
        revision_before
    );
    assert_eq!(
        attempt_evidence(&fixture, "seeded-incomplete"),
        attempt_before
    );
    assert_eq!(
        fixture.phase(&claim, Phase::Read).unwrap().state,
        State::Published
    );
}

#[test]
fn a_complete_revision_with_different_content_keeps_its_evidence() {
    let fixture = Fixture::open();
    let host = fixture.admit(fixture.physical(), "mismatch-host");
    let upstream = "c".repeat(64);
    seed_complete_conflict(&fixture, &host, &upstream);
    let revision_before = revision_evidence_for_key(&fixture, host.source_id, &upstream);
    let attempt_before = attempt_evidence(&fixture, "seeded-complete");
    let op = admit_files(
        &fixture,
        fixture.physical(),
        "mismatch-reuse",
        Target::Existing {
            source_id: host.source_id,
        },
        vec![file("part.stl", "input", 'a')],
    );
    let claim = fixture.claim(&op);
    fixture.phase(&claim, owned(&op)).unwrap();
    fixture.phase(&claim, published(&op)).unwrap();
    let before = fixture.snapshot();
    let error = fixture.phase(&claim, Phase::Activate).unwrap_err();
    assert_eq!(error.to_string(), "Accepted revision identity mismatch");
    assert!(error.downcast_ref::<IncompleteRevisionKey>().is_none());
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(
        revision_evidence_for_key(&fixture, host.source_id, &upstream),
        revision_before
    );
    assert_eq!(
        attempt_evidence(&fixture, "seeded-complete"),
        attempt_before
    );
}

fn binding_of(
    revision_id: i64,
    revision: &SourceAuthorityRevision,
) -> (i64, i64, String, String, String) {
    (
        revision_id,
        revision.source_configuration_version,
        revision.activation_observation_digest.clone(),
        revision.input_digest.clone(),
        revision.producer_version.clone(),
    )
}

fn attempt_binding(fixture: &Fixture, key: &str) -> (i64, i64, String, String, String) {
    fixture
        .connection()
        .query_row(
            "SELECT revision_id,source_configuration_version,activation_observation_digest,input_digest,producer_version FROM source_revision_attempts WHERE operation_key=?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap()
}

fn revision_binding(fixture: &Fixture, id: i64) -> (i64, i64, String, String, String) {
    fixture
        .connection()
        .query_row(
            "SELECT id,source_configuration_version,activation_observation_digest,input_digest,producer_version FROM source_revisions WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .unwrap()
}

fn revision_evidence(fixture: &Fixture, id: i64) -> String {
    fixture
        .connection()
        .query_row(
            "SELECT completeness||':'||manifest_digest||':'||snapshot_locator||':'||synced_at||':'||source_configuration_version||':'||activation_observation_digest||':'||input_digest||':'||producer_version FROM source_revisions WHERE id=?1",
            [id],
            |row| row.get(0),
        )
        .unwrap()
}

fn revision_evidence_for_key(fixture: &Fixture, source_id: i64, key: &str) -> String {
    fixture
        .connection()
        .query_row(
            "SELECT id||':'||completeness||':'||manifest_digest||':'||snapshot_locator||':'||synced_at||':'||source_configuration_version||':'||activation_observation_digest||':'||input_digest||':'||producer_version FROM source_revisions WHERE project_id=?1 AND upstream_revision_key=?2",
            params![source_id, key],
            |row| row.get(0),
        )
        .unwrap()
}

fn attempt_evidence(fixture: &Fixture, key: &str) -> String {
    fixture
        .connection()
        .query_row(
            "SELECT revision_id||':'||source_configuration_version||':'||activation_observation_digest||':'||input_digest||':'||producer_version||':'||attempt_generation||':'||attempt_fence||':'||activated FROM source_revision_attempts WHERE operation_key=?1",
            [key],
            |row| row.get(0),
        )
        .unwrap()
}

fn seed_complete_conflict(fixture: &Fixture, host: &Operation, upstream: &str) {
    seed_revision(fixture, host, upstream, "complete", "seeded-complete");
}

fn seed_incomplete(fixture: &Fixture, host: &Operation, upstream: &str) {
    seed_revision(fixture, host, upstream, "partial", "seeded-incomplete");
}

fn seed_revision(
    fixture: &Fixture,
    host: &Operation,
    upstream: &str,
    completeness: &str,
    operation_key: &str,
) {
    let connection = fixture.connection();
    let tenant: String = connection
        .query_row(
            "SELECT tenant_id FROM projects WHERE id=?1",
            [host.source_id],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute_batch("PRAGMA ignore_check_constraints=ON")
        .unwrap();
    connection
        .execute(
            "INSERT INTO source_revisions(tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at,completeness,source_configuration_version,activation_observation_digest,input_digest,producer_version) VALUES(?1,?2,?3,?4,?5,'2026-01-01T00:00:00.000Z',?6,4,?7,?8,'seeded-producer')",
            params![
                tenant,
                host.source_id,
                upstream,
                "a".repeat(64),
                format!("{}/revisions/{upstream}", host.source_id),
                completeness,
                "b".repeat(64),
                "c".repeat(64),
            ],
        )
        .unwrap();
    let revision_id = connection.last_insert_rowid();
    connection
        .execute(
            "INSERT INTO source_revision_attempts(tenant_id,operation_key,job_id,source_id,revision_id,source_configuration_version,activation_observation_digest,input_digest,producer_version,attempt_generation,attempt_fence,activated,created_at) VALUES(?1,?2,?3,?4,?5,4,?6,?7,'seeded-producer',1,'seed-fence',0,'2026-01-01T00:00:00.000Z')",
            params![
                tenant,
                operation_key,
                "d".repeat(64),
                host.source_id,
                revision_id,
                "b".repeat(64),
                "c".repeat(64),
            ],
        )
        .unwrap();
}

#[test]
fn a_changed_source_conflicts_and_a_cancelled_import_settles_as_cancelled() {
    let fixture = Fixture::open();
    let op = fixture.admit(fixture.physical(), "conflict");
    let lease = fixture.claim(&op);
    fixture.phase(&lease, owned(&op)).unwrap();
    fixture.phase(&lease, published(&op)).unwrap();
    fixture
        .connection()
        .execute(
            "UPDATE projects SET last_commit_sha='moved' WHERE id=?1",
            [op.source_id],
        )
        .unwrap();
    let conflict = fixture.phase(&lease, Phase::Activate).unwrap();
    let receipt = conflict.receipt.clone().unwrap();
    assert_eq!(
        (&conflict.state, receipt.activated, receipt.postprocessing),
        (&State::Conflict, false, Postprocessing::NotActivated)
    );
    assert_eq!(
        fixture.phase(&lease, fail()).unwrap_err().to_string(),
        "Import already completed"
    );
    let settled = fixture.phase(&lease, Phase::Cleanup).unwrap();
    fixture.assert_retry(&lease, Phase::Cleanup, &settled);
    assert_eq!(fixture.job(&op).0, "failed");
    let current: Option<i64> = fixture
        .connection()
        .query_row(
            "SELECT current_source_revision_id FROM projects WHERE id=?1",
            [op.source_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(current, None);

    let op = fixture.admit(fixture.physical(), "cancel");
    let lease = fixture.claim(&op);
    let Outcome::Job(..) = fixture
        .owner
        .jobs(policy())
        .unwrap()
        .submit(
            fixture.physical(),
            UserOperation::Cancel {
                job_id: op.job_id.clone(),
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .receive()
        .unwrap()
    else {
        panic!("job expected")
    };
    assert_eq!(
        fixture.phase(&lease, Phase::Read).unwrap_err().to_string(),
        "Stale attempt fence"
    );
    drop(lease);
    let lease = fixture.claim(&op);
    assert_eq!(
        fixture.phase(&lease, owned(&op)).unwrap_err().to_string(),
        "Import cancelled"
    );
    let failed = fixture.phase(&lease, fail()).unwrap();
    assert_eq!(failed.state, State::Cancelled);
    fixture.assert_retry(&lease, fail(), &failed);
    let settled = fixture.phase(&lease, Phase::Cleanup).unwrap();
    fixture.assert_retry(&lease, Phase::Cleanup, &settled);
    assert_eq!(fixture.job(&op).0, "cancelled");
    let phases: Vec<_> = fixture
        .observations(&op)
        .into_iter()
        .map(|(cursor, phase, generation, .., cancel)| (cursor, phase, generation, cancel))
        .collect();
    assert_eq!(
        phases,
        [
            (1, "failed".into(), 3, true),
            (2, "cleanup".into(), 3, true)
        ]
    );
    fixture.reopen();
}

#[test]
fn generic_claims_leave_admitted_imports_to_their_owner() {
    let fixture = Fixture::open();
    let op = fixture.admit(fixture.physical(), "owner-only");
    assert!(fixture.worker.claim().unwrap().is_none());
    assert!(
        fixture
            .worker
            .claim_kind(JobKind::SuppliedSourceImport)
            .unwrap()
            .is_none()
    );
    let lease = fixture.claim(&op);
    assert_eq!(lease.lease.job_id(), op.job_id);
    fixture.reopen();
}

#[test]
fn a_revoked_owner_fails_the_phase_and_commits_a_source_writer_refusal() {
    let fixture = Fixture::open();
    let token = fixture.register();
    let op = fixture.admit(Credential::Session(Secret::new(token.clone())), "revoked");
    let lease = fixture.claim(&op);
    fixture.phase(&lease, owned(&op)).unwrap();
    fixture.logout(&token);
    let error = fixture.phase(&lease, published(&op)).unwrap_err();
    assert_eq!(
        authority(&error),
        Some(&AuthorityFailure::CredentialInvalid)
    );
    assert_eq!(
        fixture.job(&op),
        ("failed".into(), Some("source_writer".into()), 2)
    );
    let fence: String = fixture
        .connection()
        .query_row(
            "SELECT attempt_fence FROM source_revision_observations WHERE cursor=1 AND operation_key=?1",
            [&op.key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        fixture.observations(&op)[1],
        (
            2,
            "authority_refused".into(),
            1,
            Some(fence),
            None,
            None,
            false
        )
    );
    let stored: (String, i64) = fixture
        .connection()
        .query_row(
            "SELECT state,json_extract(document,'$.observation_cursor') FROM source_import_operations WHERE operation_key=?1",
            [&op.key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, ("owned_input_ready".into(), 2));
    fixture.reopen();
}

#[test]
fn a_targeted_refusal_after_an_attempt_records_its_prior_generation() {
    let fixture = Fixture::open();
    let token = fixture.register();
    let op = fixture.admit(Credential::Session(Secret::new(token.clone())), "prior");
    let lease = fixture.claim(&op);
    fixture.phase(&lease, owned(&op)).unwrap();
    fixture.phase(&lease, published(&op)).unwrap();
    fixture.logout(&token);
    let fixture = fixture.reopen();
    let error = fixture.worker.claim_import(&op.job_id).err().unwrap();
    assert_eq!(
        authority(&error),
        Some(&AuthorityFailure::CredentialInvalid)
    );
    assert_eq!(
        fixture.job(&op),
        ("failed".into(), Some("targeted_claim".into()), 3)
    );
    assert_eq!(
        fixture.observations(&op)[2],
        (3, "authority_refused".into(), 2, None, None, None, false)
    );
    fixture.reopen();
}

#[test]
fn a_revoked_credential_on_generic_recovery_records_one_authority_refusal() {
    let fixture = Fixture::open();
    let token = fixture.register();
    let op = fixture.admit(
        Credential::Session(Secret::new(token.clone())),
        "generic-recovery",
    );
    let lease = fixture.claim(&op);
    fixture.phase(&lease, owned(&op)).unwrap();
    let fixture = fixture.reopen();
    let recovered: i64 = fixture
        .connection()
        .query_row(
            "SELECT generation FROM durable_jobs WHERE id=?1",
            [&op.job_id],
            |row| row.get(0),
        )
        .unwrap();
    fixture.logout(&token);
    assert!(fixture.worker.claim().unwrap().is_none());
    assert_eq!(
        fixture.job(&op),
        ("failed".into(), Some("claim".into()), recovered + 1)
    );
    assert_eq!(fixture.refusal_events(&op), 1);
    let refusal = (
        2,
        "authority_refused".into(),
        recovered,
        None,
        None,
        None,
        false,
    );
    assert_eq!(fixture.observations(&op)[1], refusal);
    assert_eq!(fixture.preclaim_rows(), 0);
    assert!(fixture.worker.claim().unwrap().is_none());
    assert!(
        fixture
            .worker
            .claim_kind(JobKind::SuppliedSourceImport)
            .unwrap()
            .is_none()
    );
    assert_eq!(fixture.refusal_events(&op), 1);
    assert_eq!(fixture.observations(&op)[1], refusal);
    assert_eq!(fixture.observations(&op).len(), 2);
    let fixture = fixture.reopen();
    assert_eq!(fixture.refusal_events(&op), 1);
    assert_eq!(fixture.observations(&op)[1], refusal);
    assert_eq!(fixture.observations(&op).len(), 2);
}

#[test]
fn a_preclaim_refusal_is_journaled_and_checked_at_startup() {
    for (mutation, refused) in [
        ("", false),
        (
            "UPDATE durable_jobs SET document=json_set(document,'$._authority_refusal.phase','claim')",
            true,
        ),
        (
            "UPDATE durable_jobs SET document=json_set(document,'$.cancel_requested',json('true'))",
            true,
        ),
    ] {
        let fixture = Fixture::open();
        let token = fixture.register();
        let op = fixture.admit(Credential::Session(Secret::new(token.clone())), "preclaim");
        fixture.logout(&token);
        let error = fixture.worker.claim_import(&op.job_id).err().unwrap();
        assert_eq!(
            authority(&error),
            Some(&AuthorityFailure::CredentialInvalid)
        );
        assert_eq!(
            fixture.job(&op),
            ("failed".into(), Some("targeted_claim".into()), 1)
        );
        let refusal: (i64, String, bool, i64) = fixture
            .connection()
            .query_row(
                "SELECT p.cursor,p.job_id,p.cancel_requested,json_extract(o.document,'$.observation_cursor') FROM source_preclaim_refusals p JOIN source_import_operations o ON o.tenant=p.tenant_id AND o.operation_key=p.operation_key",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(refusal, (1, op.job_id.clone(), false, 1));
        assert!(fixture.observations(&op).is_empty());
        let Fixture { root, owner, .. } = fixture;
        owner.shutdown().unwrap();
        Connection::open(root.join("print-partner.db"))
            .unwrap()
            .execute_batch(mutation)
            .unwrap();
        match WriterOwner::open(&root, Limits::default()) {
            Ok((owner, _)) => {
                assert!(!refused, "{mutation}");
                owner.shutdown().unwrap();
            }
            Err(error) => assert!(
                refused && format!("{error:#}").contains("Invalid Source preclaim refusal binding"),
                "{mutation}: {error:#}"
            ),
        }
    }
}

/// Revokes the owner's session after `Owned`, so `Published` fails the job and
/// leaves its import holding the whole quota.
fn refused_after_owned(fixture: &Fixture) -> Operation {
    let token = fixture.register();
    let session = Credential::Session(Secret::new(token.clone()));
    let op = fixture.try_admit(session, "refused", QUOTA).unwrap();
    let lease = fixture.claim(&op);
    fixture.phase(&lease, owned(&op)).unwrap();
    fixture.logout(&token);
    let error = fixture.phase(&lease, published(&op)).unwrap_err();
    assert_eq!(
        authority(&error),
        Some(&AuthorityFailure::CredentialInvalid)
    );
    assert_eq!(
        fixture
            .phase(&lease, Phase::Cleanup)
            .unwrap_err()
            .to_string(),
        "Stale attempt fence"
    );
    assert_eq!(fixture.quota(&op), (QUOTA as i64, 0, false));
    op
}

#[test]
fn the_owner_settles_a_refused_import_and_frees_its_quota() {
    let fixture = Fixture::open();
    let op = refused_after_owned(&fixture);
    let token = fixture.login();
    let session = || Credential::Session(Secret::new(token.clone()));
    assert_eq!(
        fixture
            .try_admit(session(), "blocked", RESERVED)
            .unwrap_err()
            .to_string(),
        "Owner import quota exceeded"
    );
    let job = fixture.cancel(&token, &op);
    assert_eq!(job.state, pp_storage::jobs::PersistentState::Failed);
    assert_eq!(fixture.quota(&op), (0, 0, true));
    let settled = fixture.imports.get(session(), op.key.clone()).unwrap();
    assert_eq!(
        (settled.state, settled.cleanup_settled),
        (State::Failed, true)
    );
    assert_eq!(settled.observation_cursor, Some(2));
    let document = fixture.document(&op);
    fixture.cancel(&token, &op);
    assert_eq!(fixture.document(&op), document);
    assert_eq!(fixture.job(&op).0, "failed");
    fixture.try_admit(session(), "next", RESERVED).unwrap();
    fixture.reopen();
}

#[test]
fn startup_settles_a_refused_import() {
    let fixture = Fixture::open();
    let token = fixture.register();
    let session = Credential::Session(Secret::new(token.clone()));
    let op = fixture.try_admit(session, "preclaim", QUOTA).unwrap();
    fixture.logout(&token);
    assert!(fixture.worker.claim_import(&op.job_id).is_err());
    assert_eq!(fixture.quota(&op), (QUOTA as i64, 0, false));
    let fixture = fixture.reopen();
    assert_eq!(fixture.quota(&op), (0, 0, true));
    let stored: (String, bool) = fixture
        .connection()
        .query_row(
            "SELECT state,json_extract(document,'$.cleanup_settled') FROM source_import_operations WHERE operation_key=?1",
            [&op.key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, ("failed".into(), true));
    fixture
        .try_admit(fixture.physical(), "next", RESERVED)
        .unwrap();
    fixture.reopen();
}

#[test]
fn a_source_with_an_unsettled_import_is_not_deleted() {
    let fixture = Fixture::open();
    let op = refused_after_owned(&fixture);
    let token = fixture.login();
    assert_eq!(fixture.delete_source(&token, &op), Deletion::ActiveWork);
    fixture.cancel(&token, &op);
    assert_eq!(
        fixture.delete_source(&token, &op),
        Deletion::Deleted {
            source_id: op.source_id
        }
    );
    fixture.reopen();
}

/// Deletes each import's Source while storage is closed, as deletion allowed
/// before it waited for imports, then reopens storage.
fn reopen_without_sources(fixture: Fixture, ops: &[&Operation]) -> Fixture {
    let Fixture { root, owner, .. } = fixture;
    owner.shutdown().unwrap();
    let connection = Connection::open(root.join("print-partner.db")).unwrap();
    for op in ops {
        connection
            .execute("DELETE FROM projects WHERE id=?1", [op.source_id])
            .unwrap();
    }
    drop(connection);
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    Fixture::with(root, owner)
}

/// Each import's journal document and job row, which a repeated startup must
/// leave unchanged.
fn stored(fixture: &Fixture, ops: &[&Operation]) -> Vec<(String, String, i64)> {
    ops.iter()
        .map(|op| {
            fixture
                .connection()
                .query_row(
                    "SELECT o.document,j.document,j.version FROM source_import_operations o JOIN durable_jobs j ON j.id=o.job_id WHERE o.operation_key=?1",
                    [&op.key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap()
        })
        .collect()
}

#[test]
fn startup_settles_a_refused_import_without_its_source() {
    let fixture = Fixture::open();
    let op = refused_after_owned(&fixture);
    let fixture = reopen_without_sources(fixture, &[&op]);
    assert_eq!(fixture.quota(&op), (0, 0, true));
    assert_eq!(fixture.job(&op).0, "failed");
    let token = fixture.login();
    let session = || Credential::Session(Secret::new(token.clone()));
    let settled = fixture.imports.get(session(), op.key.clone()).unwrap();
    assert_eq!(
        (settled.state, settled.cleanup_settled),
        (State::Failed, true)
    );
    let before = stored(&fixture, &[&op]);
    let fixture = fixture.reopen();
    assert_eq!(stored(&fixture, &[&op]), before);
    fixture.try_admit(session(), "next", QUOTA).unwrap();
}

#[test]
fn startup_fails_a_queued_import_without_its_source() {
    let fixture = Fixture::open();
    let token = fixture.register();
    let session = || Credential::Session(Secret::new(token.clone()));
    let queued = fixture.admit(session(), "queued");
    let crashed = fixture.admit(session(), "crashed");
    let lease = fixture.claim(&crashed);
    fixture.phase(&lease, owned(&crashed)).unwrap();
    let fixture = reopen_without_sources(fixture, &[&queued, &crashed]);
    for op in [&queued, &crashed] {
        assert_eq!(fixture.quota(op), (0, 0, true));
        assert_eq!(fixture.job(op).0, "failed");
        let (recovery, event): (String, String) = fixture
            .connection()
            .query_row(
                "SELECT json_extract(document,'$.recovery'),(SELECT event FROM durable_job_history WHERE job_id=?1 ORDER BY version DESC LIMIT 1) FROM durable_jobs WHERE id=?1",
                [&op.job_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            (recovery.as_str(), event.as_str()),
            ("Source is unavailable for this job", "source_unavailable")
        );
        let settled = fixture.imports.get(session(), op.key.clone()).unwrap();
        assert_eq!(
            (settled.state, settled.cleanup_settled),
            (State::Failed, true)
        );
    }
    let before = stored(&fixture, &[&queued, &crashed]);
    let fixture = fixture.reopen();
    assert_eq!(stored(&fixture, &[&queued, &crashed]), before);
    fixture.try_admit(session(), "next", QUOTA).unwrap();
}

#[test]
fn postprocessing_keeps_its_durable_wire_values() {
    for (state, wire) in [
        (Postprocessing::NotActivated, "not_activated"),
        (
            Postprocessing::DocumentMetadataIndexed,
            "document_metadata_indexed",
        ),
        (
            Postprocessing::DocumentMetadataIndexedPdfPending,
            "document_metadata_indexed_pdf_pending",
        ),
        (Postprocessing::IndexError, "index_error"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), wire);
        assert_eq!(
            serde_json::from_value::<Postprocessing>(wire.into()).unwrap(),
            state
        );
    }
    for wire in ["complete", "unknown", "", "DocumentMetadataIndexed"] {
        assert!(serde_json::from_value::<Postprocessing>(wire.into()).is_err());
    }
}

#[test]
fn a_released_import_source_lease_cannot_advance_under_a_replacement() {
    let fixture = Fixture::open();
    let op = fixture.admit(fixture.physical(), "released-source-fence");
    let mut claim = fixture.claim(&op);
    claim.source_work.as_mut().unwrap().release().unwrap();
    let catalog = fixture.owner.local_source_catalog();
    let replacement = catalog
        .begin_work(
            op.source_id,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    for phase in [
        Phase::Read,
        owned(&op),
        published(&op),
        Phase::Activate,
        fail(),
        Phase::Cleanup,
    ] {
        fixture.rejected_phase(
            &claim,
            claim.source_work.as_ref().unwrap(),
            phase,
            "Lease already released",
        );
    }
    assert_eq!(
        fixture
            .worker
            .import_phase(&claim.lease, &replacement, owned(&op))
            .unwrap()
            .state,
        State::OwnedInputReady
    );
}

#[test]
fn phases_reject_another_source_or_tenant_without_writes() {
    for wrong_tenant in [false, true] {
        let fixture = Fixture::open();
        let other = fixture.admit(fixture.physical(), "other-source");
        let credential = if wrong_tenant {
            Credential::Session(Secret::new(fixture.register()))
        } else {
            fixture.physical()
        };
        let op = fixture.admit(credential, "bound-source");
        assert_eq!(op.tenant != other.tenant, wrong_tenant);
        let claim = fixture.claim(&op);
        let wrong = fixture
            .owner
            .local_source_catalog()
            .begin_work(
                other.source_id,
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap();
        fixture.rejected_phase(&claim, &wrong, fail(), "Wrong Source reservation");
        for phase in [
            Phase::Read,
            owned(&op),
            published(&op),
            Phase::Activate,
            Phase::Cleanup,
        ] {
            fixture.rejected_phase(&claim, &wrong, phase.clone(), "Wrong Source reservation");
            fixture.phase(&claim, phase).unwrap();
        }
        fixture.rejected_phase(&claim, &wrong, Phase::Cleanup, "Wrong Source reservation");
        assert_eq!(fixture.job(&op).0, "succeeded");
        assert_eq!(fixture.quota(&op), (0, 9, true));
        fixture.reopen();
    }
}

#[test]
fn a_wrong_source_cannot_commit_an_authority_refusal() {
    let fixture = Fixture::open();
    let token = fixture.register();
    let op = fixture.admit(
        Credential::Session(Secret::new(token.clone())),
        "revoked-fence",
    );
    let claim = fixture.claim(&op);
    let other = fixture.admit(fixture.physical(), "other-source");
    let wrong = fixture
        .owner
        .local_source_catalog()
        .begin_work(
            other.source_id,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    fixture.logout(&token);
    for phase in [
        Phase::Read,
        owned(&op),
        published(&op),
        Phase::Activate,
        fail(),
        Phase::Cleanup,
    ] {
        fixture.rejected_phase(&claim, &wrong, phase, "Wrong Source reservation");
    }
    let error = fixture.phase(&claim, Phase::Read).unwrap_err();
    assert_eq!(
        authority(&error),
        Some(&AuthorityFailure::CredentialInvalid)
    );
    assert_eq!(fixture.job(&op).1.as_deref(), Some("source_writer"));
}

#[test]
fn a_previous_writer_source_proof_cannot_advance_a_fresh_attempt() {
    let fixture = Fixture::open();
    let op = fixture.admit(fixture.physical(), "old-incarnation");
    let old = fixture.claim(&op);
    let fixture = fixture.reopen();
    let claim = fixture.claim(&op);
    let old_source = old.source_work.as_ref().unwrap();
    fixture.rejected_phase(
        &claim,
        old_source,
        fail(),
        "Wrong Source writer incarnation",
    );
    for phase in [
        Phase::Read,
        owned(&op),
        published(&op),
        Phase::Activate,
        Phase::Cleanup,
    ] {
        fixture.rejected_phase(
            &claim,
            old_source,
            phase.clone(),
            "Wrong Source writer incarnation",
        );
        fixture.phase(&claim, phase).unwrap();
    }
    assert_eq!(fixture.job(&op).0, "succeeded");
    fixture.reopen();
}

#[test]
fn an_import_source_lease_blocks_sync_until_released() {
    let fixture = Fixture::open_with_admission(2);
    let token = fixture.register();
    let op = fixture.admit(
        Credential::Session(Secret::new(token.clone())),
        "sync-exclusion",
    );
    let mut claim = fixture.claim(&op);
    let Outcome::Job(sync, _) = fixture
        .owner
        .jobs(policy())
        .unwrap()
        .submit(
            Credential::Session(Secret::new(token)),
            UserOperation::Enqueue {
                key: "competing-sync".into(),
                payload_version: 1,
                payload: pp_storage::jobs::Payload::Sync {
                    project_ids: Some(vec![op.source_id as u64]),
                },
            },
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap()
        .receive()
        .unwrap()
    else {
        panic!("Sync expected")
    };
    let mut sync_claim = fixture.worker.claim_kind(JobKind::Sync).unwrap().unwrap();
    assert_eq!(sync_claim.job.job_id, sync.job_id);
    fixture.worker.open_source_sync(&sync_claim.lease).unwrap();
    for settled in [false, true] {
        if settled {
            for phase in [owned(&op), published(&op), Phase::Activate, Phase::Cleanup] {
                fixture.phase(&claim, phase).unwrap();
            }
            assert_eq!(fixture.job(&op).0, "succeeded");
        }
        let before = fixture.snapshot();
        let error = fixture
            .worker
            .claim_next_source_sync_target(&mut sync_claim.lease)
            .err()
            .unwrap();
        assert!(
            error
                .downcast_ref::<pp_storage::catalog::SourceBusy>()
                .is_some(),
            "{error:#}"
        );
        assert_eq!(fixture.snapshot(), before);
    }
    claim.source_work.as_mut().unwrap().release().unwrap();
    let pp_storage::source_sync::NextSyncTarget::Local(target) = fixture
        .worker
        .claim_next_source_sync_target(&mut sync_claim.lease)
        .unwrap()
    else {
        panic!("Local target expected")
    };
    assert_eq!(target.observation().source_id(), op.source_id);
    fixture.rejected_phase(
        &claim,
        claim.source_work.as_ref().unwrap(),
        owned(&op),
        "Lease already released",
    );
}
