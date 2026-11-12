#![cfg(feature = "desktop_rebuild")]
//! `ReadClient::read_plan` (GRE-1447) reads on the reader pool and refuses
//! with typed `WriterFailure`s: a refused credential, a cancelled caller, a
//! reader pool that stays full, and stopped storage.
use pp_storage::{
    Limits, WriterFailure, WriterOwner,
    auth::{AuthFailure, Secret},
    read_model::{AcceptedRead, Credential},
};
use std::{sync::atomic::AtomicBool, time::Duration};

const WAIT: Duration = Duration::from_millis(50);

fn session(secret: &str) -> Credential {
    Credential::Session(Secret::new(secret.into()))
}

#[test]
fn read_plan_reads_on_the_reader_pool_with_typed_refusals() {
    let directory = tempfile::tempdir().unwrap();
    // The accepted Plan's pinned Source revision.
    std::fs::create_dir_all(directory.path().join("repos/1/revisions/fixture")).unwrap();
    std::fs::write(
        directory.path().join("print-partner.db"),
        include_bytes!("fixtures/required-units/unchanged.db"),
    )
    .unwrap();
    let owner = WriterOwner::open(directory.path(), Limits::default())
        .unwrap()
        .0;
    let reads = owner.accepted_reads().unwrap();
    let go = AtomicBool::new(false);
    let fixture = || session("required-unit-fixture-secret");

    let plan = reads.read_plan(fixture(), Some(1), &go, WAIT).unwrap();
    let AcceptedRead::Ready { snapshot } = plan else {
        panic!("{plan:?}");
    };
    assert_eq!(snapshot.profile.name, "Fixture Build");
    assert!(matches!(
        reads.read_plan(fixture(), None, &go, WAIT),
        Ok(AcceptedRead::Missing)
    ));
    assert!(
        matches!(
            reads.read_plan(fixture(), Some(2), &go, WAIT),
            Ok(AcceptedRead::Missing)
        ),
        "another tenant's Build"
    );

    let refused = |credential| reads.read_plan(credential, Some(1), &go, WAIT);
    assert!(matches!(
        refused(session("wrong")),
        Err(WriterFailure::Auth(_))
    ));
    assert!(matches!(
        refused(session(&"x".repeat(5000))),
        Err(WriterFailure::Auth(AuthFailure::InvalidCredentials))
    ));
    let key = Credential::ApiKey {
        routed_tenant: "default".into(),
        secret: Secret::new("ppk_not_a_key".into()),
    };
    assert!(matches!(
        refused(key),
        Err(WriterFailure::Auth(AuthFailure::InvalidCredentials))
    ));
    assert!(matches!(
        reads.read_plan(fixture(), Some(1), &AtomicBool::new(true), WAIT),
        Err(WriterFailure::Cancelled)
    ));

    let settings = owner.client();
    let held: Vec<_> = std::iter::from_fn(|| settings.reader(Duration::ZERO).ok()).collect();
    assert_eq!(held.len(), Limits::default().readers);
    assert!(matches!(
        reads.read_plan(fixture(), Some(1), &go, WAIT),
        Err(WriterFailure::QueueFull)
    ));
    drop(held);
    assert!(
        reads.read_plan(fixture(), Some(1), &go, WAIT).is_ok(),
        "readers return"
    );

    owner.shutdown().unwrap();
    assert!(matches!(
        reads.read_plan(fixture(), Some(1), &go, WAIT),
        Err(WriterFailure::Stopped)
    ));
}
