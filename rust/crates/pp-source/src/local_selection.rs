use crate::{
    Directory, Error, FileKind, LocalFiles, MAX_ENTRIES, PathCollisions, Result, SelectedFile,
    SourcePath, SourceRoot,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InputFile {
    pub path: SourcePath,
    pub size: u64,
    pub sha256: String,
}

fn walk(root: &Directory, prefix: &str, files: &mut Vec<SourcePath>) -> Result<()> {
    for name in root.entries()? {
        if name.starts_with('.') {
            continue;
        }
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = SourcePath::try_from(relative.clone())?;
        match root.child(&name, false) {
            Ok(child) => walk(&child, &relative, files)?,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotADirectory => {
                root.file(&name, false)?;
                files.push(path);
                if files.len() > MAX_ENTRIES {
                    return Err(Error::Limit);
                }
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Bytes stored under `root`, counting every regular file.
pub fn stored_bytes(root: &Path, cancelled: &AtomicBool) -> Result<u64> {
    fn count(dir: &Directory, cancelled: &AtomicBool) -> Result<u64> {
        let mut total = 0u64;
        for entry in dir.entries()? {
            if cancelled.load(Ordering::Acquire) {
                return Err(Error::Limit);
            }
            let bytes = match dir.child(&entry, false) {
                Ok(child) => count(&child, cancelled)?,
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotADirectory => {
                    dir.file(&entry, false)?.metadata()?.len()
                }
                Err(e) => return Err(e),
            };
            total = total.checked_add(bytes).ok_or(Error::Limit)?;
        }
        Ok(total)
    }
    count(&Directory::open_root(root)?, cancelled)
}

impl LocalFiles {
    /// Every regular file below the selected directory in path order. Names
    /// starting with `.` are skipped; a link or special file is refused.
    pub fn paths(&self) -> Result<Vec<SourcePath>> {
        let mut paths = Vec::new();
        walk(&self.root, "", &mut paths)?;
        paths.sort_by(|a, b| crate::compare_paths(a.as_str(), b.as_str()));
        Ok(paths)
    }
    pub fn inventory(&self, paths: &[SourcePath], max_bytes: u64) -> Result<Vec<InputFile>> {
        self.inventory_until(paths, max_bytes, &AtomicBool::new(false))
    }
    /// `inventory` that stops with `Error::Limit` once `cancelled` is set.
    pub fn inventory_until(
        &self,
        paths: &[SourcePath],
        max_bytes: u64,
        cancelled: &AtomicBool,
    ) -> Result<Vec<InputFile>> {
        validate_paths(paths)?;
        let mut result = Vec::new();
        let mut total = 0u64;
        for path in paths {
            let mut file = self.root.file(path.as_str(), false)?;
            let mut hash = Sha256::new();
            let mut size = 0u64;
            let mut buf = [0u8; 8192];
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Err(Error::Limit);
                }
                let n = file.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                size += n as u64;
                total += n as u64;
                if total > max_bytes {
                    return Err(Error::Limit);
                }
                hash.update(&buf[..n]);
            }
            result.push(InputFile {
                path: path.clone(),
                size,
                sha256: hex::encode(hash.finalize()),
            });
        }
        Ok(result)
    }
    pub fn capture(
        &self,
        paths: &[SourcePath],
        owned_root: &Path,
        operation: &str,
        max_bytes: u64,
        cancelled: &AtomicBool,
    ) -> Result<(LocalFiles, Vec<InputFile>)> {
        crate::validate_revision(operation)?;
        validate_paths(paths)?;
        let parent = Directory::open_root(owned_root)?;
        let imports = parent.child(".pp-imports", true)?;
        let root = match imports.create_child_exclusive(operation) {
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::InvalidIdentity);
            }
            result => result?,
        };
        imports.sync()?;
        let owned = LocalFiles { root };
        let mut total = 0u64;
        for path in paths {
            let mut input = self.root.file(path.as_str(), false)?;
            let (parent, name) = owned.root.parent(path.as_str(), true)?;
            let mut output = parent.file_exclusive(&name)?;
            let mut buf = [0u8; 8192];
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Err(Error::Limit);
                }
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                total += n as u64;
                if total > max_bytes {
                    return Err(Error::Limit);
                }
                output.write_all(&buf[..n])?;
            }
            output.sync_all()?;
            parent.sync()?;
        }
        owned.root.sync()?;
        imports.sync()?;
        let inventory = owned.inventory(paths, max_bytes)?;
        Ok((owned, inventory))
    }

    /// Classifies retained files into a snapshot selection and an upstream key.
    ///
    /// The caller supplies the operation's limits. A file whose kind cannot be
    /// classified is refused rather than dropped, so a revision never claims to
    /// be complete while omitting a stored file.
    pub fn local_snapshot_selection(
        &self,
        limits: SelectionLimits,
    ) -> Result<(String, Vec<SelectedFile>)> {
        let mut paths = self.paths()?;
        paths.sort_by(|a, b| crate::compare_paths(a.as_str(), b.as_str()));
        let inventory = self.inventory(&paths, limits.max_bytes)?;
        let mut hash = Sha256::new();
        let mut files = Vec::new();
        let mut stls = 0u64;
        let mut documentation_bytes = 0u64;
        for file in inventory {
            let kind = classify(file.path.as_str());
            if kind == FileKind::Stl {
                stls += 1;
                if stls > limits.max_stl_files {
                    return Err(Error::Limit);
                }
            } else {
                documentation_bytes = documentation_bytes
                    .checked_add(file.size)
                    .ok_or(Error::Limit)?;
                if documentation_bytes > limits.max_documentation_bytes {
                    return Err(Error::Limit);
                }
            }
            hash.update(serde_json::to_vec(&file)?);
            hash.update(b"\n");
            files.push(SelectedFile {
                path: file.path,
                kind,
                size_hint_bytes: Some(file.size),
            });
        }
        if files.is_empty() {
            return Err(Error::EmptySelection);
        }
        Ok((hex::encode(hash.finalize()), files))
    }
}

/// Snapshot selection caps taken from an import operation's reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionLimits {
    /// Largest number of STL files the revision may contain.
    pub max_stl_files: u64,
    /// Largest number of non-STL bytes the revision may contain.
    pub max_documentation_bytes: u64,
    /// Largest number of bytes the inventory may read.
    pub max_bytes: u64,
}

/// The kind a file is kept as. A file of any other type, such as an image or
/// a 3MF, stays in the revision as an `Artifact` (GRE-1444).
fn classify(path: &str) -> FileKind {
    let lower = path.to_lowercase();
    if lower.ends_with(".stl") {
        FileKind::Stl
    } else if lower.ends_with(".pdf") {
        FileKind::Pdf
    } else if !lower.ends_with(".md") {
        FileKind::Artifact
    } else if lower
        .rsplit('/')
        .next()
        .is_some_and(|base| base == "readme.md" || base.starts_with("readme."))
    {
        FileKind::Readme
    } else {
        FileKind::Md
    }
}

/// Removes `repos/.pp-imports/{operation}` after publication settles.
pub fn discard_owned(root: &Path, operation: &str) -> Result<()> {
    crate::validate_revision(operation)?;
    let parent = Directory::open_root(root)?.child(".pp-imports", true)?;
    parent.remove_tree(operation)?;
    parent.sync()
}

impl SourceRoot {
    /// Reloads a published revision and checks its stored bytes and digest.
    pub fn verify_published(&self, key: &str) -> Result<crate::PublishedSnapshot> {
        crate::validate_revision(key)?;
        self.load(&self.revisions.child(key, false)?, key)
    }

    /// Removes abandoned media, archive, and snapshot staging directories.
    pub fn discard_staging(&mut self) -> Result<()> {
        self.revisions.try_lock()?;
        let result = (|| {
            for name in [".pp-source-archives", ".pp-source-media"] {
                let parent = match self.revisions.child(name, false) {
                    Ok(parent) => parent,
                    Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                parent.try_lock()?;
                parent.remove_tree(".candidate")?;
                parent.sync()?;
            }
            self.revisions.remove_tree(crate::CANDIDATE)?;
            self.revisions.sync()
        })();
        let unlocked = self.revisions.unlock();
        result?;
        unlocked?;
        Ok(())
    }
}

fn validate_paths(paths: &[SourcePath]) -> Result<()> {
    if paths.len() > MAX_ENTRIES {
        return Err(Error::Limit);
    }
    let mut collisions = PathCollisions::default();
    for path in paths {
        collisions.insert(path, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn open_local(files: &[(&str, &[u8])]) -> (tempfile::TempDir, LocalFiles) {
        let dir = tempfile::tempdir().unwrap();
        for (path, bytes) in files {
            let path = dir.path().join(path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(path, bytes).unwrap();
        }
        let local = LocalFiles::open(dir.path()).unwrap();
        (dir, local)
    }

    fn limits(max_stl_files: u64, max_documentation_bytes: u64) -> SelectionLimits {
        SelectionLimits {
            max_stl_files,
            max_documentation_bytes,
            max_bytes: 16 * 1024 * 1024,
        }
    }

    #[test]
    fn local_snapshot_selection_applies_supplied_operation_limits() {
        // 501 tiny STLs exceed the old hardcoded cap of 500 but fit the byte
        // budget and count the caller supplies.
        let names = (0..501)
            .map(|index| format!("model-{index:03}.stl"))
            .collect::<Vec<_>>();
        let files = names
            .iter()
            .map(|name| (name.as_str(), b"solid\n".as_slice()))
            .collect::<Vec<_>>();
        let (_dir, local) = open_local(&files);
        assert!(matches!(
            local.local_snapshot_selection(limits(500, 1024 * 1024)),
            Err(Error::Limit)
        ));
        let (_, selected) = local
            .local_snapshot_selection(limits(501, 8 * 1024 * 1024))
            .unwrap();
        assert_eq!(selected.len(), 501);

        // A phase manifest above the old 1 MiB hardcode passes when the
        // operation's documentation budget allows it.
        let phases = vec![b'x'; 1024 * 1024 + 1];
        let (_dir, local) = open_local(&[("pp-phases.json", phases.as_slice())]);
        assert!(matches!(
            local.local_snapshot_selection(limits(500, 1024 * 1024)),
            Err(Error::Limit)
        ));
        let (_, selected) = local
            .local_snapshot_selection(limits(500, 4 * 1024 * 1024))
            .unwrap();
        assert_eq!(selected.len(), 1);
    }

    #[test]
    fn an_empty_selection_has_its_own_error() {
        let (_dir, local) = open_local(&[]);
        assert!(matches!(
            local.local_snapshot_selection(limits(500, 1024 * 1024)),
            Err(Error::EmptySelection)
        ));
    }

    #[test]
    fn local_snapshot_selection_keeps_other_files_as_artifacts() {
        let (_dir, local) = open_local(&[
            ("triangle.stl", b"solid triangle".as_slice()),
            ("notes.txt", b"not a snapshot kind".as_slice()),
            ("photo.png", b"not a model".as_slice()),
        ]);
        let (_, files) = local
            .local_snapshot_selection(limits(500, 4 * 1024 * 1024))
            .unwrap();
        let kinds = files
            .iter()
            .map(|file| (file.path.as_str(), file.kind.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            [
                ("notes.txt", FileKind::Artifact),
                ("photo.png", FileKind::Artifact),
                ("triangle.stl", FileKind::Stl),
            ]
        );
    }
}
