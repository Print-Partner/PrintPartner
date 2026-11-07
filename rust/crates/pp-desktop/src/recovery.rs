use pp_core::{CoreHandle, CoreStatus, RuntimeStatus};
use pp_desktop::AllowedOrigin;
use pp_desktop::platform::{REVEAL, message as show_message, opener};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::AppHandle;
use tokio::process::Command;

#[derive(Default)]
pub(crate) struct RecoveryGate(AtomicBool);

impl RecoveryGate {
    pub(crate) fn admit(&self, status: &CoreStatus) -> bool {
        matches!(status, CoreStatus::Guarded)
            && self
                .0
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    pub(crate) fn observe(&self, status: &CoreStatus) -> bool {
        if !matches!(status, CoreStatus::Guarded) {
            self.0.store(false, Ordering::Release);
        }
        matches!(status, CoreStatus::Guarded) && !self.0.load(Ordering::Acquire)
    }

    pub(crate) fn refused(&self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) struct RecoveryUi {
    data: PathBuf,
    dialog: Arc<AtomicBool>,
}

impl RecoveryUi {
    pub(crate) fn new(data: PathBuf) -> Self {
        Self {
            data,
            dialog: Arc::default(),
        }
    }

    pub(crate) fn view_logs(&self, app: &AppHandle) {
        if self.dialog.swap(true, Ordering::AcqRel) {
            return;
        }
        let data = self.data.clone();
        let app = app.clone();
        let dialog = self.dialog.clone();
        tauri::async_runtime::spawn(async move {
            let text = tauri::async_runtime::spawn_blocking(move || pp_core::recent_logs(&data))
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_else(|| "Service logs are unavailable.".to_owned());
            let released = dialog.clone();
            if app
                .run_on_main_thread(move || {
                    show_message("Service logs", &text, move || {
                        released.store(false, Ordering::Release)
                    });
                })
                .is_err()
            {
                dialog.store(false, Ordering::Release);
            }
        });
    }

    pub(crate) fn open_data(&self, app: &AppHandle) {
        let mut command = Command::from(opener());
        command.arg(&self.data);
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if !open(command).await {
                let _ = app.run_on_main_thread(|| {
                    show_message(
                        "Open data folder",
                        "The data folder could not be opened.",
                        || {},
                    )
                });
            }
        });
    }

    pub(crate) fn reveal_logs(&self, app: &AppHandle) {
        let data = self.data.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if !reveal_logs(Command::from(opener()), data).await {
                let _ = app.run_on_main_thread(|| {
                    show_message(
                        "Reveal log folder",
                        "The log folder could not be opened.",
                        || {},
                    )
                });
            }
        });
    }
}

/// Show `<data>/logs` only once it passes the log directory checks. On macOS
/// `open -R` selects it in Finder; elsewhere the opener shows its contents.
async fn reveal_logs(mut opener: Command, data: PathBuf) -> bool {
    let Ok(Ok(folder)) =
        tauri::async_runtime::spawn_blocking(move || pp_core::log_folder(&data)).await
    else {
        return false;
    };
    opener.args(REVEAL).arg(folder);
    open(opener).await
}

async fn open(mut command: Command) -> bool {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = pp_core::spawn_child(&mut command) else {
        return false;
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
        .await
        .is_ok_and(|result| result.is_ok_and(|status| status.success()))
}

/// Tray Open in browser: mint a fresh bootstrap, check it targets this
/// desktop origin and port, then hand it to the default browser.
pub(crate) fn open_in_browser(app: &AppHandle, handle: &CoreHandle, origin: &AllowedOrigin) {
    let command = handle
        .browser_launch_target()
        .and_then(|target| pp_desktop::browser::bootstrap_url(origin, &target.into_url()))
        .map(|url| Command::from(pp_desktop::browser::opener(&url)));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let opened = match command {
            Ok(command) => open(command).await,
            Err(_) => false,
        };
        if !opened {
            tracing::warn!("browser_open_failed");
            let _ = app.run_on_main_thread(|| {
                show_message(
                    "Open in browser",
                    "Print Partner could not open your default browser.",
                    || {},
                )
            });
        }
    });
}

pub(crate) fn restart(app: &AppHandle, handle: CoreHandle, gate: Arc<RecoveryGate>) {
    // A Rust-only runtime has no Node child to restart.
    match handle.subscribe().current() {
        RuntimeStatus::Node(state) if gate.admit(&state) => {}
        _ => return,
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if handle.recover_compat().await.is_err() {
            gate.refused();
            let _ = app.run_on_main_thread(|| {
                show_message(
                    "Restart service",
                    "The service cannot restart in its current state.",
                    || {},
                )
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_t_2_native_restart_admits_once_per_guard_episode() {
        let gate = RecoveryGate::default();
        assert!(!gate.admit(&CoreStatus::Starting));
        assert!(gate.observe(&CoreStatus::Guarded));
        assert!(gate.admit(&CoreStatus::Guarded));
        assert!(!gate.admit(&CoreStatus::Guarded));
        assert!(!gate.observe(&CoreStatus::Guarded));
        assert!(!gate.observe(&CoreStatus::Backoff { attempt: 0 }));
        assert!(gate.admit(&CoreStatus::Guarded));
        gate.refused();
        assert!(gate.admit(&CoreStatus::Guarded));
        assert!(!gate.admit(&CoreStatus::Stopping));
    }

    /// An opener that records its arguments, one per line, in `record`.
    #[cfg(unix)]
    use std::os::unix::fs::symlink as symlink_dir;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;

    /// This test binary, run as an opener: `reveal_recorder` writes the
    /// arguments after `--` to `record`.
    fn recorder(record: &std::path::Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "recovery::tests::reveal_recorder", "--"])
            .env("PP_REVEAL_RECORD", record);
        command
    }

    /// Does nothing unless `recorder` started it.
    #[test]
    fn reveal_recorder() {
        let Some(record) = std::env::var_os("PP_REVEAL_RECORD") else {
            return;
        };
        let args: String = std::env::args()
            .skip_while(|arg| arg != "--")
            .skip(1)
            .map(|arg| format!("{arg}\n"))
            .collect();
        std::fs::write(record, args).unwrap();
    }

    /// A directory only this user can open, as the log folder checks require.
    #[cfg(unix)]
    fn private_dir(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(windows)]
    fn private_dir(path: &std::path::Path) {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, READ_CONTROL, WRITE_DAC,
        };
        std::fs::create_dir_all(path).unwrap();
        let folder = std::fs::OpenOptions::new()
            .access_mode(READ_CONTROL | WRITE_DAC)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .unwrap();
        pp_os::windows_security::CurrentUser::query()
            .unwrap()
            .restrict(&folder, true)
            .unwrap();
    }

    #[tokio::test]
    async fn ticket_t_13_reveal_log_folder_opens_only_the_validated_folder() {
        let scratch = std::env::temp_dir().join(format!(
            "pp-reveal-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data = scratch.join("data");
        private_dir(&data.join("logs"));
        let record = scratch.join("args");
        assert!(reveal_logs(recorder(&record), data.clone()).await);
        let folder = data.join("logs").display().to_string();
        let expected: String = REVEAL
            .iter()
            .chain([&folder.as_str()])
            .map(|line| format!("{line}\n"))
            .collect();
        assert_eq!(std::fs::read_to_string(&record).unwrap(), expected);

        std::fs::remove_file(&record).unwrap();
        let linked = scratch.join("linked");
        std::fs::create_dir(&linked).unwrap();
        symlink_dir(data.join("logs"), linked.join("logs")).unwrap();
        assert!(!reveal_logs(recorder(&record), linked).await);
        assert!(!reveal_logs(recorder(&record), scratch.join("missing")).await);
        assert!(!record.exists(), "the opener ran for a refused folder");

        let mut failing = Command::new(std::env::current_exe().unwrap());
        failing.arg("--no-such-flag");
        assert!(!reveal_logs(failing, data).await);
        let _ = std::fs::remove_dir_all(&scratch);
    }
}
