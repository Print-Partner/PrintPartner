use pp_core::BackupFailure;
use pp_desktop::tray::{BACKUP, backup_message};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tauri::{AppHandle, Manager};

pub(crate) const LABEL: &str = "Back up now";

pub(crate) type Effect = Arc<dyn Fn(&Path) -> Result<PathBuf, BackupFailure> + Send + Sync>;

pub(crate) struct TrayBackup {
    effect: Option<Effect>,
    downloads: Option<PathBuf>,
    running: Arc<AtomicBool>,
}

struct Slot(Arc<AtomicBool>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) struct BackupRun(Effect, Option<PathBuf>, Slot);

pub(crate) struct FinishedBackup {
    saved: Result<PathBuf, BackupFailure>,
    _slot: Slot,
}

impl TrayBackup {
    pub(crate) fn new(effect: Option<Effect>, downloads: Option<PathBuf>) -> Self {
        let downloads = downloads.filter(|dir| dir.is_absolute());
        let running = Arc::default();
        Self {
            effect,
            downloads,
            running,
        }
    }

    pub(crate) fn is_available(&self) -> bool {
        self.effect.is_some()
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    pub(crate) fn begin(&self, id: &str) -> Option<BackupRun> {
        let effect = Arc::clone(self.effect.as_ref().filter(|_| id == BACKUP)?);
        if self.running.swap(true, Ordering::AcqRel) {
            return None;
        }
        let slot = Slot(Arc::clone(&self.running));
        Some(BackupRun(effect, self.downloads.clone(), slot))
    }
}

impl BackupRun {
    pub(crate) async fn finish(self) -> FinishedBackup {
        let Self(effect, downloads, slot) = self;
        let saved = match downloads {
            None => Err(BackupFailure::Failed),
            Some(folder) => tokio::task::spawn_blocking(move || effect(&folder))
                .await
                .unwrap_or(Err(BackupFailure::Failed)),
        };
        match &saved {
            Ok(_) => tracing::info!("tray_backup_saved"),
            Err(failure) => tracing::warn!(?failure, "tray_backup_failed"),
        }
        FinishedBackup { saved, _slot: slot }
    }
}

impl FinishedBackup {
    pub(crate) fn message(&self) -> (&'static str, String) {
        backup_message(&self.saved)
    }
}

pub(crate) fn start(app: &AppHandle, run: BackupRun) {
    show_running(app, true);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let finished = run.finish().await;
        let shown = app.clone();
        let _ = app.run_on_main_thread(move || {
            show_running(&shown, false);
            let (title, text) = finished.message();
            pp_desktop::platform::message(title, &text, || {});
            drop(finished);
        });
    });
}

fn show_running(app: &AppHandle, running: bool) {
    if let Some(state) = app.try_state::<crate::TrayStatus>()
        && let Ok(items) = state.lock()
        && let Some(items) = items.as_ref()
    {
        let text = if running { "Backing up…" } else { LABEL };
        let _ = items.backup.set_text(text);
        let _ = items.backup.set_enabled(!running);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn tray(
        saved: Result<&'static str, BackupFailure>,
        downloads: Option<PathBuf>,
    ) -> (TrayBackup, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let effect: Effect = Arc::new(move |folder: &Path| {
            counted.fetch_add(1, Ordering::SeqCst);
            saved.map(|name| folder.join(name))
        });
        (TrayBackup::new(Some(effect), downloads), calls)
    }

    #[tokio::test]
    async fn ticket_t_29_tray_backup_runs_once_and_reports_the_saved_path() {
        let downloads = std::env::temp_dir();
        let (backup, calls) = tray(Ok("print-partner-backup.tar.gz"), Some(downloads.clone()));
        assert!(backup.is_available());
        assert!(backup.begin("data").is_none());

        let run = backup.begin(BACKUP).unwrap();
        assert!(
            backup.begin(BACKUP).is_none(),
            "a double click runs one backup"
        );
        let finished = run.finish().await;
        assert!(backup.is_running(), "held until the dialog is submitted");

        let saved = downloads.join("print-partner-backup.tar.gz");
        assert_eq!(
            finished.message(),
            backup_message(&Ok(saved)),
            "Backup saved"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        drop(finished);
        assert!(!backup.is_running());
        assert!(
            backup.begin(BACKUP).is_some(),
            "free again after the result"
        );
    }

    #[tokio::test]
    async fn ticket_t_29_tray_backup_reports_failure_without_a_path_or_no_entry_without_rust() {
        let downloads = std::env::temp_dir();
        let (failing, calls) = tray(Err(BackupFailure::Failed), Some(downloads.clone()));
        let (title, text) = failing.begin(BACKUP).unwrap().finish().await.message();
        assert_eq!(title, "Backup failed");
        assert!(!text.contains(&*downloads.to_string_lossy()), "{text}");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let (no_downloads, calls) = tray(Ok("never"), None);
        let (title, _) = no_downloads.begin(BACKUP).unwrap().finish().await.message();
        assert_eq!((title, calls.load(Ordering::SeqCst)), ("Backup failed", 0));

        let node = TrayBackup::new(None, Some(downloads));
        assert!(!node.is_available() && node.begin(BACKUP).is_none());
    }
}
