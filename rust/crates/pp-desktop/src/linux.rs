//! Linux: GTK dialogs, `xdg-open`, and the Unix focus socket and SIGTERM.

pub(crate) use crate::unix::{FocusListener, exchange};
pub use crate::unix::{SOCKET, terminate};
use std::process::Command;

#[cfg(feature = "desktop_rebuild")]
pub(crate) const HOST: pp_core::layout::Platform = pp_core::layout::Platform::Linux;

/// The command that opens a URL or folder given as its last argument.
pub fn opener() -> Command {
    Command::new("xdg-open")
}

/// Arguments before a folder that the opener should show, not open.
pub const REVEAL: &[&str] = &[];

/// Open at login is a macOS LaunchAgent, so only macOS offers it (T-31).
pub const LOGIN_AGENT: bool = false;

/// The staged runtime inside the installed resources.
pub fn bundled_stage(resources: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    Ok(resources.join("desktop-runtime"))
}

/// The command that opens the sponsor page, where the platform has one.
pub fn sponsor_opener() -> Option<Command> {
    Some(opener())
}

/// Only macOS reopens an app whose windows are all closed.
pub fn reopen_requested(_event: &tauri::RunEvent) -> bool {
    false
}

/// A modal information dialog, run on the calling (main) thread.
pub fn notice(text: &str) {
    use gtk::prelude::*;
    let dialog = gtk::MessageDialog::builder()
        .title("Print Partner")
        .text(text)
        .message_type(gtk::MessageType::Info)
        .buttons(gtk::ButtonsType::Ok)
        .modal(true)
        .build();
    dialog.run();
    dialog.close();
}

/// Tauri initializes GTK on this thread and owns its main context. A
/// background dialog thread cannot acquire that context before run(), so the
/// modal GTK loop runs here, including failures before shell setup.
pub fn startup_error(title: &str, description: &str) {
    use gtk::prelude::*;
    if gtk::init().is_ok() {
        let dialog = gtk::MessageDialog::builder()
            .title(title)
            .text(title)
            .secondary_text(description)
            .message_type(gtk::MessageType::Error)
            .buttons(gtk::ButtonsType::Ok)
            .modal(true)
            .build();
        dialog.run();
        dialog.close();
    }
}

/// A scrollable read-only text dialog; `closed` runs when it closes.
pub fn message(title: &str, text: &str, closed: impl Fn() + Send + 'static) {
    use gtk::prelude::*;
    let dialog = gtk::Dialog::builder()
        .title(title)
        .modal(true)
        .default_width(760)
        .default_height(440)
        .build();
    dialog.add_button("Close", gtk::ResponseType::Close);
    let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
    buffer.set_text(text);
    let view = gtk::TextView::builder()
        .buffer(&buffer)
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .build();
    scroll.add(&view);
    dialog.content_area().add(&scroll);
    dialog.connect_response(|dialog, _| dialog.close());
    dialog.connect_destroy(move |_| closed());
    dialog.show_all();
}
