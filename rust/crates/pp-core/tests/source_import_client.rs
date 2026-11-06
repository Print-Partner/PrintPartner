#![cfg(feature = "desktop_rebuild")]
//! The synchronous supplied-import client over disposable storage and input
//! directories.
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
        Admission, AdmissionLimits, CaptureId, CapturedPayloadV1, File, Input, State, Target,
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

struct Fixture {
    root: PathBuf,
    owner: Option<WriterOwner>,
    imports: SourceImports,
}

impl Fixture {
    fn open() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-source-import-client-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        std::fs::create_dir_all(root.join("supplied")).unwrap();
        // macOS temp paths pass through the `/var` link, which source reads refuse.
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

    fn supplied(&self) -> PathBuf {
        self.root.join("supplied")
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.supplied().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn credential(&self) -> Credential {
        Credential::PhysicalOwner(self.owner.as_ref().unwrap().job_physical_owner())
    }

    fn admit(&self, request: Admission) -> anyhow::Result<pp_storage::uploads::Operation> {
        self.admit_with(request, false)
    }

    fn admit_with(
        &self,
        request: Admission,
        cancelled: bool,
    ) -> anyhow::Result<pp_storage::uploads::Operation> {
        self.imports.admit(
            self.credential(),
            request,
            &self.supplied(),
            &AtomicBool::new(cancelled),
        )
    }

    fn rows(&self) -> [i64; 4] {
        let db = rusqlite::Connection::open(self.root.join("data/print-partner.db")).unwrap();
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

    /// Every entry under the repos and supplied directories with its bytes.
    fn files(&self) -> BTreeMap<String, Vec<u8>> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                let relative = path.strip_prefix(root).unwrap().display().to_string();
                if path.is_dir() {
                    out.insert(format!("{relative}/"), vec![]);
                    walk(root, &path, out);
                } else {
                    out.insert(relative, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut files = BTreeMap::new();
        for dir in ["data/repos", "supplied"] {
            walk(&self.root, &self.root.join(dir), &mut files);
        }
        files
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.owner.take();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn request(key: &str, input: Input) -> Admission {
    Admission {
        key: key.into(),
        target: Target::Create {
            metadata: Box::new(CreateSource {
                name: key.into(),
                source_kind: Some("local".into()),
                source_type: Some("local".into()),
                ..Default::default()
            }),
        },
        input,
        reserved_bytes: 32 * 1024 * 1024,
        max_input_bytes: 1024,
        max_prepared_bytes: 1024 * 1024,
    }
}

fn input(path: &str, bytes: &[u8]) -> File {
    File {
        path: path.into(),
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(bytes)),
        kind: "input".into(),
    }
}

#[test]
fn source_import_client_admits_once() {
    let fixture = Fixture::open();
    fixture.write("part.stl", b"solid a");
    fixture.write("docs/readme.txt", b"hi");
    fixture.write(".hidden", b"skipped");
    let directory = SourceImports::directory_input(&fixture.supplied()).unwrap();
    assert_eq!(
        directory,
        Input::Files {
            paths: vec!["docs/readme.txt".into(), "part.stl".into()]
        }
    );
    let before = fixture.files();

    let operation = fixture.admit(request("first", directory.clone())).unwrap();
    assert_eq!(
        (operation.key.as_str(), &operation.state, &operation.input),
        ("first", &State::Admitted, &directory)
    );
    assert_eq!(
        operation.requested_files,
        [
            input("docs/readme.txt", b"hi"),
            input("part.stl", b"solid a")
        ]
    );
    assert_eq!(
        (operation.owned.as_ref(), operation.artifact.as_ref()),
        (None, None)
    );

    let rows = fixture.rows();
    let replay = fixture.admit(request("first", directory)).unwrap();
    assert_eq!((replay, fixture.rows()), (operation.clone(), rows));
    let read = fixture
        .imports
        .get(fixture.credential(), "first".into())
        .unwrap();
    assert_eq!(read, operation);
    assert_eq!(fixture.files(), before);
}

fn os_error(supplied: &Path) -> Option<i32> {
    let error = SourceImports::directory_input(supplied).unwrap_err();
    match error.downcast_ref::<pp_source::Error>() {
        Some(pp_source::Error::Io(error)) => error.raw_os_error(),
        _ => panic!("unexpected refusal: {error}"),
    }
}

#[test]
fn source_import_client_refuses_invalid_directory_input() {
    let fixture = Fixture::open();
    fixture.write("part.stl", b"solid a");
    let link = fixture.supplied().join("link.stl");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(fixture.supplied().join("part.stl"), &link).unwrap();
        assert_eq!(os_error(&fixture.supplied()), Some(libc::ELOOP));
        assert_eq!(os_error(&fixture.root.join("missing")), Some(libc::ENOENT));
    }
    // pp-os refuses a Windows reparse point without an OS error code.
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(fixture.supplied().join("part.stl"), &link).unwrap();
        assert_eq!(os_error(&fixture.supplied()), None);
        assert_eq!(os_error(&fixture.root.join("missing")), Some(2));
    }
    std::fs::remove_file(link).unwrap();
    let before = fixture.files();
    let rows = fixture.rows();
    let missing = format!("io: {}", std::io::Error::from_raw_os_error(2));

    let refusals = [
        (
            Input::Files {
                paths: vec!["absent.stl".into()],
            },
            1024,
            missing.as_str(),
        ),
        (
            Input::Files {
                paths: vec!["../part.stl".into()],
            },
            1024,
            "unsafe-path",
        ),
        (
            Input::Zip {
                path: "absent.zip".into(),
            },
            1024,
            missing.as_str(),
        ),
        (
            Input::Files {
                paths: vec!["part.stl".into()],
            },
            3,
            "limit",
        ),
        (
            Input::Files {
                paths: vec!["part.stl".into()],
            },
            256 * 1024 * 1024 + 1,
            "Invalid import input limit",
        ),
    ];
    for (input, max_input_bytes, expected) in refusals {
        let mut refused = request("refused", input);
        refused.max_input_bytes = max_input_bytes;
        let error = fixture.admit(refused).unwrap_err().to_string();
        assert_eq!((error.as_str(), fixture.rows()), (expected, rows));
    }
    let directory = SourceImports::directory_input(&fixture.supplied()).unwrap();
    let error = fixture
        .admit_with(request("refused", directory.clone()), true)
        .unwrap_err()
        .to_string();
    assert_eq!(
        (error.as_str(), fixture.rows()),
        ("Cancelled before admission", rows)
    );
    let part = [pp_source::SourcePath::try_from("part.stl".to_owned()).unwrap()];
    let local = pp_source::LocalFiles::open(&fixture.supplied()).unwrap();
    assert_eq!(local.inventory(&part, 1024).unwrap().len(), 1);
    assert!(matches!(
        local.inventory_until(&part, 1024, &AtomicBool::new(true)),
        Err(pp_source::Error::Limit)
    ));
    let missing = fixture
        .imports
        .get(fixture.credential(), "refused".into())
        .unwrap_err()
        .to_string();
    assert_eq!(missing, "Import not found");
    assert_eq!(fixture.files(), before);

    let admitted = fixture.admit(request("refused", directory)).unwrap();
    assert_eq!(admitted.requested_files, [input("part.stl", b"solid a")]);
    assert_eq!(fixture.rows(), [rows[0] + 1, 1, 1, rows[3] + 1]);
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
    let bytes = b"solid triangle";
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
            bytes,
            capture_limits().max_input_bytes,
        )
        .unwrap();
    let inventory = vec![File {
        path: "triangle.stl".into(),
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(bytes)),
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

#[test]
fn source_import_frozen_claim_matches_manifest() {
    let fixture = Fixture::open();
    let staged = stage_frozen(&fixture, "frozen-match");
    let vault = RetainedCaptureVault::open(&fixture.root.join("data")).unwrap();
    let frozen = vault
        .open_frozen(&RetainedCaptureId::parse(staged.id).unwrap())
        .unwrap();

    let claimed = fixture.imports.prepare_frozen_claim(&frozen).unwrap();
    assert_eq!(claimed.job.job_id, staged.operation.job_id);
    assert_eq!(claimed.job.state, PersistentState::Running);
    assert!(claimed.source_work.is_some());
    assert_eq!(staged.operation.input_version, 2);
    assert_eq!(fixture.rows()[1], 1);
}

#[test]
fn source_import_frozen_claim_rejects_stale_source() {
    let fixture = Fixture::open();
    let staged = stage_frozen(&fixture, "frozen-stale");

    let connection = Connection::open(fixture.root.join("data/print-partner.db")).unwrap();
    connection
        .execute(
            "DELETE FROM projects WHERE tenant_id=?1 AND id=?2",
            params!["default", staged.operation.source_id],
        )
        .unwrap();
    drop(connection);

    let before_rows = fixture.rows();
    let before_files = data_files(&fixture.root.join("data"));
    let vault = RetainedCaptureVault::open(&fixture.root.join("data")).unwrap();
    let frozen = vault
        .open_frozen(&RetainedCaptureId::parse(staged.id).unwrap())
        .unwrap();
    let refused = match fixture.imports.prepare_frozen_claim(&frozen) {
        Ok(_) => panic!("stale Source must refuse frozen claim"),
        Err(error) => error.to_string(),
    };
    assert_eq!(refused, "Captured import requires repair");
    assert_eq!(fixture.rows(), before_rows);
    assert_eq!(data_files(&fixture.root.join("data")), before_files);
}
