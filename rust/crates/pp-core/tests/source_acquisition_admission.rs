#![cfg(feature = "desktop_rebuild")]
//! Bounded admission of Source acquisitions through SourceAcquisitions::acquire.
use pp_core::{
    source_acquisition::{AcquisitionReply, CaptureRequest, CapturedFile, SourceAcquisitions},
    uploads::SourceImports,
};
use pp_source::SourcePath;
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    catalog::CreateSource,
    jobs::Credential,
    uploads::{AdmissionLimits, CapturedPayloadV1, Operation, State, Target},
};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
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
    _root: tempfile::TempDir,
    data: PathBuf,
    owner: Option<WriterOwner>,
    imports: SourceImports,
}

impl Fixture {
    fn open() -> Self {
        let root = tempfile::Builder::new()
            .prefix("pp-source-acquisition-admission-")
            .tempdir()
            .unwrap();
        let data = std::fs::canonicalize(root.path()).unwrap().join("data");
        let (owner, _) = WriterOwner::open(&data, Limits::default()).unwrap();
        let imports = SourceImports::new(&owner, policy(), QUOTA).unwrap();
        Self {
            _root: root,
            data,
            owner: Some(owner),
            imports,
        }
    }

    fn new_acquisitions(&self, capacity: usize) -> anyhow::Result<SourceAcquisitions> {
        SourceAcquisitions::new(self.owner.as_ref().unwrap(), policy(), QUOTA, capacity)
    }

    /// Acquisitions with ingress opened by recovery over an empty vault.
    fn acquisitions(&self, capacity: usize) -> SourceAcquisitions {
        let acquisitions = self.new_acquisitions(capacity).unwrap();
        assert_eq!(acquisitions.recover_before_accepting().unwrap(), 0);
        acquisitions
    }

    fn credential(&self) -> Credential {
        Credential::PhysicalOwner(self.owner.as_ref().unwrap().job_physical_owner())
    }

    fn request(&self, key: &str, input: impl Read + Send + 'static) -> CaptureRequest {
        CaptureRequest {
            credential: self.credential(),
            operation_key: key.into(),
            target: Target::Create {
                metadata: Box::new(CreateSource {
                    name: key.into(),
                    source_kind: Some("local".into()),
                    source_type: Some("local".into()),
                    ..Default::default()
                }),
            },
            payload: CapturedPayloadV1::files(vec!["triangle.stl".into()]),
            limits: AdmissionLimits {
                reserved_bytes: 3_506_438_144,
                max_input_bytes: 268_435_456,
                max_prepared_bytes: 1_073_741_824,
            },
            files: vec![CapturedFile {
                path: SourcePath::try_from("triangle.stl".to_owned()).unwrap(),
                input: Box::new(input),
            }],
        }
    }

    fn get(&self, key: &str) -> anyhow::Result<Operation> {
        self.imports.get(self.credential(), key.into())
    }

    /// Journal rows and import job rows, as one comparable snapshot.
    fn rows(&self) -> Vec<String> {
        let db = Connection::open(self.data.join("print-partner.db")).unwrap();
        let mut rows = Vec::new();
        for sql in [
            "SELECT document FROM source_import_operations ORDER BY rowid",
            "SELECT id || ':' || state || ':' || version FROM durable_jobs ORDER BY rowid",
        ] {
            let mut statement = db.prepare(sql).unwrap();
            rows.extend(
                statement
                    .query_map([], |row| row.get::<_, String>(0))
                    .unwrap()
                    .map(|row| row.unwrap()),
            );
        }
        rows
    }

    fn captures(&self) -> BTreeMap<String, Vec<u8>> {
        tree(&self.data.join("source-captures"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            let _ = owner.shutdown();
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
    if dir.exists() {
        walk(dir, dir, &mut out);
    }
    out
}

/// Input that records whether anything read it.
struct Watched {
    bytes: &'static [u8],
    read: Arc<AtomicBool>,
}

impl Watched {
    fn new() -> (Self, Arc<AtomicBool>) {
        let read = Arc::new(AtomicBool::new(false));
        (
            Self {
                bytes: BYTES,
                read: read.clone(),
            },
            read,
        )
    }
}

impl Read for Watched {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read.store(true, Ordering::SeqCst);
        self.bytes.read(buf)
    }
}

/// Input whose every read fails.
struct Broken;

impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("input device gone"))
    }
}

/// Input that reports its first read, then blocks until released.
struct Held {
    started: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
    bytes: &'static [u8],
}

impl Read for Held {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(started) = self.started.take() {
            started.send(()).unwrap();
            self.release.recv().unwrap();
        }
        self.bytes.read(buf)
    }
}

/// Starts an acquisition of `key` on its own thread whose input blocks until
/// the returned sender is used, and waits until it holds a capacity slot.
fn hold<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    fixture: &'scope Fixture,
    acquisitions: &'scope SourceAcquisitions,
    key: &'static str,
) -> (
    mpsc::Sender<()>,
    std::thread::ScopedJoinHandle<'scope, Operation>,
) {
    let (started, held) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let input = Held {
        started: Some(started),
        release: released,
        bytes: BYTES,
    };
    let worker = scope.spawn(move || {
        acquisitions
            .acquire(fixture.request(key, input), &AtomicBool::new(false))
            .unwrap()
            .wait()
            .unwrap()
    });
    held.recv().unwrap();
    (release, worker)
}

fn refusal(
    result: Result<AcquisitionReply, pp_core::source_acquisition::AcquisitionError>,
) -> String {
    match result {
        Ok(reply) => panic!("expected refusal, got {:?}", reply.wait()),
        Err(error) => format!("{error:#}"),
    }
}

#[test]
fn source_acquisition_busy_has_no_effects() {
    let fixture = Fixture::open();
    let acquisitions = fixture.acquisitions(1);
    std::thread::scope(|scope| {
        let (release, first) = hold(scope, &fixture, &acquisitions, "busy-first");

        // The only slot is held, so a second request is refused before it reads
        // its input, allocates a capture, or writes a journal or job row.
        let rows = fixture.rows();
        let captures = fixture.captures();
        let (input, read) = Watched::new();
        assert_eq!(
            refusal(acquisitions.acquire(
                fixture.request("busy-second", input),
                &AtomicBool::new(false)
            )),
            "Source acquisition capacity exhausted"
        );
        assert!(!read.load(Ordering::SeqCst));
        assert_eq!(fixture.rows(), rows);
        assert_eq!(fixture.captures(), captures);
        assert!(format!("{:#}", fixture.get("busy-second").unwrap_err()).contains("not found"));

        // The held acquisition keeps its slot until its result is recorded.
        release.send(()).unwrap();
        let first = first.join().unwrap();
        assert_eq!(first.state, State::Activated);
        assert!(first.cleanup_settled);
        assert_eq!(fixture.get("busy-first").unwrap(), first);
    });

    // With the slot returned, the refused request is admitted afresh.
    let (input, read) = Watched::new();
    let second = acquisitions
        .acquire(
            fixture.request("busy-second", input),
            &AtomicBool::new(false),
        )
        .unwrap()
        .wait()
        .unwrap();
    assert!(read.load(Ordering::SeqCst));
    assert_eq!(second.state, State::Activated);
    assert!(second.cleanup_settled);
    assert_eq!(fixture.captures(), BTreeMap::new());
}

#[test]
fn source_acquisition_reply_matches_recorded_operation() {
    let fixture = Fixture::open();
    let acquisitions = fixture.acquisitions(1);
    let (input, _) = Watched::new();
    let recorded = acquisitions
        .acquire(fixture.request("reply-key", input), &AtomicBool::new(false))
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(recorded.key, "reply-key");
    assert_eq!(recorded.state, State::Activated);
    assert!(recorded.cleanup_settled);
    assert!(recorded.receipt.as_ref().unwrap().activated);
    assert_eq!(fixture.get("reply-key").unwrap(), recorded);
    assert_eq!(fixture.captures(), BTreeMap::new());

    // Repeating the key replies with the recorded operation, even while every
    // slot is held, without reading input or writing anything.
    std::thread::scope(|scope| {
        let (release, other) = hold(scope, &fixture, &acquisitions, "reply-other");
        let rows = fixture.rows();
        let captures = fixture.captures();
        let (input, read) = Watched::new();
        let replayed = acquisitions
            .acquire(fixture.request("reply-key", input), &AtomicBool::new(false))
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(replayed, recorded);
        assert!(!read.load(Ordering::SeqCst));
        assert_eq!(fixture.rows(), rows);
        assert_eq!(fixture.captures(), captures);
        release.send(()).unwrap();
        assert_eq!(other.join().unwrap().state, State::Activated);
    });
}

#[test]
fn source_acquisition_in_flight_key_is_refused_without_effects() {
    let fixture = Fixture::open();
    let acquisitions = fixture.acquisitions(2);
    std::thread::scope(|scope| {
        let (release, first) = hold(scope, &fixture, &acquisitions, "dup-key");

        // A retry of the same key while the first is in flight is refused before
        // it reads input or freezes a second capture, even with a slot free.
        let rows = fixture.rows();
        let captures = fixture.captures();
        let (input, read) = Watched::new();
        assert_eq!(
            refusal(
                acquisitions.acquire(fixture.request("dup-key", input), &AtomicBool::new(false))
            ),
            "Source acquisition already in progress for this key"
        );
        assert!(!read.load(Ordering::SeqCst));
        assert_eq!(fixture.rows(), rows);
        assert_eq!(fixture.captures(), captures);

        release.send(()).unwrap();
        assert_eq!(first.join().unwrap().state, State::Activated);
    });

    // Once recorded, the retry replies with the recorded operation, and no
    // capture is left behind for recovery to repair.
    let (input, _) = Watched::new();
    let replayed = acquisitions
        .acquire(fixture.request("dup-key", input), &AtomicBool::new(false))
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(replayed, fixture.get("dup-key").unwrap());
    assert_eq!(fixture.captures(), BTreeMap::new());
    assert_eq!(acquisitions.recover_before_accepting().unwrap(), 0);
}

#[test]
fn source_acquisition_recovery_refused_while_active_keeps_ingress() {
    let fixture = Fixture::open();
    let acquisitions = fixture.acquisitions(2);
    std::thread::scope(|scope| {
        let (release, first) = hold(scope, &fixture, &acquisitions, "active-first");
        assert_eq!(
            format!("{:#}", acquisitions.recover_before_accepting().unwrap_err()),
            "Source acquisitions are active"
        );

        // The refused recovery left ingress open for the free slot.
        let (input, _) = Watched::new();
        let second = acquisitions
            .acquire(
                fixture.request("active-second", input),
                &AtomicBool::new(false),
            )
            .unwrap()
            .wait()
            .unwrap();
        assert_eq!(second.state, State::Activated);

        release.send(()).unwrap();
        assert_eq!(first.join().unwrap().state, State::Activated);
    });
    assert_eq!(acquisitions.recover_before_accepting().unwrap(), 0);
}

#[test]
fn source_acquisition_closed_ingress_has_no_effects() {
    let fixture = Fixture::open();
    let acquisitions = fixture.new_acquisitions(1).unwrap();
    let rows = fixture.rows();
    let (input, read) = Watched::new();
    assert_eq!(
        refusal(acquisitions.acquire(
            fixture.request("closed-key", input),
            &AtomicBool::new(false)
        )),
        "Source acquisition ingress is closed"
    );
    assert!(!read.load(Ordering::SeqCst));
    assert_eq!(fixture.rows(), rows);
    assert_eq!(fixture.captures(), BTreeMap::new());

    // The refused key was released: once recovery opens ingress it is admitted.
    assert_eq!(acquisitions.recover_before_accepting().unwrap(), 0);
    let (input, _) = Watched::new();
    let op = acquisitions
        .acquire(
            fixture.request("closed-key", input),
            &AtomicBool::new(false),
        )
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(op.state, State::Activated);
}

#[test]
fn source_acquisition_failure_before_freeze_removes_candidate() {
    let fixture = Fixture::open();
    let acquisitions = fixture.acquisitions(1);
    let rows = fixture.rows();

    let (input, read) = Watched::new();
    assert_eq!(
        refusal(acquisitions.acquire(
            fixture.request("fail-cancelled", input),
            &AtomicBool::new(true)
        )),
        "Owned capture candidate removed: Source acquisition cancelled"
    );
    assert!(!read.load(Ordering::SeqCst));
    assert_eq!(fixture.rows(), rows);
    assert_eq!(fixture.captures(), BTreeMap::new());

    let error = refusal(acquisitions.acquire(
        fixture.request("fail-broken", Broken),
        &AtomicBool::new(false),
    ));
    assert!(
        error.starts_with("Owned capture candidate removed: "),
        "{error}"
    );
    assert_eq!(fixture.rows(), rows);
    assert_eq!(fixture.captures(), BTreeMap::new());

    // Each failure returned its slot and key, and left nothing to repair.
    let (input, _) = Watched::new();
    let op = acquisitions
        .acquire(
            fixture.request("fail-broken", input),
            &AtomicBool::new(false),
        )
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(op.state, State::Activated);
    assert_eq!(acquisitions.recover_before_accepting().unwrap(), 0);
}

#[test]
fn source_acquisition_zero_capacity_is_refused() {
    let fixture = Fixture::open();
    assert_eq!(
        format!("{:#}", fixture.new_acquisitions(0).err().unwrap()),
        "Source acquisition capacity must be positive"
    );
}
