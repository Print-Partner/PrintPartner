//! The schema preflight's working copy (GRE-1488).
//!
//! The preflight copies `print-partner.db` and its `-wal` and `-journal` into
//! a private folder named `.pp-preflight-<32 hex>`, inside the directory it
//! checks: the data folder at start, or the stage during restore and import.
//! The copy therefore needs free disk on the data filesystem, never on the
//! system temporary directory. [`WorkingCopy::create`] refuses with
//! [`InsufficientSpace`] before it creates anything. Every file is read and
//! written in chunks and hashed as it goes, so no database file is held in
//! memory.
//!
//! The folder is removed when the [`WorkingCopy`] drops. A folder left by a
//! killed start is removed by [`sweep`], which the next start runs under the
//! writer lock. It removes only real directories with this name pattern,
//! through the directory handle, and touches nothing else.

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

/// The name every working folder starts with; 32 lowercase hex digits follow.
pub(crate) const PREFIX: &str = ".pp-preflight-";
/// Read and write size for the streamed copy and compare.
const CHUNK: usize = 1 << 20;

/// Too little free disk for the preflight's working copy. Nothing was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "Not enough free disk on the data volume for the schema preflight copy: {needed} bytes needed, {available} bytes available"
)]
pub struct InsufficientSpace {
    /// The size of the database files to copy.
    pub needed: u64,
    /// Free bytes the filesystem reported for the folder.
    pub available: u64,
}

/// The SHA-256 and length of one database file, or `None` when it is absent.
pub(super) type Captured = Option<([u8; 32], u64)>;

/// A private folder holding the copy. Dropping it removes the folder.
pub(super) struct WorkingCopy<'a> {
    root: &'a pp_os::Dir,
    name: String,
    folder: pp_os::Dir,
    path: PathBuf,
}

impl<'a> WorkingCopy<'a> {
    /// Checks free space at `root_path` for `needed` bytes, then creates the
    /// folder in `root` and checks that `root_path` names that same folder,
    /// so SQLite opens the copy made in the checked directory.
    pub(super) fn create(root: &'a pp_os::Dir, root_path: &Path, needed: u64) -> Result<Self> {
        let available = fs4::available_space(root_path).with_context(|| {
            format!(
                "schema::preflight: read free space in {}",
                root_path.display()
            )
        })?;
        if available < needed {
            return Err(InsufficientSpace { needed, available }.into());
        }
        let name = format!("{PREFIX}{}", hex::encode(rand::random::<[u8; 16]>()));
        root.create_dir(&name)
            .with_context(|| format!("schema::preflight: create working folder {name}"))?;
        let path = root_path.join(&name);
        let folder = match root.open_dir_nofollow(&name) {
            Ok(folder) => folder,
            Err(error) => {
                // The folder is still empty, so a plain remove suffices.
                let _ = root.remove_dir(&name);
                return Err(error)
                    .with_context(|| format!("schema::preflight: open working folder {name}"));
            }
        };
        // From here a failure drops the copy, which removes the folder.
        let copy = Self {
            root,
            name,
            folder,
            path,
        };
        let by_handle = copy.folder.status()?.identity();
        let by_path = pp_os::status_nofollow(&copy.path)
            .with_context(|| format!("schema::preflight: inspect {}", copy.path.display()))?;
        ensure!(
            by_path.is_dir() && by_path.identity() == by_handle,
            "Data directory was replaced during the schema preflight"
        );
        Ok(copy)
    }

    /// Creates the copy of `name` in the folder.
    pub(super) fn create_file(&self, name: &str) -> io::Result<File> {
        self.folder.create_new_nofollow(name)
    }

    /// The copied database, for SQLite to open.
    pub(super) fn database(&self) -> PathBuf {
        self.path.join("print-partner.db")
    }
}

impl Drop for WorkingCopy<'_> {
    fn drop(&mut self) {
        // SQLite may add `-shm` or `-wal` files beside the copy; remove every
        // file, then the folder.
        empty(&self.folder);
        let _ = self.root.remove_dir(&self.name);
    }
}

/// Removes every regular file in `folder`. Entries it cannot remove stay.
fn empty(folder: &pp_os::Dir) {
    let Ok(entries) = folder.entries() else {
        return;
    };
    for entry in entries.flatten() {
        if let Some(name) = entry.to_str()
            && folder
                .status_nofollow(name)
                .is_ok_and(|status| status.is_file())
        {
            let _ = folder.remove_file(name);
        }
    }
}

/// Whether `name` is a working folder's name: [`PREFIX`] and 32 lowercase hex.
fn is_working_folder(name: &str) -> bool {
    name.strip_prefix(PREFIX).is_some_and(|id| {
        id.len() == 32
            && id
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Removes working folders a killed start left in `root`. Only real
/// directories with the working-folder name are touched; a link, a file or
/// any other name stays. Call it only while holding the writer lock, so no
/// live preflight owns a folder it removes.
pub(crate) fn sweep(root: &pp_os::Dir) -> Result<()> {
    let entries = root
        .entries()
        .context("schema::preflight: list the data folder")?;
    for entry in entries {
        let entry = entry.context("schema::preflight: list the data folder")?;
        let Some(name) = entry.to_str().filter(|name| is_working_folder(name)) else {
            continue;
        };
        if !root
            .status_nofollow(name)
            .is_ok_and(|status| status.is_dir())
        {
            continue;
        }
        let folder = root
            .open_dir_nofollow(name)
            .with_context(|| format!("schema::preflight: open leftover {name}"))?;
        empty(&folder);
        root.remove_dir(name)
            .with_context(|| format!("schema::preflight: remove leftover {name}"))?;
    }
    Ok(())
}

/// Streams `source` into `sink`, when given, in [`CHUNK`]-sized pieces and
/// returns the SHA-256 and length of what it read.
pub(super) fn stream(
    source: &mut File,
    mut sink: Option<&mut File>,
) -> io::Result<([u8; 32], u64)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; CHUNK];
    let mut len = 0u64;
    loop {
        let read = match source.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
        len += read as u64;
        if let Some(sink) = sink.as_mut() {
            sink.write_all(&buffer[..read])?;
        }
    }
    Ok((hasher.finalize().into(), len))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_folder_names_are_exact() {
        let id = "0123456789abcdef0123456789abcdef";
        assert!(is_working_folder(&format!("{PREFIX}{id}")));
        for name in [
            "backups".to_owned(),
            format!("{PREFIX}{}", &id[1..]),
            format!("{PREFIX}{}", id.to_uppercase()),
            format!("{PREFIX}{id}x"),
            format!("pp-preflight-{id}"),
        ] {
            assert!(!is_working_folder(&name), "{name}");
        }
    }

    #[test]
    fn sweep_removes_only_real_working_folders() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        let id = |n: u8| format!("{PREFIX}{}", hex::encode([n; 16]));
        std::fs::create_dir(path.join(id(1))).unwrap();
        std::fs::write(path.join(id(1)).join("print-partner.db"), b"copy").unwrap();
        std::fs::write(path.join(id(2)), b"a file with the name").unwrap();
        std::fs::create_dir_all(path.join("backups").join(id(3))).unwrap();
        std::fs::create_dir(path.join("kept")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(path.join("kept"), path.join(id(4))).unwrap();
        let dir = pp_os::Dir::open(path).unwrap();
        sweep(&dir).unwrap();
        assert!(!path.join(id(1)).exists());
        assert!(path.join(id(2)).is_file());
        assert!(path.join("backups").join(id(3)).is_dir());
        assert!(path.join("kept").is_dir());
        #[cfg(unix)]
        assert!(path.join(id(4)).symlink_metadata().unwrap().is_symlink());
    }

    #[test]
    fn too_little_space_creates_nothing() {
        let root = tempfile::tempdir().unwrap();
        let dir = pp_os::Dir::open(root.path()).unwrap();
        let error = WorkingCopy::create(&dir, root.path(), u64::MAX)
            .err()
            .expect("u64::MAX bytes never fit");
        let refusal = error.downcast_ref::<InsufficientSpace>().unwrap();
        assert_eq!(refusal.needed, u64::MAX);
        assert!(refusal.available < u64::MAX);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_working_copy_is_removed_when_it_drops() {
        let root = tempfile::tempdir().unwrap();
        let dir = pp_os::Dir::open(root.path()).unwrap();
        {
            let copy = WorkingCopy::create(&dir, root.path(), 0).unwrap();
            let mut file = copy.create_file("print-partner.db").unwrap();
            let original = tempfile::NamedTempFile::new().unwrap();
            // More than one chunk, so the copy crosses a chunk boundary.
            std::fs::write(original.path(), vec![7u8; CHUNK + 5]).unwrap();
            let mut source = File::open(original.path()).unwrap();
            let digest = stream(&mut source, Some(&mut file)).unwrap();
            assert_eq!(digest.1, (CHUNK + 5) as u64);
            let mut again = File::open(copy.database()).unwrap();
            assert_eq!(stream(&mut again, None).unwrap(), digest);
            std::fs::write(copy.path.join("print-partner.db-shm"), b"x").unwrap();
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
