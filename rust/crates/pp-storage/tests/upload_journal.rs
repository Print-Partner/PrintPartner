#![cfg(feature = "desktop_rebuild")]
//! Upload journal admission and its startup checks. The file count, size and path
//! cases compare with Node's `writeUploadedFiles` (`fixtures/parity/uploads`).
//! Node has no journal, quota or idempotency, so those cases and the codec
//! fixture port the reference `source_import_codec.rs`.
//! A targeted claim of a legacy entry (no authority revision) writes no Source
//! observation. A revisioned entry still records its refusal.
//! `CARGO_BUILD_JOBS=3 cargo test --manifest-path rust/Cargo.toml --locked -p pp-storage --features desktop_rebuild --test upload_journal legacy_targeted_refusal_writes_no_revisioned_observation`
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, AuthPolicy, FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    catalog::CreateSource,
    jobs::{Credential, JobKind, Payload, WorkerAdmission},
    uploads::{
        Admission, Basis, File, ImportClient, Input, MissingPreclaimRefusal, Operation, State,
        Target,
    },
};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "common/scratch.rs"]
mod scratch;

const RESERVED: u64 = 3_506_438_144;
const MAX_INPUT: u64 = 268_435_456;
const MAX_PREPARED: u64 = 1_073_741_824;
const QUOTA: u64 = 16 * 1024 * 1024 * 1024;

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

// Fields drop in declaration order: `root` must stay last so the owner closes
// before the scratch directory is removed.
struct Fixture {
    owner: WriterOwner,
    client: ImportClient,
    root: scratch::Scratch,
}

impl Fixture {
    fn open() -> Self {
        let root = scratch::Scratch::new("pp-upload-journal-");
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        let client = owner.imports(policy(), QUOTA).unwrap();
        Self {
            root,
            owner,
            client,
        }
    }

    fn physical(&self) -> Credential {
        Credential::PhysicalOwner(self.owner.job_physical_owner())
    }

    fn admit(&self, request: Admission, files: Vec<File>, disk: u64) -> anyhow::Result<Operation> {
        let epoch = self.client.accounting_epoch();
        let cancelled = AtomicBool::new(false);
        self.client
            .admit(self.physical(), request, files, disk, epoch, &cancelled)
    }

    fn connection(&self) -> Connection {
        Connection::open(self.root.join("print-partner.db")).unwrap()
    }

    fn rows(&self) -> Vec<i64> {
        let tables = [
            "projects",
            "source_import_operations",
            "source_import_quota",
            "durable_jobs",
        ];
        let connection = self.connection();
        let count = |table| format!("SELECT count(*) FROM {table}");
        tables
            .map(|table| {
                connection
                    .query_row(&count(table), [], |row| row.get(0))
                    .unwrap()
            })
            .to_vec()
    }

    /// Returns the refusal and checks that it wrote nothing.
    fn refused(&self, request: Admission, files: Vec<File>, disk: u64) -> String {
        let before = self.rows();
        let error = self.admit(request, files, disk).unwrap_err().to_string();
        assert_eq!(self.rows(), before, "{error}");
        error
    }
}

fn create(name: &str) -> Target {
    let source = CreateSource {
        name: name.into(),
        source_kind: Some("local".into()),
        source_type: Some("local".into()),
        ..Default::default()
    };
    Target::Create {
        metadata: Box::new(source),
    }
}

fn file(path: &str, size: u64) -> File {
    File {
        path: path.into(),
        size,
        sha256: "b".repeat(64),
        kind: "input".into(),
    }
}

fn admission(key: &str, target: Target, files: &[File]) -> Admission {
    let paths = files.iter().map(|file| file.path.clone()).collect();
    Admission {
        key: key.into(),
        target,
        input: Input::Files { paths },
        reserved_bytes: RESERVED,
        max_input_bytes: MAX_INPUT,
        max_prepared_bytes: MAX_PREPARED,
    }
}

/// A request that creates a Source named after its key, for one 3-byte file.
fn upload(key: &str) -> (Admission, Vec<File>) {
    let files = vec![file("part.stl", 3)];
    (admission(key, create(key), &files), files)
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn session(owner: &WriterOwner) -> Credential {
    let request = auth::Request::Register {
        email: "other@example.test".into(),
        display_name: "Other".into(),
        password: Secret::new("long-test-password".into()),
    };
    let reply = owner
        .auth_with_policy(policy())
        .unwrap()
        .submit(
            request,
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    let Ok(auth::Outcome::Session { token, .. }) = reply.recv().unwrap() else {
        panic!("session expected")
    };
    Credential::Session(Secret::new(token.expose().into()))
}

#[test]
fn admission_creates_the_source_journal_and_internal_job_once() {
    let fixture = Fixture::open();
    let before = fixture.rows();
    let (request, files) = upload("first");
    assert_eq!(fixture.client.accounting_epoch(), 0);
    let operation = fixture.admit(request, files.clone(), 0).unwrap();
    assert_eq!(fixture.client.accounting_epoch(), 1);
    let revision = operation.authority_revision.as_ref().unwrap();
    let digest = hash(&serde_json::to_vec(&files).unwrap());
    assert_eq!(
        (
            &operation.state,
            operation.observation_cursor,
            &revision.input_digest
        ),
        (&State::Admitted, Some(0), &digest)
    );
    let after = fixture.rows();
    assert_eq!(after, [before[0] + 1, 1, 1, before[3] + 1]);
    let stored: String = fixture
        .connection()
        .query_row(
            "SELECT j.kind||' '||j.state||' '||q.reserved_bytes||' '||q.retained_bytes||' '||q.settled FROM durable_jobs j, source_import_quota q WHERE j.id=?1",
            [&operation.job_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        stored,
        format!("supplied-source-import queued {RESERVED} 0 0")
    );

    let get = |credential, key: &str| fixture.client.get(credential, key.into());
    assert_eq!(get(fixture.physical(), "first").unwrap(), operation);
    assert_eq!(fixture.client.accounting_epoch(), 1);
    assert_eq!(
        fixture.admit(upload("first").0, files.clone(), 0).unwrap(),
        operation
    );
    let renamed = admission("first", create("Renamed"), &files);
    assert_eq!(
        fixture.refused(renamed, files, 0),
        "Import idempotency conflict"
    );
    assert_eq!(fixture.rows(), after);
    for (credential, key) in [
        (fixture.physical(), "missing"),
        (session(&fixture.owner), "first"),
    ] {
        assert_eq!(
            get(credential, key).unwrap_err().to_string(),
            "Import not found"
        );
    }
    let (request, files) = upload("stale");
    let cancelled = AtomicBool::new(false);
    let stale = fixture
        .client
        .admit(fixture.physical(), request, files, 0, 0, &cancelled)
        .unwrap_err();
    assert_eq!(
        stale.to_string(),
        "Import accounting changed; retry admission"
    );
}

#[test]
fn admission_refuses_quota_and_limits_without_writing() {
    let fixture = Fixture::open();
    let limited = |reserved, max_input, max_prepared| {
        let (request, files) = upload("quota");
        let request = Admission {
            reserved_bytes: reserved,
            max_input_bytes: max_input,
            max_prepared_bytes: max_prepared,
            ..request
        };
        (request, files)
    };
    for ((request, files), disk, message) in [
        (
            limited(RESERVED - 1, MAX_INPUT, MAX_PREPARED),
            0,
            "Import quota exceeded",
        ),
        (
            limited(QUOTA + 1, MAX_INPUT, MAX_PREPARED),
            0,
            "Import quota exceeded",
        ),
        (
            limited(RESERVED, MAX_INPUT + 1, MAX_PREPARED),
            0,
            "Import quota exceeded",
        ),
        (limited(RESERVED, MAX_INPUT, 0), 0, "Invalid import limits"),
        (limited(RESERVED, 2, MAX_PREPARED), 0, "Input limit"),
        (
            upload("quota"),
            QUOTA - RESERVED + 1,
            "Owner import quota exceeded",
        ),
    ] {
        assert_eq!(fixture.refused(request, files, disk), message);
    }

    let (request, files) = upload("first");
    fixture.admit(request, files, 0).unwrap();
    let (request, files) = upload("second");
    let pending = fixture.refused(request, files, QUOTA - 2 * RESERVED + 1);
    assert_eq!(pending, "Owner import quota exceeded");
    let (request, files) = upload("second");
    fixture.admit(request, files, QUOTA - 2 * RESERVED).unwrap();

    let max_input = 300 * 1024 * 1024;
    let reserved = max_input + 3 * MAX_PREPARED + 16 * 1024 * 1024;
    let (request, files) = upload("above-old-caps");
    let request = Admission {
        reserved_bytes: reserved,
        max_input_bytes: max_input,
        max_prepared_bytes: MAX_PREPARED,
        ..request
    };
    fixture.admit(request, files, 0).unwrap();
}

#[test]
fn admission_refuses_unbound_inputs_and_targets_without_writing() {
    let fixture = Fixture::open();
    let (request, files) = upload("existing");
    let created = fixture.admit(request, files.clone(), 0).unwrap();
    let with = |change: fn(&mut File)| {
        let mut changed = file("part.stl", 3);
        change(&mut changed);
        vec![changed]
    };
    let zip = || Input::Zip {
        path: "upload.zip".into(),
    };
    let missing = Target::Existing { source_id: 999 };
    for (request, files, message) in [
        (upload("").0, files.clone(), "Invalid operation key"),
        (
            upload(&"k".repeat(129)).0,
            files.clone(),
            "Invalid operation key",
        ),
        (upload("key\n").0, files.clone(), "Invalid operation key"),
        (
            upload("digest").0,
            with(|file| file.sha256 = "B".repeat(64)),
            "Invalid digest",
        ),
        (
            upload("typed").0,
            with(|file| file.kind = "stl".into()),
            "Input binding mismatch",
        ),
        (
            upload("paths").0,
            with(|file| file.path = "other.stl".into()),
            "Input binding mismatch",
        ),
        (
            Admission {
                input: zip(),
                ..upload("zip").0
            },
            files.clone(),
            "Input binding mismatch",
        ),
        (
            admission("missing", missing, &files),
            files.clone(),
            "Source not found",
        ),
        (
            admission("named", create("existing"), &files),
            files.clone(),
            "Source already exists: existing",
        ),
    ] {
        assert_eq!(fixture.refused(request, files, 0), message);
    }

    let target = Target::Existing {
        source_id: created.source_id,
    };
    let zipped = vec![file("upload.zip", 3)];
    let request = Admission {
        input: zip(),
        ..admission("zip", target, &zipped)
    };
    let existing = fixture.admit(request, zipped, 0).unwrap();
    assert_eq!(
        (existing.source_id, existing.input),
        (created.source_id, zip())
    );
}

#[test]
fn node_upload_limits_match_the_journal_boundaries() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../../fixtures/parity/uploads/uploads.json"
    ))
    .unwrap();
    let limits = json!({"max_source_upload_bytes": MAX_INPUT, "max_source_upload_files": 10000});
    assert_eq!(fixture["limits"], limits);
    let journal = Fixture::open();
    // Where Node writes files, the journal admits the same canonical paths.
    let refused = [
        ("empty", "At least one file is required", "Invalid files"),
        (
            "too_many",
            "Archive has too many entries (10001, max 10000)",
            "Invalid files",
        ),
        (
            "over_limit",
            "Uploaded source exceeds the 256 MiB upload limit",
            "Input limit",
        ),
        (
            "parent",
            "File path escapes extraction directory: ../part.stl",
            "Invalid relative path",
        ),
        (
            "nested_parent",
            "File path escapes extraction directory: parts/../a.stl",
            "Invalid relative path",
        ),
        (
            "disguised_parent",
            "File path escapes extraction directory: ./parts//../a.stl",
            "Invalid relative path",
        ),
    ];
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 22);
    for case in cases {
        let id = case["id"].as_str().unwrap();
        let node = match &case["result"]["error"] {
            Value::String(error) => Err(error.as_str()),
            _ => {
                Ok(serde_json::from_value::<Vec<String>>(case["result"]["paths"].clone()).unwrap())
            }
        };
        let files: Vec<File> = match &case["files"] {
            Value::Array(files) => files
                .iter()
                .map(|entry| {
                    file(
                        entry["path"].as_str().unwrap(),
                        entry["size"].as_u64().unwrap(),
                    )
                })
                .collect(),
            counted => (0..counted["count"].as_u64().unwrap())
                .map(|index| file(&format!("part-{index}.stl"), 1))
                .collect(),
        };
        // The smallest reservation lets every admitted case fit one quota.
        let request = Admission {
            max_prepared_bytes: 1,
            reserved_bytes: MAX_INPUT + 3 + 16 * 1024 * 1024,
            ..admission(id, create(id), &files)
        };
        let before = journal.rows();
        let result = journal.admit(request, files, 0);
        match (node, result) {
            (Ok(written), Ok(operation)) => {
                let Input::Files { paths } = &operation.input else {
                    panic!("{id}: files expected")
                };
                let mut admitted: Vec<String> = operation
                    .requested_files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect();
                assert_eq!(paths, &admitted, "{id}");
                assert_eq!(
                    operation.requested_digest, case["result"]["requested_digest"],
                    "{id}"
                );
                admitted.sort();
                assert_eq!(admitted, written, "{id}");
            }
            (Err(node), Err(error)) => {
                assert_eq!(journal.rows(), before, "{id}");
                let journal = error.to_string();
                if node.starts_with("Duplicate uploaded file path: ") {
                    assert_eq!(journal, node, "{id}");
                } else {
                    assert!(
                        refused.contains(&(id, node, &journal)),
                        "{id}: {node} / {journal}"
                    );
                }
            }
            (node, result) => panic!("{id}: Node {node:?}, journal {:?}", result.map(drop)),
        }
    }
}

#[test]
fn admission_hashes_the_canonical_paths() {
    let journal = Fixture::open();
    let supplied = vec![file("./\\Parts//./a.stl", 3)];
    let request = admission("canonical", create("canonical"), &supplied);
    let first = journal.admit(request, supplied, 0).unwrap();
    let canonical = vec![file("Parts/a.stl", 3)];
    let digest = hash(&serde_json::to_vec(&canonical).unwrap());
    assert_eq!(first.requested_digest, digest);
    let rows = journal.rows();
    for spelling in [
        "Parts/a.stl",
        "./Parts/a.stl",
        "Parts//a.stl",
        "Parts/./a.stl",
    ] {
        let files = vec![file(spelling, 3)];
        let request = admission("canonical", create("canonical"), &files);
        assert_eq!(journal.admit(request, files, 0).unwrap(), first);
        assert_eq!(journal.rows(), rows);
    }
}

#[test]
fn admission_retries_zip_aliases_with_the_same_intent() {
    let journal = Fixture::open();
    let files = vec![file("./\\parts//./upload.zip", 3)];
    let request = Admission {
        input: Input::Zip {
            path: "./\\parts//./upload.zip".into(),
        },
        ..admission("zip-alias", create("zip-alias"), &files)
    };
    let first = journal.admit(request, files, 0).unwrap();
    assert_eq!(
        first.input,
        Input::Zip {
            path: "parts/upload.zip".into()
        }
    );
    assert_eq!(first.requested_files, vec![file("parts/upload.zip", 3)]);
    let rows = journal.rows();
    for spelling in [
        "parts/upload.zip",
        "./parts/upload.zip",
        "parts//upload.zip",
        "parts/./upload.zip",
    ] {
        let files = vec![file(spelling, 3)];
        let request = Admission {
            input: Input::Zip {
                path: spelling.into(),
            },
            ..admission("zip-alias", create("zip-alias"), &files)
        };
        assert_eq!(journal.admit(request, files, 0).unwrap(), first);
        assert_eq!(journal.rows(), rows);
    }
}

#[test]
fn imports_requires_one_valid_owner_quota() {
    let root = scratch::Scratch::new("pp-upload-quota-");
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    let refusal = |quota| owner.imports(policy(), quota).err().unwrap().to_string();
    assert_eq!(refusal(0), "Invalid desktop quota");
    let large = 64 * 1024 * 1024 * 1024 + 1;
    owner.imports(policy(), large).unwrap();
    owner.imports(policy(), large).unwrap();
    assert_eq!(refusal(QUOTA), "Import quota already configured");
}

#[test]
fn startup_checks_journal_documents_against_their_jobs() {
    for (mutation, message) in [
        (
            "UPDATE source_import_operations SET document=json_set(document,'$.input_version',2)",
            "Invalid import input version",
        ),
        (
            "UPDATE durable_jobs SET document=json_set(document,'$.payload.payload.input_version',2)",
            "Import job binding mismatch",
        ),
        (
            "UPDATE durable_jobs SET document=json_set(document,'$.payload.payload.operation_key','other')",
            "Import job binding mismatch",
        ),
        (
            "UPDATE source_import_operations SET state='failed'",
            "Corrupt Source import journal",
        ),
        (
            "UPDATE source_import_quota SET reserved_bytes=1",
            "Corrupt Source quota journal",
        ),
        (
            "UPDATE source_import_quota SET settled=1",
            "Corrupt Source quota journal",
        ),
    ] {
        let journal = Fixture::open();
        let (request, files) = upload("startup");
        journal.admit(request, files, 0).unwrap();
        let Fixture {
            root,
            owner,
            client,
        } = journal;
        drop(client);
        owner.shutdown().unwrap();
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        owner.shutdown().unwrap();
        let database = Connection::open(root.join("print-partner.db")).unwrap();
        database.execute_batch(mutation).unwrap();
        let error = WriterOwner::open(&root, Limits::default()).err().unwrap();
        assert!(
            format!("{error:#}").contains(message),
            "{mutation}: {error:#}"
        );
    }
}

fn encoded<T: Serialize>(value: &T) -> Value {
    let bytes = serde_json::to_vec(value).unwrap();
    json!({"utf8": String::from_utf8(bytes.clone()).unwrap(), "sha256": hash(&bytes)})
}

fn legacy_case(key: &str, input: Input, path: &str, byte: char) -> Value {
    let files = vec![File {
        sha256: byte.to_string().repeat(64),
        ..file(path, 3)
    }];
    let admission = Admission {
        input: input.clone(),
        ..admission(key, Target::Existing { source_id: 42 }, &files)
    };
    let requested = serde_json::to_vec(&files).unwrap();
    let intent = serde_json::to_vec(&(&admission, &files)).unwrap();
    let basis = Basis {
        current_source_revision_id: None,
        url: String::new(),
        branch: "main".into(),
        tag: None,
        source_kind: "local".into(),
        source_type: "local".into(),
        local_path: None,
        last_commit_sha: None,
        legacy_manifest_cutover: false,
    };
    let operation = Operation {
        tenant: "default".into(),
        actor: "physical-owner".into(),
        key: key.into(),
        intent_digest: hash(&intent),
        source_id: 42,
        job_id: "c".repeat(64),
        input_version: 1,
        input: input.clone(),
        requested_files: files,
        requested_digest: hash(&requested),
        state: State::Admitted,
        basis,
        default_rules: true,
        original_rules: None,
        reserved_bytes: RESERVED,
        max_input_bytes: MAX_INPUT,
        max_prepared_bytes: MAX_PREPARED,
        owned: None,
        artifact: None,
        receipt: None,
        cleanup_settled: false,
        authority_revision: None,
        observation_cursor: None,
        commit_sha: None,
        revision_key: None,
    };
    let payload = Payload::SuppliedSourceImport {
        project_id: 42,
        operation_key: key.into(),
        input_version: 1,
    };
    json!({
        "input": encoded(&input),
        "admission": encoded(&admission),
        "operation": encoded(&operation),
        "supplied_job_payload": encoded(&payload),
        "requested": {"utf8": String::from_utf8(requested.clone()).unwrap(), "sha256": hash(&requested)},
        "intent": {"utf8": String::from_utf8(intent.clone()).unwrap(), "sha256": hash(&intent)},
        "job_intent": encoded(&(1u32, &payload)),
    })
}

#[test]
fn legacy_files_and_zip_match_the_detached_prior_serializer() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/source-import-v1-serde.json")).unwrap();
    let cases = &fixture["legacy"]["cases"];
    assert_eq!(
        fixture["baseline_commit"],
        "2f24c9b42ba637bb13138837356099b32f5704aa"
    );
    let paths = vec!["part.stl".into()];
    let files = legacy_case("legacy-files-1", Input::Files { paths }, "part.stl", 'b');
    assert_eq!(cases["files"], files);
    let path = "upload.zip".to_owned();
    let zip = legacy_case("legacy-zip-1", Input::Zip { path }, "upload.zip", 'a');
    assert_eq!(cases["zip"], zip);
    for field in ["files", "zip"]
        .iter()
        .flat_map(|case| cases[case].as_object().unwrap().values())
    {
        assert_eq!(
            hash(field["utf8"].as_str().unwrap().as_bytes()),
            field["sha256"]
        );
    }
}

/// Every journal, quota, preclaim and job row, which a repeated startup must
/// leave unchanged.
fn journal_rows(root: &std::path::Path) -> Vec<Vec<String>> {
    let connection = Connection::open(root.join("print-partner.db")).unwrap();
    [
        "source_import_operations",
        "source_import_quota",
        "source_preclaim_refusals",
        "source_revision_observations",
        "durable_jobs",
        "durable_job_keys",
    ]
    .into_iter()
    .map(|table| {
        let mut statement = connection
            .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
            .unwrap();
        let width = statement.column_count();
        statement
            .query_map([], |row| {
                (0..width)
                    .map(|index| {
                        Ok(format!(
                            "{:?}",
                            row.get::<_, rusqlite::types::Value>(index)?
                        ))
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map(|row| row.join("|"))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    })
    .collect()
}

/// Removes the only preclaim row past its immutability trigger, then restores the
/// trigger so the schema stays intact.
fn delete_preclaim(root: &std::path::Path) {
    let connection = Connection::open(root.join("print-partner.db")).unwrap();
    let trigger: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name='trg_source_preclaim_refusals_immutable_delete'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    connection
        .execute_batch("DROP TRIGGER trg_source_preclaim_refusals_immutable_delete")
        .unwrap();
    let deleted = connection
        .execute("DELETE FROM source_preclaim_refusals", [])
        .unwrap();
    assert_eq!(deleted, 1);
    connection.execute_batch(&trigger).unwrap();
}

/// Admits an import with an owner session, revokes it, and lets a targeted claim
/// refuse the job before its first attempt, which writes the only preclaim row.
fn preclaim_refused() -> scratch::Scratch {
    let Fixture {
        root,
        owner,
        client,
    } = Fixture::open();
    let credential = session(&owner);
    let Credential::Session(token) = &credential else {
        unreachable!()
    };
    let token = Secret::new(token.expose().into());
    let (request, files) = upload("preclaim");
    let epoch = client.accounting_epoch();
    let op = client
        .admit(
            credential,
            request,
            files,
            0,
            epoch,
            &AtomicBool::new(false),
        )
        .unwrap();
    owner
        .auth_with_policy(policy())
        .unwrap()
        .submit(
            auth::Request::Logout { token },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    let admission = WorkerAdmission {
        kinds: JobKind::ALL.into_iter().map(|kind| (kind, 2)).collect(),
        total: 4,
        per_resource: 1,
        lease_seconds: 60,
    };
    let worker = owner.job_worker_with_policy(policy(), admission).unwrap();
    assert!(worker.claim_import(&op.job_id).is_err());
    let (cursor, phase): (i64, String) = Connection::open(root.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT p.cursor,json_extract(j.document,'$._authority_refusal.phase') FROM source_preclaim_refusals p JOIN durable_jobs j ON j.id=p.job_id",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((cursor, phase.as_str()), (1, "targeted_claim"));
    drop((worker, client));
    owner.shutdown().unwrap();
    root
}

fn refused_open(root: &std::path::Path) {
    let error = WriterOwner::open(root, Limits::default()).err().unwrap();
    assert_eq!(
        error
            .chain()
            .find_map(|cause| cause.downcast_ref::<MissingPreclaimRefusal>()),
        Some(&MissingPreclaimRefusal),
        "{error:#}"
    );
}

#[test]
fn startup_refuses_an_import_missing_its_preclaim_refusal() {
    // Before startup settles the refused entry.
    let root = preclaim_refused();
    delete_preclaim(&root);
    let missing = journal_rows(&root);
    refused_open(&root);
    assert_eq!(journal_rows(&root), missing);

    // After startup settles it, valid records reopen unchanged.
    let root = preclaim_refused();
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    let stored: (String, bool) = Connection::open(root.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT state,json_extract(document,'$.cleanup_settled') FROM source_import_operations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(stored, ("failed".into(), true));
    let settled = journal_rows(&root);
    for _ in 0..2 {
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        owner.shutdown().unwrap();
        assert_eq!(journal_rows(&root), settled);
    }
    delete_preclaim(&root);
    let missing = journal_rows(&root);
    refused_open(&root);
    assert_eq!(journal_rows(&root), missing);

    // A refusal recorded before the preclaim row existed left the cursor at 0
    // and still opens.
    let root = preclaim_refused();
    delete_preclaim(&root);
    Connection::open(root.join("print-partner.db"))
        .unwrap()
        .execute_batch(
            "UPDATE source_import_operations SET document=json_set(document,'$.observation_cursor',0)",
        )
        .unwrap();
    for _ in 0..2 {
        let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
        owner.shutdown().unwrap();
    }
}

/// Legacy entries omit the authority revision. A targeted claim must fail the
/// job without a Source observation, and a revisioned sibling must keep its
/// preclaim observation.
#[test]
fn legacy_targeted_refusal_writes_no_revisioned_observation() {
    let Fixture {
        root,
        owner,
        client,
    } = Fixture::open();
    let credential = session(&owner);
    let Credential::Session(token) = &credential else {
        unreachable!()
    };
    let token = Secret::new(token.expose().into());
    let admit = |key: &str| {
        let (request, files) = upload(key);
        let epoch = client.accounting_epoch();
        client
            .admit(
                Credential::Session(Secret::new(token.expose().into())),
                request,
                files,
                0,
                epoch,
                &AtomicBool::new(false),
            )
            .unwrap()
    };
    let current = admit("current");
    let legacy = admit("legacy");
    let revision_before: String = Connection::open(root.join("print-partner.db"))
        .unwrap()
        .query_row(
            "SELECT json_extract(document,'$.authority_revision') FROM source_import_operations WHERE operation_key=?1",
            [&current.key],
            |row| row.get(0),
        )
        .unwrap();
    assert!(current.authority_revision.is_some());
    assert!(legacy.authority_revision.is_some());
    Connection::open(root.join("print-partner.db"))
        .unwrap()
        .execute(
            "UPDATE source_import_operations SET document=json_remove(document,'$.authority_revision','$.observation_cursor') WHERE operation_key=?1",
            [&legacy.key],
        )
        .unwrap();
    owner
        .auth_with_policy(policy())
        .unwrap()
        .submit(
            auth::Request::Logout { token },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap();
    let admission = WorkerAdmission {
        kinds: JobKind::ALL.into_iter().map(|kind| (kind, 2)).collect(),
        total: 4,
        per_resource: 1,
        lease_seconds: 60,
    };
    let worker = owner.job_worker_with_policy(policy(), admission).unwrap();
    assert!(worker.claim_import(&current.job_id).is_err());
    assert!(worker.claim_import(&legacy.job_id).is_err());
    let connection = Connection::open(root.join("print-partner.db")).unwrap();
    let count = |sql: &str, key: &str| -> i64 {
        connection.query_row(sql, [key], |row| row.get(0)).unwrap()
    };
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM source_preclaim_refusals WHERE operation_key=?1",
            &current.key,
        ),
        1
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM source_revision_observations WHERE operation_key=?1",
            &current.key,
        ),
        0
    );
    let revision_after: String = connection
        .query_row(
            "SELECT json_extract(document,'$.authority_revision') FROM source_import_operations WHERE operation_key=?1",
            [&current.key],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(revision_after, revision_before);
    let current_refusal: (i64, String, Option<String>) = connection
        .query_row(
            "SELECT p.cursor, json_extract(j.document,'$._authority_refusal.phase'), json_extract(j.document,'$.fence') FROM source_preclaim_refusals p JOIN durable_jobs j ON j.id=p.job_id WHERE p.operation_key=?1",
            [&current.key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(current_refusal, (1, "targeted_claim".into(), None));
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM source_preclaim_refusals WHERE operation_key=?1",
            &legacy.key,
        ),
        0
    );
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM source_revision_observations WHERE operation_key=?1",
            &legacy.key,
        ),
        0
    );
    let legacy_row: (Option<String>, Option<i64>, String) = connection
        .query_row(
            "SELECT json_extract(o.document,'$.authority_revision'), json_extract(o.document,'$.observation_cursor'), json_extract(j.document,'$._authority_refusal.phase') FROM source_import_operations o JOIN durable_jobs j ON j.id=o.job_id WHERE o.operation_key=?1",
            [&legacy.key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(legacy_row, (None, None, "targeted_claim".into()));
    drop((worker, client, connection));
    owner.shutdown().unwrap();
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    owner.shutdown().unwrap();
    let connection = Connection::open(root.join("print-partner.db")).unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM source_revision_observations WHERE operation_key=?1",
                [&legacy.key],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert!(
        connection
            .query_row(
                "SELECT json_extract(document,'$.authority_revision') IS NULL AND json_extract(document,'$.observation_cursor') IS NULL FROM source_import_operations WHERE operation_key=?1",
                [&legacy.key],
                |row| row.get::<_, bool>(0),
            )
            .unwrap()
    );
}
