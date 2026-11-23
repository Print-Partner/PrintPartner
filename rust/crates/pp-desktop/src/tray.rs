//! Tray status model: menu text and icon badge color per core status.
use crate::autostart::LaunchAgent;
use anyhow::Result;
use pp_core::{BackupFailure, CoreStatus, RuntimeStatus, RustStatus};
use std::path::PathBuf;

/// Menu id of the macOS "Open at login" check item (T-31).
pub const OPEN_AT_LOGIN: &str = "open_at_login";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Ready,
    Busy,
    Stopped,
    Failed,
}

impl Tone {
    pub fn rgba(self) -> [u8; 4] {
        match self {
            Tone::Ready => [0x2e, 0x7d, 0x32, 0xff],
            Tone::Busy => [0xf9, 0xa8, 0x25, 0xff],
            Tone::Stopped => [0x75, 0x75, 0x75, 0xff],
            Tone::Failed => [0xc6, 0x28, 0x28, 0xff],
        }
    }
}

pub fn status(state: &CoreStatus) -> (&'static str, Tone) {
    match state {
        CoreStatus::Starting => ("Service starting", Tone::Busy),
        CoreStatus::Ready { .. } => ("Service ready", Tone::Ready),
        CoreStatus::Backoff { .. } => ("Service reconnecting", Tone::Busy),
        CoreStatus::Guarded => ("Service stopped after repeated failures", Tone::Failed),
        CoreStatus::Stopping => ("Service stopping", Tone::Busy),
        CoreStatus::Stopped => ("Service stopped", Tone::Stopped),
        CoreStatus::Failed => ("Service failed", Tone::Failed),
    }
}

/// The tray text and badge for either runtime. A Rust-only runtime uses the
/// same labels; it never reconnects or hits the crash guard.
pub fn runtime_status(state: &RuntimeStatus) -> (&'static str, Tone) {
    match state {
        RuntimeStatus::Node(state) => status(state),
        RuntimeStatus::Rust(RustStatus::Starting) => ("Service starting", Tone::Busy),
        RuntimeStatus::Rust(RustStatus::Ready) => ("Service ready", Tone::Ready),
        RuntimeStatus::Rust(RustStatus::Stopping) => ("Service stopping", Tone::Busy),
        RuntimeStatus::Rust(RustStatus::Stopped) => ("Service stopped", Tone::Stopped),
    }
}

/// Menu id of Back up now (GRE-1036), shown when the Rust writer owns the data.
pub const BACKUP: &str = "backup";

/// The items a tray shows on top of the defaults.
#[derive(Clone, Copy, Debug, Default)]
pub struct MenuExtras {
    pub backup: bool,
    pub open_at_login: bool,
}

/// Every tray item id, in display order.
#[rustfmt::skip]
const ORDER: [&str; 10] = [
    "show", "browser", "status", "logs", "log_folder", "recover", "data", BACKUP, OPEN_AT_LOGIN, "quit",
];

/// Menu item ids in display order: the defaults, plus each extra that is on.
pub fn menu_ids(extras: MenuExtras) -> Vec<&'static str> {
    ORDER
        .into_iter()
        .filter(|id| match *id {
            BACKUP => extras.backup,
            OPEN_AT_LOGIN => extras.open_at_login,
            _ => true,
        })
        .collect()
}

/// The dialog for a finished tray backup: the saved path, or fixed text.
pub fn backup_message(saved: &Result<PathBuf, BackupFailure>) -> (&'static str, String) {
    let reason = match saved {
        Ok(path) => {
            return (
                "Backup saved",
                format!("Saved {}", pp_os::display_path(path).display()),
            );
        }
        Err(BackupFailure::NoSpace) => "Not enough free space on the data folder's drive.",
        Err(BackupFailure::RestoreRunning) => "A restore is running. Try again after it.",
        Err(BackupFailure::Stopping) => "Print Partner is shutting down.",
        Err(BackupFailure::Failed) => "It could not be made or saved in Downloads.",
    };
    ("Backup failed", format!("{reason} No backup was saved."))
}

/// Turns open-at-login off when this app's agent is installed, otherwise on,
/// and returns whether it is on afterwards.
pub fn toggle_open_at_login(agent: &LaunchAgent) -> Result<bool> {
    if agent.is_enabled() {
        agent.disable()?;
    } else {
        agent.enable()?;
    }
    Ok(agent.is_enabled())
}

/// Copies an RGBA icon and paints an opaque status dot in its bottom-right quarter.
/// A buffer that does not hold width * height RGBA pixels comes back unchanged.
pub fn badge(rgba: &[u8], width: u32, height: u32, tone: Tone) -> Vec<u8> {
    let mut out = rgba.to_vec();
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    if expected != Some(out.len()) {
        return out;
    }
    let radius = (width.min(height) / 4).max(1) as i64;
    let (cx, cy) = (width as i64 - radius - 1, height as i64 - radius - 1);
    let color = tone.rgba();
    for y in (cy - radius).max(0)..=(cy + radius).min(height as i64 - 1) {
        for x in (cx - radius).max(0)..=(cx + radius).min(width as i64 - 1) {
            if (x - cx).pow(2) + (y - cy).pow(2) < radius.pow(2) {
                let at = ((y as usize) * (width as usize) + x as usize) * 4;
                out[at..at + 4].copy_from_slice(&color);
            }
        }
    }
    out
}
