use pp_storage::{WriterOwner, auth::Secret, checkoff_progress::Request, read_model::Credential};
use serde_json::Value;
use std::{sync::atomic::AtomicBool, time::Duration};

/// Applies one Checkoff request as the fixture's session and returns the
/// response as JSON.
pub fn apply(owner: &WriterOwner, request: Request) -> Value {
    let response = owner
        .checkoff_progress()
        .unwrap()
        .apply(
            Credential::Session(Secret::new("required-unit-fixture-secret".into())),
            request,
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    serde_json::to_value(response).unwrap()
}
