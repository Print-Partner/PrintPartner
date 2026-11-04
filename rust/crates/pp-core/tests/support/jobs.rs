//! Job submission shared by the rebuild job tests.

use pp_storage::{
    auth::Secret,
    jobs::{AtomicJobClient, Credential, Outcome, Payload, UserOperation},
};
use std::{sync::atomic::AtomicBool, time::Duration};

/// Enqueue a version-1 `CheckSourceUpdates` job as the session `token` under
/// idempotency `key`, and return its ID.
pub fn enqueue_check(jobs: &AtomicJobClient, token: &str, key: &str) -> String {
    let session = Credential::Session(Secret::new(token.into()));
    let check = UserOperation::Enqueue {
        key: key.into(),
        payload_version: 1,
        payload: Payload::CheckSourceUpdates {},
    };
    let cancel = AtomicBool::new(false);
    let submitted = jobs.submit(session, check, &cancel, Duration::from_secs(5));
    let Outcome::Job(job, _) = submitted.unwrap().receive().unwrap() else {
        panic!("enqueued job expected");
    };
    job.job_id
}
