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

pub(super) fn validate_work(work: &Work) -> Result<()> {
    let Some(journal) = Journal::read(work)? else {
        return Ok(());
    };
    for name in work.dir.entries()? {
        let name = name?;
        let name = name.to_str().unwrap_or("");
        if name == JOURNAL {
            ensure!(
                work.dir.status_nofollow(name)?.is_file(),
                "Restore journal is not a regular file"
            );
        } else if name == journal.prior || name == journal.staged {
            ensure!(
                work.dir.status_nofollow(name)?.is_dir(),
                "Restore copy is not a directory"
            );
        } else {
            ensure!(
                !name.is_empty() && leftover(&work.dir, name)?.is_some(),
                "Restore work directory holds unexpected entry {name}"
            );
        }
    }
    if let Some(prior) = open_optional(&work.dir, &journal.prior)?
        && exists(&prior, archive::DATABASE_FILE)?
    {
        admit_staged(&prior, &work.path.join(&journal.prior))?;
    }
    Ok(())
}

pub(super) fn recover(work: &Work) -> Result<bool> {
    let Some(mut journal) = Journal::read(work)? else {
        return Ok(false);
    };
    match journal.state {
        JournalState::Swapping => journal.rollback(work)?,
        JournalState::Committed => journal.finish(work),
    }
    ensure!(
        !exists(&work.dir, JOURNAL)?,
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

/// Whether `directory` has an entry `name`, a link counting as one.
fn exists(directory: &pp_os::Dir, name: &str) -> Result<bool> {
    match directory.status_nofollow(name) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// The directory `name` under `directory`, without following a link, or
/// `None` when it is missing.
fn open_optional(directory: &pp_os::Dir, name: &str) -> Result<Option<pp_os::Dir>> {
    match directory.open_dir_nofollow(name) {
        Ok(opened) => Ok(Some(opened)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
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
    /// `staged` names a directory in `work`.
    pub(super) fn begin(work: &Work, staged: &str) -> Result<Self> {
        let stage = work.dir.open_dir_nofollow(staged)?;
        sync_stage(&stage)?;
        let mut live = Vec::new();
        let mut incoming = Vec::new();
        for name in managed() {
            if exists(&work.data, name)? {
                live.push(name.to_owned());
            }
            if exists(&stage, name)? {
                incoming.push(name.to_owned());
            }
        }
        ensure!(
            live.iter().any(|name| name == archive::DATABASE_FILE),
            "Live database disappeared before restore"
        );
        let journal = Self {
            version: 1,
            state: JournalState::Swapping,
            staged: staged.to_owned(),
            prior: format!("prior-{}", random_id()),
            live,
            incoming,
        };
        ensure!(
            journal
                .incoming
                .iter()
                .any(|name| name == archive::DATABASE_FILE),
            "Staged restore has no database"
        );
        journal.write(work, || work.forward_step())?;
        work.dir.create_dir(&journal.prior)?;
        work.dir.sync()?;
        Ok(journal)
    }

    pub(super) fn read(work: &Work) -> Result<Option<Self>> {
        let path = work.path.join(JOURNAL);
        let opened = work.dir.open_read_nofollow(JOURNAL);
        if opened
            .as_ref()
            .is_err_and(|error| error.kind() == ErrorKind::NotFound)
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
                && [&journal.live, &journal.incoming].into_iter().all(|names| {
                    names.iter().any(|name| name == archive::DATABASE_FILE)
                        && names
                            .iter()
                            .collect::<std::collections::BTreeSet<_>>()
                            .len()
                            == names.len()
                })
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

    /// Publishes the journal, running `step` before and after the rename.
    fn write(&self, work: &Work, step: impl Fn() -> Result<()>) -> Result<()> {
        let temporary = format!("{JOURNAL}.tmp");
        match work.dir.remove_file(&temporary) {
            Err(error) if error.kind() != ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        let mut file = work.dir.create_new_nofollow(&temporary)?;
        file.write_all(&serde_json::to_vec(self)?)?;
        file.sync_all()?;
        drop(file);
        step()?;
        work.dir.rename_replace(&temporary, JOURNAL)?;
        work.dir.sync()?;
        step()?;
        Ok(())
    }

    /// Moves every live name aside and every staged one in. Each rename
    /// first checks that `.pp-restore` still names the directory holding
    /// the journal; if not, nothing more moves and the caller rolls back
    /// through the same handles.
    pub(super) fn swap(&self, work: &Work) -> Result<()> {
        let step = || work.forward_step();
        let prior = work.dir.open_dir_nofollow(&self.prior)?;
        let staged = work.dir.open_dir_nofollow(&self.staged)?;
        for name in &self.live {
            step()?;
            (work.data.rename_to(name, &prior, name))
                .with_context(|| format!("Move {name} aside"))?;
            work.data.sync()?;
            prior.sync()?;
            step()?;
        }
        for name in &self.incoming {
            step()?;
            (staged.rename_to(name, &work.data, name))
                .with_context(|| format!("Move restored {name} into place"))?;
            work.data.sync()?;
            staged.sync()?;
            step()?;
        }
        work.data.sync()?;
        prior.sync()?;
        Ok(())
    }

    pub(super) fn commit(&mut self, work: &Work) -> Result<()> {
        self.state = JournalState::Committed;
        self.write(work, || work.forward_step())
    }

    /// Moves every prior entry back. An entry is new when it was not live
    /// before the swap or its prior copy is still aside; new entries are
    /// removed first. Safe to repeat after a crash. It works through the held
    /// handles even when `.pp-restore` has moved, and checks nothing.
    pub(super) fn rollback(&mut self, work: &Work) -> Result<()> {
        super::activation::hooks::fail_rollback(&work.data_dir)?;
        if self.state == JournalState::Committed {
            self.state = JournalState::Swapping;
            self.write(work, || Ok(()))?;
        }
        // A crash between publishing the journal and creating `prior` leaves
        // nothing aside.
        let prior = open_optional(&work.dir, &self.prior)?;
        for name in managed() {
            let was_live = self.live.iter().any(|known| known == name);
            let kept = match &prior {
                Some(prior) if exists(prior, name)? => Some(prior),
                _ => None,
            };
            if exists(&work.data, name)? && (!was_live || kept.is_some()) {
                (work.data.remove_all(name)).with_context(|| format!("Remove restored {name}"))?;
            }
            if let Some(prior) = kept {
                (prior.rename_to(name, &work.data, name))
                    .with_context(|| format!("Move {name} back"))?;
            }
        }
        work.data.sync()?;
        super::activation::hooks::after_rollback(&work.data_dir);
        self.finish(work);
        Ok(())
    }

    /// Keep the journal until both copies are removed; restart can repeat cleanup.
    pub(super) fn finish(&self, work: &Work) {
        for name in [&self.prior, &self.staged] {
            if let Err(error) = work.dir.remove_all(name)
                && error.kind() != ErrorKind::NotFound
            {
                return;
            }
        }
        if work.dir.sync().is_ok() {
            let _ = work.dir.remove_file(JOURNAL);
            let _ = work.dir.sync();
        }
    }
}

/// Makes every file and directory of a stage durable, reading through
/// handles. A link anywhere in it is refused.
fn sync_stage(stage: &pp_os::Dir) -> Result<()> {
    for name in stage.entries()? {
        let name = name?;
        let name =
            (name.to_str()).ok_or_else(|| anyhow!("Restore stage holds a non-UTF-8 name"))?;
        let status = stage.status_nofollow(name)?;
        if status.is_dir() {
            sync_stage(&stage.open_dir_nofollow(name)?)?;
        } else {
            ensure!(status.is_file(), "Restore stage contains a link");
            stage.open_write_nofollow(name)?.sync_all()?;
        }
    }
    stage.sync()?;
    Ok(())
}
