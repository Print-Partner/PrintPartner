#![cfg(feature = "desktop_rebuild")]
//! GRE-1088: Checkoff writes its domain event in the same transaction, reads
//! authenticate, and a restart starts a new cursor generation.
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    checkoff_progress::{Basis, Request},
    domain_events::{Cursor, DomainEventsClient, Read, ResyncReason, Waker},
    read_model::{AcceptedRead, Credential},
};
use rusqlite::Connection;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(5);

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

fn token(n: u8) -> String {
    format!("ppu_{n:032x}")
}

struct Fixture {
    root: PathBuf,
    owner: Option<WriterOwner>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-domain-events-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        std::fs::create_dir_all(root.join("repos/1/revisions/fixture")).unwrap();
        std::fs::write(
            root.join("print-partner.db"),
            include_bytes!("fixtures/required-units/unchanged.db"),
        )
        .unwrap();
        let owner = WriterOwner::open(&root, Limits::default()).unwrap().0;
        Self {
            root,
            owner: Some(owner),
        }
    }

    fn owner(&self) -> &WriterOwner {
        self.owner.as_ref().unwrap()
    }

    fn reopen(&mut self) {
        self.owner.take().unwrap().shutdown().unwrap();
        self.owner = Some(WriterOwner::open(&self.root, Limits::default()).unwrap().0);
    }

    fn events(&self) -> DomainEventsClient {
        self.owner().domain_events(policy()).unwrap()
    }

    fn read(&self, after: Option<&Cursor>) -> Read {
        self.events()
            .read(session(), after.cloned(), &AtomicBool::new(false), WAIT)
            .unwrap()
    }

    fn position(&self) -> Cursor {
        match self.read(None) {
            Read::Events { events, position } if events.is_empty() => position,
            other => panic!("expected a position, got {other:?}"),
        }
    }

    fn basis(&self) -> Basis {
        let batch = self
            .owner()
            .accepted_reads()
            .unwrap()
            .read(session(), &[1], &AtomicBool::new(false), WAIT)
            .unwrap();
        match batch.builds.into_iter().next().unwrap().accepted {
            AcceptedRead::Ready { snapshot } => Basis::from(&*snapshot),
            other => panic!("fixture is not ready: {other:?}"),
        }
    }

    fn complete(&self, n: u8) -> u16 {
        self.owner()
            .checkoff_progress()
            .unwrap()
            .apply(
                session(),
                Request::Completion {
                    expected: self.basis(),
                    token: token(n),
                    completed: true,
                },
                &AtomicBool::new(false),
                WAIT,
            )
            .unwrap()
            .status
    }

    fn sql(&self, sql: &str) {
        Connection::open(self.root.join("print-partner.db"))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    fn completed_units(&self) -> i64 {
        Connection::open(self.root.join("print-partner.db"))
            .unwrap()
            .query_row(
                "SELECT count(*) FROM print_progress WHERE completed=1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            owner.shutdown().unwrap();
        }
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn domain_events_checkoff_commits_one_event_with_its_change() {
    let fixture = Fixture::new();
    let start = fixture.position();
    assert_eq!(fixture.complete(2), 200);
    let Read::Events { events, position } = fixture.read(Some(&start)) else {
        panic!("expected events");
    };
    assert_eq!(events.len(), 1);
    let event = serde_json::to_value(&events[0]).unwrap();
    assert_eq!(event["type"], "checkoff.progress_changed");
    assert_eq!(event["schema_version"], 1);
    assert_eq!(event["aggregate_type"], "build");
    assert_eq!(event["aggregate_id"], "1");
    assert_eq!(event["payload"], serde_json::json!({ "part_id": 1 }));
    assert_eq!(event["operation_id"].as_str().unwrap().len(), 32);
    assert_eq!(event["cursor"], position.to_string());
    assert_eq!(
        fixture.read(Some(&position)),
        Read::Events {
            events: Vec::new(),
            position: position.clone(),
        }
    );
}

#[test]
fn domain_events_refused_or_failed_checkoff_writes_no_event() {
    let fixture = Fixture::new();
    let start = fixture.position();
    assert_eq!(fixture.complete(99), 404, "unknown token");
    let before = fixture.completed_units();
    fixture.sql(
        "CREATE TRIGGER refuse_event BEFORE INSERT ON domain_events BEGIN SELECT RAISE(ABORT,'probe'); END",
    );
    assert_eq!(fixture.complete(2), 500);
    assert_eq!(
        fixture.completed_units(),
        before,
        "a failed event write rolls back the change"
    );
    fixture.sql("DROP TRIGGER refuse_event");
    assert_eq!(
        fixture.read(Some(&start)),
        Read::Events {
            events: Vec::new(),
            position: start.clone(),
        }
    );
}

#[test]
fn domain_events_unauthenticated_read_is_refused() {
    let fixture = Fixture::new();
    let wrong = Credential::Session(Secret::new("not-a-session".into()));
    assert!(
        fixture
            .events()
            .read(wrong, None, &AtomicBool::new(false), WAIT)
            .is_err()
    );
}

#[test]
fn domain_events_restart_starts_a_new_generation() {
    let mut fixture = Fixture::new();
    let start = fixture.position();
    assert_eq!(fixture.complete(2), 200);
    fixture.reopen();
    match fixture.read(Some(&start)) {
        Read::Resync { reason, position } => {
            assert_eq!(reason, ResyncReason::GenerationChanged);
            assert_eq!(position, fixture.position());
        }
        other => panic!("expected a resync, got {other:?}"),
    }
}

#[test]
fn domain_events_subscriber_is_woken_by_commits_not_by_reads() {
    let fixture = Fixture::new();
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = woken.clone();
    let waker: Waker = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    fixture.events().subscribe(&waker).unwrap();
    let start = fixture.position();
    fixture.read(Some(&start));
    assert_eq!(woken.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.complete(2), 200);
    // The writer wakes subscribers after it replies to the write.
    let deadline = std::time::Instant::now() + WAIT;
    while woken.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(woken.load(Ordering::SeqCst) >= 1);
}
