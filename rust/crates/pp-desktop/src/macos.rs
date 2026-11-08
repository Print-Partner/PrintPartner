//! macOS: rfd dialogs, `open`, Reopen, and the Unix focus socket and SIGTERM.

pub use crate::rfd_dialogs::{message, notice, startup_error};
pub(crate) use crate::unix::{FocusListener, exchange};
pub use crate::unix::{SOCKET, terminate};
use std::process::Command;

#[cfg(feature = "desktop_rebuild")]
pub(crate) const HOST: pp_core::layout::Platform = pp_core::layout::Platform::MacOs;

/// The command that opens a URL or folder given as its last argument.
pub fn opener() -> Command {
    Command::new("/usr/bin/open")
}

/// `open -R` selects the folder in Finder instead of opening it.
pub const REVEAL: &[&str] = &["-R"];

/// Open at login is a per-user LaunchAgent (T-31).
pub const LOGIN_AGENT: bool = true;

/// The staged runtime is the app bundle's Contents directory.
pub fn bundled_stage(resources: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    use anyhow::Context;
    Ok(resources
        .parent()
        .context("App Contents unavailable")?
        .to_owned())
}

/// The command that opens the sponsor page, where the platform has one.
pub fn sponsor_opener() -> Option<Command> {
    Some(opener())
}

/// The Dock icon was clicked while every window was closed.
pub fn reopen_requested(event: &tauri::RunEvent) -> bool {
    matches!(event, tauri::RunEvent::Reopen { .. })
}
