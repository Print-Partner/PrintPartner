//! Writer controls and the feature-gated whole-installation restore command.
use crate::{Envelope, Shared, WriterControl, WriterState};
use anyhow::Result;
use rusqlite::Connection;
use std::sync::mpsc;

pub(crate) enum Command {
    Query {
        control: WriterControl,
        reply: mpsc::Sender<Result<u32>>,
    },
    #[cfg(feature = "desktop_rebuild")]
    Restore(crate::restore::activation::Command),
}

pub(crate) fn queued(control: WriterControl, reply: mpsc::Sender<Result<u32>>) -> Envelope {
    Envelope::Control(Command::Query { control, reply })
}

fn query(connection: &Connection, control: WriterControl, reply: mpsc::Sender<Result<u32>>) {
    let _ = reply.send(crate::writer_control(connection, control));
}

#[cfg(feature = "desktop_rebuild")]
pub(crate) fn execute(
    connection: &mut Connection,
    shared: &Shared,
    state: &mut WriterState,
    command: Command,
) {
    match command {
        Command::Query { control, reply } => query(connection, control, reply),
        Command::Restore(command) => {
            crate::restore::activation::execute(connection, shared, state, command)
        }
    }
}

#[cfg(not(feature = "desktop_rebuild"))]
pub(crate) fn execute(
    connection: &mut Connection,
    _: &Shared,
    _: &mut WriterState,
    command: Command,
) {
    let Command::Query { control, reply } = command;
    query(connection, control, reply);
}
