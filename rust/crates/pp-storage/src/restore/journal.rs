use super::*;
use serde::{Deserialize, Serialize};
use std::io::Read;
const JOURNAL: &str = "journal.json";
const MAX_JOURNAL_BYTES: u64 = 64 * 1024;
const DATABASE_FILES: [&str; 4] = [
    archive::DATABASE_FILE,
    archive::WAL_FILE,
    "print-partner.db-shm",
    "print-partner.db-journal",
];

pub(super) fn validate_work(data_dir: &Path) -> Result<()> {
    let Some(journal) = Journal::read(data_dir)? else {
        return Ok(());
    };
    let work = data_dir.join(WORK_DIR);
    for entry in std::fs::read_dir(&work)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if name == JOURNAL {
            ensure!(
                path.symlink_metadata()?.is_file(),
                "Restore journal is not a regular file"
            );
        } else if name == journal.prior || name == journal.staged {
            ensure!(
                path.symlink_metadata()?.is_dir(),
                "Restore copy is not a directory"
            );
        } else {
            ensure!(
                leftover(&path, name)?.is_some(),
                "Restore work directory holds unexpected entry {name}"
            );
        }
    }
    let prior = work.join(&journal.prior);
    if prior.join(archive::DATABASE_FILE).exists() {
        admit_schema(&prior)?;
    }
    Ok(())
}

pub(super) fn recover(data_dir: &Path) -> Result<bool> {
    let Some(mut journal) = Journal::read(data_dir)? else {
        return Ok(false);
    };
    match journal.state {
        JournalState::Swapping => journal.rollback(data_dir)?,
        JournalState::Committed => journal.finish(data_dir),
    }
    ensure!(
        !data_dir.join(WORK_DIR).join(JOURNAL).exists(),
        "Restore journal cleanup failed"
    );
    Ok(true)
}

/// Every top-level name a restore replaces. Everything else in the data
/// directory, including the ownership marker and lease files, stays.
fn managed() -> impl Iterator<Item = &'static str> {
    DATABASE_FILES
        .into_iter()
        .chain(archive::FULL_ROOTS.iter().map(|(name, _)| *name))
}

fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn remove_entry(path: &Path) -> std::io::Result<()> {
    if path.symlink_metadata()?.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum JournalState {
    Swapping,
    Committed,
}

/// `<data>/.pp-restore/journal.json`. `live` lists the managed names present
/// before the swap; each moves to `prior`. `incoming` lists the names that
/// move in from `staged`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Journal {
    version: u8,
    pub(super) state: JournalState,
    staged: String,
    prior: String,
    live: Vec<String>,
    incoming: Vec<String>,
}

impl Journal {
    pub(super) fn begin(data_dir: &Path, staged: &Path) -> Result<Self> {
        sync_stage(staged)?;
        let work = data_dir.join(WORK_DIR);
        let staged_name = staged
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow!("Staged restore has no name"))?;
        let journal = Self {
            version: 1,
            state: JournalState::Swapping,
            staged: staged_name.to_owned(),
            prior: format!("prior-{}", random_id()),
            live: managed()
                .filter(|name| exists(&data_dir.join(name)))
                .map(str::to_owned)
                .collect(),
            incoming: managed()
                .filter(|name| exists(&staged.join(name)))
                .map(str::to_owned)
                .collect(),
        };
        ensure!(
            journal
                .incoming
                .iter()
                .any(|name| name == archive::DATABASE_FILE),
            "Staged restore has no database"
        );
        journal.write(data_dir)?;
        pp_os::create_private_dir(&work.join(&journal.prior))?;
        pp_os::sync_directory(&work)?;
        Ok(journal)
    }

    pub(super) fn read(data_dir: &Path) -> Result<Option<Self>> {
        let path = data_dir.join(WORK_DIR).join(JOURNAL);
        let opened = pp_os::open_read_nofollow(&path);
        if opened
            .as_ref()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(None);
        }
        let file = opened?;
        let mut bytes = Vec::new();
        file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
        let journal: Self = serde_json::from_slice(&bytes)
            .ok()
            .filter(|_| bytes.len() as u64 <= MAX_JOURNAL_BYTES)
            .ok_or_else(|| anyhow!("Restore journal {} is unreadable", path.display()))?;
        let named = |name: &str, prefix: &str| {
            name.strip_prefix(prefix)
                .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        };
        ensure!(
            journal.version == 1
                && named(&journal.staged, "staged-")
                && named(&journal.prior, "prior-")
                && journal
                    .live
                    .iter()
                    .chain(&journal.incoming)
                    .all(|name| managed().any(|known| known == name)),
            "Restore journal {} is invalid",
            path.display()
        );
        Ok(Some(journal))
    }

    fn write(&self, data_dir: &Path) -> Result<()> {
        let work = data_dir.join(WORK_DIR);
        let temporary = work.join(format!("{JOURNAL}.tmp"));
        std::fs::write(&temporary, serde_json::to_vec(self)?)?;
        pp_os::sync_file(&temporary)?;
        super::activation::hooks::step(data_dir)?;
        pp_os::publish_rename(&temporary, &work.join(JOURNAL))?;
        pp_os::sync_directory(&work)?;
        super::activation::hooks::step(data_dir)?;
        Ok(())
    }

    pub(super) fn swap(&self, data_dir: &Path) -> Result<()> {
        let work = data_dir.join(WORK_DIR);
        let (prior, staged) = (work.join(&self.prior), work.join(&self.staged));
        for name in &self.live {
            super::activation::hooks::step(data_dir)?;
            std::fs::rename(data_dir.join(name), prior.join(name))
                .with_context(|| format!("Move {name} aside"))?;
            pp_os::sync_directory(data_dir)?;
            pp_os::sync_directory(&prior)?;
            super::activation::hooks::step(data_dir)?;
        }
        for name in &self.incoming {
            super::activation::hooks::step(data_dir)?;
            std::fs::rename(staged.join(name), data_dir.join(name))
                .with_context(|| format!("Move restored {name} into place"))?;
            pp_os::sync_directory(data_dir)?;
            pp_os::sync_directory(&staged)?;
            super::activation::hooks::step(data_dir)?;
        }
        pp_os::sync_directory(data_dir)?;
        pp_os::sync_directory(&prior)?;
        Ok(())
    }

    pub(super) fn commit(&mut self, data_dir: &Path) -> Result<()> {
        self.state = JournalState::Committed;
        self.write(data_dir)
    }

    /// Moves every prior entry back. An entry is new when it was not live
    /// before the swap or its prior copy is still aside; new entries are
    /// removed first. Safe to repeat after a crash.
    pub(super) fn rollback(&mut self, data_dir: &Path) -> Result<()> {
        if self.state == JournalState::Committed {
            self.state = JournalState::Swapping;
            self.write(data_dir)?;
        }
        let prior = data_dir.join(WORK_DIR).join(&self.prior);
        for name in managed() {
            let (live, kept) = (data_dir.join(name), prior.join(name));
            let was_live = self.live.iter().any(|known| known == name);
            if exists(&live) && (!was_live || exists(&kept)) {
                remove_entry(&live).with_context(|| format!("Remove restored {name}"))?;
            }
            if exists(&kept) {
                std::fs::rename(&kept, &live).with_context(|| format!("Move {name} back"))?;
            }
        }
        pp_os::sync_directory(data_dir)?;
        super::activation::hooks::after_rollback(data_dir);
        self.finish(data_dir);
        Ok(())
    }

    /// Keep the journal until both copies are removed; restart can repeat cleanup.
    pub(super) fn finish(&self, data_dir: &Path) {
        let work = data_dir.join(WORK_DIR);
        for name in [&self.prior, &self.staged] {
            let path = work.join(name);
            if let Err(error) = std::fs::remove_dir_all(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                return;
            }
        }
        if pp_os::sync_directory(&work).is_ok() {
            let _ = std::fs::remove_file(work.join(JOURNAL));
            let _ = pp_os::sync_directory(&work);
        }
    }
}

fn sync_stage(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if metadata.is_dir() {
            sync_stage(&entry.path())?;
        } else {
            ensure!(metadata.is_file(), "Restore stage contains a link");
            pp_os::sync_file(&entry.path())?;
        }
    }
    pp_os::sync_directory(path)?;
    Ok(())
}
