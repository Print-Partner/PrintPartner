//! The read seam: how a storage family runs one read.
//!
//! A family's client calls [`SettingsClient::read`] with a closure. It runs
//! on the caller's thread, on a connection from the read-only pool, inside
//! one deferred transaction, so everything the closure reads is one snapshot.
//! Nothing is committed. Reads do not wait in the writer queue; writes go
//! through `Shared::write`.
//!
//! Like the write seam, the helper is crate-private and transaction-scoped:
//! ADR 0009 rules out unrestricted SQL closures for callers outside
//! `pp-storage`. Never export it. Its failures are the same
//! [`WriterFailure`] a write reports, so one mapping serves both.

use crate::{SettingsClient, WriterFailure};
use rusqlite::{Transaction, TransactionBehavior};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

impl SettingsClient {
    /// Runs `operation` in one read transaction and returns its typed result.
    /// `name` is the operation's name, such as `"printer_setup.read_printer"`,
    /// for the tracing span. Call `write::authorize` inside the closure, as a
    /// write does, so the credential check and the read see the same snapshot.
    ///
    /// The outer `Err` means the operation did not run: the caller cancelled
    /// before a reader was checked out, storage has stopped, or no reader
    /// came free within `wait` ([`WriterFailure::Busy`]).
    pub(crate) fn read<T, E>(
        &self,
        name: &'static str,
        cancelled: &AtomicBool,
        wait: Duration,
        operation: impl FnOnce(&Transaction<'_>) -> Result<T, E>,
    ) -> Result<Result<T, E>, WriterFailure> {
        let gone = || cancelled.load(Ordering::Acquire);
        if gone() {
            return Err(WriterFailure::Cancelled);
        }
        let _span = tracing::debug_span!("read", operation = name).entered();
        let reader = self.checkout(wait)?;
        // The caller may have left while this waited for a reader.
        if gone() {
            return Err(WriterFailure::Cancelled);
        }
        let behavior = TransactionBehavior::Deferred;
        let tx = Transaction::new_unchecked(reader.connection(), behavior)?;
        Ok(operation(&tx))
    }
}

#[cfg(test)]
mod tests;
