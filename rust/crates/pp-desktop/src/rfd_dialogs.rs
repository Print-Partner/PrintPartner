//! Dialogs through rfd, for macOS and Windows.

/// A modal information dialog.
pub fn notice(text: &str) {
    rfd::MessageDialog::new()
        .set_title("Print Partner")
        .set_description(text)
        .set_level(rfd::MessageLevel::Info)
        .show();
}

/// A modal error dialog.
pub fn startup_error(title: &str, description: &str) {
    rfd::MessageDialog::new()
        .set_title(title)
        .set_description(description)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

/// A text dialog; `closed` runs when it closes.
pub fn message(title: &str, text: &str, closed: impl Fn() + Send + 'static) {
    let dialog = rfd::AsyncMessageDialog::new()
        .set_title(title)
        .set_description(text);
    tauri::async_runtime::spawn(async move {
        dialog.show().await;
        closed();
    });
}
