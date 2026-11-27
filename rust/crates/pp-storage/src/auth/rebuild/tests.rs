use super::*;
use crate::{Limits, Setting, SettingCommand};

static RUNTIME_TEST: Mutex<()> = Mutex::new(());
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

#[test]
fn bounded_auth_workers_and_cancelled_queue_do_not_write() {
    let _serial = RUNTIME_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let directory = std::env::temp_dir().join(format!(
        "pp-auth-queue-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let (owner, _) = WriterOwner::open(
        &directory,
        Limits {
            queued_writes: 1,
            readers: 1,
        },
    )
    .unwrap();
    let client = open_client(&owner);
    let storage = owner.client();
    let raw = Connection::open(directory.join("print-partner.db")).unwrap();
    raw.execute_batch("BEGIN IMMEDIATE").unwrap();
    let blocking = storage
        .submit(
            SettingCommand::Set(Setting {
                tenant: "test".into(),
                key: "blocking".into(),
                value: "yes".into(),
            }),
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !storage.shared.queue.lock().unwrap().pending.is_empty() {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut replies = Vec::new();
    for _ in 0..35 {
        match client.submit(
            Request::RequestReset {
                email: "queue@example.com".into(),
            },
            cancelled.clone(),
            Duration::ZERO,
        ) {
            Ok(reply) => replies.push(reply),
            Err(error) => {
                assert_eq!(error.to_string(), "Authentication queue full");
                break;
            }
        }
    }
    assert!((32..=34).contains(&replies.len()));
    cancelled.store(true, Ordering::Release);
    raw.execute_batch("ROLLBACK").unwrap();
    assert!(
        blocking
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok()
    );
    for reply in replies {
        assert!(reply.recv_timeout(Duration::from_secs(5)).unwrap().is_err());
    }
    owner.shutdown().unwrap();
    let connection = Connection::open(directory.join("print-partner.db")).unwrap();
    let users: i64 = connection
        .query_row("SELECT count(*) FROM users", [], |row| row.get(0))
        .unwrap();
    let resets: i64 = connection
        .query_row("SELECT count(*) FROM password_reset_tokens", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!((users, resets), (0, 0));
}

fn run(client: &RebuildAuthClient, request: Request) -> Result<Outcome> {
    client
        .submit(
            request,
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )?
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
}

fn legacy_hash(password: &str) -> String {
    let salt = [7; 16];
    let params = scrypt::Params::new(14, 8, 1, 64).unwrap();
    let mut hash = [0; 64];
    scrypt::scrypt(password.as_bytes(), &salt, &params, &mut hash).unwrap();
    format!(
        "scrypt:16384:8:1:{}:{}",
        URL_SAFE_NO_PAD.encode(salt),
        URL_SAFE_NO_PAD.encode(hash)
    )
}

#[test]
fn verified_short_legacy_password_logs_in_without_rehashing() {
    let _serial = RUNTIME_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let directory = std::env::temp_dir().join(format!(
        "pp-auth-short-legacy-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let (owner, _) = WriterOwner::open(&directory, Limits::default()).unwrap();
    let client = open_client(&owner);
    run(
        &client,
        Request::Register {
            email: "legacy@example.com".into(),
            display_name: "Legacy".into(),
            password: Secret::new("initial-password".into()),
        },
    )
    .unwrap();
    owner.shutdown().unwrap();

    let stored = legacy_hash("short");
    let db = Connection::open(directory.join("print-partner.db")).unwrap();
    db.execute(
        "UPDATE users SET password_hash=?1 WHERE email='legacy@example.com'",
        [&stored],
    )
    .unwrap();
    drop(db);

    let (owner, _) = WriterOwner::open(&directory, Limits::default()).unwrap();
    let client = open_client(&owner);
    assert!(
        run(
            &client,
            Request::Login {
                email: "legacy@example.com".into(),
                password: Secret::new("wrong".into()),
            },
        )
        .is_err()
    );
    assert!(matches!(
        run(
            &client,
            Request::Login {
                email: "legacy@example.com".into(),
                password: Secret::new("short".into()),
            },
        )
        .unwrap(),
        Outcome::Session { .. }
    ));
    owner.shutdown().unwrap();

    let db = Connection::open(directory.join("print-partner.db")).unwrap();
    let current: String = db
        .query_row(
            "SELECT password_hash FROM users WHERE email='legacy@example.com'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let sessions: i64 = db
        .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(current, stored);
    assert_eq!(sessions, 2);
}

#[test]
fn cheap_auth_progresses_while_two_valid_logins_are_in_kdf() {
    let _serial = RUNTIME_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let directory = std::env::temp_dir().join(format!(
        "pp-auth-kdf-progress-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let (owner, _) = WriterOwner::open(&directory, Limits::default()).unwrap();
    let client = open_client(&owner);
    let Outcome::Session { token, .. } = run(
        &client,
        Request::Register {
            email: "progress@example.com".into(),
            display_name: "Progress".into(),
            password: Secret::new("ordinary-password".into()),
        },
    )
    .unwrap() else {
        panic!("session expected")
    };
    let (gate, entered, mut release) = TestKdfGate::new();
    let gated = client.with_kdf_gate(gate);
    let first_login = gated
        .submit(
            Request::Login {
                email: "progress@example.com".into(),
                password: Secret::new("ordinary-password".into()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    let second_login = gated
        .submit(
            Request::Login {
                email: "progress@example.com".into(),
                password: Secret::new("ordinary-password".into()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let status = gated
        .submit(
            Request::Status,
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    let session = gated
        .submit(
            Request::ResolveSession { token },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    let status_before_release = status.recv_timeout(Duration::from_millis(250));
    let session_before_release = session.recv_timeout(Duration::from_millis(250));
    release.release();
    assert!(
        first_login
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok()
    );
    assert!(
        second_login
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .is_ok()
    );
    let status_progressed = match status_before_release {
        Ok(Ok(Outcome::Status(_))) => true,
        Ok(_) => panic!("status outcome expected"),
        Err(_) => {
            let _ = status.recv_timeout(Duration::from_secs(5));
            false
        }
    };
    let session_progressed = match session_before_release {
        Ok(Ok(Outcome::User(Some(_)))) => true,
        Ok(_) => panic!("resolved session expected"),
        Err(_) => {
            let _ = session.recv_timeout(Duration::from_secs(5));
            false
        }
    };
    owner.shutdown().unwrap();
    assert!(status_progressed);
    assert!(session_progressed);
}

#[test]
fn kdf_completion_after_owner_close_returns_stopped() {
    let _serial = RUNTIME_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let directory = std::env::temp_dir().join(format!(
        "pp-auth-kdf-close-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let (owner, _) = WriterOwner::open(&directory, Limits::default()).unwrap();
    let client = open_client(&owner);
    run(
        &client,
        Request::Register {
            email: "close@example.com".into(),
            display_name: "Close".into(),
            password: Secret::new("ordinary-password".into()),
        },
    )
    .unwrap();
    let (gate, entered, mut release) = TestKdfGate::new();
    let reply = client
        .with_kdf_gate(gate)
        .submit(
            Request::Login {
                email: "close@example.com".into(),
                password: Secret::new("ordinary-password".into()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    owner.shutdown().unwrap();
    release.release();
    assert_eq!(
        reply
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .err()
            .unwrap()
            .to_string(),
        "Storage stopped"
    );
}

#[test]
fn expired_reset_link_preserves_password_and_session() {
    let directory = tempfile::tempdir().unwrap();
    let (owner, _) = WriterOwner::open(directory.path(), Limits::default()).unwrap();
    let client = open_client(&owner);
    let email = "recovery@example.test";
    let password = "fixture-password";
    let Outcome::Session { token: session, .. } = run(
        &client,
        Request::Register {
            email: email.into(),
            display_name: "Fixture".into(),
            password: Secret::new(password.into()),
        },
    )
    .unwrap() else {
        panic!("session expected")
    };
    let Outcome::ResetToken(Some(token)) = run(
        &client,
        Request::RequestReset {
            email: email.into(),
        },
    )
    .unwrap() else {
        panic!("reset token expected")
    };
    let db = Connection::open(directory.path().join("print-partner.db")).unwrap();
    assert_eq!(
        db.execute(
            "UPDATE password_reset_tokens SET expires_at='2000-01-01T00:00:00Z' WHERE id=?1",
            [crypto::digest(token.expose())],
        )
        .unwrap(),
        1
    );
    assert!(matches!(
        run(
            &client,
            Request::ResetPassword {
                token,
                replacement: Secret::new("fixture-replacement".into()),
            },
        )
        .unwrap(),
        Outcome::Changed(false)
    ));
    assert!(matches!(
        run(&client, Request::ResolveSession { token: session }).unwrap(),
        Outcome::User(Some(_))
    ));
    assert!(matches!(
        run(
            &client,
            Request::Login {
                email: email.into(),
                password: Secret::new(password.into()),
            },
        )
        .unwrap(),
        Outcome::Session { .. }
    ));
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_17_transaction_cas_rejects_verified_login_after_public_reset() {
    let _serial = RUNTIME_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let directory = std::env::temp_dir().join(format!(
        "pp-auth-cas-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let (owner, _) = WriterOwner::open(&directory, Limits::default()).unwrap();
    let client = open_client(&owner);
    let Outcome::Session {
        user,
        token: old_session,
    } = run(
        &client,
        Request::Register {
            email: "race@example.com".into(),
            display_name: "Race".into(),
            password: Secret::new("password-before".into()),
        },
    )
    .unwrap()
    else {
        panic!("session expected")
    };
    let Outcome::ResetToken(Some(token)) = run(
        &client,
        Request::RequestReset {
            email: "race@example.com".into(),
        },
    )
    .unwrap() else {
        panic!("reset expected")
    };
    owner.shutdown().unwrap();
    let db = Connection::open(directory.join("print-partner.db")).unwrap();
    let legacy_hash = include_str!("../../../tests/node-scrypt.txt")
        .trim()
        .to_owned();
    db.execute("UPDATE users SET password_hash=?1", [&legacy_hash])
        .unwrap();
    drop(db);
    let (owner, _) = WriterOwner::open(&directory, Limits::default()).unwrap();
    let client = open_client(&owner);
    let credential = Credential {
        user,
        hash: Some(legacy_hash),
    };
    let old_password = Secret::new("legacy-🖨️-password".into());
    assert!(crypto::verify(
        &old_password,
        credential.hash.as_ref().unwrap()
    ));
    let replacement = crypto::hash(&old_password).unwrap();
    let Outcome::Session {
        token: reset_session,
        ..
    } = run(
        &client,
        Request::ResetPassword {
            token,
            replacement: Secret::new("reset-winner-password".into()),
        },
    )
    .unwrap()
    else {
        panic!("reset session expected")
    };
    let old = client
        .submit_verified_login(credential, replacement, old_password)
        .unwrap()
        .recv()
        .unwrap();
    assert!(matches!(
        old.err().unwrap().downcast_ref::<AuthFailure>(),
        Some(AuthFailure::InvalidCredentials)
    ));
    assert!(matches!(
        run(&client, Request::ResolveSession { token: old_session }).unwrap(),
        Outcome::User(None)
    ));
    assert!(matches!(
        run(
            &client,
            Request::ResolveSession {
                token: reset_session,
            }
        )
        .unwrap(),
        Outcome::User(Some(_))
    ));
    assert!(
        run(
            &client,
            Request::Login {
                email: "race@example.com".into(),
                password: Secret::new("reset-winner-password".into())
            }
        )
        .is_ok()
    );
    assert!(
        run(
            &client,
            Request::Login {
                email: "race@example.com".into(),
                password: Secret::new("legacy-🖨️-password".into())
            }
        )
        .is_err()
    );
    owner.shutdown().unwrap();
    let db = Connection::open(directory.join("print-partner.db")).unwrap();
    let sessions: i64 = db
        .query_row("SELECT count(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    let resets: i64 = db
        .query_row("SELECT count(*) FROM password_reset_tokens", [], |row| {
            row.get(0)
        })
        .unwrap();
    let stored: String = db
        .query_row(
            "SELECT password_hash FROM users WHERE email='race@example.com'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((sessions, resets), (2, 0));
    assert!(crypto::verify(
        &Secret::new("reset-winner-password".into()),
        &stored
    ));
    assert!(!crypto::verify(
        &Secret::new("legacy-🖨️-password".into()),
        &stored
    ));
}

#[test]
fn lifetimes_apply_only_to_the_opted_in_client() {
    let _serial = RUNTIME_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    assert!(AuthLifetimes::shorter(Duration::from_secs(600), Some(Duration::MAX)).is_err());
    assert!(AuthLifetimes::shorter(Duration::ZERO, None).is_err());
    assert!(AuthLifetimes::shorter(Duration::from_secs(14 * 86400), None).is_err());
    assert!(AuthLifetimes::shorter(Duration::from_secs(600), Some(Duration::ZERO)).is_err());
    let root = std::env::temp_dir().join(format!(
        "pp-lifetimes-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    let policy = AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    };
    let client = owner
        .rebuild_auth_with_policy(
            policy,
            AuthLifetimes::shorter(Duration::from_secs(600), Some(Duration::from_secs(120)))
                .unwrap(),
        )
        .unwrap();
    let Outcome::Session { token, user } = run(
        &client,
        Request::Register {
            email: "lifetime@example.com".into(),
            display_name: "Lifetime".into(),
            password: Secret::new("ordinary-password".into()),
        },
    )
    .unwrap() else {
        panic!("session expected")
    };
    assert!(user.has_password);
    let db = Connection::open(root.join("print-partner.db")).unwrap();
    let expiry: String = db
        .query_row(
            "SELECT expires_at FROM sessions WHERE id=?1",
            [crypto::digest(token.expose())],
            |r| r.get(0),
        )
        .unwrap();
    let remaining = |expiry: &str| {
        (time::OffsetDateTime::parse(expiry, &time::format_description::well_known::Rfc3339)
            .unwrap()
            - time::OffsetDateTime::now_utc())
        .whole_seconds()
    };
    assert!((590..=600).contains(&remaining(&expiry)));
    let Outcome::KeyCreated { info, .. } = run(
        &client,
        Request::CreateKey {
            session: Secret::new(token.expose().into()),
        },
    )
    .unwrap() else {
        panic!("key expected")
    };
    assert!((110..=120).contains(&remaining(info.expires_at.as_deref().unwrap())));
    let Outcome::KeyCreated { info, .. } = run(
        &client,
        Request::RotateKey {
            session: Secret::new(token.expose().into()),
            key_id: info.id,
        },
    )
    .unwrap() else {
        panic!("key expected")
    };
    assert!((110..=120).contains(&remaining(info.expires_at.as_deref().unwrap())));
    let old = owner.auth_with_policy(policy).unwrap();
    let Outcome::Session { token, .. } = old
        .submit(
            Request::Login {
                email: "lifetime@example.com".into(),
                password: Secret::new("ordinary-password".into()),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    else {
        panic!("session expected")
    };
    let expiry: String = db
        .query_row(
            "SELECT expires_at FROM sessions WHERE id=?1",
            [crypto::digest(token.expose())],
            |r| r.get(0),
        )
        .unwrap();
    assert!((14 * 86400 - 10..=14 * 86400).contains(&remaining(&expiry)));
    let Outcome::KeyCreated { info, .. } = old
        .submit(
            Request::CreateKey { session: token },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap()
        .recv()
        .unwrap()
        .unwrap()
    else {
        panic!("key expected")
    };
    assert!(info.expires_at.is_none());
    drop(db);
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

fn open_client(owner: &WriterOwner) -> RebuildAuthClient {
    owner
        .rebuild_auth_with_policy(
            AuthPolicy {
                registration: RegistrationPolicy::Open,
                session_tenant: SessionTenantPolicy::AccountTenant,
                first_user: FirstUserTenant::NewUser,
            },
            AuthLifetimes::default(),
        )
        .unwrap()
}

#[test]
fn ordinary_write_capacity_is_available_while_auth_waits() {
    let _serial = RUNTIME_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = std::env::temp_dir().join(format!(
        "pp-auth-capacity-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let (owner, _) = WriterOwner::open(
        &root,
        Limits {
            queued_writes: 1,
            readers: 1,
        },
    )
    .unwrap();
    let storage = owner.client();
    let client = open_client(&owner);
    let db = Connection::open(root.join("print-partner.db")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let write = |key: &str| {
        storage.submit(
            SettingCommand::Set(Setting {
                tenant: "default".into(),
                key: key.into(),
                value: "saved".into(),
            }),
            &AtomicBool::new(false),
            Duration::ZERO,
        )
    };
    let blocking = write("blocking").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !storage.shared.queue.lock().unwrap().pending.is_empty() {
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let first = client
        .submit(
            Request::Status,
            Arc::new(AtomicBool::new(false)),
            Duration::ZERO,
        )
        .unwrap();
    let second = client
        .submit(
            Request::Status,
            Arc::new(AtomicBool::new(false)),
            Duration::ZERO,
        )
        .unwrap();
    let admitted = write("ordinary");
    let excess = write("excess");
    db.execute_batch("ROLLBACK").unwrap();
    assert!(
        excess.is_err(),
        "normal writes must retain their own capacity limit"
    );
    for reply in [
        blocking,
        admitted.expect("auth must not consume ordinary capacity"),
    ] {
        assert!(reply.recv_timeout(Duration::from_secs(5)).unwrap().unwrap());
    }
    for reply in [first, second] {
        assert!(matches!(
            reply.recv_timeout(Duration::from_secs(5)).unwrap().unwrap(),
            Outcome::Status(_)
        ));
    }
    assert_eq!(
        storage
            .reader(Duration::from_secs(5))
            .unwrap()
            .get_setting("default", "ordinary", None)
            .unwrap()
            .as_deref(),
        Some("saved")
    );
    drop(db);
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn concurrent_legacy_upgrade_reverifies_without_replacing_the_winning_hash() {
    let _serial = RUNTIME_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let root = std::env::temp_dir().join(format!(
        "pp-auth-upgrade-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    run(
        &open_client(&owner),
        Request::Register {
            email: "upgrade@example.com".into(),
            display_name: "Upgrade".into(),
            password: Secret::new("initial-password".into()),
        },
    )
    .unwrap();
    owner.shutdown().unwrap();
    let mut db = Connection::open(root.join("print-partner.db")).unwrap();
    db.execute(
        "UPDATE users SET password_hash=?1",
        [include_str!("../../../tests/node-scrypt.txt").trim()],
    )
    .unwrap();
    let tx = db.transaction().unwrap();
    let first = by_email(&tx, "upgrade@example.com").unwrap().unwrap();
    let second = by_email(&tx, "upgrade@example.com").unwrap().unwrap();
    tx.commit().unwrap();
    let (owner, _) = WriterOwner::open(&root, Limits::default()).unwrap();
    let client = open_client(&owner);
    let password = || Secret::new("legacy-🖨️-password".into());
    let hash = crypto::hash(&password()).unwrap();
    let first = client
        .submit_verified_login(first, hash.clone(), password())
        .unwrap()
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let second = client
        .submit_verified_login(second, crypto::hash(&password()).unwrap(), password())
        .unwrap()
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    for outcome in [first, second] {
        let Outcome::Session { token, .. } = outcome else {
            panic!("session expected")
        };
        assert!(matches!(
            run(&client, Request::ResolveSession { token }).unwrap(),
            Outcome::User(Some(_))
        ));
    }
    let stored: String = db
        .query_row("SELECT password_hash FROM users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, hash);
    let sessions: i64 = db
        .query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sessions, 3);
    drop(db);
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn poisoned_writer_queue_stops_auth_without_panicking_or_hanging() {
    let _serial = RUNTIME_TEST
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let directory = std::env::temp_dir().join(format!(
        "pp-auth-poison-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    let (owner, _) = WriterOwner::open(&directory, Limits::default()).unwrap();
    let client = open_client(&owner);
    let shared = owner.client().shared.clone();
    let poison = shared.clone();
    assert!(
        thread::spawn(move || {
            let _queue = poison.queue.lock().unwrap();
            panic!("injected queue poison");
        })
        .join()
        .is_err()
    );
    let reply = client
        .submit(
            Request::RequestReset {
                email: "poison@example.com".into(),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    let error = reply
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .err()
        .unwrap();
    assert!(matches!(
        error.downcast_ref::<AuthFailure>(),
        Some(AuthFailure::Stopped)
    ));
    assert!(shared.queue.lock().err().unwrap().get_ref().closed);
    shared.queue.clear_poison();
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
