use super::super::*;
use crate::{Queue, QueueClass, auth::Secret, required_units::AdmissionFailure};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Condvar, Mutex},
    thread,
};

fn policy() -> auth::AuthPolicy {
    auth::AuthPolicy {
        registration: auth::RegistrationPolicy::Open,
        session_tenant: auth::SessionTenantPolicy::AccountTenant,
        first_user: auth::FirstUserTenant::NewUser,
    }
}
fn client(capacity: usize) -> PublicationClient {
    PublicationClient {
        shared: Arc::new(Shared {
            queue: Mutex::new(Queue {
                auth_ready: VecDeque::new(),
                next: QueueClass::Normal,
                closed: false,
                restoring: false,
                pending: VecDeque::new(),
            }),
            changed: Condvar::new(),
            capacity,
            job_admission: Mutex::new(None),
            orphaned_source_leases: Mutex::new(Vec::new()),
            job_subscriptions: crate::jobs::JobSubscriptions::new(),
            import_epoch: std::sync::atomic::AtomicU64::new(0),
            import_quota: Mutex::new(None),
            domain_events: crate::domain_events::Feed::new(0),
            restoration: crate::restore::activation::Context::for_test(),
        }),
        policy: policy(),
    }
}
fn credential() -> Credential {
    Credential::Session(Secret::new("publication-test-session".into()))
}
const API_KEY: &str = "ppk_publication-test-key";
fn case_credential(case: &Value) -> Credential {
    match case["credential"].as_str().unwrap() {
        "session" => credential(),
        "api_key" => Credential::ApiKey {
            routed_tenant: case["tenant"].as_str().unwrap().into(),
            secret: Secret::new(API_KEY.into()),
        },
        other => panic!("unknown credential {other}"),
    }
}
fn input(case: &Value) -> Result<PublicationCommand> {
    PublicationCommand::new(
        serde_json::from_value(case["profile"].clone())?,
        serde_json::from_value(case["draft"].clone())?,
        serde_json::from_value(case["request"].clone())?,
        case["key"].as_str().unwrap_or("").into(),
        case_credential(case),
    )
}
fn seed_session(c: &Connection, tenant: &str) {
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(b"publication-test-session"));
    c.execute("INSERT INTO users(id,display_name,is_admin,created_at) VALUES(?1,'Publication test',0,'2026-09-06T00:00:00.000Z')", [tenant]).unwrap();
    c.execute("INSERT INTO sessions(id,user_id,expires_at,provider) VALUES(?1,?2,'2099-01-01T00:00:00.000Z','email')", params![digest,tenant]).unwrap();
}
/// Stores a disposable key the way Node's `createApiKey` does: an HMAC digest in `api_keys_v1`.
fn seed_api_key(c: &Connection, tenant: &str) {
    use hmac::{Hmac, Mac};
    let mut mac =
        Hmac::<sha2::Sha256>::new_from_slice(b"print-partner:settings-api-key:v1").unwrap();
    mac.update(API_KEY.as_bytes());
    let keys = json!([{"id":"key_publication","keyHash":hex::encode(mac.finalize().into_bytes()),"createdAt":"2026-09-06T00:00:00.000Z","lastUsedAt":null,"expiresAt":null,"isActive":true}]);
    c.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,'api_keys_v1',?2)",
        params![tenant, keys.to_string()],
    )
    .unwrap();
}
struct Tokens(u128);
impl RequiredUnitTokenAllocator for Tokens {
    fn allocate(&mut self) -> std::result::Result<[u8; 16], TokenAllocationFailure> {
        self.0 += 1;
        Ok(self.0.to_be_bytes())
    }
}
fn queued(shared: &Shared) -> Envelope {
    let mut queue = shared.queue.lock().unwrap();
    while queue.pending.is_empty() {
        queue = shared.changed.wait(queue).unwrap();
    }
    let work = queue.pending.pop_front().unwrap();
    shared.changed.notify_all();
    work
}
fn run(
    client: &PublicationClient,
    input: PublicationCommand,
    c: &mut Connection,
    tokens: &mut Tokens,
) -> Result<Outcome> {
    thread::scope(|scope| {
        let caller = scope.spawn(|| client.apply(input, &AtomicBool::new(false), Duration::ZERO));
        let Envelope::Publication { command, reply } = queued(&client.shared) else {
            panic!("publication envelope")
        };
        reply
            .send(execute(
                c,
                command,
                tokens,
                crate::writer_admission::Deadline::at_dequeue(),
            ))
            .unwrap();
        caller.join().unwrap()
    })
}
fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../../../../../fixtures/parity/plan-publication/node.json"
    ))
    .unwrap()
}
fn database(data: &Value) -> Connection {
    seeded_database(data, &json!({"seed": {}}))
}
/// Adds the rows Node recorded for a client case to the fixture database.
fn seeded_database(data: &Value, case: &Value) -> Connection {
    let mut state = data["transactions"][0].clone();
    for (table, rows) in case["seed"].as_object().unwrap() {
        state["before"][table]
            .as_array_mut()
            .unwrap()
            .extend(rows.as_array().unwrap().iter().cloned());
    }
    super::transaction::database(&json!({"definitions": data["transactionSchema"]}), &state)
}
fn captured<'a>(data: &'a Value, id: &str) -> &'a Value {
    data["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("captured {id} Node contract"))
}

fn node_not_found_outcome(case: &Value) -> Value {
    assert_eq!(
        case["status"], 404,
        "{}: unrecognized Node status",
        case["id"]
    );
    assert!(
        [
            json!({"detail":"Plan not found","code":"profile_not_found"}),
            json!({"detail":"Plan draft not found","code":"draft_not_found"}),
        ]
        .contains(&case["body"]),
        "{}: unrecognized Node not-found body {}",
        case["id"],
        case["body"]
    );
    json!({"kind":"not_found"})
}

fn database_rows(c: &Connection) -> BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
    let tables = c
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let quoted = format!("\"{}\"", table.replace('"', "\"\""));
            let columns = c
                .prepare(&format!("SELECT * FROM {quoted}"))
                .unwrap()
                .column_count();
            let order = (1..=columns)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let mut statement = c
                .prepare(&format!("SELECT * FROM {quoted} ORDER BY {order}"))
                .unwrap();
            let rows = statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|i| row.get(i))
                        .collect::<rusqlite::Result<Vec<_>>>()
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            (table, rows)
        })
        .collect()
}

/// `expected_owner` is the tenant that owns the requested Plan, if any.
/// `draft_plan` is the Plan that owns the requested draft, if any.
fn assert_client_not_found(
    data: &Value,
    case: &Value,
    expected_owner: Option<&str>,
    draft_plan: Option<i64>,
) {
    let id = case["id"].as_str().unwrap();
    let expected = node_not_found_outcome(case);
    let mut c = seeded_database(data, case);
    let tenant = case["tenant"].as_str().unwrap();
    assert_eq!(case["credential"], "session", "{id}: session credential");
    seed_session(&c, tenant);
    let profile = case["profile"].as_i64().unwrap();
    assert!(profile > 0, "{id}: positive Plan id");
    let owners = c
        .prepare("SELECT tenant_id FROM build_profiles WHERE id=?")
        .unwrap()
        .query_map([profile], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        owners,
        expected_owner
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        "{id}: Plan existence and ownership"
    );
    let plan_lookup = case["body"]["code"] == "profile_not_found";
    if let Some(owner) = expected_owner {
        assert_eq!(
            owner != tenant,
            plan_lookup,
            "{id}: only a Plan lookup case uses another tenant's Plan"
        );
    }
    let draft_plans = c
        .prepare("SELECT profile_id FROM plan_drafts WHERE id=?")
        .unwrap()
        .query_map([case["draft"].as_i64().unwrap()], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert_eq!(
        draft_plans,
        draft_plan.into_iter().collect::<Vec<_>>(),
        "{id}: draft existence and Plan"
    );
    if !plan_lookup {
        assert_ne!(
            draft_plan,
            Some(profile),
            "{id}: draft is absent from the requested Plan"
        );
    }
    let before = database_rows(&c);
    assert!(
        before.contains_key("plan_apply_admissions"),
        "{id}: snapshot includes durable admissions"
    );
    assert!(
        !before["filament_profiles"].is_empty(),
        "{id}: snapshot includes unrelated existing rows"
    );
    let outcome = run(&client(1), input(case).unwrap(), &mut c, &mut Tokens(0))
        .unwrap_or_else(|error| panic!("{id}: publication failed: {error:#}"));
    assert_eq!(
        serde_json::to_value(outcome).unwrap(),
        expected,
        "{id}: complete Rust Outcome matches normalized Node contract"
    );
    let after = database_rows(&c);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>(),
        "{id}: table set unchanged"
    );
    for (table, rows) in before {
        assert_eq!(after[&table], rows, "{id}: {table} rows unchanged");
    }
}

#[test]
fn client_matches_node_not_found_contract_for_missing_plan() {
    let data = fixture();
    let mut case = captured(&data, "other-tenant").clone();
    case["id"] = json!("missing-plan");
    case["tenant"] = json!("default");
    case["profile"] = json!(999_999);
    assert_client_not_found(&data, &case, None, Some(1));
}

#[test]
fn client_matches_node_not_found_contract_for_missing_draft() {
    let data = fixture();
    assert_client_not_found(
        &data,
        captured(&data, "missing-draft"),
        Some("default"),
        None,
    );
}

#[test]
fn client_matches_node_not_found_contract_for_mismatched_draft() {
    let data = fixture();
    let case = captured(&data, "mismatched-draft");
    assert_eq!(case["profile"], 2, "mismatched-draft: second owned Plan");
    assert_client_not_found(&data, case, Some("default"), Some(1));
}

#[test]
fn client_matches_node_apply_route_keys_replay_context_and_error_text() {
    let data = fixture();
    let cases = data["clients"].as_array().unwrap();
    assert_eq!(cases.len(), 11);
    for case in cases {
        // The missing-draft and mismatched-draft tests replay the draft cases.
        if ["missing-draft", "mismatched-draft"].contains(&case["id"].as_str().unwrap()) {
            continue;
        }
        if case["status"] == 404 {
            assert_client_not_found(&data, case, Some("default"), Some(1));
            continue;
        }
        let mut c = database(&data);
        let tenant = case["tenant"].as_str().unwrap();
        if case["credential"] == "api_key" {
            assert_eq!(case["receipt"]["actor_id"], format!("tenant:{tenant}"));
            seed_api_key(&c, tenant);
        } else {
            seed_session(&c, tenant);
        }
        if case["corrupt"] == true {
            c.execute_batch("CREATE TRIGGER reject_publication BEFORE INSERT ON plan_revisions BEGIN SELECT RAISE(ABORT,'Rust-only probe detail'); END").unwrap();
        }
        let client = client(1);
        let command = input(case);
        if case["status"] == 400 {
            assert_eq!(command.err().unwrap().to_string(), case["body"]["detail"]);
            continue;
        }
        let mut tokens = Tokens(0);
        let outcome = run(&client, command.unwrap(), &mut c, &mut tokens);
        if case["status"] == 500 {
            let error = outcome.err().unwrap();
            assert_eq!(error.to_string(), case["body"]["detail"]);
            assert!(
                error
                    .chain()
                    .any(|cause| cause.to_string().contains("Rust-only probe detail"))
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM plan_revisions", [], |r| r
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            continue;
        }
        let outcome = serde_json::to_value(outcome.unwrap()).unwrap();
        let mut receipt = outcome["receipt"].clone();
        let at = receipt["applied_at"].as_str().unwrap().to_owned();
        super::transaction::replace_time(&mut receipt, &at, &data["transactionAt"]);
        assert_eq!(receipt, case["body"], "{}", case["id"]);
        let stored: (String, String) = c
            .query_row(
                "SELECT idempotency_key,actor_id FROM plan_apply_requests",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            json!({"idempotency_key": stored.0, "actor_id": stored.1}),
            case["receipt"],
            "{}",
            case["id"]
        );
        let revision: (String, String) = c
            .query_row(
                "SELECT created_by,accepted_by FROM plan_revisions",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            json!({"created_by": revision.0, "accepted_by": revision.1}),
            case["revision"],
            "{}",
            case["id"]
        );
        let again =
            serde_json::to_value(run(&client, input(case).unwrap(), &mut c, &mut tokens).unwrap())
                .unwrap();
        assert_eq!(again["receipt"], outcome["receipt"]);
        assert_eq!(case["replay"]["body"], receipt);
        assert_eq!(
            c.query_row("SELECT count(*) FROM plan_apply_requests", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}

#[test]
fn admission_matches_reference_cancel_full_stop_and_unknown_outcome() {
    let data = fixture();
    let case = &data["clients"][0];
    let cancelled = client(0);
    let failure = |result: Result<Outcome>| {
        result
            .err()
            .unwrap()
            .downcast::<AdmissionFailure>()
            .unwrap()
    };
    let result =
        failure(cancelled.apply(input(case).unwrap(), &AtomicBool::new(true), Duration::ZERO));
    assert!(matches!(result, AdmissionFailure::Cancelled), "{result:?}");
    assert!(cancelled.shared.queue.lock().unwrap().pending.is_empty());
    let client = client(1);
    let (reply, _receiver) = mpsc::channel();
    client
        .shared
        .queue
        .lock()
        .unwrap()
        .pending
        .push_back(crate::storage_control::queued(
            crate::WriterControl::PageCount,
            reply,
        ));
    assert!(matches!(
        failure(client.apply(
            input(case).unwrap(),
            &AtomicBool::new(false),
            Duration::ZERO
        )),
        AdmissionFailure::QueueFull
    ));
    client.shared.queue.lock().unwrap().closed = true;
    assert!(matches!(
        failure(client.apply(input(case).unwrap(), &AtomicBool::new(true), Duration::ZERO)),
        AdmissionFailure::Stopped
    ));
    client.shared.queue.lock().unwrap().pending.clear();
    client.shared.queue.lock().unwrap().closed = false;
    thread::scope(|scope| {
        let caller = scope.spawn(|| {
            client.apply(
                input(case).unwrap(),
                &AtomicBool::new(false),
                Duration::ZERO,
            )
        });
        drop(queued(&client.shared));
        assert!(matches!(
            failure(caller.join().unwrap()),
            AdmissionFailure::OutcomeUnknown
        ));
    });
}

#[test]
fn cancellation_after_admission_does_not_retract_publication() {
    let data = fixture();
    let case = &data["clients"][0];
    let mut c = database(&data);
    seed_session(&c, "default");
    let client = client(1);
    let cancelled = AtomicBool::new(false);
    thread::scope(|scope| {
        let caller = scope.spawn(|| client.apply(input(case).unwrap(), &cancelled, Duration::ZERO));
        let Envelope::Publication { command, reply } = queued(&client.shared) else {
            panic!("publication envelope")
        };
        cancelled.store(true, Ordering::Release);
        reply
            .send(execute(
                &mut c,
                command,
                &mut Tokens(0),
                crate::writer_admission::Deadline::at_dequeue(),
            ))
            .unwrap();
        assert!(matches!(
            caller.join().unwrap().unwrap(),
            Outcome::Applied { .. }
        ));
    });
}

fn await_publication(
    client: &PublicationClient,
    completed: &mpsc::Receiver<()>,
    wait: Duration,
) -> std::result::Result<(), mpsc::RecvTimeoutError> {
    let result = completed.recv_timeout(wait);
    if result.is_err() {
        client.shared.queue.lock().unwrap().pending.clear();
        client.shared.changed.notify_all();
    }
    result
}

#[test]
fn abandoned_publication_reply_is_released_when_completion_times_out() {
    let data = fixture();
    let case = &data["clients"][0];
    let client = client(1);
    thread::scope(|scope| {
        let (finished, completed) = mpsc::channel();
        let publication = &client;
        let caller = scope.spawn(move || {
            let result = publication.apply(
                input(case).unwrap(),
                &AtomicBool::new(false),
                Duration::ZERO,
            );
            finished.send(()).unwrap();
            result
        });
        let (mut queue, _) = client
            .shared
            .changed
            .wait_timeout_while(
                client.shared.queue.lock().unwrap(),
                Duration::from_secs(5),
                |queue| queue.pending.is_empty(),
            )
            .unwrap();
        let admitted = queue.pending.len();
        queue.closed = true;
        if admitted != 1 {
            queue.pending.clear();
        }
        drop(queue);
        client.shared.changed.notify_all();
        assert_eq!(admitted, 1);
        let result = await_publication(&client, &completed, Duration::ZERO);
        let mut queue = client.shared.queue.lock().unwrap();
        let stranded = !queue.pending.is_empty();
        queue.pending.clear();
        drop(queue);
        let error = caller.join().unwrap().unwrap_err();
        assert!(matches!(
            error.downcast::<AdmissionFailure>().unwrap(),
            AdmissionFailure::OutcomeUnknown
        ));
        assert!(matches!(result, Err(mpsc::RecvTimeoutError::Timeout)));
        assert!(
            !stranded,
            "timeout left the admitted reply sender in the stopped queue"
        );
    });
}

/// Run from `rust` with `cargo test -p pp-storage --features desktop_rebuild
/// owner_installs_schema_dispatches_random_tokens_and_drains_before_stop`.
/// Queue notifications establish admission and shutdown ordering; timeouts only bound failures.
#[test]
fn owner_installs_schema_dispatches_random_tokens_and_drains_before_stop() {
    let data = fixture();
    let case = &data["clients"][0];
    let c = database(&data);
    seed_session(&c, "default");
    c.execute_batch("DROP TABLE plan_apply_admissions; DROP TABLE IF EXISTS domain_events; DROP TABLE IF EXISTS plan_snapshot_headers; DROP TABLE IF EXISTS plan_draft_header_receipts; DROP TABLE IF EXISTS plan_draft_headers; DROP TABLE IF EXISTS build_profile_details; DROP TABLE IF EXISTS printer_send_queue; UPDATE app_settings SET value='35' WHERE key='schema_version' AND tenant_id='default'").unwrap();
    let dir = crate::tests::directory("publication-owner");
    c.backup(
        "main",
        dir.join("print-partner.db"),
        rusqlite::backup::NO_PROGRESS,
    )
    .unwrap();
    let before = std::fs::read(dir.join("print-partner.db")).unwrap();
    let (owner, ready) = WriterOwner::open(&dir, crate::Limits::default()).unwrap();
    assert_eq!(ready.version, crate::schema::CURRENT_VERSION);
    assert_eq!(ready.previous_version, 35);
    let backup = dir.join("backups/pre-schema35-to-48.db");
    assert_eq!(ready.backup.as_ref(), Some(&backup));
    assert_eq!(std::fs::read(&backup).unwrap(), before);
    let saved =
        Connection::open_with_flags(&backup, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert_eq!(
        saved
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        saved
            .query_row(
                "SELECT value FROM app_settings WHERE tenant_id='default' AND key='schema_version'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "35"
    );
    validate_schema(&saved, 35).unwrap();
    drop(saved);
    let client = owner.publication();
    let outcome = thread::scope(|scope| {
        // Dispatch takes this mutex after dequeue. Park a control envelope there so
        // the real apply call remains admitted in the queue until shutdown closes it.
        let checkpoint = client.shared.orphaned_source_leases.lock().unwrap();
        let (reply, control) = mpsc::channel();
        let mut queue = client.shared.queue.lock().unwrap();
        queue.pending.push_back(crate::storage_control::queued(
            crate::WriterControl::PageCount,
            reply,
        ));
        client.shared.changed.notify_all();
        let (queue, _) = client
            .shared
            .changed
            .wait_timeout_while(queue, Duration::from_secs(5), |queue| {
                !queue.pending.is_empty()
            })
            .unwrap();
        assert!(
            queue.pending.is_empty(),
            "control envelope was not dequeued"
        );
        drop(queue);
        let (finished, completed) = mpsc::channel();
        let publication = &client;
        let caller = scope.spawn(move || {
            let result = publication.apply(
                input(case).unwrap(),
                &AtomicBool::new(false),
                Duration::ZERO,
            );
            let _ = finished.send(());
            result
        });
        let (queue, _) = client
            .shared
            .changed
            .wait_timeout_while(
                client.shared.queue.lock().unwrap(),
                Duration::from_secs(5),
                |queue| queue.pending.is_empty(),
            )
            .unwrap();
        assert_eq!(queue.pending.len(), 1);
        assert!(matches!(
            queue.pending.front(),
            Some(Envelope::Publication { .. })
        ));
        drop(queue);
        let shutdown = scope.spawn(move || owner.shutdown());
        let (queue, _) = client
            .shared
            .changed
            .wait_timeout_while(
                client.shared.queue.lock().unwrap(),
                Duration::from_secs(5),
                |queue| !queue.closed,
            )
            .unwrap();
        assert!(queue.closed, "shutdown did not close admission");
        assert_eq!(queue.pending.len(), 1);
        assert!(matches!(
            queue.pending.front(),
            Some(Envelope::Publication { .. })
        ));
        assert!(!caller.is_finished(), "publication finished before release");
        assert!(!shutdown.is_finished(), "shutdown abandoned admitted work");
        drop(queue);
        drop(checkpoint);
        await_publication(&client, &completed, Duration::from_secs(5))
            .expect("admitted publication did not complete during shutdown");
        control
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        let outcome = serde_json::to_value(caller.join().unwrap().unwrap()).unwrap();
        shutdown.join().unwrap().unwrap();
        outcome
    });
    assert_eq!(outcome["kind"], "applied");
    let receipt = &outcome["receipt"];
    for field in [
        "profile_id",
        "draft_id",
        "revision_id",
        "plan_version",
        "draft_lifecycle_version",
        "revision_digest",
    ] {
        assert_eq!(receipt[field], case["body"][field], "{field}");
    }
    assert!(matches!(
        client
            .apply(
                input(case).unwrap(),
                &AtomicBool::new(false),
                Duration::ZERO
            )
            .err()
            .unwrap()
            .downcast::<AdmissionFailure>()
            .unwrap(),
        AdmissionFailure::Stopped
    ));
    let c = Connection::open(dir.join("print-partner.db")).unwrap();
    validate_schema(&c, 37).unwrap();
    let committed: Value = c.query_row(
        "SELECT json_object('profile_id', profile_id, 'draft_id', draft_id, 'revision_id', revision_id, 'plan_version', plan_version, 'draft_lifecycle_version', draft_lifecycle_version, 'revision_digest', revision_digest, 'required_unit_mapping_digest', required_unit_mapping_digest, 'applied_at', applied_at) FROM plan_apply_requests WHERE tenant_id='default' AND idempotency_key='client-key' AND actor_id='default'",
        [],
        |row| row.get::<_, String>(0),
    ).map(|text| serde_json::from_str(&text).unwrap()).unwrap();
    assert_eq!(&committed, receipt);
    let state: (i64, i64, String, i64, i64, i64, i64) = c.query_row(
        "SELECT p.accepted_plan_revision_id, p.accepted_plan_version, d.state, d.lifecycle_version, d.consumed_revision_id, (SELECT count(*) FROM plan_revisions), (SELECT count(*) FROM plan_apply_requests) FROM build_profiles p JOIN plan_drafts d ON d.profile_id=p.id WHERE p.id=1 AND d.id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    ).unwrap();
    assert_eq!(state, (1, 1, "consumed".into(), 1, 1, 1, 1));
    let token: String = c
        .query_row("SELECT token FROM required_units LIMIT 1", [], |r| r.get(0))
        .unwrap();
    ru::model::validate_token(&token).unwrap();
    let (owner, ready) = WriterOwner::open(&dir, crate::Limits::default()).unwrap();
    assert_eq!(ready.previous_version, crate::schema::CURRENT_VERSION);
    assert!(ready.backup.is_none());
    assert_eq!(std::fs::read(&backup).unwrap(), before);
    let replay = serde_json::to_value(
        owner
            .publication()
            .apply(
                input(case).unwrap(),
                &AtomicBool::new(false),
                Duration::ZERO,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(outcome["receipt"], replay["receipt"]);
    owner.shutdown().unwrap();
    drop(c);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn schema_validation_rejects_early_objects_drift_and_orphans() {
    let data = fixture();
    let c = database(&data);
    assert!(validate_schema(&c, 35).is_err());
    validate_schema(&c, 37).unwrap();
    c.execute_batch("DROP TRIGGER trg_plan_apply_admissions_immutable_update")
        .unwrap();
    assert!(validate_schema(&c, 37).is_err());
    c.execute_batch("DROP TABLE plan_apply_admissions").unwrap();
    validate_schema(&c, 35).unwrap();
    assert!(validate_schema(&c, 37).is_err());
    c.execute_batch(include_str!("../schema.sql")).unwrap();
    c.execute_batch("PRAGMA foreign_keys=OFF; INSERT INTO plan_apply_admissions VALUES(999,'plan-apply-request-v1','digest','snapshot',0,NULL,0)").unwrap();
    assert_eq!(
        validate_schema(&c, 37).unwrap_err().to_string(),
        "Orphan Plan Apply admission"
    );
}

#[test]
fn normalized_http_keeps_admission_separate_and_expired_writer_never_publishes() {
    let data = fixture();
    let case = &data["clients"][0];
    let admitted: ApplyRequest = serde_json::from_value(case["request"].clone()).unwrap();
    let mut execution = case["request"].clone();
    execution["expected_lifecycle_version"] = json!(17);
    let input = PublicationCommand::normalized_http(
        serde_json::from_value(case["profile"].clone()).unwrap(),
        serde_json::from_value(case["draft"].clone()).unwrap(),
        admitted.clone(),
        serde_json::from_value(execution.clone()).unwrap(),
        "key".into(),
        credential(),
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(&input.admitted_request).unwrap(),
        serde_json::to_value(admitted).unwrap()
    );
    assert_eq!(
        serde_json::to_value(&input.execution_request).unwrap(),
        execution
    );
    let mut c = database(&data);
    seed_session(&c, "default");
    let error = execute(
        &mut c,
        Command {
            input,
            policy: policy(),
        },
        &mut Tokens(0),
        crate::writer_admission::Deadline::expired(),
    )
    .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<rusqlite::Error>()
            .unwrap()
            .sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy)
    );
    assert!(c.is_autocommit());
    assert_eq!(
        c.query_row("SELECT count(*) FROM plan_apply_requests", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn client_rejects_unauthenticated_publication_and_preserves_normalized_admission() {
    let data = fixture();
    let case = &data["clients"][0];
    let mut c = database(&data);
    let client = client(1);
    let error = run(&client, input(case).unwrap(), &mut c, &mut Tokens(0)).unwrap_err();
    assert!(matches!(
        error.downcast::<auth::AuthFailure>().unwrap(),
        auth::AuthFailure::SessionRequired
    ));
    assert_eq!(
        c.query_row("SELECT count(*) FROM plan_apply_requests", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    seed_session(&c, "default");
    let normalized = || {
        let mut admitted = case["request"].clone();
        admitted["expected_snapshot_digest"] = json!("0".repeat(64));
        PublicationCommand::normalized_http(
            serde_json::from_value(case["profile"].clone()).unwrap(),
            serde_json::from_value(case["draft"].clone()).unwrap(),
            serde_json::from_value(admitted).unwrap(),
            serde_json::from_value(case["request"].clone()).unwrap(),
            "normalized".into(),
            credential(),
        )
        .unwrap()
    };
    let outcome =
        serde_json::to_value(run(&client, normalized(), &mut c, &mut Tokens(0)).unwrap()).unwrap();
    assert_eq!(outcome["kind"], "applied");
    let admitted: String = c
        .query_row(
            "SELECT expected_snapshot_digest FROM plan_apply_admissions",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(admitted, "0".repeat(64));
    let replay =
        serde_json::to_value(run(&client, normalized(), &mut c, &mut Tokens(0)).unwrap()).unwrap();
    assert_eq!(replay["receipt"], outcome["receipt"]);
}
