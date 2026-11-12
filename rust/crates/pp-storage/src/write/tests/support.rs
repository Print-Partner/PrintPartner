//! Fixtures for the tests of families that write through the write seam.

use crate::{
    Limits, Setting, SettingCommand, WriterOwner,
    auth::{
        AuthPolicy, FirstUserTenant, Outcome, RegistrationPolicy, Request, Secret,
        SessionTenantPolicy,
    },
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

/// A session is admitted to its own account's installation id.
pub(crate) const ACCOUNT_POLICY: AuthPolicy = AuthPolicy {
    registration: RegistrationPolicy::Open,
    session_tenant: SessionTenantPolicy::AccountTenant,
    first_user: FirstUserTenant::NewUser,
};

/// A temporary data directory and the owner opened on it.
pub(crate) fn opened(limits: Limits) -> (tempfile::TempDir, WriterOwner) {
    let directory = tempfile::tempdir().unwrap();
    let (owner, _) = WriterOwner::open(directory.path(), limits).unwrap();
    (directory, owner)
}

/// Registers an account and creates an API key for it. Answers the
/// installation id [`ACCOUNT_POLICY`] admits it to, its session token and
/// the key.
pub(crate) fn account_with_key(owner: &WriterOwner) -> (String, String, Secret) {
    let auth = |request| {
        let client = owner.auth(FirstUserTenant::NewUser);
        let reply = client.submit(request, Default::default(), WAIT);
        reply.unwrap().recv().unwrap().unwrap()
    };
    let Outcome::Session { user, token } = auth(Request::Register {
        email: "seam@example.com".into(),
        display_name: "Seam".into(),
        password: Secret::new("correct-horse".into()),
    }) else {
        panic!("registration returns a session")
    };
    let session = Secret::new(token.expose().to_owned());
    let Outcome::KeyCreated { key, .. } = auth(Request::CreateKey { session }) else {
        panic!("key creation returns the key")
    };
    (user.tenant_id, token.expose().to_owned(), key)
}
