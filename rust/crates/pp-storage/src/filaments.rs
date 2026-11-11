//! Filament dispatch with an uninhabited request when rebuild support is absent.
#[cfg(feature = "desktop_rebuild")]
mod runtime;
#[cfg(feature = "desktop_rebuild")]
pub use runtime::{FilamentClient, Outcome, Request};

/// Node-compatible custom filament JSON format.
#[cfg(feature = "desktop_rebuild")]
pub const FORMAT_VERSION: u32 = 1;

#[cfg(feature = "desktop_rebuild")]
pub(super) type Message = (runtime::Command, runtime::Reply);
#[cfg(not(feature = "desktop_rebuild"))]
pub(super) type Message = std::convert::Infallible;

impl From<Message> for crate::Envelope {
    fn from(message: Message) -> Self {
        Self::Filaments(message)
    }
}

#[cfg(feature = "desktop_rebuild")]
pub(super) fn dispatch(
    connection: &mut rusqlite::Connection,
    message: Message,
    deadline: crate::writer_admission::Deadline,
) {
    let (command, reply) = message;
    reply.send(runtime::execute(connection, command, deadline));
}

#[cfg(not(feature = "desktop_rebuild"))]
pub(super) fn dispatch(
    _: &mut rusqlite::Connection,
    message: Message,
    _: crate::writer_admission::Deadline,
) {
    match message {}
}
