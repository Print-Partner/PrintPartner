//! Tray status model: menu text and icon badge color per core status.
use crate::autostart::LaunchAgent;
use anyhow::Result;
use pp_core::{CoreStatus, RuntimeStatus, RustStatus};

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

/// Menu item ids in display order.
pub fn menu_ids() -> &'static [&'static str] {
    &[
        "show",
        "browser",
        "status",
        "logs",
        "log_folder",
        "recover",
        "data",
        "quit",
    ]
}

/// The macOS menu order: "Open at login" sits just before Quit.
pub fn menu_ids_with_open_at_login() -> Vec<&'static str> {
    let mut ids = menu_ids().to_vec();
    ids.insert(ids.len() - 1, OPEN_AT_LOGIN);
    ids
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
