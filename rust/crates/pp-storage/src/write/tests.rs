use super::*;
use crate::{
    Limits, SettingSnapshot, WriterOwner,
    auth::{FirstUserTenant, Outcome, RegistrationPolicy, Request, Secret, SessionTenantPolicy},
};
use rusqlite::params;
use std::{
    convert::Infallible,
    sync::{Arc, atomic::Ordering},
    thread,
};

const WAIT: Duration = Duration::from_secs(5);
static IDLE: AtomicBool = AtomicBool::new(false);

fn opened(limits: Limits) -> (tempfile::TempDir, WriterOwner) {
    let directory = tempfile::tempdir().unwrap();
    let (owner, _) = WriterOwner::open(directory.path(), limits).unwrap();
    (directory, owner)
}

fn put(tx: &Transaction<'_>, value: &str) -> rusqlite::Result<usize> {
    tx.execute(
        "INSERT INTO app_settings(tenant_id,key,value) VALUES('default','seam',?1)",
        params![value],
    )
}

fn stored(owner: &WriterOwner) -> SettingSnapshot {
    let reader = owner.client().reader(WAIT).unwrap();
    reader.snapshot("default", "seam").unwrap()
}

#[test]
fn ok_commits_and_err_rolls_back_with_the_typed_result() {
    let (_directory, owner) = opened(Limits::default());
    let shared = owner.client.shared.clone();

    let refused = shared.write("test.refuse", &IDLE, WAIT, |tx, _| {
        put(tx, "discarded").unwrap();
        Err::<(), _>("refused")
    });
    assert!(matches!(refused, Ok(Err("refused"))));
    assert_eq!(stored(&owner), SettingSnapshot::Missing);

    let written = shared.write("test.keep", &IDLE, WAIT, |tx, _| {
        put(tx, "kept").map(|rows| rows + 41)
    });
    assert!(matches!(written, Ok(Ok(42))), "{written:?}");
    let value = "kept".to_owned();
    assert_eq!(stored(&owner), SettingSnapshot::Stored { value });

    // The failed insert is the operation's own error, and nothing new is kept.
    let duplicate = shared.write("test.duplicate", &IDLE, WAIT, |tx, _| put(tx, "again"));
    assert!(matches!(duplicate, Ok(Err(_))), "{duplicate:?}");
}

#[test]
fn a_full_queue_a_cancelled_caller_and_a_stopped_writer_do_not_run_the_operation() {
    let limits = Limits {
        queued_writes: 1,
        ..Limits::default()
    };
    let (_directory, owner) = opened(limits);
    let shared = owner.client.shared.clone();
    let ran = Arc::new(AtomicBool::new(false));
    let mark = |ran: &Arc<AtomicBool>| {
        let ran = ran.clone();
        move |_: &Transaction<'_>, _| {
            ran.store(true, Ordering::Release);
            Ok::<_, Infallible>(())
        }
    };

    // Hold the writer thread inside one write and fill the one queue slot.
    let (entered, is_running) = mpsc::channel();
    let (release, held) = mpsc::channel::<()>();
    let holder = {
        let shared = shared.clone();
        thread::spawn(move || {
            shared.write("test.hold", &IDLE, WAIT, move |_, _| {
                entered.send(()).unwrap();
                held.recv().unwrap();
                Ok::<_, Infallible>(())
            })
        })
    };
    is_running.recv().unwrap();
    let queued = {
        let shared = shared.clone();
        thread::spawn(move || {
            shared.write("test.queued", &IDLE, WAIT, |_, _| Ok::<_, Infallible>(7))
        })
    };
    while shared.queue.lock().unwrap().pending.is_empty() {
        thread::yield_now();
    }

    let full = shared.write("test.full", &IDLE, Duration::from_millis(20), mark(&ran));
    assert!(matches!(full, Err(WriterFailure::QueueFull)), "{full:?}");
    let cancelled = shared.write("test.cancelled", &AtomicBool::new(true), WAIT, mark(&ran));
    assert!(
        matches!(cancelled, Err(WriterFailure::Cancelled)),
        "{cancelled:?}"
    );

    release.send(()).unwrap();
    assert!(matches!(holder.join().unwrap(), Ok(Ok(()))));
    assert!(matches!(queued.join().unwrap(), Ok(Ok(7))));

    owner.shutdown().unwrap();
    let stopped = shared.write("test.stopped", &IDLE, WAIT, mark(&ran));
    assert!(
        matches!(stopped, Err(WriterFailure::Stopped)),
        "{stopped:?}"
    );
    assert!(!ran.load(Ordering::Acquire));
}

#[test]
fn a_deadline_that_passed_before_the_start_is_busy_and_does_not_run_the_operation() {
    let mut connection = Connection::open_in_memory().unwrap();
    let mut ran = false;
    let refused = run(&mut connection, Deadline::expired(), |_, _| {
        ran = true;
        Ok::<_, Infallible>(())
    });
    assert!(matches!(refused, Err(WriterFailure::Busy)), "{refused:?}");
    assert!(!ran);
}

#[test]
fn authorize_names_the_installation_and_types_a_refusal() {
    let (_directory, owner) = opened(Limits::default());
    let shared = owner.client.shared.clone();
    let policy = AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    };
    let auth = |request| {
        let reply = owner.auth(FirstUserTenant::NewUser).submit(
            request,
            Arc::new(AtomicBool::new(false)),
            WAIT,
        );
        reply.unwrap().recv().unwrap().unwrap()
    };
    let Outcome::Session { user, token } = auth(Request::Register {
        email: "seam@example.com".into(),
        display_name: "Seam".into(),
        password: Secret::new("correct-horse".into()),
    }) else {
        panic!("registration returns a session")
    };
    let session = || Secret::new(token.expose().to_owned());
    let Outcome::KeyCreated { key, .. } = auth(Request::CreateKey { session: session() }) else {
        panic!("key creation returns the key")
    };
    let api_key = |secret: Secret| Credential::ApiKey {
        routed_tenant: user.tenant_id.clone(),
        secret,
    };
    let proven = |credential: Credential| {
        let checked = shared.write("test.authorize", &IDLE, WAIT, move |tx, _| {
            authorize(tx, &credential, policy)
        });
        checked.unwrap()
    };

    for credential in [Credential::Session(session()), api_key(key)] {
        assert_eq!(proven(credential).unwrap().id(), user.tenant_id);
    }
    for credential in [
        Credential::Session(Secret::new("not-a-session".into())),
        Credential::Session(Secret::new(String::new())),
        api_key(Secret::new("not-a-key".into())),
    ] {
        let refused = proven(credential);
        assert!(
            matches!(
                refused,
                Err(WriterFailure::Auth(AuthFailure::SessionRequired))
            ),
            "{refused:?}"
        );
    }
}
