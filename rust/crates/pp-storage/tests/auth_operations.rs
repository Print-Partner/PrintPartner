use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthClient, FirstUserTenant, Outcome, Provider, Request, Secret},
};
use rusqlite::{Connection, params};
use std::os::unix::fs::DirBuilderExt;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "pp-auth-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)
        .unwrap();
    path
}
fn open(path: &std::path::Path) -> WriterOwner {
    WriterOwner::open(path, Limits::default()).unwrap().0
}
fn seed_keys(path: &std::path::Path, tenant: &str, value: &str) {
    let db = Connection::open(path.join("print-partner.db")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute("INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,'api_keys_v1',?2) ON CONFLICT(tenant_id,key) DO UPDATE SET value=excluded.value", params![tenant, value]).unwrap();
}
fn secret(value: &str) -> Secret {
    Secret::new(value.to_owned())
}
fn call(client: &AuthClient, request: Request) -> anyhow::Result<Outcome> {
    client
        .submit(
            request,
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )?
        .recv()
        .unwrap()
}
fn register(client: &AuthClient, email: &str) -> (pp_storage::auth::User, String) {
    let Outcome::Session { user, token } = call(
        client,
        Request::Register {
            email: email.into(),
            display_name: "  User Name  ".into(),
            password: secret("password-before"),
        },
    )
    .unwrap() else {
        panic!("session expected")
    };
    (user, token.expose().to_owned())
}
fn login(client: &AuthClient, email: &str, password: &str) -> anyhow::Result<Outcome> {
    call(
        client,
        Request::Login {
            email: email.into(),
            password: secret(password),
        },
    )
}
fn resolve(client: &AuthClient, token: &str) -> Option<pp_storage::auth::User> {
    let Outcome::User(user) = call(
        client,
        Request::ResolveSession {
            token: secret(token),
        },
    )
    .unwrap() else {
        panic!("user expected")
    };
    user
}
fn reset_token(client: &AuthClient, email: &str) -> String {
    let Outcome::ResetToken(Some(token)) = call(
        client,
        Request::RequestReset {
            email: email.into(),
        },
    )
    .unwrap() else {
        panic!("reset expected")
    };
    token.expose().to_owned()
}
#[test]
fn ticket_t_17_credentials() {
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::ClaimDefault);
    let a = client
        .submit(
            Request::Register {
                email: "FIRST@EXAMPLE.COM".into(),
                display_name: "\u{feff}  ".into(),
                password: secret("😀😀😀😀"),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    let b = client
        .submit(
            Request::Register {
                email: "second@example.com".into(),
                display_name: " Second ".into(),
                password: secret("password-before"),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    let results: Vec<_> = [a, b]
        .into_iter()
        .map(|r| {
            let Outcome::Session { user, token } = r.recv().unwrap().unwrap() else {
                panic!("session expected")
            };
            (user, token)
        })
        .collect();
    assert_eq!(results.iter().filter(|(u, _)| u.is_admin).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|(u, _)| u.user_id == "default")
            .count(),
        1
    );
    assert_eq!(results[0].0.display_name, "User");
    assert_eq!(results[0].0.email.as_deref(), Some("first@example.com"));
    assert!(
        call(
            &client,
            Request::Register {
                email: "first@example.com".into(),
                display_name: "duplicate".into(),
                password: secret("password-before")
            }
        )
        .is_err()
    );
    assert!(login(&client, "first@example.com", "wrong-password").is_err());
    let reset = reset_token(&client, "first@example.com");
    assert_eq!(reset.len(), 43);
    let x = client
        .submit(
            Request::ResetPassword {
                token: secret(&reset),
                replacement: secret("password-after"),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    let y = client
        .submit(
            Request::ResetPassword {
                token: secret(&reset),
                replacement: secret("password-after"),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    let resets = [x.recv().unwrap().unwrap(), y.recv().unwrap().unwrap()];
    assert_eq!(
        resets
            .iter()
            .filter(|o| matches!(o, Outcome::Session { .. }))
            .count(),
        1
    );
    assert_eq!(
        resets
            .iter()
            .filter(|o| matches!(o, Outcome::Changed(false)))
            .count(),
        1
    );
    assert!(resolve(&client, results[0].1.expose()).is_none());
    assert!(login(&client, "first@example.com", "😀😀😀😀").is_err());
    let Outcome::Session { token, .. } =
        login(&client, "first@example.com", "password-after").unwrap()
    else {
        panic!("session expected")
    };
    let pending = reset_token(&client, "first@example.com");
    assert!(
        call(
            &client,
            Request::ChangePassword {
                session: secret(token.expose()),
                current: secret("wrong"),
                replacement: secret("password-final")
            }
        )
        .is_err()
    );
    let Outcome::Session {
        token: new_token, ..
    } = call(
        &client,
        Request::ChangePassword {
            session: secret(token.expose()),
            current: secret("password-after"),
            replacement: secret("password-final"),
        },
    )
    .unwrap()
    else {
        panic!("fresh session expected")
    };
    assert!(resolve(&client, token.expose()).is_none());
    assert!(resolve(&client, new_token.expose()).is_some());
    assert!(matches!(
        call(
            &client,
            Request::ResetPassword {
                token: secret(&pending),
                replacement: secret("must-not-work")
            }
        )
        .unwrap(),
        Outcome::Changed(false)
    ));
    owner.shutdown().unwrap();
    let db = Connection::open(path.join("print-partner.db")).unwrap();
    let hashes: Vec<String> = db
        .prepare("SELECT password_hash FROM users")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        hashes
            .iter()
            .all(|h| h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"))
    );
    assert!(hashes.iter().all(|h| !h.contains("password")));
}
#[test]
fn ticket_t_17_sessions() {
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let before = time::OffsetDateTime::now_utc();
    let (user, token) = register(&client, "sessions@example.com");
    let after = time::OffsetDateTime::now_utc();
    assert_ne!(user.user_id, "default");
    assert_eq!(user.user_id, user.tenant_id);
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    owner.shutdown().unwrap();
    let db = Connection::open(path.join("print-partner.db")).unwrap();
    let (id, expires): (String, String) = db
        .query_row("SELECT id,expires_at FROM sessions", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_ne!(id, token);
    assert_eq!(id.len(), 64);
    let expiry =
        time::OffsetDateTime::parse(&expires, &time::format_description::well_known::Rfc3339)
            .unwrap();
    assert!(expiry >= before + time::Duration::days(14) - time::Duration::milliseconds(1));
    assert!(expiry <= after + time::Duration::days(14));
    drop(db);
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert_eq!(resolve(&client, &token), Some(user));
    assert!(matches!(
        call(
            &client,
            Request::Logout {
                token: secret(&token)
            }
        )
        .unwrap(),
        Outcome::Changed(true)
    ));
    assert!(resolve(&client, &token).is_none());
    owner.shutdown().unwrap();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert!(resolve(&client, &token).is_none());
    let Outcome::Session { token, .. } =
        login(&client, "sessions@example.com", "password-before").unwrap()
    else {
        panic!("session expected")
    };
    call(
        &client,
        Request::LogoutAll {
            session: secret(token.expose()),
        },
    )
    .unwrap();
    assert!(resolve(&client, token.expose()).is_none());
    owner.shutdown().unwrap();
}
#[test]
fn resolved_oauth_session_preserves_authenticated_provider_after_reopen() {
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let Outcome::Session { user, token } = call(
        &client,
        Request::OAuthLogin {
            provider: Provider::Github,
            provider_user_id: "session-account".into(),
            email: Some("session-account@example.com".into()),
            display_name: "Session Account".into(),
        },
    )
    .unwrap() else {
        panic!("session expected")
    };
    assert_eq!(user.provider, Provider::Github);
    let account = user;
    assert_eq!(resolve(&client, token.expose()), Some(account.clone()));
    owner.shutdown().unwrap();

    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert_eq!(resolve(&client, token.expose()), Some(account.clone()));
    let Outcome::Session { user: linked, .. } = call(
        &client,
        Request::OAuthLogin {
            provider: Provider::Github,
            provider_user_id: "session-account".into(),
            email: Some("different-account@example.com".into()),
            display_name: "Different Account".into(),
        },
    )
    .unwrap() else {
        panic!("linked session expected")
    };
    assert_eq!(linked, account);
    assert_eq!(resolve(&client, token.expose()), Some(account));
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_17_identity_and_tenant_keys() {
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let (a, at) = register(&client, "a@example.com");
    let (b, bt) = register(&client, "b@example.com");
    call(
        &client,
        Request::LinkIdentity {
            session: secret(&at),
            provider: Provider::Github,
            provider_user_id: "gh-a".into(),
        },
    )
    .unwrap();
    assert!(
        call(
            &client,
            Request::LinkIdentity {
                session: secret(&bt),
                provider: Provider::Github,
                provider_user_id: "gh-a".into()
            }
        )
        .is_err()
    );
    let Outcome::Session { user, .. } = call(
        &client,
        Request::OAuthLogin {
            provider: Provider::Github,
            provider_user_id: "gh-a".into(),
            email: Some("b@example.com".into()),
            display_name: "ignored".into(),
        },
    )
    .unwrap() else {
        panic!("session expected")
    };
    assert_eq!(user.user_id, a.user_id);
    assert_eq!(user.provider, Provider::Github);
    let Outcome::Session { user, .. } = call(
        &client,
        Request::OAuthLogin {
            provider: Provider::Discord,
            provider_user_id: "discord-b".into(),
            email: Some("B@EXAMPLE.COM".into()),
            display_name: "ignored".into(),
        },
    )
    .unwrap() else {
        panic!("session expected")
    };
    assert_eq!(user.user_id, b.user_id);
    let Outcome::KeyCreated { info, key } = call(
        &client,
        Request::CreateKey {
            session: secret(&at),
        },
    )
    .unwrap() else {
        panic!("key expected")
    };
    assert_eq!(key.expose().len(), 68);
    assert!(key.expose().starts_with("ppk_"));
    assert_eq!(info.id.len(), 20);
    assert!(matches!(
        call(
            &client,
            Request::ResolveKey {
                tenant_id: b.tenant_id.clone(),
                key: secret(key.expose())
            }
        )
        .unwrap(),
        Outcome::KeyResolved {
            principal: None,
            ..
        }
    ));
    assert!(matches!(
        call(
            &client,
            Request::ResolveKey {
                tenant_id: a.tenant_id.clone(),
                key: secret(key.expose())
            }
        )
        .unwrap(),
        Outcome::KeyResolved {
            principal: Some(_),
            ..
        }
    ));
    assert!(matches!(
        call(
            &client,
            Request::RevokeKey {
                session: secret(&bt),
                key_id: info.id.clone()
            }
        )
        .unwrap(),
        Outcome::KeyChanged { changed: false, .. }
    ));
    let Outcome::KeyCreated {
        info: new_info,
        key: new_key,
    } = call(
        &client,
        Request::RotateKey {
            session: secret(&at),
            key_id: info.id,
        },
    )
    .unwrap()
    else {
        panic!("key expected")
    };
    assert!(matches!(
        call(
            &client,
            Request::ResolveKey {
                tenant_id: a.tenant_id.clone(),
                key: secret(key.expose())
            }
        )
        .unwrap(),
        Outcome::KeyResolved {
            principal: None,
            ..
        }
    ));
    call(
        &client,
        Request::RevokeKey {
            session: secret(&at),
            key_id: new_info.id,
        },
    )
    .unwrap();
    assert!(matches!(
        call(
            &client,
            Request::ResolveKey {
                tenant_id: a.tenant_id.clone(),
                key: secret(new_key.expose())
            }
        )
        .unwrap(),
        Outcome::KeyResolved {
            principal: None,
            ..
        }
    ));
    let Outcome::Keys { keys, .. } = call(
        &client,
        Request::ListKeys {
            session: secret(&at),
        },
    )
    .unwrap() else {
        panic!("keys expected")
    };
    let json = serde_json::to_string(&keys).unwrap();
    assert!(!json.contains("keyHash"));
    assert!(!json.contains(new_key.expose()));
    seed_keys(&path, &a.tenant_id, "{broken");
    assert!(
        call(
            &client,
            Request::ListKeys {
                session: secret(&at)
            }
        )
        .is_err()
    );
    assert!(
        call(
            &client,
            Request::CreateKey {
                session: secret(&at)
            }
        )
        .is_err()
    );
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_17_reset_rollback_and_cancellation() {
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let (_, token) = register(&client, "rollback@example.com");
    let reset = reset_token(&client, "rollback@example.com");
    owner.shutdown().unwrap();
    let db = Connection::open(path.join("print-partner.db")).unwrap();
    let before: String = db
        .query_row("SELECT password_hash FROM users", [], |r| r.get(0))
        .unwrap();
    db.execute_batch("CREATE TRIGGER reject_new_session BEFORE INSERT ON sessions BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;").unwrap();
    drop(db);
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert!(
        call(
            &client,
            Request::ResetPassword {
                token: secret(&reset),
                replacement: secret("rollback-password")
            }
        )
        .is_err()
    );
    assert!(resolve(&client, &token).is_some());
    owner.shutdown().unwrap();
    let db = Connection::open(path.join("print-partner.db")).unwrap();
    let after: String = db
        .query_row("SELECT password_hash FROM users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
    let resets: i64 = db
        .query_row("SELECT count(*) FROM password_reset_tokens", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(resets, 1);
    db.execute_batch("DROP TRIGGER reject_new_session").unwrap();
    drop(db);
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert!(
        client
            .submit(
                Request::Logout {
                    token: secret(&token)
                },
                Arc::new(AtomicBool::new(true)),
                Duration::ZERO
            )
            .is_err()
    );
    assert!(resolve(&client, &token).is_some());
    let reply = client
        .submit(
            Request::ResetPassword {
                token: secret(&reset),
                replacement: secret("final-password"),
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(5),
        )
        .unwrap();
    drop(reply);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while resolve(&client, &token).is_some() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    owner.shutdown().unwrap();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert!(login(&client, "rollback@example.com", "final-password").is_ok());
    assert!(matches!(
        call(
            &client,
            Request::ResetPassword {
                token: secret(&reset),
                replacement: secret("another-password")
            }
        )
        .unwrap(),
        Outcome::Changed(false)
    ));
    owner.shutdown().unwrap();
}
#[test]
fn ticket_t_17_missing_and_expired_sessions() {
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let (_, token) = register(&client, "expired@example.com");
    let reset = reset_token(&client, "expired@example.com");
    owner.shutdown().unwrap();
    let db = Connection::open(path.join("print-partner.db")).unwrap();
    db.execute(
        "UPDATE sessions SET expires_at=?1",
        ["2000-01-01T00:00:00.000Z"],
    )
    .unwrap();
    db.execute(
        "UPDATE password_reset_tokens SET expires_at=?1",
        ["2000-01-01T00:00:00.000Z"],
    )
    .unwrap();
    drop(db);
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert!(resolve(&client, &token).is_none());
    assert!(matches!(
        call(
            &client,
            Request::ResetPassword {
                token: secret(&reset),
                replacement: secret("expired-password")
            }
        )
        .unwrap(),
        Outcome::Changed(false)
    ));
    owner.shutdown().unwrap();
    let db = Connection::open(path.join("print-partner.db")).unwrap();
    db.execute(
        "UPDATE sessions SET expires_at=?1",
        ["2999-01-01T00:00:00.000Z"],
    )
    .unwrap();
    db.execute(
        "DELETE FROM users WHERE email=?1",
        params!["expired@example.com"],
    )
    .unwrap();
    drop(db);
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert!(resolve(&client, &token).is_none());
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_17_hostile_costs_reject_before_hashing() {
    // crypto's matching unit test proves rejection never invokes either KDF.
    let path = directory();
    let owner = open(&path);
    owner.shutdown().unwrap();
    let db = Connection::open(path.join("print-partner.db")).unwrap();
    let invalid = [
        "scrypt:1073741824:8:1:AAAA:AAAA",
        "scrypt:16384:4294967295:1:AAAA:AAAA",
        "scrypt:16384:8:999999999:AAAA:AAAA",
        "scrypt:16384:8:1:AAAA:AAAA",
        "$argon2id$v=19$m=4294967295,t=2,p=1$c29tZXNhbHQ$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "$argon2id$v=19$m=19456,t=999999999,p=1$c29tZXNhbHQ$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "not-a-password-hash",
    ];
    for (i, hash) in invalid.iter().enumerate() {
        db.execute("INSERT INTO users(id,email,display_name,password_hash,is_admin,created_at) VALUES(?1,?2,'Hostile',?3,0,'2020-01-01T00:00:00.000Z')",params![i.to_string(),format!("hostile{i}@example.com"),hash]).unwrap();
    }
    drop(db);
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    for i in 0..invalid.len() {
        assert!(
            login(
                &client,
                &format!("hostile{i}@example.com"),
                "password-before"
            )
            .is_err()
        );
    }
    assert!(
        client
            .submit(
                Request::Login {
                    email: "hostile0@example.com".into(),
                    password: Secret::new("a".repeat(4097))
                },
                Arc::new(AtomicBool::new(false)),
                Duration::ZERO
            )
            .is_err()
    );
    owner.shutdown().unwrap();
}

#[test]
fn ticket_t_17_key_rotation_rollback() {
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let (_, token) = register(&client, "key-rollback@example.com");
    let Outcome::KeyCreated { info, key } = call(
        &client,
        Request::CreateKey {
            session: secret(&token),
        },
    )
    .unwrap() else {
        panic!("key expected")
    };
    owner.shutdown().unwrap();
    let db = Connection::open(path.join("print-partner.db")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_key_update BEFORE UPDATE ON app_settings WHEN NEW.key='api_keys_v1' BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;").unwrap();
    drop(db);
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert!(
        call(
            &client,
            Request::RotateKey {
                session: secret(&token),
                key_id: info.id.clone()
            }
        )
        .is_err()
    );
    let Outcome::Keys { keys, .. } = call(
        &client,
        Request::ListKeys {
            session: secret(&token),
        },
    )
    .unwrap() else {
        panic!("keys expected")
    };
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].id, info.id);
    assert!(keys[0].is_active);
    owner.shutdown().unwrap();
    let db = Connection::open(path.join("print-partner.db")).unwrap();
    let persisted: String = db
        .query_row(
            "SELECT value FROM app_settings WHERE key='api_keys_v1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!persisted.contains(key.expose()));
}

#[test]
fn ticket_t_17_key_collection_growth_rejects_atomically() {
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let (user, token) = register(&client, "key-bound@example.com");
    let mut collection = serde_json::json!([{
        "id": "legacy", "keyHash": "a".repeat(64), "createdAt": "",
        "lastUsedAt": null, "expiresAt": null, "isActive": true
    }]);
    let padding = 1024 * 1024 - 16 - collection.to_string().len();
    collection[0]["createdAt"] = serde_json::Value::String("a".repeat(padding));
    let raw = collection.to_string();
    let storage = owner.client();
    seed_keys(&path, &user.tenant_id, &raw);
    assert!(
        call(
            &client,
            Request::CreateKey {
                session: secret(&token)
            }
        )
        .is_err()
    );
    let reader = storage.reader(Duration::from_secs(5)).unwrap();
    assert_eq!(
        reader
            .get_setting(&user.tenant_id, "api_keys_v1", None)
            .unwrap(),
        Some(raw)
    );
    drop(reader);
    owner.shutdown().unwrap();
}

#[test]
fn session_provider_migrates_old_rows_and_rejects_unknown_values() {
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let (user, token) = register(&client, "legacy-session@example.com");
    owner.shutdown().unwrap();

    let db = Connection::open(path.join("print-partner.db")).unwrap();
    let session: (String, String) = db
        .query_row(
            "SELECT id,expires_at FROM sessions WHERE user_id=?1",
            [&user.user_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    db.execute_batch("ALTER TABLE sessions DROP COLUMN provider; DELETE FROM sessions")
        .unwrap();
    db.execute(
        "INSERT INTO sessions(id,user_id,expires_at) VALUES(?1,?2,?3)",
        params![session.0, user.user_id, session.1],
    )
    .unwrap();
    drop(db);

    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert_eq!(resolve(&client, &token).unwrap().provider, Provider::Email);
    owner.shutdown().unwrap();

    let db = Connection::open(path.join("print-partner.db")).unwrap();
    db.execute("UPDATE sessions SET provider='unknown'", [])
        .unwrap();
    drop(db);
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let error = match call(
        &client,
        Request::ResolveSession {
            token: secret(&token),
        },
    ) {
        Err(error) => error,
        Ok(_) => panic!("invalid provider accepted"),
    };
    assert!(matches!(
        error.downcast_ref(),
        Some(pp_storage::auth::AuthFailure::Storage)
    ));
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn legacy_base64_key_migration_preserves_access_after_reopen() {
    const RAW: &str = "ppk_1212121212121212121212121212121212121212121212121212121212121212";
    const ENCODED: &str = "cHBrXzEyMTIxMjEyMTIxMjEyMTIxMjEyMTIxMjEyMTIxMjEyMTIxMjEyMTIxMjEyMTIxMjEyMTIxMjEyMTIxMjEyMTI=";
    const EXPECTED_HASH: &str = "02ea6a27919f5a598dfab53b246d464dde14dcb5233309302dfbfb19d21a1d7a";
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let (user, _) = register(&client, "legacy-key@example.com");
    let collection = serde_json::json!([{
        "id": "legacy-key", "keyHash": ENCODED,
        "createdAt": "2020-01-01T00:00:00.000Z", "lastUsedAt": null,
        "expiresAt": null, "isActive": true
    }]);
    seed_keys(&path, &user.tenant_id, &collection.to_string());
    let resolve_key = |client: &AuthClient, raw: &str| {
        call(
            client,
            Request::ResolveKey {
                tenant_id: user.tenant_id.clone(),
                key: secret(raw),
            },
        )
        .unwrap()
    };
    let assert_access = |outcome| {
        let Outcome::KeyResolved {
            principal: Some(principal),
            commit,
        } = outcome
        else {
            panic!("legacy key must remain authorized")
        };
        assert_eq!(principal.tenant_id, user.tenant_id);
        assert_eq!(principal.key_id, "legacy-key");
        assert!(matches!(commit, pp_storage::auth::LocalCommit::Committed));
    };
    let saved = |owner: &WriterOwner| {
        let reader = owner.client().reader(Duration::from_secs(5)).unwrap();
        let value = reader
            .get_setting(&user.tenant_id, "api_keys_v1", None)
            .unwrap()
            .unwrap();
        serde_json::from_str::<serde_json::Value>(&value).unwrap()
    };
    assert_access(resolve_key(&client, RAW));
    let migrated = saved(&owner);
    assert_eq!(migrated[0]["keyHash"], EXPECTED_HASH);
    assert!(migrated[0]["lastUsedAt"].is_string());
    owner.shutdown().unwrap();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    assert_eq!(saved(&owner), migrated);
    assert_access(resolve_key(&client, RAW));
    assert_eq!(saved(&owner)[0]["keyHash"], EXPECTED_HASH);
    for wrong in [ENCODED, "ppk_wrong"] {
        assert!(matches!(
            resolve_key(&client, wrong),
            Outcome::KeyResolved {
                principal: None,
                ..
            }
        ));
    }
    owner.shutdown().unwrap();
}

#[test]
fn generic_settings_refuse_auth_owned_keys_and_write_others() {
    use pp_storage::{Setting, SettingCommand, SettingSnapshot};
    let path = directory();
    let owner = open(&path);
    let client = owner.auth(FirstUserTenant::NewUser);
    let (user, token) = register(&client, "settings-guard@example.com");
    let Outcome::KeyCreated { key, .. } = call(
        &client,
        Request::CreateKey {
            session: secret(&token),
        },
    )
    .unwrap() else {
        panic!("key expected")
    };
    let storage = owner.client();
    let stored = |key: &str| {
        storage
            .reader(Duration::from_secs(5))
            .unwrap()
            .get_setting(&user.tenant_id, key, None)
            .unwrap()
    };
    let before = stored("api_keys_v1").unwrap();
    let setting = |key: &str, value: &str| Setting {
        tenant: user.tenant_id.clone(),
        key: key.into(),
        value: value.into(),
    };
    for command in [
        SettingCommand::Set(setting("api_keys_v1", "[]")),
        SettingCommand::CompareAndSet {
            setting: setting("api_keys_v1", "[]"),
            expected: SettingSnapshot::Stored {
                value: before.clone(),
            },
        },
    ] {
        let refused = storage
            .submit(command, &AtomicBool::new(false), Duration::from_secs(5))
            .expect_err("auth-owned settings must be refused");
        assert_eq!(
            refused.to_string(),
            "Setting api_keys_v1 is owned by auth and changes only through auth requests"
        );
    }
    assert_eq!(stored("api_keys_v1"), Some(before));
    let Outcome::KeyResolved {
        principal: Some(principal),
        ..
    } = call(
        &client,
        Request::ResolveKey {
            tenant_id: user.tenant_id.clone(),
            key: secret(key.expose()),
        },
    )
    .unwrap()
    else {
        panic!("the key must stay authorized")
    };
    assert_eq!(principal.tenant_id, user.tenant_id);

    assert!(
        storage
            .submit(
                SettingCommand::Set(setting("theme", "dark")),
                &AtomicBool::new(false),
                Duration::from_secs(5),
            )
            .unwrap()
            .recv()
            .unwrap()
            .unwrap()
    );
    assert_eq!(stored("theme").as_deref(), Some("dark"));
    owner.shutdown().unwrap();
    std::fs::remove_dir_all(path).unwrap();
}
