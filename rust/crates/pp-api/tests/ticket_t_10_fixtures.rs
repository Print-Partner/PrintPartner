#![cfg(feature = "desktop_rebuild")]
//! T-10-fixtures: copied Node auth fixtures replayed through the isolated
//! parity runner in `pp_api::fixtures`, comparing responses and state graphs.
mod ticket_t_10_fixtures {
    use pp_api::{
        auth::{AuthHttpConfig, CookieTransport, ProviderClient, ResetMailer, auth_router},
        fixtures::{
            Correction, CorrectionKind, Exchange, Ledger, ParityError, Report, Sandbox, Served,
            StateGraph,
        },
    };
    use pp_storage::{
        Limits, WriterOwner,
        auth::{
            AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy,
            rebuild::AuthLifetimes,
        },
    };
    use reqwest::Method;
    use rusqlite::params;
    use serde::Deserialize;
    use serde_json::{Value, json};

    const SESSIONS: &str = "print-partner.db/sessions";

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LoginFixture {
        generated: String,
        source: String,
        configuration: Value,
        account: Value,
        cases: Vec<LoginCase>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct LoginCase {
        id: String,
        registration_open: bool,
        seed_account: bool,
        request: LoginRequest,
        node: Exchange,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LoginRequest {
        path: String,
        body: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SessionFixture {
        generated: String,
        sources: Vec<String>,
        seed: Value,
        cases: Vec<Transition>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Transition {
        name: String,
        method: String,
        path: String,
        credential: String,
        storage: String,
        expected: SessionExpected,
        node_set_cookie: Vec<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SessionExpected {
        status: u16,
        body: Value,
        clearing_cookies: Vec<Value>,
        session_rows: usize,
    }

    fn login_fixture() -> LoginFixture {
        let fixture: LoginFixture = serde_json::from_str(include_str!(
            "../../../../fixtures/parity/auth-login/node.json"
        ))
        .unwrap();
        assert!(fixture.generated.contains("auth-login/generate.mjs"));
        assert_eq!(fixture.source, "web/apps/server/src/routes/auth.ts");
        assert_eq!(fixture.configuration["singleUserAuth"], json!(true));
        // Node and Rust both send a same-origin `Origin` header.
        assert_eq!(fixture.configuration["sameOriginRequests"], json!(true));
        assert_eq!(fixture.cases.len(), 14);
        fixture
    }

    fn session_fixture() -> SessionFixture {
        let fixture: SessionFixture = serde_json::from_str(include_str!(
            "../../../../fixtures/parity/auth-session/node.json"
        ))
        .unwrap();
        assert!(fixture.generated.contains("auth-session/generate.mjs"));
        assert_eq!(fixture.sources.len(), 2);
        assert_eq!(fixture.cases.len(), 7);
        fixture
    }

    fn invalid_json(case: &'static str, reason: &'static str) -> Correction {
        Correction {
            case,
            kind: CorrectionKind::Security,
            reason,
            rust: Exchange {
                status: 400,
                body: json!({"detail": "Invalid JSON body"}),
                set_cookie: vec![],
            },
        }
    }

    /// Rust's strict JSON-object boundary, documented in auth-login/README.md.
    fn login_corrections() -> Vec<Correction> {
        vec![
            invalid_json(
                "array-body",
                "arrays are not JSON objects; Rust refuses them before credential checks",
            ),
            invalid_json(
                "null-body",
                "Node answers 500 with an internal TypeError message; Rust does not expose it",
            ),
            invalid_json(
                "malformed-json",
                "Rust does not expose Fastify parser codes or messages",
            ),
        ]
    }

    fn cleared(case: &'static str) -> Correction {
        Correction {
            case,
            kind: CorrectionKind::Security,
            reason: "Rust marks the clearing cookie HttpOnly and expires it with Max-Age=0 only",
            rust: Exchange {
                status: 200,
                body: json!({"ok": true}),
                set_cookie: vec!["pp_session=; Max-Age=0; Path=/; HttpOnly; SameSite=Lax".into()],
            },
        }
    }

    /// Logout clearing-cookie headers, documented in auth-session/README.md.
    fn session_corrections() -> Vec<Correction> {
        vec![
            cleared("absent_logout"),
            cleared("logout"),
            cleared("repeated_logout"),
        ]
    }

    /// Decodes a recorded Node `Set-Cookie` header into the fields that
    /// `clearing_cookies` records; any other attribute fails the test.
    fn clearing_cookie(header: &str) -> Value {
        let mut parts = header.split(';').map(str::trim);
        let (name, value) = parts.next().unwrap().split_once('=').unwrap();
        let mut cookie = json!({
            "name": name,
            "value": value,
            "path": null,
            "domain": null,
            "same_site": null,
            "secure": false,
            "expires_immediately": false,
        });
        for attribute in parts {
            let (key, value) = attribute.split_once('=').unwrap_or((attribute, ""));
            match key.to_ascii_lowercase().as_str() {
                "path" => cookie["path"] = json!(value),
                "domain" => cookie["domain"] = json!(value),
                "samesite" => cookie["same_site"] = json!(value.to_ascii_lowercase()),
                "secure" => cookie["secure"] = json!(true),
                "max-age" if value == "0" => cookie["expires_immediately"] = json!(true),
                "expires" if value == "Thu, 01 Jan 1970 00:00:00 GMT" => {
                    cookie["expires_immediately"] = json!(true)
                }
                _ => panic!("{header}: unexpected attribute {attribute}"),
            }
        }
        cookie
    }

    fn print_report(corpus: &str, report: &Report) {
        println!(
            "{corpus}: {} exact, {} corrected",
            report.matched,
            report.corrected.len()
        );
        for (case, kind) in &report.corrected {
            println!("{corpus}: {case} corrected ({kind:?})");
        }
    }

    async fn login_case(case: &LoginCase, account: &Value) -> (Exchange, Vec<String>) {
        let sandbox = Sandbox::new("t10-login").unwrap();
        let owner = WriterOwner::open(sandbox.path(), Limits::default())
            .unwrap()
            .0;
        let auth = owner
            .auth_with_policy(AuthPolicy {
                registration: if case.registration_open {
                    RegistrationPolicy::FirstAccountOnly
                } else {
                    RegistrationPolicy::Closed
                },
                first_user: FirstUserTenant::NewUser,
                session_tenant: SessionTenantPolicy::SingleAccountDefault,
            })
            .unwrap();
        // No OAuth provider and no mailer: the router has no outbound effects.
        let served = Served::start(|origin| {
            auth_router(
                AuthHttpConfig::new(origin, CookieTransport::LoopbackHttp, false, None, false)
                    .unwrap(),
                auth,
                ProviderClient::new(None, None).unwrap(),
                ResetMailer::disabled(),
            )
        })
        .await
        .unwrap();
        let post = |path: &str, body: String| {
            served
                .client
                .post(format!("{}{path}", served.origin))
                .header("Origin", &served.origin)
                .header("Content-Type", "application/json")
                .body(body)
                .send()
        };
        if case.seed_account {
            for path in ["/auth/register", "/auth/login"] {
                let setup = Exchange::observe(post(path, account.to_string()).await.unwrap())
                    .await
                    .unwrap();
                assert_eq!(setup.status, 200, "{} setup {path}", case.id);
            }
        }
        let before = StateGraph::capture(sandbox.path()).unwrap();
        let actual = Exchange::observe(
            post(&case.request.path, case.request.body.clone())
                .await
                .unwrap(),
        )
        .await
        .unwrap();
        let after = StateGraph::capture(sandbox.path()).unwrap();
        served.stop().await.unwrap();
        owner.shutdown().unwrap();
        (actual, before.changed(&after))
    }

    #[tokio::test]
    async fn login_refusals_match_node_with_declared_corrections_and_write_nothing() {
        let fixture = login_fixture();
        let mut ledger = Ledger::new(login_corrections()).unwrap();
        for case in &fixture.cases {
            let (actual, changed) = login_case(case, &fixture.account).await;
            ledger
                .compare(&case.id, &case.node, &actual)
                .unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(changed, Vec::<String>::new(), "{} wrote state", case.id);
        }
        let report = ledger.finish().unwrap();
        print_report("auth-login", &report);
        assert_eq!(report.matched, 11);
        assert_eq!(report.corrected.len(), 3);
    }

    struct Rust {
        served: Served,
        owner: WriterOwner,
    }

    impl Rust {
        async fn start(sandbox: &Sandbox) -> Self {
            let owner = WriterOwner::open(sandbox.path(), Limits::default())
                .unwrap()
                .0;
            let auth = owner
                .rebuild_auth_with_policy(
                    AuthPolicy {
                        registration: RegistrationPolicy::Open,
                        first_user: FirstUserTenant::NewUser,
                        session_tenant: SessionTenantPolicy::AccountTenant,
                    },
                    AuthLifetimes::default(),
                )
                .unwrap();
            let served = Served::start(|origin| {
                auth_router(
                    AuthHttpConfig::new(origin, CookieTransport::LoopbackHttp, true, None, false)
                        .unwrap(),
                    auth,
                    ProviderClient::new(None, None).unwrap(),
                    ResetMailer::disabled(),
                )
            })
            .await
            .unwrap();
            Self { served, owner }
        }

        async fn stop(self) {
            self.served.stop().await.unwrap();
            self.owner.shutdown().unwrap();
        }
    }

    fn seed_session(sandbox: &Sandbox, seed: &Value) -> String {
        let (user, session) = (&seed["user"], &seed["session"]);
        let text = |value: &Value| value.as_str().map(str::to_owned);
        let mut db = rusqlite::Connection::open(sandbox.path().join("print-partner.db")).unwrap();
        let tx = db.transaction().unwrap();
        tx.execute(
            "INSERT INTO users(id,email,display_name,password_hash,is_admin,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                text(&user["id"]),
                text(&user["email"]),
                text(&user["display_name"]),
                text(&user["password_hash"]),
                user["is_admin"].as_i64(),
                text(&user["created_at"]),
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO sessions(id,user_id,expires_at,provider) VALUES(?1,?2,?3,?4)",
            params![
                text(&session["id"]),
                text(&session["user_id"]),
                text(&session["expires_at"]),
                text(&session["provider"]),
            ],
        )
        .unwrap();
        tx.commit().unwrap();
        session["token"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn session_transitions_match_node_responses_and_session_rows() {
        let fixture = session_fixture();
        let mut ledger = Ledger::new(session_corrections()).unwrap();
        let sandbox = Sandbox::new("t10-session").unwrap();
        let mut rust = Rust::start(&sandbox).await;
        let token = seed_session(&sandbox, &fixture.seed);
        let mut node_rows = StateGraph::capture(sandbox.path())
            .unwrap()
            .rows(SESSIONS)
            .unwrap();
        assert_eq!(node_rows, 1);
        for transition in &fixture.cases {
            if transition.storage == "reopen" {
                rust.stop().await;
                rust = Rust::start(&sandbox).await;
            }
            let served = &rust.served;
            let mut request = served
                .client
                .request(
                    Method::from_bytes(transition.method.as_bytes()).unwrap(),
                    format!("{}{}", served.origin, transition.path),
                )
                .header("Origin", &served.origin);
            if transition.credential == "original" {
                request = request.header("Cookie", format!("pp_session={token}"));
            }
            let before = StateGraph::capture(sandbox.path()).unwrap();
            let actual = Exchange::observe(request.send().await.unwrap())
                .await
                .unwrap();
            let after = StateGraph::capture(sandbox.path()).unwrap();
            let expected = &transition.expected;
            assert_eq!(
                expected.clearing_cookies,
                transition
                    .node_set_cookie
                    .iter()
                    .map(|header| clearing_cookie(header))
                    .collect::<Vec<_>>(),
                "{} clearing cookies",
                transition.name
            );
            let node = Exchange {
                status: expected.status,
                body: expected.body.clone(),
                set_cookie: transition.node_set_cookie.clone(),
            };
            ledger
                .compare(&transition.name, &node, &actual)
                .unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(
                after.rows(SESSIONS),
                Some(expected.session_rows),
                "{} session rows",
                transition.name
            );
            // Node's only recorded state is the session row count; Rust may
            // change the sessions table when, and only when, that count moves.
            let moved: Vec<String> = if expected.session_rows == node_rows {
                vec![]
            } else {
                vec![SESSIONS.into()]
            };
            assert_eq!(before.changed(&after), moved, "{} state", transition.name);
            node_rows = expected.session_rows;
        }
        rust.stop().await;
        let report = ledger.finish().unwrap();
        print_report("auth-session", &report);
        assert_eq!(report.matched, 4);
        assert_eq!(report.corrected.len(), 3);
    }

    #[test]
    fn ledger_rejects_drift_stale_unused_and_duplicate_corrections() {
        let fixture = login_fixture();
        let node = |id: &str| {
            fixture
                .cases
                .iter()
                .find(|case| case.id == id)
                .unwrap()
                .node
                .clone()
        };
        let corrected = login_corrections().remove(0).rust;
        let unknown = node("unknown-account");

        let mut changed = unknown.clone();
        changed.status += 1;
        let mut ledger = Ledger::new(login_corrections()).unwrap();
        assert!(matches!(
            ledger.compare("unknown-account", &unknown, &changed),
            Err(ParityError::Mismatch { .. })
        ));
        let mut cookie = unknown.clone();
        cookie.set_cookie.push("pp_session=unexpected".into());
        assert!(matches!(
            ledger.compare("bad-password", &node("bad-password"), &cookie),
            Err(ParityError::Mismatch { .. })
        ));
        assert_eq!(
            ledger.compare("array-body", &node("array-body"), &node("array-body")),
            Err(ParityError::StaleCorrection("array-body".into()))
        );
        let mut drifted = corrected.clone();
        drifted.body = json!({"detail": "Invalid body"});
        assert!(matches!(
            ledger.compare("null-body", &node("null-body"), &drifted),
            Err(ParityError::Mismatch { .. })
        ));
        assert_eq!(
            ledger.compare("null-body", &node("null-body"), &corrected),
            Err(ParityError::DuplicateCase("null-body".into()))
        );
        assert_eq!(
            ledger.finish(),
            Err(ParityError::UnusedCorrections(vec![
                "malformed-json".into()
            ]))
        );

        assert_eq!(
            Ledger::new(vec![]).unwrap().finish(),
            Err(ParityError::NoCases)
        );
        let mut twice = login_corrections();
        twice.push(login_corrections().remove(0));
        assert!(matches!(
            Ledger::new(twice),
            Err(ParityError::DuplicateCorrection(case)) if case == "array-body"
        ));

        let mut ledger = Ledger::new(login_corrections()).unwrap();
        ledger
            .compare("unknown-account", &unknown, &unknown)
            .unwrap();
        for id in ["array-body", "null-body", "malformed-json"] {
            ledger.compare(id, &node(id), &corrected).unwrap();
        }
        let report = ledger.finish().unwrap();
        assert_eq!(report.matched, 1);
        assert_eq!(
            report.corrected,
            ["array-body", "null-body", "malformed-json"]
                .map(|id| (id.to_owned(), CorrectionKind::Security))
        );
    }

    #[test]
    fn state_graph_detects_row_file_and_nested_database_changes() {
        let sandbox = Sandbox::new("t10-graph").unwrap();
        let path = sandbox.path().to_path_buf();
        let db = rusqlite::Connection::open(path.join("print-partner.db")).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE t(a INTEGER, b TEXT, c BLOB); INSERT INTO t VALUES(1,'x',x'00ff'),(2,NULL,NULL);",
        )
        .unwrap();
        std::fs::create_dir(path.join("nested")).unwrap();
        std::fs::write(path.join("nested/file.txt"), "one").unwrap();
        let initial = StateGraph::capture(&path).unwrap();
        assert_eq!(initial.rows("print-partner.db/t"), Some(2));
        assert_eq!(initial.rows("print-partner.db/missing"), None);
        assert_eq!(StateGraph::capture(&path).unwrap(), initial);

        db.execute("UPDATE t SET b='y' WHERE a=1", []).unwrap();
        let updated = StateGraph::capture(&path).unwrap();
        assert_eq!(updated.rows("print-partner.db/t"), Some(2));
        assert_eq!(initial.changed(&updated), ["print-partner.db/t"]);

        // Distinct invalid UTF-8 text must not collapse to the same cell.
        db.execute("UPDATE t SET b=CAST(x'ff' AS TEXT) WHERE a=1", [])
            .unwrap();
        let invalid = StateGraph::capture(&path).unwrap();
        db.execute("UPDATE t SET b=CAST(x'fe' AS TEXT) WHERE a=1", [])
            .unwrap();
        assert_eq!(
            invalid.changed(&StateGraph::capture(&path).unwrap()),
            ["print-partner.db/t"]
        );
        db.execute("UPDATE t SET b='y' WHERE a=1", []).unwrap();

        std::fs::write(path.join("nested/file.txt"), "two").unwrap();
        std::fs::write(path.join("added.json"), "{}").unwrap();
        let other = rusqlite::Connection::open(path.join("nested/other.db")).unwrap();
        other.execute_batch("CREATE TABLE u(v INTEGER);").unwrap();
        drop(other);
        let changed = StateGraph::capture(&path).unwrap();
        assert_eq!(
            updated.changed(&changed),
            ["added.json", "nested/file.txt", "nested/other.db/u"]
        );
        drop(db);

        let gone = path.clone();
        drop(sandbox);
        assert!(!gone.exists());
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct JobsFixture {
        generated: String,
        sources: Vec<String>,
        cases: Vec<JobsCase>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct JobsCase {
        id: String,
        path: String,
        node: Exchange,
    }

    fn jobs_fixture() -> JobsFixture {
        let fixture: JobsFixture = serde_json::from_str(include_str!(
            "../../../../fixtures/parity/jobs-http/node.json"
        ))
        .unwrap();
        assert!(fixture.generated.contains("jobs-http/generate.mjs"));
        assert_eq!(fixture.sources.len(), 3);
        assert_eq!(fixture.cases.len(), 13);
        fixture
    }

    /// The durable job snapshot differences documented in docs/desktop-rebuild.md
    /// (GRE-609): every snapshot adds `created_at`, and a failed job keeps its
    /// last progress (none here) and reports "Job failed" instead of the
    /// handler's error, which Node holds only in memory.
    fn durable_snapshot(node: &Value) -> Value {
        let mut rust = node.clone();
        let created = match node["job_id"].as_str().unwrap() {
            "job-1" => "created-1",
            "job-2" => "created-2",
            other => panic!("unexpected job label {other}"),
        };
        rust["created_at"] = json!(created);
        if node["status"] == "error" {
            rust["message"] = json!("Job failed");
            rust["error"] = json!("Job failed");
            rust["progress"] = Value::Null;
        }
        rust
    }

    /// One declared correction for each Node response that carries a job.
    fn jobs_corrections(fixture: &JobsFixture) -> Vec<Correction> {
        fixture
            .cases
            .iter()
            .filter(|case| case.node.status == 200)
            .filter_map(|case| {
                let body = &case.node.body;
                let rust = match body.get("jobs") {
                    Some(Value::Array(jobs)) if jobs.is_empty() => return None,
                    Some(Value::Array(jobs)) => {
                        json!({"jobs": jobs.iter().map(durable_snapshot).collect::<Vec<_>>()})
                    }
                    _ => durable_snapshot(body),
                };
                Some(Correction {
                    case: case.id.clone().leak(),
                    kind: CorrectionKind::Durable,
                    reason: "durable snapshots add created_at and report a generic failure",
                    rust: Exchange {
                        status: 200,
                        body: rust,
                        set_cookie: vec![],
                    },
                })
            })
            .collect()
    }

    const ACCOUNT_POLICY: AuthPolicy = AuthPolicy {
        registration: RegistrationPolicy::Open,
        first_user: FirstUserTenant::NewUser,
        session_tenant: SessionTenantPolicy::AccountTenant,
    };

    /// Registers `email` through the real auth router and returns its
    /// `pp_session=<token>` cookie pair. Node's corpora need no session, so
    /// this setup runs before any state is captured.
    async fn register(owner: &WriterOwner, email: &str) -> String {
        let auth = owner.auth_with_policy(ACCOUNT_POLICY).unwrap();
        let served = Served::start(|origin| {
            auth_router(
                AuthHttpConfig::new(origin, CookieTransport::LoopbackHttp, false, None, false)
                    .unwrap(),
                auth,
                ProviderClient::new(None, None).unwrap(),
                ResetMailer::disabled(),
            )
        })
        .await
        .unwrap();
        let registered = served
            .client
            .post(format!("{}/auth/register", served.origin))
            .header("Origin", &served.origin)
            .json(&json!({"email": email, "password": "password-1234"}))
            .send()
            .await
            .unwrap();
        assert_eq!(registered.status(), 200);
        let cookie = registered.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        served.stop().await.unwrap();
        cookie
    }

    /// Enqueues one job as the session in `cookie` and returns its id.
    fn enqueue(
        owner: &WriterOwner,
        cookie: &str,
        key: &str,
        payload: pp_storage::jobs::Payload,
    ) -> String {
        use pp_storage::{
            auth::Secret,
            jobs::{Credential, Outcome, UserOperation},
        };
        use std::{sync::atomic::AtomicBool, time::Duration};

        let token = cookie.strip_prefix("pp_session=").unwrap().to_owned();
        match owner
            .jobs(ACCOUNT_POLICY)
            .unwrap()
            .submit(
                Credential::Session(Secret::new(token)),
                UserOperation::Enqueue {
                    key: key.into(),
                    payload_version: 1,
                    payload,
                },
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
            .receive()
            .unwrap()
        {
            Outcome::Job(job, _) => job.job_id,
            _ => panic!("enqueued job expected"),
        }
    }

    /// The finished update check's public result, empty as in Node's probe.
    fn no_source_updates() -> pp_storage::jobs::WorkerOperation {
        use pp_storage::jobs::{CompletedResult, SourceUpdatesResult, WorkerOperation};

        WorkerOperation::FinishPublic {
            artifact: None,
            result: Box::new(CompletedResult::SourceUpdates(SourceUpdatesResult {
                checked: vec![],
                skipped: vec![],
                updates_available: 0,
                checked_count: 0,
            })),
        }
    }

    /// Seeds the two retained jobs Node's generator runs, in the same order:
    /// a finished update check, then (a second later) a failed checklist export.
    /// Returns the session cookie, the two job ids and the worker, which is the
    /// owner's only worker admission.
    async fn seed_jobs(
        owner: &WriterOwner,
    ) -> (String, String, String, pp_storage::jobs::ServerWorkerClient) {
        use pp_storage::jobs::{JobKind, Payload, WorkerAdmission, WorkerOperation};
        use std::time::Duration;

        let policy = ACCOUNT_POLICY;
        let cookie = register(owner, "jobs-http@example.test").await;
        let submit = |key: &str, payload: Payload| enqueue(owner, &cookie, key, payload);
        let worker = owner
            .job_worker_with_policy(
                policy,
                WorkerAdmission {
                    kinds: vec![
                        (JobKind::CheckSourceUpdates, 1),
                        (JobKind::ExportChecklistHtml, 1),
                    ],
                    total: 2,
                    per_resource: 1,
                    lease_seconds: 60,
                },
            )
            .unwrap();
        let done = submit("check", Payload::CheckSourceUpdates {});
        let mut lease = worker.claim().unwrap().unwrap().lease;
        assert_eq!(lease.job_id(), done);
        worker.update(&mut lease, no_source_updates()).unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let failed = submit("export", Payload::ExportChecklistHtml { profile_id: 7 });
        let mut lease = worker.claim().unwrap().unwrap().lease;
        assert_eq!(lease.job_id(), failed);
        worker.update(&mut lease, WorkerOperation::Fail).unwrap();
        (cookie, done, failed, worker)
    }

    #[tokio::test]
    async fn jobs_flat_and_v1_reads_match_node_and_write_nothing() {
        use pp_api::{
            fixtures::Labels,
            jobs::{JobsHttpConfig, jobs_router},
        };

        let fixture = jobs_fixture();
        let sandbox = Sandbox::new("t10-jobs").unwrap();
        let owner = WriterOwner::open(sandbox.path(), Limits::default())
            .unwrap()
            .0;
        let (cookie, done, failed, _) = seed_jobs(&owner).await;
        let jobs = owner.jobs(ACCOUNT_POLICY).unwrap();
        let served = Served::start(|origin| {
            jobs_router(
                JobsHttpConfig::from_startup(origin, "default", Some("1")).unwrap(),
                jobs,
            )
        })
        .await
        .unwrap();
        let mut labels = Labels::new(vec![
            ("job", &["job_id"]),
            // Rust's `created_at` has no Node counterpart; its own prefix keeps
            // the shared time labels independent of whole-second clock ticks.
            ("created", &["created_at"]),
            ("time", &["started_at", "finished_at"]),
            // Node stamps `updated_at` with a second clock read, so its
            // equality with `finished_at` is a millisecond race.
            ("updated", &["updated_at"]),
        ]);
        let mut ledger = Ledger::new(jobs_corrections(&fixture)).unwrap();
        let before = StateGraph::capture(sandbox.path()).unwrap();
        for case in &fixture.cases {
            let path = case
                .path
                .replace("{done}", &done)
                .replace("{failed}", &failed);
            let mut actual = Exchange::observe(
                served
                    .client
                    .get(format!("{}{path}", served.origin))
                    .header("Origin", &served.origin)
                    .header("Cookie", &cookie)
                    .send()
                    .await
                    .unwrap(),
            )
            .await
            .unwrap();
            labels.scrub(&mut actual);
            ledger
                .compare(&case.id, &case.node, &actual)
                .unwrap_or_else(|error| panic!("{error}"));
        }
        let after = StateGraph::capture(sandbox.path()).unwrap();
        served.stop().await.unwrap();
        owner.shutdown().unwrap();
        assert_eq!(
            before.changed(&after),
            Vec::<String>::new(),
            "job reads wrote state"
        );
        let report = ledger.finish().unwrap();
        print_report("jobs-http", &report);
        assert_eq!(report.matched, 5);
        assert_eq!(report.corrected.len(), 8);
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct JobsWsFixture {
        generated: String,
        sources: Vec<String>,
        cases: Vec<JobsWsCase>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct JobsWsCase {
        id: String,
        path: String,
        tenant: String,
        node: Exchange,
    }

    fn jobs_ws_fixture() -> JobsWsFixture {
        let fixture: JobsWsFixture = serde_json::from_str(include_str!(
            "../../../../fixtures/parity/jobs-ws/node.json"
        ))
        .unwrap();
        assert!(fixture.generated.contains("jobs-ws/generate.mjs"));
        assert_eq!(fixture.sources.len(), 2);
        assert_eq!(fixture.cases.len(), 5);
        fixture
    }

    /// Node ends a finished stream with `socket.close()`, an empty close frame
    /// the client reads as 1005. Rust sends 1000 with an empty reason, and its
    /// frame is the durable snapshot (see `durable_snapshot`).
    fn jobs_ws_corrections(fixture: &JobsWsFixture) -> Vec<Correction> {
        fixture
            .cases
            .iter()
            .filter(|case| case.node.body["close"]["code"] == 1005 && case.id != "ws_live")
            .map(|case| {
                let frames: Vec<Value> = case.node.body["frames"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(durable_snapshot)
                    .collect();
                Correction {
                    case: case.id.clone().leak(),
                    kind: CorrectionKind::WebSocket,
                    reason: "a finished stream closes 1000 and sends the durable snapshot",
                    rust: Exchange {
                        status: 101,
                        body: json!({"frames": frames, "close": {"code": 1000, "reason": ""}}),
                        set_cookie: vec![],
                    },
                }
            })
            .chain([live_correction()])
            .collect()
    }

    /// `ws_live` streams the durable model's wording, as docs/desktop-rebuild.md
    /// and `jobs_engine_parity` record it: a queued job names worker admission,
    /// a claimed job says "Running" with no progress, and every frame carries
    /// `created_at` and `finished_at`. Node's live frames add `updated_at`,
    /// which the read-one snapshot omits (GRE-797). The stream closes 1000.
    fn live_correction() -> Correction {
        let frame =
            |status: &str, message: &str, progress: Value, result: Value, finished: Value| {
                json!({
                    "job_id": "job-3",
                    "kind": "check-source-updates",
                    "status": status,
                    "message": message,
                    "progress": progress,
                    "result": result,
                    "error": null,
                    "created_at": "created-3",
                    "finished_at": finished,
                })
            };
        let result =
            json!({"checked": [], "skipped": [], "updates_available": 0, "checked_count": 0});
        Correction {
            case: "ws_live",
            kind: CorrectionKind::WebSocket,
            reason: "live frames use the durable model's wording and close 1000",
            rust: Exchange {
                status: 101,
                body: json!({
                    "frames": [
                        frame("pending", "Waiting for a compatible worker", json!(0), Value::Null, Value::Null),
                        frame("running", "Running", Value::Null, Value::Null, Value::Null),
                        frame("done", "Complete", json!(100), result, json!("time-3")),
                    ],
                    "close": {"code": 1000, "reason": ""},
                }),
                set_cookie: vec![],
            },
        }
    }

    /// Opens `path` as a WebSocket with `cookie` and reads every text message
    /// until the server's close frame, calling `step(i)` after message `i`.
    /// Fragmented text is joined, and ping and pong frames are skipped. An empty
    /// close payload reads as 1005, as a browser or Node's `ws` client reports
    /// it. A socket that has not closed within ten seconds fails the test.
    async fn observe_socket(
        served: &Served,
        path: &str,
        cookie: &str,
        step: &mut dyn FnMut(usize),
    ) -> Exchange {
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            read_socket(served, path, cookie, step),
        )
        .await
        .unwrap_or_else(|_| panic!("{path}: socket did not close within 10s"))
    }

    async fn read_socket(
        served: &Served,
        path: &str,
        cookie: &str,
        step: &mut dyn FnMut(usize),
    ) -> Exchange {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let host = served.origin.trim_start_matches("http://");
        let mut stream = tokio::net::TcpStream::connect(host).await.unwrap();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nCookie: {cookie}\r\n\r\n",
            origin = served.origin,
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(stream.read_u8().await.unwrap());
        }
        let head = String::from_utf8(head).unwrap();
        let status = head[9..12].parse().unwrap();
        assert_eq!(status, 101, "{path}: {head}");
        let mut frames = Vec::new();
        let mut text = Vec::new();
        loop {
            let first = stream.read_u8().await.unwrap();
            let len = match stream.read_u8().await.unwrap() & 0x7f {
                126 => usize::from(stream.read_u16().await.unwrap()),
                127 => usize::try_from(stream.read_u64().await.unwrap()).unwrap(),
                len => usize::from(len),
            };
            assert!(len <= 1 << 20, "{path}: frame over 1 MiB");
            let mut payload = vec![0; len];
            stream.read_exact(&mut payload).await.unwrap();
            let fin = first & 0x80 != 0;
            match first & 0x0f {
                0 | 1 => {
                    text.extend_from_slice(&payload);
                    if fin {
                        frames.push(serde_json::from_slice::<Value>(&text).unwrap());
                        text.clear();
                        step(frames.len() - 1);
                    }
                }
                9 | 10 => {}
                8 => {
                    let (code, reason) = match payload.as_slice() {
                        [] => (1005, String::new()),
                        [high, low, reason @ ..] => (
                            u16::from_be_bytes([*high, *low]),
                            String::from_utf8(reason.to_vec()).unwrap(),
                        ),
                        _ => panic!("{path}: one-byte close payload"),
                    };
                    return Exchange {
                        status,
                        body: json!({"frames": frames, "close": {"code": code, "reason": reason}}),
                        set_cookie: vec![],
                    };
                }
                opcode => panic!("{path}: unexpected opcode {opcode}"),
            }
        }
    }

    #[tokio::test]
    async fn job_socket_matches_node_and_writes_nothing() {
        use pp_api::{
            fixtures::Labels,
            jobs::{JobsHttpConfig, jobs_router},
        };

        use pp_storage::jobs::Payload;

        let fixture = jobs_ws_fixture();
        let sandbox = Sandbox::new("t10-jobs-ws").unwrap();
        let owner = WriterOwner::open(sandbox.path(), Limits::default())
            .unwrap()
            .0;
        let (cookie, done, failed, worker) = seed_jobs(&owner).await;
        // Node's `other` tenant is a second account, which has its own tenant.
        let other = register(&owner, "jobs-ws-other@example.test").await;
        // `ws_live` follows a queued update check that this worker claims after
        // the first frame and finishes after the second, as Node's probe does.
        // Rust's `created_at` has whole seconds, so the wait keeps its label
        // apart from the failed job's.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        let live = enqueue(&owner, &cookie, "live", Payload::CheckSourceUpdates {});
        let mut lease = None;
        let mut live_step = |frame: usize| match frame {
            0 => lease = Some(worker.claim().unwrap().unwrap().lease),
            1 => {
                let lease = lease.as_mut().unwrap();
                assert_eq!(lease.job_id(), live);
                worker.update(lease, no_source_updates()).unwrap();
            }
            _ => {}
        };
        let jobs = owner.jobs(ACCOUNT_POLICY).unwrap();
        let served = Served::start(|origin| {
            jobs_router(
                JobsHttpConfig::from_startup(origin, "default", Some("1")).unwrap(),
                jobs,
            )
        })
        .await
        .unwrap();
        let mut labels = Labels::new(vec![
            ("job", &["job_id"]),
            ("created", &["created_at"]),
            ("time", &["started_at", "finished_at"]),
            ("updated", &["updated_at"]),
        ]);
        let mut ledger = Ledger::new(jobs_ws_corrections(&fixture)).unwrap();
        let before = StateGraph::capture(sandbox.path()).unwrap();
        for case in &fixture.cases {
            let path = case
                .path
                .replace("{done}", &done)
                .replace("{failed}", &failed)
                .replace("{live}", &live);
            let cookie = match case.tenant.as_str() {
                "default" => &cookie,
                "other" => &other,
                tenant => panic!("unexpected tenant {tenant}"),
            };
            let mut actual = if case.id == "ws_live" {
                // Only the worker writes from here on, so the sockets so far
                // must have changed nothing.
                let after = StateGraph::capture(sandbox.path()).unwrap();
                assert_eq!(
                    before.changed(&after),
                    Vec::<String>::new(),
                    "job sockets wrote state"
                );
                observe_socket(&served, &path, cookie, &mut live_step).await
            } else {
                observe_socket(&served, &path, cookie, &mut |_| {}).await
            };
            labels.scrub(&mut actual);
            ledger
                .compare(&case.id, &case.node, &actual)
                .unwrap_or_else(|error| panic!("{error}"));
        }
        served.stop().await.unwrap();
        owner.shutdown().unwrap();
        let report = ledger.finish().unwrap();
        print_report("jobs-ws", &report);
        assert_eq!(report.matched, 2);
        assert_eq!(report.corrected.len(), 3);
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PlansFixture {
        generated: String,
        sources: Vec<String>,
        ids: std::collections::BTreeMap<String, u64>,
        cases: Vec<HttpCase>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct HttpCase {
        id: String,
        method: String,
        path: String,
        body: Option<Value>,
        /// JSON text sent verbatim, for bodies a `Value` would not keep
        /// byte for byte (`2.0`, `1e0`).
        #[serde(rename = "rawBody")]
        raw_body: Option<String>,
        node: Exchange,
        changed: Vec<String>,
        /// External effects Node's stubs recorded instead of running.
        #[serde(default)]
        effects: Vec<Value>,
    }

    /// Sends one recorded request to `path` with the session cookie and
    /// returns Rust's response with the files and tables it changed, named
    /// as Node's generators name them (bare table names for the main database).
    async fn send_case(
        served: &Served,
        cookie: &str,
        sandbox: &Sandbox,
        case: &HttpCase,
        path: &str,
    ) -> (Exchange, Vec<String>) {
        let mut request = served
            .client
            .request(
                Method::from_bytes(case.method.as_bytes()).unwrap(),
                format!("{}{path}", served.origin),
            )
            .header("Origin", &served.origin)
            .header("Cookie", cookie);
        if let Some(body) = &case.body {
            request = request.json(body);
        }
        if let Some(raw) = &case.raw_body {
            request = request
                .header("Content-Type", "application/json")
                .body(raw.clone());
        }
        let before = StateGraph::capture(sandbox.path()).unwrap();
        let actual = Exchange::observe(request.send().await.unwrap())
            .await
            .unwrap();
        let after = StateGraph::capture(sandbox.path()).unwrap();
        let changed = before
            .changed(&after)
            .into_iter()
            .map(|name| match name.strip_prefix("print-partner.db/") {
                Some(table) => table.to_owned(),
                None => name,
            })
            .collect();
        (actual, changed)
    }

    fn plans_fixture() -> PlansFixture {
        let fixture: PlansFixture = serde_json::from_str(include_str!(
            "../../../../fixtures/parity/plans-http/node.json"
        ))
        .unwrap();
        assert!(fixture.generated.contains("plans-http/generate.mjs"));
        assert_eq!(fixture.sources.len(), 3);
        assert_eq!(fixture.cases.len(), 28);
        fixture
    }

    /// Node's `POST /plans` inserts the Plan before it looks up the base
    /// project, so a refused create leaves the row behind. Rust refuses before
    /// it writes; this is the one case whose tables differ from Node's.
    const ATOMIC_CREATE: &str = "create_missing_base";

    /// The list after that refusal: Node's shows the leftover Plan, Rust's does not.
    fn plans_corrections(fixture: &PlansFixture) -> Vec<Correction> {
        let node = &fixture
            .cases
            .iter()
            .find(|case| case.id == "list_after_refusal_v1")
            .unwrap()
            .node;
        let mut rust = node.clone();
        let profiles = rust.body["profiles"].as_array_mut().unwrap();
        let before = profiles.len();
        profiles.retain(|profile| profile["name"] != "Orphan");
        assert_eq!(
            profiles.len(),
            before - 1,
            "Node's list shows the leftover Plan"
        );
        vec![Correction {
            case: "list_after_refusal_v1",
            kind: CorrectionKind::Durable,
            reason: "Rust refuses a missing base project before it inserts the Plan",
            rust,
        }]
    }

    #[tokio::test]
    async fn plans_flat_v1_v2_match_node_responses_and_changed_tables() {
        use pp_api::{
            builds::{BuildHttpConfig, build_router},
            fixtures::Labels,
        };

        let fixture = plans_fixture();
        let sandbox = Sandbox::new("t10-plans").unwrap();
        let owner = WriterOwner::open(sandbox.path(), Limits::default())
            .unwrap()
            .0;
        let cookie = register(&owner, "plans-http@example.test").await;
        let graph = owner.build_graph_with_policy(ACCOUNT_POLICY).unwrap();
        let served = Served::start(|origin| {
            build_router(
                BuildHttpConfig::from_startup(origin, "default", Some("1")).unwrap(),
                graph,
            )
        })
        .await
        .unwrap();
        let mut labels = Labels::new(vec![(
            "time",
            &[
                "created_at",
                "updated_at",
                "last_used_at",
                "config_modified_at",
                "archived_at",
            ],
        )]);
        let mut ledger = Ledger::new(plans_corrections(&fixture)).unwrap();
        let mut atomic = false;
        for case in &fixture.cases {
            let path = fixture
                .ids
                .iter()
                .fold(case.path.clone(), |path, (name, id)| {
                    path.replace(&format!("{{{name}}}"), &id.to_string())
                });
            let (mut actual, changed) = send_case(&served, &cookie, &sandbox, case, &path).await;
            labels.scrub(&mut actual);
            ledger
                .compare(&case.id, &case.node, &actual)
                .unwrap_or_else(|error| panic!("{error}"));
            if case.id == ATOMIC_CREATE {
                assert_eq!(case.changed, ["build_profiles"], "Node left a Plan");
                assert_eq!(changed, Vec::<String>::new(), "Rust refusal wrote state");
                atomic = true;
            } else {
                assert_eq!(changed, case.changed, "{}: changed tables", case.id);
            }
        }
        assert!(atomic, "{ATOMIC_CREATE} not replayed");
        served.stop().await.unwrap();
        owner.shutdown().unwrap();
        let report = ledger.finish().unwrap();
        print_report("plans-http", &report);
        assert_eq!(report.matched, 27);
        assert_eq!(report.corrected.len(), 1);
    }

    /// A corpus Node captured on a copy of the required-units `unchanged.db`,
    /// after running `seed` (if any) on it.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct SeededFixture {
        generated: String,
        sources: Vec<String>,
        database: String,
        #[serde(default)]
        seed: Option<String>,
        /// Each Source's `local_path`, relative to the data directory.
        #[serde(default)]
        checkouts: std::collections::BTreeMap<i64, String>,
        /// Checkout and data-directory files, relative to the data directory.
        #[serde(default)]
        files: std::collections::BTreeMap<String, String>,
        cases: Vec<HttpCase>,
    }

    /// The Node session in the required-units fixture database, tenant `default`.
    const FIXTURE_COOKIE: &str = "pp_session=required-unit-fixture-secret";

    fn seeded_fixture(json: &str, corpus: &str, cases: usize) -> SeededFixture {
        let fixture: SeededFixture = serde_json::from_str(json).unwrap();
        assert!(
            fixture
                .generated
                .contains(&format!("{corpus}/generate.mjs"))
        );
        assert_eq!(fixture.sources.len(), 3);
        assert_eq!(
            fixture.database,
            "rust/crates/pp-storage/tests/fixtures/required-units/unchanged.db"
        );
        assert_eq!(fixture.cases.len(), cases);
        fixture
    }

    /// Copies the fixture database into `sandbox`, runs the corpus seed on it,
    /// writes its checkout files and Source paths, and opens the writer, as
    /// Node's generator prepared its copy.
    fn open_seeded(sandbox: &Sandbox, fixture: &SeededFixture) -> WriterOwner {
        let path = sandbox.path().join("print-partner.db");
        std::fs::write(
            &path,
            include_bytes!("../../pp-storage/tests/fixtures/required-units/unchanged.db"),
        )
        .unwrap();
        for (file, body) in &fixture.files {
            let file = sandbox.path().join(file);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, body).unwrap();
        }
        if let Some(seed) = &fixture.seed {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection.execute_batch(seed).unwrap();
            for (source, checkout) in &fixture.checkouts {
                let checkout = checkout
                    .split('/')
                    .fold(sandbox.path().to_path_buf(), |path, part| path.join(part));
                connection
                    .execute(
                        "UPDATE projects SET local_path=?1 WHERE id=?2",
                        params![checkout.to_str().unwrap(), source],
                    )
                    .unwrap();
            }
        }
        WriterOwner::open(sandbox.path(), Limits::default())
            .unwrap()
            .0
    }

    /// Replays every case in order with `cookie`, comparing each response and
    /// the tables it changed. A corrected case is a Rust refusal, so it must
    /// change nothing, whatever Node wrote.
    async fn replay_seeded(
        served: &Served,
        sandbox: &Sandbox,
        cookie: &str,
        cases: &[HttpCase],
        corrections: Vec<Correction>,
    ) -> Report {
        let refused: Vec<&str> = corrections
            .iter()
            .map(|correction| correction.case)
            .collect();
        let mut ledger = Ledger::new(corrections).unwrap();
        let data = sandbox.path().to_str().unwrap();
        for case in cases {
            let (mut actual, changed) = send_case(served, cookie, sandbox, case, &case.path).await;
            // Node's generators write their scratch data directory as `{data}`.
            if let Ok(text) = serde_json::to_string(&actual.body) {
                actual.body = serde_json::from_str(&text.replace(data, "{data}")).unwrap();
            }
            ledger
                .compare(&case.id, &case.node, &actual)
                .unwrap_or_else(|error| panic!("{error}"));
            let expected = if refused.contains(&case.id.as_str()) {
                &[][..]
            } else {
                &case.changed[..]
            };
            assert_eq!(changed, expected, "{}: changed tables", case.id);
        }
        ledger.finish().unwrap()
    }

    #[tokio::test]
    async fn drafts_flat_v2_match_node_responses_and_changed_tables() {
        use pp_api::drafts::{DraftHttpConfig, draft_router};

        let fixture = seeded_fixture(
            include_str!("../../../../fixtures/parity/drafts-http/node.json"),
            "drafts-http",
            30,
        );
        assert_eq!(fixture.seed, None);
        let sandbox = Sandbox::new("t10-drafts").unwrap();
        let owner = open_seeded(&sandbox, &fixture);
        let drafts = owner.working_drafts_with_policy(ACCOUNT_POLICY).unwrap();
        let auth = owner
            .rebuild_auth_with_policy(ACCOUNT_POLICY, AuthLifetimes::default())
            .unwrap();
        let served = Served::start(|origin| {
            draft_router(
                DraftHttpConfig::from_startup(origin, "default", Some("1")).unwrap(),
                drafts,
                auth,
            )
        })
        .await
        .unwrap();
        // Rust problems name the missing field; Node sends no field errors.
        let field = |case, pointer| Correction {
            case,
            kind: CorrectionKind::Validation,
            reason: "problem field errors",
            rust: Exchange {
                status: 400,
                body: json!({"detail": "Request is invalid", "code": "invalid_request",
                    "errors": [{"pointer": pointer, "detail": "is required"}]}),
                set_cookie: vec![],
            },
        };
        let corrections = vec![
            field("edit_invalid_body_flat", "/expected_snapshot_digest"),
            field("abandon_invalid_body_v2", "/expected_lifecycle_version"),
        ];
        let report = replay_seeded(
            &served,
            &sandbox,
            FIXTURE_COOKIE,
            &fixture.cases,
            corrections,
        )
        .await;
        served.stop().await.unwrap();
        owner.shutdown().unwrap();
        print_report("drafts-http", &report);
        assert_eq!(report.matched, 28);
        assert_eq!(report.corrected.len(), 2);
    }

    #[tokio::test]
    async fn layers_flat_v1_v2_match_node_responses_and_changed_tables() {
        use pp_api::builds::{BuildHttpConfig, build_router};

        let fixture = seeded_fixture(
            include_str!("../../../../fixtures/parity/layers-http/node.json"),
            "layers-http",
            33,
        );
        assert!(fixture.seed.is_some());
        let sandbox = Sandbox::new("t10-layers").unwrap();
        let owner = open_seeded(&sandbox, &fixture);
        let graph = owner.build_graph_with_policy(ACCOUNT_POLICY).unwrap();
        let served = Served::start(|origin| {
            build_router(
                BuildHttpConfig::from_startup(origin, "default", Some("1")).unwrap(),
                graph,
            )
        })
        .await
        .unwrap();
        let report = replay_seeded(&served, &sandbox, FIXTURE_COOKIE, &fixture.cases, vec![]).await;
        served.stop().await.unwrap();
        owner.shutdown().unwrap();
        print_report("layers-http", &report);
        assert_eq!(report.matched, 33);
        assert_eq!(report.corrected.len(), 0);
    }

    #[tokio::test]
    async fn manifest_flat_v1_v2_match_node_responses_and_changed_tables() {
        use pp_api::builds::{BuildHttpConfig, build_router};

        let fixture = seeded_fixture(
            include_str!("../../../../fixtures/parity/manifest-http/node.json"),
            "manifest-http",
            46,
        );
        let unknown_group = |case: &'static str, group: &str| Correction {
            case,
            kind: CorrectionKind::Validation,
            reason: "Rust refuses a selection for an option group no attached Source defines \
                     (docs/desktop-rebuild.md); Node skips the check and saves it",
            rust: Exchange {
                status: 400,
                body: json!({"detail": format!("kit.selections.{group} is an unknown option group")}),
                set_cookie: vec![],
            },
        };
        let corrections = vec![
            Correction {
                case: "save_malformed_json_flat",
                kind: CorrectionKind::Security,
                reason: "Rust does not expose Fastify parser codes or messages",
                rust: Exchange {
                    status: 400,
                    body: json!({"detail": "Request is invalid"}),
                    set_cookie: vec![],
                },
            },
            unknown_group("save_unknown_group_v1", "toolhead"),
            unknown_group("save_unknown_group_list_flat", "extras"),
        ];
        assert!(fixture.seed.is_some());
        let sandbox = Sandbox::new("t10-manifest").unwrap();
        let owner = open_seeded(&sandbox, &fixture);
        let graph = owner.build_graph_with_policy(ACCOUNT_POLICY).unwrap();
        let served = Served::start(|origin| {
            build_router(
                BuildHttpConfig::from_startup(origin, "default", Some("1")).unwrap(),
                graph,
            )
        })
        .await
        .unwrap();
        let report = replay_seeded(
            &served,
            &sandbox,
            FIXTURE_COOKIE,
            &fixture.cases,
            corrections,
        )
        .await;
        served.stop().await.unwrap();
        owner.shutdown().unwrap();
        print_report("manifest-http", &report);
        assert_eq!(report.matched, 43);
        assert_eq!(report.corrected.len(), 3);
    }

    /// Builder reads and kit saves over real option groups: Sources 2-6 have
    /// checkouts under `repos/<id>` (declared, merged, inferred, missing and
    /// invalid manifests), read through the production `SourceReads` adapter
    /// that `pp_core::rebuild_api::clients` wires.
    #[tokio::test]
    async fn manifest_checkouts_flat_v1_v2_match_node_responses_and_changed_tables() {
        use pp_api::builds::{BuildHttpConfig, build_router};
        use pp_storage::working_drafts::observation::{DraftReads, PreparationLimits};
        use std::sync::Arc;

        let fixture = seeded_fixture(
            include_str!("../../../../fixtures/parity/manifest-checkouts-http/node.json"),
            "manifest-checkouts-http",
            20,
        );
        assert!(fixture.seed.is_some());
        assert_eq!(
            fixture.checkouts.keys().copied().collect::<Vec<_>>(),
            [2, 3, 4, 5, 6]
        );
        let unknown = |case: &'static str, detail: &str| Correction {
            case,
            kind: CorrectionKind::Validation,
            reason: "Rust refuses a selection naming an option group or variant no attached \
                     Source defines (docs/desktop-rebuild.md); Node checks only the count and saves it",
            rust: Exchange {
                status: 400,
                body: json!({"detail": detail}),
                set_cookie: vec![],
            },
        };
        let corrections = vec![
            unknown(
                "save_unknown_variant_v1",
                "kit.selections.toolhead contains an unknown variant id",
            ),
            unknown(
                "save_unknown_group_flat",
                "kit.selections.wheels is an unknown option group",
            ),
            unknown(
                "save_unknown_inferred_group_v2",
                "kit.selections.toolhead is an unknown option group",
            ),
        ];
        let sandbox = Sandbox::new("t10-manifest-checkouts").unwrap();
        let owner = open_seeded(&sandbox, &fixture);
        let reads: Arc<dyn DraftReads> = Arc::new(owner.source_reads().unwrap());
        let graph = owner
            .build_graph_with_observations(ACCOUNT_POLICY, reads, PreparationLimits::default())
            .unwrap();
        let served = Served::start(|origin| {
            build_router(
                BuildHttpConfig::from_startup(origin, "default", Some("1")).unwrap(),
                graph,
            )
        })
        .await
        .unwrap();
        let report = replay_seeded(
            &served,
            &sandbox,
            FIXTURE_COOKIE,
            &fixture.cases,
            corrections,
        )
        .await;
        served.stop().await.unwrap();
        owner.shutdown().unwrap();
        print_report("manifest-checkouts-http", &report);
        assert_eq!(report.matched, 17);
        assert_eq!(report.corrected.len(), 3);
    }

    #[tokio::test]
    async fn catalog_flat_v1_match_node_responses_and_changed_tables() {
        use pp_api::catalog::{CatalogHttpConfig, admit_catalog_routes};
        use std::sync::{Arc, Mutex};

        let fixture = seeded_fixture(
            include_str!("../../../../fixtures/parity/catalog-http/node.json"),
            "catalog-http",
            56,
        );
        // Node creates a GitHub Source and starts a sync job; its stub recorded
        // that start instead of cloning. Rust's catalog create starts no job.
        let started: Vec<(&str, &Value)> = fixture
            .cases
            .iter()
            .flat_map(|case| case.effects.iter().map(|effect| (case.id.as_str(), effect)))
            .collect();
        assert_eq!(
            started,
            [(
                "create_github_v1",
                &json!({"kind": "sync", "params": {"project_ids": [5]}})
            )]
        );
        let refusal = |case, kind, reason, detail: &str| Correction {
            case,
            kind,
            reason,
            rust: Exchange {
                status: 400,
                body: json!({ "detail": detail }),
                set_cookie: vec![],
            },
        };
        let corrections = vec![
            refusal(
                "global_naming_bad_v1",
                CorrectionKind::Validation,
                "Rust names the invalid field without zod's issue path and wording",
                "Invalid export role order",
            ),
            refusal(
                "patch_duplicate_v1",
                CorrectionKind::Security,
                "Rust does not expose SQLite constraint text; it uses Node's create refusal",
                "Source already exists: Renamed Source",
            ),
            refusal(
                "create_malformed_json_flat",
                CorrectionKind::Security,
                "Rust does not expose Fastify parser codes or messages",
                "Invalid JSON body",
            ),
        ];
        assert!(fixture.seed.is_some());
        let sandbox = Sandbox::new("t10-catalog").unwrap();
        let owner = Arc::new(Mutex::new(open_seeded(&sandbox, &fixture)));
        let served = Served::start(|origin| {
            admit_catalog_routes(
                CatalogHttpConfig::from_startup(origin, "default", ACCOUNT_POLICY, Some("1"))
                    .unwrap(),
                Arc::clone(&owner),
            )
        })
        .await
        .unwrap();
        let report = replay_seeded(
            &served,
            &sandbox,
            FIXTURE_COOKIE,
            &fixture.cases,
            corrections,
        )
        .await;
        served.stop().await.unwrap();
        let owner = Arc::into_inner(owner).unwrap().into_inner().unwrap();
        owner.shutdown().unwrap();
        print_report("catalog-http", &report);
        assert_eq!(report.matched, 53);
        assert_eq!(report.corrected.len(), 3);
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AcceptedFixture {
        generated: String,
        sources: Vec<String>,
        database: String,
        phases: Vec<AcceptedPhase>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct AcceptedPhase {
        name: String,
        #[serde(default)]
        seed: Option<String>,
        cases: Vec<HttpCase>,
    }

    /// The Node session in `accepted-plan-node.db`, tenant `default`.
    const ACCEPTED_COOKIE: &str = "pp_session=read-fixture-secret";

    /// Copies `accepted-plan-node.db` into `sandbox` with the revision
    /// directory Plan 1 names. A seed runs with every trigger dropped and
    /// foreign keys off, as the generator ran it.
    fn open_accepted(sandbox: &Sandbox, seed: Option<&str>) -> WriterOwner {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(sandbox.path().join("repos/1/revisions/accepted"))
            .unwrap();
        let path = sandbox.path().join("print-partner.db");
        std::fs::write(
            &path,
            include_bytes!("../../pp-storage/tests/fixtures/accepted-plan-node.db"),
        )
        .unwrap();
        if let Some(seed) = seed {
            let db = rusqlite::Connection::open(&path).unwrap();
            db.pragma_update(None, "foreign_keys", false).unwrap();
            let triggers = db
                .prepare("SELECT name FROM sqlite_schema WHERE type = 'trigger'")
                .unwrap()
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            for trigger in triggers {
                db.execute_batch(&format!("DROP TRIGGER \"{trigger}\";"))
                    .unwrap();
            }
            db.execute_batch(seed).unwrap();
        }
        WriterOwner::open(sandbox.path(), Limits::default())
            .unwrap()
            .0
    }

    #[tokio::test]
    async fn accepted_flat_v1_v2_match_node_responses_and_changed_tables() {
        use pp_api::accepted::{AcceptedHttpConfig, accepted_router};

        let fixture: AcceptedFixture = serde_json::from_str(include_str!(
            "../../../../fixtures/parity/accepted-http/node.json"
        ))
        .unwrap();
        assert!(fixture.generated.contains("accepted-http/generate.mjs"));
        assert_eq!(fixture.sources.len(), 3);
        assert_eq!(
            fixture.database,
            "rust/crates/pp-storage/tests/fixtures/accepted-plan-node.db"
        );
        let names: Vec<&str> = fixture.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            ["ready", "foreign_plan", "foreign_parts", "dirty", "corrupt"]
        );
        let (mut matched, mut corrected) = (0, vec![]);
        for phase in &fixture.phases {
            let corrections = match phase.name.as_str() {
                // Node finds the Part, then its Plan's accepted read fails for
                // the wrong tenant and the route answers 500. Rust hides a Part
                // whose Plan belongs to another tenant, as it hides the Plan.
                "foreign_plan" => vec![Correction {
                    case: "foreign_assembled_v1",
                    kind: CorrectionKind::Security,
                    reason: "Rust answers a Part on another tenant's Plan as missing \
                             instead of Node's internal error",
                    rust: Exchange {
                        status: 404,
                        body: json!({"detail": "Part not found"}),
                        set_cookie: vec![],
                    },
                }],
                _ => vec![],
            };
            let sandbox = Sandbox::new(&format!("t10-accepted-{}", phase.name)).unwrap();
            let owner = open_accepted(&sandbox, phase.seed.as_deref());
            let reads = owner.accepted_reads().unwrap();
            let served = Served::start(|origin| {
                accepted_router(
                    AcceptedHttpConfig::from_startup(origin, "default", Some("1")).unwrap(),
                    reads,
                )
            })
            .await
            .unwrap();
            let report = replay_seeded(
                &served,
                &sandbox,
                ACCEPTED_COOKIE,
                &phase.cases,
                corrections,
            )
            .await;
            served.stop().await.unwrap();
            owner.shutdown().unwrap();
            print_report(&format!("accepted-http/{}", phase.name), &report);
            matched += report.matched;
            corrected.extend(report.corrected);
        }
        assert_eq!(matched, 31);
        assert_eq!(
            corrected,
            [("foreign_assembled_v1".to_owned(), CorrectionKind::Security)]
        );
    }

    #[test]
    fn labels_keep_equality_links_and_catch_swapped_ids() {
        use pp_api::fixtures::Labels;
        let exchange = |body: Value| Exchange {
            status: 200,
            body,
            set_cookie: vec![],
        };
        let rules = || Labels::new(vec![("job", &["job_id"]), ("time", &["finished_at"])]);
        let node = exchange(json!({"jobs": [
            {"job_id": "job-1", "finished_at": "time-1"},
            {"job_id": "job-2", "finished_at": "time-1"},
        ]}));
        let mut labels = rules();
        let mut rust = exchange(json!({"jobs": [
            {"job_id": "a", "finished_at": "2026-01-01T00:00:00.000Z"},
            {"job_id": "b", "finished_at": "2026-01-01T00:00:00.000Z"},
        ]}));
        labels.scrub(&mut rust);
        assert_eq!(rust, node);
        // A repeated id where Node had two jobs no longer compares equal.
        let mut labels = rules();
        let mut collapsed = exchange(json!({"jobs": [
            {"job_id": "a", "finished_at": "2026-01-01T00:00:00.000Z"},
            {"job_id": "a", "finished_at": "2026-01-01T00:00:00.000Z"},
        ]}));
        labels.scrub(&mut collapsed);
        let mut ledger = Ledger::new(vec![]).unwrap();
        assert!(matches!(
            ledger.compare("collapsed", &node, &collapsed),
            Err(ParityError::Mismatch { .. })
        ));
    }
}
