#![cfg(feature = "desktop_rebuild")]
//! Restart reconciliation of retained captures through SourceAcquisitions.
use pp_core::{
    source_acquisition::SourceAcquisitions,
    uploads::{SourceImports, Through},
};
use pp_source::{
    SourcePath,
    retained_capture::{FreezeResult, RetainedCaptureVault},
};
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    catalog::CreateSource,
    jobs::Credential,
    uploads::{
        AdmissionLimits, CaptureId, CapturedPayloadV1, File, PreparedCapturedAdmission, State,
        Target,
    },
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

const QUOTA: u64 = 16 * 1024 * 1024 * 1024;
const BYTES: &[u8] = b"solid triangle";

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

struct Fixture {
    root: PathBuf,
    remove_root: bool,
    owner: Option<WriterOwner>,
    imports: SourceImports,
}

impl Fixture {
    fn open_at(root: PathBuf) -> Self {
        let root = std::fs::canonicalize(root).unwrap();
        let (owner, _) = WriterOwner::open(&root.join("data"), Limits::default()).unwrap();
        let imports = SourceImports::new(&owner, policy(), QUOTA).unwrap();
        Self {
            root,
            remove_root: true,
            owner: Some(owner),
            imports,
        }
    }

    fn open() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-source-acquisition-recovery-{}",
            hex::encode(rand::random::<[u8; 8]>())
        ));
        std::fs::create_dir_all(&root).unwrap();
        Self::open_at(root)
    }

    /// Drops every handle and reopens storage at the same root, as a restart.
    fn reopen(mut self) -> Self {
        let root = self.root.clone();
        self.remove_root = false;
        drop(self);
        Self::open_at(root)
    }

    fn acquisitions(&self) -> SourceAcquisitions {
        SourceAcquisitions::new(self.owner.as_ref().unwrap(), policy(), QUOTA, 1).unwrap()
    }

    fn credential(&self) -> Credential {
        Credential::PhysicalOwner(self.owner.as_ref().unwrap().job_physical_owner())
    }

    fn capture_dir(&self, id: &str) -> PathBuf {
        self.root.join("data/source-captures").join(id)
    }

    fn journal(&self) -> Vec<String> {
        let db = Connection::open(self.root.join("data/print-partner.db")).unwrap();
        let mut statement = db
            .prepare("SELECT document FROM source_import_operations ORDER BY rowid")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|row| row.unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            let _ = owner.shutdown();
        }
        if self.remove_root {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// Every file below `dir` with its bytes; directories map to empty values.
fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
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
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn capture_limits() -> AdmissionLimits {
    AdmissionLimits {
        reserved_bytes: 3_506_438_144,
        max_input_bytes: 268_435_456,
        max_prepared_bytes: 1_073_741_824,
    }
}

/// Preflights a captured import of one STL and freezes its retained capture
/// without admitting it. Returns the retained capture id and the prepared
/// admission.
fn freeze(fixture: &Fixture, key: &str) -> (String, PreparedCapturedAdmission) {
    let payload = CapturedPayloadV1::files(vec!["triangle.stl".into()]);
    let limits = capture_limits();
    let preflight = fixture
        .imports
        .preflight_capture(
            fixture.credential(),
            key.into(),
            Target::Create {
                metadata: Box::new(CreateSource {
                    name: key.into(),
                    source_kind: Some("local".into()),
                    source_type: Some("local".into()),
                    ..Default::default()
                }),
            },
            payload.clone(),
            limits,
        )
        .unwrap();
    let vault = RetainedCaptureVault::open(&fixture.root.join("data")).unwrap();
    let mut candidate = vault.allocate().unwrap();
    let id = candidate.capture_id().as_str().to_owned();
    candidate
        .write_file(
            SourcePath::try_from("triangle.stl".to_owned()).unwrap(),
            BYTES,
            limits.max_input_bytes,
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
            limits,
            inventory,
        )
        .unwrap();
    let manifest = prepared.manifest().to_vec();
    assert!(matches!(
        candidate.freeze_owned(&manifest),
        FreezeResult::Frozen(_)
    ));
    (id, prepared)
}

/// Freezes and admits a captured import, as a process that stopped right after
/// admission. Returns the retained capture id.
fn admitted(fixture: &Fixture, key: &str) -> String {
    let (id, prepared) = freeze(fixture, key);
    let op = fixture.imports.admit_captured(prepared).unwrap();
    assert_eq!(op.state, State::Admitted);
    id
}

fn refusal(result: anyhow::Result<impl std::fmt::Debug>) -> String {
    match result {
        Ok(value) => panic!("expected refusal, got {value:?}"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn source_acquisition_recovery_precedes_admission() {
    let fixture = Fixture::open();
    let interrupted = admitted(&fixture, "recover-admitted");
    let (unadmitted, late) = freeze(&fixture, "recover-unadmitted");
    let fixture = fixture.reopen();
    let acquisitions = fixture.acquisitions();

    // New work is refused until recovery has run, and leaves the journal as-is.
    let journal = fixture.journal();
    assert_eq!(
        refusal(acquisitions.admit_captured(late)),
        "Source acquisition ingress is closed"
    );
    assert_eq!(fixture.journal(), journal);

    assert_eq!(acquisitions.recover_before_accepting().unwrap(), 2);
    let settled = fixture
        .imports
        .get(fixture.credential(), "recover-admitted".into())
        .unwrap();
    assert_eq!(settled.state, State::Activated);
    assert!(settled.cleanup_settled);
    assert!(settled.receipt.as_ref().unwrap().activated);
    assert!(!fixture.capture_dir(&interrupted).exists());
    assert!(!fixture.capture_dir(&unadmitted).exists());
    assert!(
        refusal(
            fixture
                .imports
                .get(fixture.credential(), "recover-unadmitted".into())
        )
        .contains("not found")
    );

    let (_, next) = freeze(&fixture, "recover-next");
    let op = acquisitions.admit_captured(next).unwrap();
    assert_eq!(op.state, State::Admitted);
    assert_eq!(op.key, "recover-next");
}

#[test]
fn source_acquisition_conflict_preserves_capture() {
    let fixture = Fixture::open();
    let conflicted = admitted(&fixture, "conflict-tampered");
    let unrelated = admitted(&fixture, "conflict-unrelated");
    let fixture = fixture.reopen();

    // The retained bytes no longer match the journaled claim.
    let input = fixture
        .capture_dir(&conflicted)
        .join("frozen/input/triangle.stl");
    // Owner read and write only. Writing a mode avoids clippy's permissions_set_readonly_false lint.
    #[cfg(unix)]
    std::fs::set_permissions(&input, std::os::unix::fs::PermissionsExt::from_mode(0o600)).unwrap();
    // Clear the read-only attribute directly, for the same lint.
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_NORMAL, SetFileAttributesW};
        let wide: Vec<u16> = input.as_os_str().encode_wide().chain([0]).collect();
        assert_ne!(
            unsafe { SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_NORMAL) },
            0
        );
    }
    std::fs::write(&input, b"solid tampered").unwrap();
    let capture = tree(&fixture.capture_dir(&conflicted));
    let tampered_row = fixture.journal()[0].clone();

    let acquisitions = fixture.acquisitions();
    assert_eq!(
        refusal(acquisitions.recover_before_accepting()),
        format!(
            "Retained captures require repair (1 remaining): {conflicted}: Retained capture requires repair"
        )
    );
    assert_eq!(tree(&fixture.capture_dir(&conflicted)), capture);
    assert_eq!(fixture.journal()[0], tampered_row);
    let kept = fixture
        .imports
        .get(fixture.credential(), "conflict-tampered".into())
        .unwrap();
    assert_eq!(kept.state, State::Admitted);

    // Safe work beside the conflict still settles.
    let settled = fixture
        .imports
        .get(fixture.credential(), "conflict-unrelated".into())
        .unwrap();
    assert!(settled.cleanup_settled);
    assert!(!fixture.capture_dir(&unrelated).exists());

    // Ingress stays closed while the conflict is retained.
    let (_, next) = freeze(&fixture, "conflict-next");
    let journal = fixture.journal();
    assert_eq!(
        refusal(acquisitions.admit_captured(next)),
        "Source acquisition ingress is closed"
    );
    assert_eq!(fixture.journal(), journal);
    assert_eq!(tree(&fixture.capture_dir(&conflicted)), capture);
}

#[test]
fn source_acquisition_discards_candidate_and_malformed_holds_ingress() {
    let fixture = Fixture::open();
    let interrupted = admitted(&fixture, "unfrozen-admitted");
    let vault = RetainedCaptureVault::open(&fixture.root.join("data")).unwrap();
    // A process that stopped while writing a candidate, and a slot that is
    // neither a candidate nor a frozen capture.
    let mut candidate = vault.allocate().unwrap();
    candidate
        .write_file(
            SourcePath::try_from("triangle.stl".to_owned()).unwrap(),
            BYTES,
            capture_limits().max_input_bytes,
        )
        .unwrap();
    let unfrozen = candidate.capture_id().as_str().to_owned();
    drop(candidate);
    let malformed = vault.allocate().unwrap().capture_id().as_str().to_owned();
    std::fs::create_dir(fixture.capture_dir(&malformed).join("frozen")).unwrap();
    drop(vault);
    let fixture = fixture.reopen();
    let kept = tree(&fixture.capture_dir(&malformed));

    // The half-written candidate is discarded; the malformed slot is kept.
    let acquisitions = fixture.acquisitions();
    assert_eq!(
        refusal(acquisitions.recover_before_accepting()),
        format!(
            "Retained captures require repair (1 remaining): {malformed}: Malformed retained capture"
        )
    );
    assert!(!fixture.capture_dir(&unfrozen).exists());
    assert_eq!(tree(&fixture.capture_dir(&malformed)), kept);

    // The admitted capture beside them still settles.
    let settled = fixture
        .imports
        .get(fixture.credential(), "unfrozen-admitted".into())
        .unwrap();
    assert!(settled.cleanup_settled);
    assert!(!fixture.capture_dir(&interrupted).exists());

    let (_, next) = freeze(&fixture, "unfrozen-next");
    let journal = fixture.journal();
    assert_eq!(
        refusal(acquisitions.admit_captured(next)),
        "Source acquisition ingress is closed"
    );
    assert_eq!(fixture.journal(), journal);
}

#[test]
fn source_acquisition_resumes_owned_imports_after_restart() {
    let fixture = Fixture::open();
    let held = admitted(&fixture, "owned-resume");
    // The worker copied the input and stopped before publishing.
    let vault = RetainedCaptureVault::open(&fixture.root.join("data")).unwrap();
    let frozen = vault
        .open_frozen(&pp_source::retained_capture::RetainedCaptureId::parse(held.clone()).unwrap())
        .unwrap();
    let owned = fixture
        .imports
        .work_frozen(frozen, Through::OwnedInput, &AtomicBool::new(false))
        .unwrap()
        .unwrap();
    assert_eq!(owned.state, State::OwnedInputReady);
    drop(vault);

    // Opening storage requeues the job; nothing is retained to recover, and
    // the import resumes with no new request.
    let fixture = fixture.reopen();
    let acquisitions = fixture.acquisitions();
    assert_eq!(acquisitions.recover_before_accepting().unwrap(), 0);
    assert_eq!(acquisitions.resume_imports().unwrap(), 1);
    let settled = fixture
        .imports
        .get(fixture.credential(), "owned-resume".into())
        .unwrap();
    assert_eq!(settled.job_id, owned.job_id);
    assert_eq!(settled.state, State::Activated);
    assert!(settled.cleanup_settled);
    assert_eq!(acquisitions.resume_imports().unwrap(), 0);
}

#[test]
fn source_acquisition_resume_stops_when_nothing_can_be_claimed() {
    let mut fixture = Fixture::open();
    let acquisitions = fixture.acquisitions();
    assert_eq!(acquisitions.recover_before_accepting().unwrap(), 0);
    // A stopped writer refuses every claim: the call ends at once and counts
    // nothing, instead of counting each refused claim as an import.
    fixture.owner.take().unwrap().shutdown().unwrap();
    assert_eq!(acquisitions.resume_imports().unwrap(), 0);
}

#[test]
fn source_acquisition_held_claim_names_its_cause() {
    let fixture = Fixture::open();
    let held = admitted(&fixture, "held-claim");
    let op = fixture
        .imports
        .get(fixture.credential(), "held-claim".into())
        .unwrap();
    // A worker without the retained capture claims the job and stops; the
    // claim holds the job until restart recovery requeues it.
    assert_eq!(
        refusal(fixture.imports.work_operation(
            &op.job_id,
            None,
            Through::Settled,
            &AtomicBool::new(false),
        )),
        "Import requires explicitly supplied input"
    );

    let acquisitions = fixture.acquisitions();
    assert_eq!(
        refusal(acquisitions.recover_before_accepting()),
        format!(
            "Retained captures require repair (1 remaining): {held}: Import job is held by another attempt"
        )
    );
    assert!(fixture.capture_dir(&held).exists());

    // After a restart requeues the job, recovery settles it and opens ingress.
    let fixture = fixture.reopen();
    let acquisitions = fixture.acquisitions();
    assert_eq!(acquisitions.recover_before_accepting().unwrap(), 1);
    assert!(!fixture.capture_dir(&held).exists());
    assert!(
        fixture
            .imports
            .get(fixture.credential(), "held-claim".into())
            .unwrap()
            .cleanup_settled
    );
}
