//! The write seam: how a storage family runs one write on the writer thread.
//!
//! A family's client calls [`Shared::write`] with a closure. The closure is
//! queued like every other writer request, then runs on the writer thread
//! inside one `BEGIN IMMEDIATE` transaction. `Ok` commits and `Err` rolls
//! back, so a family cannot leave a transaction open or forget one. The
//! family needs no `Envelope` variant, no arm in the writer loop and no
//! `desktop_rebuild` gate of its own.
//!
//! The helper is crate-private and transaction-scoped on purpose: ADR 0009
//! rules out unrestricted SQL closures for callers outside `pp-storage`, and
//! SQL stays in this crate. Never export it.
//!
//! Admission, the queue, the deadline and the reply behave as they do for
//! the older envelopes. A panic in the closure is not caught here either.

use crate::{
    Shared, WriteJob, WriterFailure,
    auth::{self, AuthFailure, AuthPolicy},
    read_model::Credential,
    writer_admission::{Deadline, begin_immediate},
};
use rusqlite::{Connection, Transaction};
use std::{
    sync::{atomic::AtomicBool, mpsc},
    time::Duration,
};

/// The installation a credential is admitted to: the Tenant of the glossary
/// (`CONTEXT.md`). `tenant_id` is a legacy column name that holds the
/// installation's single value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Installation(String);

impl Installation {
    /// The value to bind to a `tenant_id` column.
    pub(crate) fn id(&self) -> &str {
        &self.0
    }
}

/// Proves `credential` inside the write's own transaction, so the check and
/// the write see the same sessions and API keys. A refused credential is
/// [`WriterFailure::Auth`].
pub(crate) fn authorize(
    tx: &Transaction<'_>,
    credential: &Credential,
    policy: AuthPolicy,
) -> Result<Installation, WriterFailure> {
    let refused = WriterFailure::Auth(AuthFailure::SessionRequired);
    match credential {
        Credential::Session(secret) if !auth::session_sized(secret) => Err(refused),
        Credential::Session(secret) => {
            Ok(Installation(auth::read_session_tenant(tx, secret, policy)?))
        }
        Credential::ApiKey {
            routed_tenant,
            secret,
        } => {
            if auth::key_proves(tx, routed_tenant, secret)? {
                Ok(Installation(routed_tenant.clone()))
            } else {
                Err(refused)
            }
        }
    }
}

impl Shared {
    /// Runs `operation` on the writer thread in one write transaction and
    /// returns its typed result. `name` is the operation's name, such as
    /// `"printer_setup.create"`, for the tracing span on the writer thread.
    ///
    /// The outer `Err` means the operation did not run or its commit failed,
    /// and nothing was written. The inner result is the operation's own.
    pub(crate) fn write<T, E>(
        &self,
        name: &'static str,
        cancelled: &AtomicBool,
        wait: Duration,
        operation: impl FnOnce(&Transaction<'_>, Deadline) -> Result<T, E> + Send + 'static,
    ) -> Result<Result<T, E>, WriterFailure>
    where
        T: Send + 'static,
        E: Send + 'static,
    {
        let (reply, receiver) = mpsc::channel();
        let job: WriteJob = Box::new(move |connection, deadline| {
            let _span = tracing::debug_span!("write", operation = name).entered();
            let _ = reply.send(run(connection, deadline, operation));
        });
        self.admit(job.into(), cancelled, wait)?;
        receiver.recv().map_err(|_| WriterFailure::Stopped)?
    }
}

fn run<T, E>(
    connection: &mut Connection,
    deadline: Deadline,
    operation: impl FnOnce(&Transaction<'_>, Deadline) -> Result<T, E>,
) -> Result<Result<T, E>, WriterFailure> {
    let tx = begin_immediate(connection, deadline)?;
    // Dropping the transaction rolls it back.
    let result = operation(&tx, deadline);
    if result.is_ok() {
        tx.commit()?;
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
