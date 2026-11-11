//! The restore command is uninhabited when the rebuild feature is off.
use crate::{Shared, WriterState};
use rusqlite::Connection;
pub(crate) enum Command {}
pub(crate) fn execute(_: &mut Connection, _: &Shared, _: &mut WriterState, command: Command) {
    match command {}
}
