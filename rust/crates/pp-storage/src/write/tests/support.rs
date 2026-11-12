//! Fixtures for the tests of families that write through the write seam.

use crate::{
    Setting, SettingCommand, WriterOwner,
    auth::{FirstUserTenant, Outcome, Request, Secret},
};
use std::{sync::atomic::AtomicBool, time::Duration};

pub(crate) const WAIT: Duration = Duration::from_secs(5);

/// Registers the installation's first account and answers its session
/// token. Under the desktop policy the session is admitted to `default`.
pub(crate) fn first_session(owner: &WriterOwner) -> String {
    let register = Request::Register {
        email: "owner@example.com".into(),
        display_name: "Owner".into(),
        password: Secret::new("correct-horse".into()),
    };
    let auth = owner.auth(FirstUserTenant::NewUser);
    let registered = auth.submit(register, Default::default(), WAIT).unwrap();
    match registered.recv().unwrap().unwrap() {
        Outcome::Session { token, .. } => token.expose().to_owned(),
        _ => panic!("registration returns a session"),
    }
}

/// Stores `value` under `key` for the `default` installation.
pub(crate) fn seed_setting(owner: &WriterOwner, key: &str, value: &str) {
    let setting = Setting {
        tenant: "default".into(),
        key: key.into(),
        value: value.into(),
    };
    let reply = owner
        .client()
        .submit(SettingCommand::Set(setting), &AtomicBool::new(false), WAIT);
    assert!(reply.unwrap().recv().unwrap().unwrap());
}
