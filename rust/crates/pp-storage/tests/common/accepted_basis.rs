use pp_storage::{
    WriterOwner,
    auth::Secret,
    checkoff_progress::Basis,
    read_model::{AcceptedRead, Credential},
};
use std::{sync::atomic::AtomicBool, time::Duration};

/// Build 1's Accepted Plan basis, read as the required-units fixture's session.
pub fn basis(owner: &WriterOwner) -> Basis {
    let batch = owner
        .accepted_reads()
        .unwrap()
        .read(
            Credential::Session(Secret::new("required-unit-fixture-secret".into())),
            &[1],
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    match batch.builds.into_iter().next().unwrap().accepted {
        AcceptedRead::Ready { snapshot } => Basis::from(&*snapshot),
        other => panic!("fixture is not ready: {other:?}"),
    }
}
