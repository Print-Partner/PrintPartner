#![cfg(feature = "desktop_rebuild")]
//! Draining Source acquisition workers through SourceAcquisitions::shutdown.
use pp_core::{
    source_acquisition::{
        AcquisitionDrain, AcquisitionReply, CaptureRequest, CapturedFile, SourceAcquisitions,
    },
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
    time::Duration,
};

const QUOTA: u64 = 16 * 1024 * 1024 * 1024;
const BYTES: &[u8] = b"solid triangle";
const BUDGET: Duration = Duration::from_secs(30);

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
            .prefix("pp-source-acquisition-shutdown-")
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

    /// Acquisitions with ingress opened by recovery over the current vault,
    /// returning how many retained captures recovery reconciled.
    fn acquisitions(&self, capacity: usize) -> (SourceAcquisitions, usize) {
        let acquisitions =
            SourceAcquisitions::new(self.owner.as_ref().unwrap(), policy(), QUOTA, capacity)
                .unwrap();
        let reconciled = acquisitions.recover_before_accepting().unwrap();
        (acquisitions, reconciled)
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

/// Input that reports its first read, then blocks until released; a failing
/// one errors once released.
struct Held {
    started: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
    bytes: &'static [u8],
    fails: bool,
}

impl Read for Held {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(started) = self.started.take() {
            started.send(()).unwrap();
            self.release.recv().unwrap();
        }
        if self.fails {
            return Err(std::io::Error::other("input device gone"));
        }
        self.bytes.read(buf)
    }
}

/// Starts an acquisition of `key` on its own thread whose input blocks until
/// the returned sender is used, and waits until it holds a capacity slot. The
/// thread yields the acquisition's reply.
fn hold<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    fixture: &'scope Fixture,
    acquisitions: &'scope SourceAcquisitions,
    key: &'static str,
) -> (
    mpsc::Sender<()>,
    std::thread::ScopedJoinHandle<'scope, anyhow::Result<Operation>>,
) {
    hold_input(scope, fixture, acquisitions, key, false)
}

/// Like [`hold`], with input that fails once released when `fails` is set.
fn hold_input<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    fixture: &'scope Fixture,
    acquisitions: &'scope SourceAcquisitions,
    key: &'static str,
    fails: bool,
) -> (
    mpsc::Sender<()>,
    std::thread::ScopedJoinHandle<'scope, anyhow::Result<Operation>>,
) {
    let (started, held) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let input = Held {
        started: Some(started),
        release: released,
        bytes: BYTES,
        fails,
    };
    let worker = scope.spawn(move || {
        acquisitions
            .acquire(fixture.request(key, input), &AtomicBool::new(false))
            .map_err(anyhow::Error::from)
            .and_then(AcquisitionReply::wait)
    });
    held.recv().unwrap();
    (release, worker)
}

fn refusal(result: Result<AcquisitionReply, impl std::fmt::Display>) -> String {
    match result {
        Ok(reply) => panic!("expected refusal, got {:?}", reply.wait()),
        Err(error) => format!("{error:#}"),
    }
}

/// Waits until `acquisitions` refuses new work because shutdown has begun.
fn await_shutdown(fixture: &Fixture, acquisitions: &SourceAcquisitions) {
    loop {
        let (input, read) = Watched::new();
        let refused = refusal(acquisitions.acquire(
            fixture.request("shutdown-probe", input),
            &AtomicBool::new(false),
        ));
        assert!(!read.load(Ordering::SeqCst));
        if refused == "Source acquisition is shutting down" {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn source_acquisition_shutdown_rejects_new_work() {
    let fixture = Fixture::open();
    let (acquisitions, _) = fixture.acquisitions(1);
    let (input, _) = Watched::new();
    let recorded = acquisitions
        .acquire(
            fixture.request("before-key", input),
            &AtomicBool::new(false),
        )
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(recorded.state, State::Activated);

    assert_eq!(
        acquisitions.shutdown(BUDGET).unwrap(),
        AcquisitionDrain {
            idle: true,
            interrupted: vec![],
            pending: vec![],
        }
    );

    // After shutdown begins, a new key, a recorded key, and recovery are all
    // refused without reading input or writing a capture, journal or job row.
    let rows = fixture.rows();
    let captures = fixture.captures();
    for key in ["after-key", "before-key"] {
        let (input, read) = Watched::new();
        assert_eq!(
            refusal(acquisitions.acquire(fixture.request(key, input), &AtomicBool::new(false))),
            "Source acquisition is shutting down"
        );
        assert!(!read.load(Ordering::SeqCst));
    }
    assert_eq!(
        format!("{:#}", acquisitions.recover_before_accepting().unwrap_err()),
        "Source acquisitions are shut down"
    );
    assert_eq!(fixture.rows(), rows);
    assert_eq!(fixture.captures(), captures);
    assert!(format!("{:#}", fixture.get("after-key").unwrap_err()).contains("not found"));

    // A repeated shutdown reports the same idle drain.
    assert_eq!(
        acquisitions.shutdown(BUDGET).unwrap(),
        AcquisitionDrain {
            idle: true,
            interrupted: vec![],
            pending: vec![],
        }
    );
}

#[test]
fn source_acquisition_shutdown_retains_interrupted_operation() {
    let fixture = Fixture::open();
    let (acquisitions, _) = fixture.acquisitions(1);
    let (drain, reply) = std::thread::scope(|scope| {
        let (release, held) = hold(scope, &fixture, &acquisitions, "kept-key");
        let drain = scope.spawn(|| acquisitions.shutdown(BUDGET));
        await_shutdown(&fixture, &acquisitions);

        // The acquisition accepted before shutdown finishes its capture, is
        // admitted, and then stops instead of settling.
        release.send(()).unwrap();
        (drain.join().unwrap().unwrap(), held.join().unwrap())
    });
    assert_eq!(
        drain,
        AcquisitionDrain {
            idle: true,
            interrupted: vec!["kept-key".into()],
            pending: vec![],
        }
    );
    assert_eq!(
        format!("{:#}", reply.unwrap_err()),
        "Source acquisition interrupted by shutdown; retained for recovery"
    );

    // The operation is recorded as admitted with its frozen capture retained;
    // nothing replays it while this instance is shut down.
    let kept = fixture.get("kept-key").unwrap();
    assert_eq!(kept.state, State::Admitted);
    assert!(!kept.cleanup_settled);
    let rows = fixture.rows();
    let captures = fixture.captures();
    assert!(captures.keys().any(|path| path.ends_with("triangle.stl")));
    assert_eq!(
        format!("{:#}", acquisitions.recover_before_accepting().unwrap_err()),
        "Source acquisitions are shut down"
    );
    assert_eq!(fixture.rows(), rows);
    assert_eq!(fixture.captures(), captures);

    // Deliberate recovery by a new instance settles the retained operation.
    let (recovered, reconciled) = fixture.acquisitions(1);
    assert_eq!(reconciled, 1);
    let settled = fixture.get("kept-key").unwrap();
    assert_eq!(settled.state, State::Activated);
    assert!(settled.cleanup_settled);
    assert_eq!(fixture.captures(), BTreeMap::new());

    // The recorded key now replies with the settled operation.
    let (input, read) = Watched::new();
    let replayed = recovered
        .acquire(fixture.request("kept-key", input), &AtomicBool::new(false))
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(replayed, settled);
    assert!(!read.load(Ordering::SeqCst));
}

#[test]
fn source_acquisition_shutdown_budget_is_shared() {
    let fixture = Fixture::open();
    let (acquisitions, _) = fixture.acquisitions(2);
    std::thread::scope(|scope| {
        let (first_release, first) = hold(scope, &fixture, &acquisitions, "slow-b");
        let (second_release, second) = hold(scope, &fixture, &acquisitions, "slow-a");

        // One budget covers every acquisition; both are still capturing when
        // it runs out, so both are reported pending and nothing is interrupted.
        let budget = Duration::from_millis(200);
        let started = std::time::Instant::now();
        assert_eq!(
            acquisitions.shutdown(budget).unwrap(),
            AcquisitionDrain {
                idle: false,
                interrupted: vec![],
                pending: vec!["slow-a".into(), "slow-b".into()],
            }
        );
        assert!(started.elapsed() >= budget);

        // Once released, each finishes its capture, is admitted, and stops for
        // shutdown instead of settling.
        for (release, held) in [(first_release, first), (second_release, second)] {
            release.send(()).unwrap();
            assert_eq!(
                format!("{:#}", held.join().unwrap().unwrap_err()),
                "Source acquisition interrupted by shutdown; retained for recovery"
            );
        }
    });
    assert_eq!(
        acquisitions.shutdown(BUDGET).unwrap(),
        AcquisitionDrain {
            idle: true,
            interrupted: vec!["slow-a".into(), "slow-b".into()],
            pending: vec![],
        }
    );
    let (_, reconciled) = fixture.acquisitions(1);
    assert_eq!(reconciled, 2);
    assert_eq!(fixture.get("slow-a").unwrap().state, State::Activated);
    assert_eq!(fixture.get("slow-b").unwrap().state, State::Activated);
    assert_eq!(fixture.captures(), BTreeMap::new());
}

#[test]
fn source_acquisition_shutdown_is_scoped_to_its_instance() {
    let fixture = Fixture::open();
    let (running, _) = fixture.acquisitions(1);
    let (other, _) = fixture.acquisitions(1);
    let settled = std::thread::scope(|scope| {
        let (release, held) = hold(scope, &fixture, &running, "own-key");

        // Shutting down another instance on the same storage neither refuses
        // this instance's work nor stops its worker.
        assert_eq!(
            other.shutdown(BUDGET).unwrap(),
            AcquisitionDrain {
                idle: true,
                interrupted: vec![],
                pending: vec![],
            }
        );
        release.send(()).unwrap();
        held.join().unwrap().unwrap()
    });
    assert_eq!(settled.state, State::Activated);
    assert!(settled.cleanup_settled);
    assert_eq!(fixture.get("own-key").unwrap(), settled);
    assert_eq!(fixture.captures(), BTreeMap::new());

    let (input, _) = Watched::new();
    let second = running
        .acquire(
            fixture.request("own-second", input),
            &AtomicBool::new(false),
        )
        .unwrap()
        .wait()
        .unwrap();
    assert_eq!(second.state, State::Activated);
    assert_eq!(
        running.shutdown(BUDGET).unwrap(),
        AcquisitionDrain {
            idle: true,
            interrupted: vec![],
            pending: vec![],
        }
    );
}

#[test]
fn source_acquisition_shutdown_pending_failure_is_not_interrupted() {
    let fixture = Fixture::open();
    let (acquisitions, _) = fixture.acquisitions(1);
    let rows = fixture.rows();
    std::thread::scope(|scope| {
        let (release, held) = hold_input(scope, &fixture, &acquisitions, "failed-key", true);
        assert_eq!(
            acquisitions.shutdown(Duration::from_millis(200)).unwrap(),
            AcquisitionDrain {
                idle: false,
                interrupted: vec![],
                pending: vec!["failed-key".into()],
            }
        );

        // Its input fails after the budget ran out: the failure is reported as
        // it is, the candidate is removed, and nothing is admitted.
        release.send(()).unwrap();
        let error = format!("{:#}", held.join().unwrap().unwrap_err());
        assert!(
            error.starts_with("Owned capture candidate removed: "),
            "{error}"
        );
    });
    assert_eq!(
        acquisitions.shutdown(BUDGET).unwrap(),
        AcquisitionDrain {
            idle: true,
            interrupted: vec![],
            pending: vec![],
        }
    );
    assert_eq!(fixture.rows(), rows);
    assert_eq!(fixture.captures(), BTreeMap::new());
    assert!(format!("{:#}", fixture.get("failed-key").unwrap_err()).contains("not found"));
}
