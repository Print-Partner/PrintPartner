//! T-29: tray status text, colour and badge.
use pp_core::BackupFailure;
use pp_core::{CoreStatus, RuntimeStatus, RustStatus};
use pp_desktop::tray::{
    BACKUP, MenuExtras, Tone, backup_message, badge, menu_ids, runtime_status, status,
};
use std::path::PathBuf;

fn every_status() -> Vec<CoreStatus> {
    vec![
        CoreStatus::Starting,
        CoreStatus::Ready {
            generation: "g1".into(),
            pid: 42,
        },
        CoreStatus::Backoff { attempt: 1 },
        CoreStatus::Guarded,
        CoreStatus::Stopping,
        CoreStatus::Stopped,
        CoreStatus::Failed,
    ]
}

#[test]
fn ticket_t_29_tray_status_maps_every_core_state_to_text_and_color() {
    let rows: Vec<_> = every_status().iter().map(status).collect();
    assert_eq!(
        rows,
        [
            ("Service starting", Tone::Busy),
            ("Service ready", Tone::Ready),
            ("Service reconnecting", Tone::Busy),
            ("Service stopped after repeated failures", Tone::Failed),
            ("Service stopping", Tone::Busy),
            ("Service stopped", Tone::Stopped),
            ("Service failed", Tone::Failed),
        ]
    );
    let colors: std::collections::HashSet<_> =
        [Tone::Ready, Tone::Busy, Tone::Stopped, Tone::Failed]
            .map(Tone::rgba)
            .into_iter()
            .collect();
    assert_eq!(colors.len(), 4, "each tone has its own color");
}

#[test]
fn tray_runtime_status_keeps_node_labels_and_maps_rust_states() {
    for state in every_status() {
        assert_eq!(
            runtime_status(&RuntimeStatus::Node(state.clone())),
            status(&state)
        );
    }
    let rows: Vec<_> = [
        RustStatus::Starting,
        RustStatus::Ready,
        RustStatus::Stopping,
        RustStatus::Stopped,
    ]
    .map(|state| runtime_status(&RuntimeStatus::Rust(state)))
    .into();
    assert_eq!(
        rows,
        [
            ("Service starting", Tone::Busy),
            ("Service ready", Tone::Ready),
            ("Service stopping", Tone::Busy),
            ("Service stopped", Tone::Stopped),
        ]
    );
}

#[test]
fn ticket_t_29_tray_badge_paints_only_the_bottom_right_dot() {
    let (w, h) = (32u32, 32u32);
    let base = vec![9u8; (w * h * 4) as usize];
    let out = badge(&base, w, h, Tone::Failed);
    assert_eq!(out.len(), base.len());
    let px = |x: u32, y: u32| &out[((y * w + x) * 4) as usize..][..4];
    assert_eq!(px(w - 9, h - 9), Tone::Failed.rgba(), "dot center");
    assert_eq!(px(0, 0), [9; 4], "top-left untouched");
    assert_eq!(px(w / 2 - 1, h / 2 - 1), [9; 4], "center untouched");
    for y in 0..h {
        for x in 0..w {
            if y < h / 2 || x < w / 2 {
                assert_eq!(px(x, y), [9; 4], "outside bottom-right quarter at {x},{y}");
            }
        }
    }
    let ready = badge(&base, w, h, Tone::Ready);
    assert_ne!(ready, out, "tones render differently");
}

#[test]
fn ticket_t_29_tray_badge_leaves_a_mismatched_buffer_unchanged() {
    let base = vec![1u8; 10];
    assert_eq!(badge(&base, 32, 32, Tone::Ready), base);
    assert_eq!(badge(&base, u32::MAX, u32::MAX, Tone::Ready), base);
}

#[test]
fn ticket_t_29_tray_badge_handles_tiny_icons_without_panicking() {
    for side in 0..=4u32 {
        let base = vec![9u8; (side * side * 4) as usize];
        assert_eq!(badge(&base, side, side, Tone::Failed).len(), base.len());
    }
    let out = badge(&[9u8; 16], 2, 2, Tone::Failed);
    assert_eq!(
        &out[..4],
        Tone::Failed.rgba(),
        "a 2px icon still shows the dot"
    );
}

#[test]
fn ticket_t_29_tray_menu_offers_status_logs_restart_data_and_quit_by_default() {
    assert_eq!(
        menu_ids(MenuExtras::default()),
        [
            "show",
            "browser",
            "status",
            "logs",
            "log_folder",
            "recover",
            "data",
            "quit"
        ]
    );
}

#[test]
fn ticket_t_29_tray_menu_puts_backup_between_open_data_folder_and_quit() {
    let extras = |open_at_login| MenuExtras {
        backup: true,
        open_at_login,
    };
    let ids = menu_ids(extras(false));
    assert_eq!(&ids[ids.len() - 3..], ["data", BACKUP, "quit"]);
    let both = menu_ids(extras(true));
    assert_eq!(&both[both.len() - 3..], ["backup", "open_at_login", "quit"]);
    assert_eq!(both.len(), 10);
}

#[test]
fn ticket_t_29_tray_backup_reports_the_saved_path_or_a_fixed_failure() {
    let path = PathBuf::from("/home/user/Downloads/print-partner-backup.tar.gz");
    let saved = backup_message(&Ok(path));
    let expected = "Saved /home/user/Downloads/print-partner-backup.tar.gz";
    assert_eq!(saved, ("Backup saved", expected.to_owned()));

    let rows: Vec<_> = [
        BackupFailure::NoSpace,
        BackupFailure::RestoreRunning,
        BackupFailure::Stopping,
        BackupFailure::Failed,
    ]
    .map(|failure| backup_message(&Err(failure)))
    .into();
    let failed = |reason: &str| ("Backup failed", format!("{reason} No backup was saved."));
    assert_eq!(
        rows,
        [
            failed("Not enough free space on the data folder's drive."),
            failed("A restore is running. Try again after it."),
            failed("Print Partner is shutting down."),
            failed("It could not be made or saved in Downloads."),
        ]
    );
}
