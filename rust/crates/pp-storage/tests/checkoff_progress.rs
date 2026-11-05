#![cfg(feature = "desktop_rebuild")]
use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    checkoff_progress::{Basis, Import, Request, Response},
    read_model::{AcceptedRead, Credential, Snapshot},
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const WAIT: Duration = Duration::from_secs(5);

fn session() -> Credential {
    Credential::Session(Secret::new("required-unit-fixture-secret".into()))
}

fn token(n: u8) -> String {
    format!("ppu_{n:032x}")
}

// Profile 1 owns Part 1 (tokens 1-3), Part 2 (token 4, excluded) and Part 3
// (tokens 5-6). Profile 2 belongs to another tenant.
struct Fixture {
    root: PathBuf,
    owner: Option<WriterOwner>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "pp-checkoff-{}",
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

    fn snapshot(&self) -> Snapshot {
        let batch = self
            .owner()
            .accepted_reads()
            .unwrap()
            .read(session(), &[1], &AtomicBool::new(false), WAIT)
            .unwrap();
        match batch.builds.into_iter().next().unwrap().accepted {
            AcceptedRead::Ready { snapshot } => *snapshot,
            other => panic!("fixture is not ready: {other:?}"),
        }
    }

    fn basis(&self) -> Basis {
        Basis::from(&self.snapshot())
    }

    fn apply(&self, request: Request) -> Response {
        self.owner()
            .checkoff_progress()
            .unwrap()
            .apply(session(), request, &AtomicBool::new(false), WAIT)
            .unwrap()
    }

    fn sql(&self, sql: &str) {
        Connection::open(self.root.join("print-partner.db"))
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }

    /// `part_id:unit_index:completed:assembled` for every progress row.
    fn progress(&self) -> Vec<String> {
        let connection = Connection::open(self.root.join("print-partner.db")).unwrap();
        let mut statement = connection
            .prepare("SELECT part_id,unit_index,completed,assembled FROM print_progress ORDER BY part_id,unit_index")
            .unwrap();
        statement
            .query_map([], |r| {
                Ok(format!(
                    "{}:{}:{}:{}",
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn complete(&self, n: u8, completed: bool) -> Value {
        json(self.apply(Request::Completion {
            expected: self.basis(),
            token: token(n),
            completed,
        }))
    }

    fn assemble(&self, n: u8, assembled: bool) -> Value {
        json(self.apply(Request::Assembly {
            expected: self.basis(),
            token: token(n),
            assembled,
        }))
    }

    fn import(&self, rows: Value) -> Value {
        let request = Import::parse(json!({ "expected": self.basis(), "rows": rows })).unwrap();
        json(self.apply(Request::Import(request)))
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

fn json(response: Response) -> Value {
    serde_json::to_value(response).unwrap()
}

fn failure(status: u16, detail: &str) -> Value {
    json!({ "status": status, "body": { "detail": detail } })
}

fn coded(status: u16, detail: &str, code: &str) -> Value {
    json!({ "status": status, "body": { "detail": detail, "code": code } })
}

const UNTOUCHED: [&str; 6] = [
    "1:0:0:0", "1:1:0:0", "1:2:0:0", "2:0:0:0", "3:0:0:0", "3:1:0:0",
];

#[test]
fn completing_a_unit_completes_every_lower_unit() {
    let f = Fixture::new();
    assert_eq!(
        f.complete(2, true),
        json!({ "status": 200, "body": {
            "part_id": 1, "printed_count": 2,
            "print_units": [true, true, false], "assembled_units": [false, false, false],
            "missing": true,
        }})
    );
    assert_eq!(f.progress()[..3], ["1:0:1:0", "1:1:1:0", "1:2:0:0"]);
}

#[test]
fn uncompleting_a_unit_clears_it_and_every_higher_unit_including_assembly() {
    let f = Fixture::new();
    f.complete(3, true);
    f.assemble(1, true);
    f.assemble(3, true);
    assert_eq!(
        f.complete(2, false),
        json!({ "status": 200, "body": {
            "part_id": 1, "printed_count": 1,
            "print_units": [true, false, false], "assembled_units": [true, false, false],
            "missing": true,
        }})
    );
    assert_eq!(f.progress()[..3], ["1:0:1:1", "1:1:0:0", "1:2:0:0"]);
}

#[test]
fn assembling_an_unprinted_unit_changes_nothing() {
    let f = Fixture::new();
    assert_eq!(
        f.assemble(5, true),
        json!({ "status": 200, "body": {
            "part_id": 3, "assembled_count": 0, "assembled_units": [false, false],
        }})
    );
    assert_eq!(f.progress(), UNTOUCHED);
    f.complete(5, true);
    assert_eq!(
        f.assemble(5, true),
        json!({ "status": 200, "body": {
            "part_id": 3, "assembled_count": 1, "assembled_units": [true, false],
        }})
    );
}

#[test]
fn stale_and_archived_plans_refuse_without_writes() {
    let f = Fixture::new();
    let mut stale = f.basis();
    stale.plan_version += 1;
    assert_eq!(
        json(f.apply(Request::Completion {
            expected: stale.clone(),
            token: token(1),
            completed: true,
        })),
        failure(409, "Accepted Plan changed; reload and retry")
    );
    assert_eq!(
        json(
            f.apply(Request::Import(
                Import::parse(json!({
                    "expected": stale, "rows": [{ "part_id": 1, "printed_count": 1 }],
                }))
                .unwrap()
            ))
        ),
        coded(
            409,
            "Accepted Plan changed; reload and retry",
            "stale_accepted_plan"
        )
    );
    f.sql("UPDATE build_profiles SET archived_at='2026-10-17T00:00:00.000Z' WHERE id=1");
    assert_eq!(
        f.complete(1, true),
        failure(409, "Archived Plan Progress cannot be changed")
    );
    assert_eq!(
        f.import(json!([{ "part_id": 1, "printed_count": 1 }])),
        coded(
            409,
            "Archived Plan Progress cannot be changed",
            "plan_archived"
        )
    );
    assert_eq!(f.progress(), UNTOUCHED);
}

#[test]
fn unknown_tokens_and_foreign_plans_are_not_found() {
    let f = Fixture::new();
    assert_eq!(f.complete(9, true), failure(404, "Part unit not found"));
    let mut foreign = f.basis();
    foreign.profile_id = 2;
    assert_eq!(
        json(f.apply(Request::Completion {
            expected: foreign.clone(),
            token: token(1),
            completed: true,
        })),
        failure(404, "Part unit not found")
    );
    // Node throws a plain error here and answers 500. The route already answers
    // 404 for a Plan it does not serve, so the port does the same.
    assert_eq!(
        json(
            f.apply(Request::Import(
                Import::parse(json!({
                    "expected": foreign, "rows": [{ "part_id": 1, "printed_count": 1 }],
                }))
                .unwrap()
            ))
        ),
        coded(404, "Plan or Part not found", "progress_target_not_found")
    );
    assert_eq!(f.progress(), UNTOUCHED);
}

#[test]
fn malformed_unit_requests_are_refused_before_admission() {
    let f = Fixture::new();
    let invalid = failure(400, "Invalid Accepted Plan progress request");
    let mut basis = f.basis();
    assert_eq!(
        json(f.apply(Request::Completion {
            expected: basis.clone(),
            token: "ppu_ABC".into(),
            completed: true,
        })),
        invalid
    );
    basis.plan_revision_digest = "A".repeat(64);
    assert_eq!(
        json(f.apply(Request::Assembly {
            expected: basis,
            token: token(1),
            assembled: true,
        })),
        invalid
    );
}

#[test]
fn import_sets_printed_prefixes_and_counts_changed_parts() {
    let f = Fixture::new();
    f.complete(1, true);
    f.assemble(1, true);
    assert_eq!(
        f.import(json!([
            { "part_id": 1, "printed_count": 1 },
            { "part_id": 3, "printed_count": 2 },
        ])),
        json!({ "status": 200, "body": { "updated_parts": 1 } })
    );
    assert_eq!(
        f.progress(),
        [
            "1:0:1:1", "1:1:0:0", "1:2:0:0", "2:0:0:0", "3:0:1:0", "3:1:1:0"
        ]
    );
    assert_eq!(
        f.import(json!([{ "part_id": 1, "printed_count": 0 }])),
        json!({ "status": 200, "body": { "updated_parts": 1 } })
    );
    assert_eq!(f.progress()[0], "1:0:0:0");
}

#[test]
fn import_checks_every_row_before_writing() {
    let f = Fixture::new();
    assert_eq!(
        f.import(json!([
            { "part_id": 1, "printed_count": 3 },
            { "part_id": 3, "printed_count": 3 },
        ])),
        coded(422, "Printed counts are invalid", "invalid_rows")
    );
    assert_eq!(
        f.import(json!([
            { "part_id": 1, "printed_count": 3 },
            { "part_id": 99, "printed_count": 0 },
        ])),
        coded(404, "Plan or Part not found", "progress_target_not_found")
    );
    assert_eq!(f.progress(), UNTOUCHED);
}

#[test]
fn import_parsing_follows_the_node_contract() {
    let f = Fixture::new();
    let basis = serde_json::to_value(f.basis()).unwrap();
    let invalid = coded(400, "Request is invalid", "invalid_request");
    for rows in [
        json!([]),
        json!([{ "part_id": 1, "printed_count": 1 }, { "part_id": 1, "printed_count": 0 }]),
        json!([{ "part_id": 0, "printed_count": 1 }]),
        json!([{ "part_id": 1, "printed_count": 10_001 }]),
        json!([{ "part_id": 1, "printed_count": 1.5 }]),
        json!([{ "part_id": 1, "printed_count": 1, "extra": true }]),
    ] {
        let parsed = Import::parse(json!({ "expected": basis, "rows": rows }));
        assert_eq!(json(parsed.unwrap_err()), invalid, "{rows}");
    }
    let parsed = Import::parse(json!({
        "expected": basis, "rows": [{ "part_id": 1.0, "printed_count": 2.0 }],
    }))
    .unwrap();
    assert_eq!(
        (parsed.rows[0].part_id, parsed.rows[0].printed_count),
        (1, 2)
    );
}

#[test]
fn part_routes_resolve_unit_indices_with_node_responses() {
    let f = Fixture::new();
    let progress =
        |part_id: i64, body: Value| json(f.apply(Request::CompletionCoordinate { part_id, body }));
    assert_eq!(
        progress(1, json!({ "unit_index": 1, "completed": true }))["body"]["print_units"],
        json!([true, true, false])
    );
    assert_eq!(
        json(f.apply(Request::AssemblyCoordinate {
            part_id: 1,
            body: json!({ "unit_index": 0, "assembled": true }),
        }))["body"],
        json!({ "part_id": 1, "assembled_count": 1, "assembled_units": [true, false, false] })
    );
    assert_eq!(
        progress(1, json!({ "completed": true })),
        failure(400, "unit_index and completed required")
    );
    assert_eq!(
        progress(1, json!({ "unit_index": -1, "completed": true })),
        failure(
            400,
            "unit_index must be a non-negative integer and completed a boolean"
        )
    );
    assert_eq!(
        progress(1, json!({ "unit_index": 0, "completed": "yes" })),
        failure(
            400,
            "unit_index must be a non-negative integer and completed a boolean"
        )
    );
    assert_eq!(
        progress(1, json!({ "unit_index": 3, "completed": true })),
        failure(400, "unit_index out of range")
    );
    assert_eq!(
        progress(99, json!({ "unit_index": 0, "completed": true })),
        failure(404, "Part not found")
    );
}

#[test]
fn a_plan_needing_compatibility_repair_refuses_every_write() {
    let f = Fixture::new();
    let basis = f.basis();
    f.sql("UPDATE build_profiles SET accepted_plan_revision_id=NULL WHERE id=1");
    assert_eq!(
        json(f.apply(Request::CompletionCoordinate {
            part_id: 1,
            body: json!({ "unit_index": 0, "completed": true }),
        })),
        failure(409, "Accepted Plan requires compatibility repair")
    );
    assert_eq!(
        json(
            f.apply(Request::Import(
                Import::parse(json!({
                    "expected": basis, "rows": [{ "part_id": 1, "printed_count": 1 }],
                }))
                .unwrap()
            ))
        ),
        coded(
            409,
            "Accepted Plan state is unavailable",
            "accepted_state_unavailable"
        )
    );
    assert_eq!(f.progress(), UNTOUCHED);
}

#[test]
fn missing_progress_rows_are_an_integrity_failure() {
    let f = Fixture::new();
    f.sql("DELETE FROM print_progress WHERE part_id=3 AND unit_index=1");
    assert_eq!(
        f.complete(1, true),
        failure(500, "Accepted Plan data is inconsistent")
    );
    assert_eq!(
        f.progress(),
        ["1:0:0:0", "1:1:0:0", "1:2:0:0", "2:0:0:0", "3:0:0:0"]
    );
}

#[test]
fn a_refused_request_leaves_api_key_usage_unrecorded() {
    let f = Fixture::new();
    let auth::Outcome::KeyCreated { key, info } = f
        .owner()
        .auth(auth::FirstUserTenant::NewUser)
        .submit(
            auth::Request::CreateKey {
                session: Secret::new("required-unit-fixture-secret".into()),
            },
            Arc::new(AtomicBool::new(false)),
            WAIT,
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    else {
        panic!("expected a created key");
    };
    let connection = Connection::open(f.root.join("print-partner.db")).unwrap();
    let last_used = || {
        connection
            .query_row(
                "SELECT json_extract(entry.value,'$.lastUsedAt') FROM app_settings, json_each(app_settings.value) entry
                 WHERE app_settings.tenant_id='default' AND app_settings.key='api_keys_v1' AND json_extract(entry.value,'$.id')=?1",
                [&info.id],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap()
    };
    let response = f
        .owner()
        .checkoff_progress()
        .unwrap()
        .apply(
            Credential::ApiKey {
                routed_tenant: "default".into(),
                secret: key,
            },
            Request::CompletionCoordinate {
                part_id: 99,
                body: json!({ "unit_index": 0, "completed": true }),
            },
            &AtomicBool::new(false),
            WAIT,
        )
        .unwrap();
    assert_eq!(json(response), failure(404, "Part not found"));
    assert_eq!(last_used(), None);
}

#[test]
fn writes_survive_a_restart() {
    let mut f = Fixture::new();
    f.complete(6, true);
    f.owner.take().unwrap().shutdown().unwrap();
    f.owner = Some(WriterOwner::open(&f.root, Limits::default()).unwrap().0);
    let snapshot = f.snapshot();
    let part = snapshot
        .parts
        .iter()
        .find(|p| p.projection_part_id == 3)
        .unwrap();
    assert!(part.units.iter().all(|u| u.completed));
}

// GRE-1088: each Checkoff write commits its domain event in the same
// transaction; reads authenticate; a restart starts a new cursor generation.
use pp_storage::domain_events::{Cursor, DomainEventsClient, Read, ResyncReason, Waker};
use std::sync::atomic::{AtomicUsize, Ordering};

fn events(f: &Fixture) -> DomainEventsClient {
    f.owner().domain_events().unwrap()
}

fn read_events(
    f: &Fixture,
    credential: Credential,
    after: Option<&Cursor>,
) -> anyhow::Result<Read> {
    events(f).read(credential, after.cloned(), &AtomicBool::new(false), WAIT)
}

fn position(f: &Fixture) -> Cursor {
    match read_events(f, session(), None).unwrap() {
        Read::Events { events, position } if events.is_empty() => position,
        other => panic!("expected a position, got {other:?}"),
    }
}

fn nothing_after(position: &Cursor) -> Read {
    Read::Events {
        events: Vec::new(),
        position: position.clone(),
    }
}

#[test]
fn domain_events_checkoff_commits_one_event_with_its_change() {
    let f = Fixture::new();
    let start = position(&f);
    assert_eq!(f.complete(2, true)["status"], 200);
    let Read::Events { events, position } = read_events(&f, session(), Some(&start)).unwrap()
    else {
        panic!("expected events");
    };
    let [event] = &events[..] else {
        panic!("expected one event, got {events:?}");
    };
    let event = serde_json::to_value(event).unwrap();
    assert_eq!(event["type"], "checkoff.progress_changed");
    assert_eq!(event["schema_version"], 1);
    assert_eq!(
        (&event["aggregate_type"], &event["aggregate_id"]),
        (&json!("build"), &json!("1"))
    );
    assert_eq!(event["payload"], json!({ "part_id": 1 }));
    assert_eq!(event["operation_id"].as_str().unwrap().len(), 32);
    assert_eq!(event["cursor"], position.to_string());
    assert_eq!(
        read_events(&f, session(), Some(&position)).unwrap(),
        nothing_after(&position)
    );
}

#[test]
fn domain_events_refused_or_failed_checkoff_writes_no_event() {
    let f = Fixture::new();
    let start = position(&f);
    assert_eq!(f.complete(9, true)["status"], 404);
    f.sql("CREATE TRIGGER refuse_event BEFORE INSERT ON domain_events BEGIN SELECT RAISE(ABORT,'probe'); END");
    assert_eq!(f.complete(2, true)["status"], 500);
    assert_eq!(
        f.progress(),
        UNTOUCHED,
        "a failed event write rolls back the change"
    );
    f.sql("DROP TRIGGER refuse_event");
    assert_eq!(
        read_events(&f, session(), Some(&start)).unwrap(),
        nothing_after(&start)
    );
}

#[test]
fn domain_events_unauthenticated_read_is_refused() {
    let f = Fixture::new();
    let wrong = Credential::Session(Secret::new("not-a-session".into()));
    assert!(read_events(&f, wrong, None).is_err());
}

#[test]
fn domain_events_restart_starts_a_new_generation() {
    let mut f = Fixture::new();
    let start = position(&f);
    f.complete(2, true);
    let previous = f.owner.take().expect("open owner");
    previous.shutdown().unwrap();
    let (reopened, _) = WriterOwner::open(&f.root, Limits::default()).unwrap();
    f.owner.replace(reopened);
    let Read::Resync {
        reason,
        position: after,
    } = read_events(&f, session(), Some(&start)).unwrap()
    else {
        panic!("expected a resync");
    };
    assert_eq!(reason, ResyncReason::GenerationChanged);
    assert_eq!(after, position(&f));
}

#[test]
fn domain_events_subscriber_is_woken_by_commits_not_by_reads() {
    let f = Fixture::new();
    let woken = Arc::new(AtomicUsize::new(0));
    let counter = woken.clone();
    let waker: Waker = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    events(&f).subscribe(&waker).unwrap();
    let start = position(&f);
    read_events(&f, session(), Some(&start)).unwrap();
    assert_eq!(woken.load(Ordering::SeqCst), 0);
    f.complete(2, true);
    // The writer wakes subscribers after it replies to the write.
    let deadline = std::time::Instant::now() + WAIT;
    while woken.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(woken.load(Ordering::SeqCst) >= 1);
}

#[test]
fn domain_events_checkoff_that_changes_no_row_writes_no_event() {
    let f = Fixture::new();
    let start = position(&f);
    assert_eq!(f.assemble(5, true)["status"], 200);
    let unchanged = f.import(json!([{ "part_id": 1, "printed_count": 0 }]));
    assert_eq!(unchanged["body"]["updated_parts"], 0);
    assert_eq!(
        read_events(&f, session(), Some(&start)).unwrap(),
        nothing_after(&start)
    );
    for _ in 0..2 {
        assert_eq!(f.complete(2, true)["status"], 200);
    }
    let Read::Events { events, .. } = read_events(&f, session(), Some(&start)).unwrap() else {
        panic!("expected events");
    };
    assert_eq!(events.len(), 1, "repeating a completion is not a change");
}
