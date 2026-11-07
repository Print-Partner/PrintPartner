#![cfg(feature = "desktop_rebuild")]
//! Retained-input copy into owned import directories through SourceImports.
use pp_core::uploads::SourceImports;
use pp_source::{
    SourcePath,
    retained_capture::{FreezeResult, RetainedCaptureId, RetainedCaptureVault},
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    catalog::CreateSource,
    jobs::{Credential, PersistentState},
    uploads::{
        AdmissionLimits, CaptureId, CapturedInputV2, CapturedPayloadV1, File, Input, OwnedInput,
        State, Target,
    },
};
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const QUOTA: u64 = 16 * 1024 * 1024 * 1024;
const BYTES: &[u8] = b"solid triangle";

struct Fixture {
    root: PathBuf,
    owner: Option<WriterOwner>,
    imports: SourceImports,
}

impl Fixture {
    fn open() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-source-import-files-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let (owner, _) = WriterOwner::open(&root.join("data"), Limits::default()).unwrap();
        let policy = AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        };
        let imports = SourceImports::new(&owner, policy, QUOTA).unwrap();
        Self {
            root,
            owner: Some(owner),
            imports,
        }
    }

    fn credential(&self) -> Credential {
        Credential::PhysicalOwner(self.owner.as_ref().unwrap().job_physical_owner())
    }

    fn rows(&self) -> [i64; 4] {
        let db = Connection::open(self.root.join("data/print-partner.db")).unwrap();
        [
            "projects",
            "source_import_operations",
            "source_import_quota",
            "durable_jobs",
        ]
        .map(|table| {
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        })
    }

    fn repos(&self) -> PathBuf {
        self.root.join("data/repos")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.owner.take();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn capture_limits() -> AdmissionLimits {
    AdmissionLimits {
        reserved_bytes: 3_506_438_144,
        max_input_bytes: 268_435_456,
        max_prepared_bytes: 1_073_741_824,
    }
}

fn capture_target(name: &str) -> Target {
    Target::Create {
        metadata: Box::new(CreateSource {
            name: name.into(),
            source_kind: Some("local".into()),
            source_type: Some("local".into()),
            ..Default::default()
        }),
    }
}

fn data_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(root).unwrap().display().to_string();
            if path.is_dir() {
                out.insert(format!("{relative}/"), vec![]);
                walk(root, &path, out);
            } else {
                let name = relative.as_str();
                if name.ends_with(".db") || name.ends_with("-wal") || name.ends_with("-shm") {
                    continue;
                }
                out.insert(relative, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files);
    files
}

struct StagedFrozen {
    id: String,
    operation: pp_storage::uploads::Operation,
}

fn stage_frozen(fixture: &Fixture, key: &str) -> StagedFrozen {
    let payload = CapturedPayloadV1::files(vec!["triangle.stl".into()]);
    let preflight = fixture
        .imports
        .preflight_capture(
            fixture.credential(),
            key.into(),
            capture_target(key),
            payload.clone(),
            capture_limits(),
        )
        .unwrap();
    let vault = RetainedCaptureVault::open(&fixture.root.join("data")).unwrap();
    let mut candidate = vault.allocate().unwrap();
    let id = candidate.capture_id().as_str().to_owned();
    candidate
        .write_file(
            SourcePath::try_from("triangle.stl".to_owned()).unwrap(),
            BYTES,
            capture_limits().max_input_bytes,
        )
        .unwrap();
    let inventory = vec![File {
        path: "triangle.stl".into(),
        size: BYTES.len() as u64,
        sha256: hex::encode(Sha256::digest(BYTES)),
        kind: "input".into(),
    }];
    let prepared = preflight
        .prepare(
            CaptureId::new(id.clone()).unwrap(),
            key.into(),
            payload,
            capture_limits(),
            inventory,
        )
        .unwrap();
    let manifest = prepared.manifest().to_vec();
    assert!(matches!(
        candidate.freeze_owned(&manifest),
        FreezeResult::Frozen(_)
    ));
    let operation = fixture.imports.admit_captured(prepared).unwrap();
    StagedFrozen { id, operation }
}

fn open_frozen(fixture: &Fixture, id: &str) -> pp_source::retained_capture::FrozenCapture {
    let vault = RetainedCaptureVault::open(&fixture.root.join("data")).unwrap();
    vault
        .open_frozen(&RetainedCaptureId::parse(id.to_owned()).unwrap())
        .unwrap()
}

fn store_operation(fixture: &Fixture, operation: &pp_storage::uploads::Operation) {
    let db = Connection::open(fixture.root.join("data/print-partner.db")).unwrap();
    db.execute(
        "UPDATE source_import_operations SET state=?3, document=?4 WHERE tenant=?1 AND operation_key=?2",
        params![
            operation.tenant,
            operation.key,
            match &operation.state {
                State::Admitted => "admitted",
                State::OwnedInputReady => "owned_input_ready",
                other => panic!("unexpected state for test store: {other:?}"),
            },
            serde_json::to_string(operation).unwrap(),
        ],
    )
    .unwrap();
}

#[test]
fn source_import_never_overwrites_destination() {
    let fixture = Fixture::open();
    let staged = stage_frozen(&fixture, "owned-ok");
    let frozen = open_frozen(&fixture, &staged.id);
    let claimed = fixture.imports.prepare_frozen_claim(&frozen).unwrap();
    assert_eq!(claimed.job.state, PersistentState::Running);

    let written = fixture
        .imports
        .write_retained_inputs(claimed, frozen, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(written.state, State::OwnedInputReady);
    assert_eq!(
        written.owned.as_ref(),
        Some(&OwnedInput {
            locator: format!(".pp-imports/{}", written.job_id),
            digest: written.requested_digest.clone(),
            files: written.requested_files.clone(),
        })
    );
    let owned = fixture.repos().join(".pp-imports").join(&written.job_id);
    assert_eq!(std::fs::read(owned.join("triangle.stl")).unwrap(), BYTES);
    assert!(
        !fixture
            .root
            .join("data/source-captures")
            .join(&staged.id)
            .exists()
    );
    let vault = RetainedCaptureVault::open(&fixture.root.join("data")).unwrap();
    assert!(vault.scan().unwrap().is_empty());
    drop(fixture);

    let fixture = Fixture::open();
    let staged = stage_frozen(&fixture, "owned-refuse");
    let dest = fixture
        .repos()
        .join(".pp-imports")
        .join(&staged.operation.job_id);
    std::fs::create_dir_all(&dest).unwrap();
    let sentinel = dest.join("sentinel");
    std::fs::write(&sentinel, b"preserve").unwrap();
    let before_rows = fixture.rows();
    let before_files = data_files(&fixture.root.join("data"));
    let frozen = open_frozen(&fixture, &staged.id);
    let claimed = fixture.imports.prepare_frozen_claim(&frozen).unwrap();
    let refused = fixture
        .imports
        .write_retained_inputs(claimed, frozen, &AtomicBool::new(false))
        .unwrap_err()
        .to_string();
    assert_eq!(refused, "invalid-identity");
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserve");
    assert_eq!(fixture.rows(), before_rows);
    assert_eq!(data_files(&fixture.root.join("data")), before_files);
    let read = fixture
        .imports
        .get(fixture.credential(), "owned-refuse".into())
        .unwrap();
    assert_eq!(read.state, State::Admitted);
    assert!(read.owned.is_none());
}

#[test]
fn source_import_rejects_symlink_and_case_collision() {
    let fixture = Fixture::open();
    let staged = stage_frozen(&fixture, "owned-symlink");
    let imports = fixture.repos().join(".pp-imports");
    std::fs::create_dir_all(&imports).unwrap();
    let evil = fixture.root.join("evil-target");
    std::fs::create_dir_all(&evil).unwrap();
    std::fs::write(evil.join("planted"), b"untouched").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&evil, imports.join(&staged.operation.job_id)).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&evil, imports.join(&staged.operation.job_id)).unwrap();
    let before_rows = fixture.rows();
    let before_files = data_files(&fixture.root.join("data"));
    let frozen = open_frozen(&fixture, &staged.id);
    let claimed = fixture.imports.prepare_frozen_claim(&frozen).unwrap();
    let refused = fixture
        .imports
        .write_retained_inputs(claimed, frozen, &AtomicBool::new(false))
        .unwrap_err()
        .to_string();
    assert_eq!(refused, "invalid-identity");
    assert_eq!(std::fs::read(evil.join("planted")).unwrap(), b"untouched");
    assert_eq!(fixture.rows(), before_rows);
    assert_eq!(data_files(&fixture.root.join("data")), before_files);
    drop(fixture);

    let fixture = Fixture::open();
    let staged = stage_frozen(&fixture, "owned-case");
    let frozen = open_frozen(&fixture, &staged.id);
    let claimed = fixture.imports.prepare_frozen_claim(&frozen).unwrap();

    let mut operation = staged.operation.clone();
    let mut files = operation.requested_files.clone();
    files.push(File {
        path: "TRIANGLE.STL".into(),
        size: files[0].size,
        sha256: files[0].sha256.clone(),
        kind: "input".into(),
    });
    let digest = hex::encode(Sha256::digest(serde_json::to_vec(&files).unwrap()));
    let rebound = CapturedInputV2::bind(
        CaptureId::new(staged.id.clone()).unwrap(),
        CapturedPayloadV1::files(vec!["triangle.stl".into(), "TRIANGLE.STL".into()]),
        &operation.actor,
        &operation.key,
        &capture_target("owned-case"),
        capture_limits(),
        &files,
    )
    .unwrap();
    let Input::Captured(original) = &staged.operation.input else {
        panic!("captured import required");
    };
    let rebound = {
        let mut value = serde_json::to_value(&rebound).unwrap();
        let original_value = serde_json::to_value(original).unwrap();
        value["target_digest"] = original_value["target_digest"].clone();
        serde_json::from_value(value).unwrap()
    };
    operation.input = Input::Captured(rebound);
    operation.requested_files = files;
    operation.requested_digest = digest.clone();
    if let Some(revision) = operation.authority_revision.as_mut() {
        revision.input_digest = digest;
    }
    store_operation(&fixture, &operation);

    let before_rows = fixture.rows();
    let before_files = data_files(&fixture.root.join("data"));
    let refused = fixture
        .imports
        .write_retained_inputs(claimed, frozen, &AtomicBool::new(false))
        .unwrap_err()
        .to_string();
    assert_eq!(refused, "duplicate-path");
    assert_eq!(fixture.rows(), before_rows);
    assert_eq!(data_files(&fixture.root.join("data")), before_files);
    assert!(
        !fixture
            .repos()
            .join(".pp-imports")
            .join(&staged.operation.job_id)
            .exists()
    );
}
