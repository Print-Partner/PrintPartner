use super::*;
use crate::{
    Limits, SettingSnapshot, WriterOwner,
    auth::{
        AuthFailure, AuthPolicy, FirstUserTenant, Outcome, RegistrationPolicy, Request, Secret,
        SessionTenantPolicy,
    },
    read_model::Credential,
    write::authorize,
};
use std::sync::Arc;

const WAIT: Duration = Duration::from_secs(5);
static IDLE: AtomicBool = AtomicBool::new(false);

fn opened(limits: Limits) -> (tempfile::TempDir, WriterOwner) {
    let directory = tempfile::tempdir().unwrap();
    let (owner, _) = WriterOwner::open(directory.path(), limits).unwrap();
    (directory, owner)
}

#[test]
fn a_read_returns_the_typed_result_and_cannot_write() {
    let (_directory, owner) = opened(Limits::default());
    let client = owner.client();
    let count = "SELECT count(*) FROM app_settings WHERE key='seam'";
    let counted = client.read("test.count", &IDLE, WAIT, |tx| {
        tx.query_row(count, [], |row| row.get::<_, i64>(0))
    });
    assert!(matches!(counted, Ok(Ok(0))), "{counted:?}");
    let refused = client.read("test.refuse", &IDLE, WAIT, |_| Err::<(), _>("refused"));
    assert!(matches!(refused, Ok(Err("refused"))));

    // The connection is read-only, so a family cannot write through a read.
    let insert = "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','seam','x')";
    let written = client.read("test.write", &IDLE, WAIT, |tx| tx.execute(insert, []));
    assert!(matches!(written, Ok(Err(_))), "{written:?}");
    let reader = client.reader(WAIT).unwrap();
    let stored = reader.snapshot("default", "seam").unwrap();
    assert_eq!(stored, SettingSnapshot::Missing);
}

#[test]
fn no_free_reader_a_cancelled_caller_and_stopped_storage_do_not_run_the_operation() {
    let limits = Limits {
        readers: 1,
        ..Limits::default()
    };
    let (_directory, owner) = opened(limits);
    let client = owner.client();
    let mut ran = false;
    let mut mark = |_: &Transaction<'_>| {
        ran = true;
        Ok::<_, std::convert::Infallible>(())
    };

    let held = client.reader(WAIT).unwrap();
    let busy = client.read("test.busy", &IDLE, Duration::from_millis(20), &mut mark);
    assert!(matches!(busy, Err(WriterFailure::Busy)), "{busy:?}");
    drop(held);
    let cancelled = client.read("test.cancelled", &AtomicBool::new(true), WAIT, &mut mark);
    assert!(
        matches!(cancelled, Err(WriterFailure::Cancelled)),
        "{cancelled:?}"
    );
    owner.shutdown().unwrap();
    let stopped = client.read("test.stopped", &IDLE, WAIT, &mut mark);
    assert!(
        matches!(stopped, Err(WriterFailure::Stopped)),
        "{stopped:?}"
    );
    assert!(!ran);
}

/// `authorize` only reads, so it runs on a read-only connection for a session
/// and for an API key, and types a refusal the same way as in a write.
#[test]
fn authorize_runs_inside_a_read() {
    let (_directory, owner) = opened(Limits::default());
    let client = owner.client();
    let policy = AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    };
    let auth = |request| {
        let client = owner.auth(FirstUserTenant::NewUser);
        let reply = client.submit(request, Arc::default(), WAIT);
        reply.unwrap().recv().unwrap().unwrap()
    };
    let Outcome::Session { user, token } = auth(Request::Register {
        email: "read@example.com".into(),
        display_name: "Read".into(),
        password: Secret::new("correct-horse".into()),
    }) else {
        panic!("registration returns a session")
    };
    let session = || Secret::new(token.expose().to_owned());
    let Outcome::KeyCreated { key, .. } = auth(Request::CreateKey { session: session() }) else {
        panic!("key creation returns the key")
    };
    let api_key = Credential::ApiKey {
        routed_tenant: user.tenant_id.clone(),
        secret: key,
    };
    let proven = |credential: Credential| {
        let checked = client.read("test.authorize", &IDLE, WAIT, |tx| {
            authorize(tx, &credential, policy)
        });
        checked.unwrap()
    };
    for credential in [Credential::Session(session()), api_key] {
        assert_eq!(proven(credential).unwrap().id(), user.tenant_id);
    }
    let refused = proven(Credential::Session(Secret::new("not-a-session".into())));
    assert!(
        matches!(
            refused,
            Err(WriterFailure::Auth(AuthFailure::SessionRequired))
        ),
        "{refused:?}"
    );
}
