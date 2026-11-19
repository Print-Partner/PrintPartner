//! A thumbnail of one STL in a Source's current revision, cached under
//! `<data>/thumbs/sources` (GRE-430 part 2a).
//!
//! The requested path is checked before anything is read: a relative path of
//! plain names ending in `.stl`, outside the snapshot writer's root entries.
//! One read transaction then finds the current revision. The file is opened
//! only when every name on the way down is a real folder or file, as the file
//! list shows them: a symbolic link anywhere is not found.
//!
//! The cache file is `<revision id>/<SHA-256 of the path>-v<renderer
//! version>.png`, so nothing from the request names a folder or file there. A
//! revision never changes, so a cached file stays right; a hit is answered
//! without opening the STL. A new file is written under a temporary name and
//! renamed into place. A corrupt or oversized STL is refused and nothing is
//! cached. A cache write that fails is logged and the rendered PNG is still
//! answered. The cache folders are opened one by one without following a
//! link: a symbolic link at `thumbs`, `thumbs/sources` or the revision's
//! folder means no cache, never a write through it.
//!
//! The write that creates a revision's folder then prunes the cache
//! (GRE-1538): every revision folder whose revision is no longer any Source's
//! current revision is removed. The folders are listed before the read that
//! finds the current revisions, so a folder made for a revision that became
//! current after that read is not on the list. Nothing else removes a folder,
//! and a full backup leaves the whole cache out
//! (`archive::SOURCE_THUMBNAIL_CACHE`).

use super::{SourceFilesClient, SourceFilesError};
use crate::{
    archive::SOURCE_THUMBNAIL_CACHE,
    production::{
        geometry::GeometryLimits,
        thumbnail::{RENDERER_VERSION, render},
    },
    read_model::{Credential, graph::safe_path},
    write::authorize,
};
use pp_os::Dir;
use rusqlite::Transaction;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

/// How many thumbnail requests may run at once; the HTTP route enforces it.
pub const THUMBNAIL_CONCURRENCY: usize = 2;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceThumbnail {
    pub revision_id: i64,
    /// The same for the same revision, path and renderer version, and
    /// different otherwise: an entity tag for these bytes.
    pub tag: String,
    pub png: Vec<u8>,
}

impl SourceFilesClient {
    /// The 256 x 256 PNG of the STL at `path` in Source `source_id`'s
    /// current revision.
    pub fn thumbnail(
        &self,
        credential: Credential,
        source_id: i64,
        path: &str,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<SourceThumbnail, SourceFilesError> {
        if !self.requestable(path, &[".stl"]) {
            return Err(SourceFilesError::FileNotFound);
        }
        let name = "source_files.thumbnail";
        let Some((revision_id, root)) =
            self.snapshot(name, &credential, source_id, cancelled, wait)?
        else {
            return Err(SourceFilesError::FileNotFound);
        };
        let digest = hex::encode(Sha256::digest(path.as_bytes()));
        let tag = format!("{revision_id}-v{RENDERER_VERSION}-{}", &digest[..16]);
        let cached = format!("{digest}-v{RENDERER_VERSION}.png");
        // A file that is not a whole PNG, as a crash between write and sync
        // could leave, is rendered again and replaced.
        let folder = revision_id.to_string();
        if let Ok(png) = read_cached(&self.data, &folder, &cached)
            && png.starts_with(PNG_SIGNATURE)
        {
            return Ok(SourceThumbnail {
                revision_id,
                tag,
                png,
            });
        }
        let limits = GeometryLimits::default();
        let stl = read_stl(&revision_file(&root, path)?, limits.max_bytes)?;
        let png = render(&stl, limits).map_err(|_| SourceFilesError::Unrenderable)?;
        match store(&self.data, &folder, &cached, &png) {
            Ok(Folder::Created) => self.prune(&credential, cancelled, wait),
            Ok(Folder::Existed) => {}
            Err(error) => tracing::warn!(error = %error, "source_thumbnail_cache_write_failed"),
        }
        Ok(SourceThumbnail {
            revision_id,
            tag,
            png,
        })
    }

    /// Removes the cache folder of every revision that is no longer any
    /// Source's current revision. A failure is logged and stops nothing: the
    /// cache is regenerable, and the next new folder prunes again.
    fn prune(&self, credential: &Credential, cancelled: &AtomicBool, wait: Duration) {
        let warn = |error: &dyn std::fmt::Display| {
            tracing::warn!(error = %error, "source_thumbnail_cache_prune_failed");
        };
        let (sources, listed) = match revision_folders(&self.data) {
            Ok(found) => found,
            Err(error) => return warn(&error),
        };
        let current = self.readers.read(
            "source_files.thumbnail_prune",
            cancelled,
            wait,
            |tx| -> Result<_, SourceFilesError> {
                authorize(tx, credential, self.policy)?;
                Ok(current_revisions(tx)?)
            },
        );
        let current = match current {
            Ok(Ok(current)) => current,
            Ok(Err(error)) => return warn(&error),
            Err(error) => return warn(&error),
        };
        let mut removed = false;
        for (name, revision_id) in listed {
            if current.contains(&revision_id) {
                continue;
            }
            match remove_revision_folder(&sources, &name) {
                Ok(()) => removed = true,
                Err(error) => warn(&error),
            }
        }
        if removed && let Err(error) = sources.sync() {
            warn(&error);
        }
    }

    /// A path the file list could show for a file ending in one of
    /// `extensions` (lowercase, with the dot). No name may hold a `:`: on
    /// Windows a later `D:` segment would replace the whole path.
    pub(super) fn requestable(&self, path: &str, extensions: &[&str]) -> bool {
        let first = path.split('/').next().unwrap_or_default();
        let lower = path.to_ascii_lowercase();
        path.len() <= 4096
            && safe_path(path)
            && !path.contains(':')
            && extensions
                .iter()
                .any(|extension| lower.ends_with(extension))
            && !self.internal.iter().any(|name| name == first)
    }
}

/// The file at `path` under `root`, reached through real folders only. Each
/// name must be one plain path component, and the result stays under `root`.
pub(super) fn revision_file(root: &Path, path: &str) -> Result<PathBuf, SourceFilesError> {
    let not_found = |_| SourceFilesError::FileNotFound;
    let mut file = root.to_path_buf();
    let names: Vec<&str> = path.split('/').collect();
    for (index, name) in names.iter().enumerate() {
        let mut components = Path::new(name).components();
        if !matches!(
            (components.next(), components.next()),
            (Some(Component::Normal(_)), None)
        ) {
            return Err(SourceFilesError::FileNotFound);
        }
        file.push(name);
        let kind = fs::symlink_metadata(&file).map_err(not_found)?.file_type();
        let last = index + 1 == names.len();
        if (last && !kind.is_file()) || (!last && !kind.is_dir()) {
            return Err(SourceFilesError::FileNotFound);
        }
    }
    if !file.starts_with(root) {
        return Err(SourceFilesError::FileNotFound);
    }
    Ok(file)
}

/// The file's bytes, refused unread when over `max_bytes`.
fn read_stl(file: &Path, max_bytes: usize) -> Result<Vec<u8>, SourceFilesError> {
    let unavailable = |_| SourceFilesError::SnapshotUnavailable;
    let opened = fs::File::open(file).map_err(unavailable)?;
    let size = opened.metadata().map_err(unavailable)?.len();
    if usize::try_from(size).map_or(true, |size| size > max_bytes) {
        return Err(SourceFilesError::Unrenderable);
    }
    let mut bytes = Vec::with_capacity(size as usize);
    // One more than the cap, so a file that grew since is still refused.
    let mut limited = opened.take(max_bytes as u64 + 1);
    limited.read_to_end(&mut bytes).map_err(unavailable)?;
    if bytes.len() > max_bytes {
        return Err(SourceFilesError::Unrenderable);
    }
    Ok(bytes)
}

/// Whether [`cache_folder`] made the revision's folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Folder {
    Created,
    Existed,
}

/// `thumbs/sources/<leaf>` under the data folder, each folder opened relative
/// to the one above without following a link, so a symbolic link or a file at
/// any of those names fails instead of leading outside. Missing folders are
/// made only when `create` is set. `leaf` is a revision id for thumbnails and
/// `step` for STEP part trees.
fn cache_folder(data: &Path, leaf: &str, create: bool) -> io::Result<(Dir, Folder)> {
    let mut folder = Dir::open(data)?;
    let mut made = Folder::Existed;
    for name in SOURCE_THUMBNAIL_CACHE.split('/').chain([leaf]) {
        folder = match folder.open_dir_nofollow(name) {
            Ok(next) => next,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                match folder.create_dir(name) {
                    Ok(()) => {
                        folder.sync()?;
                        made = Folder::Created;
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                folder.open_dir_nofollow(name)?
            }
            Err(error) => return Err(error),
        };
    }
    Ok((folder, made))
}

/// `thumbs/sources`, opened as [`cache_folder`] opens it, and the revision
/// folders in it: each real folder named by a revision id as Rust writes it,
/// decimal with no leading zero. Links, files and other names are left alone.
fn revision_folders(data: &Path) -> io::Result<(Dir, Vec<(String, i64)>)> {
    let mut sources = Dir::open(data)?;
    for name in SOURCE_THUMBNAIL_CACHE.split('/') {
        sources = sources.open_dir_nofollow(name)?;
    }
    let mut listed = Vec::new();
    for name in sources.entries()? {
        let Some(name) = name?.to_str().map(str::to_owned) else {
            continue;
        };
        let Some(revision_id) = name.parse::<i64>().ok().filter(|id| id.to_string() == name) else {
            continue;
        };
        if sources.status_nofollow(&name)?.is_dir() {
            listed.push((name, revision_id));
        }
    }
    Ok((sources, listed))
}

/// Every revision that is some Source's current revision, in any tenant.
fn current_revisions(tx: &Transaction<'_>) -> rusqlite::Result<BTreeSet<i64>> {
    tx.prepare(
        "SELECT current_source_revision_id FROM projects \
         WHERE current_source_revision_id IS NOT NULL",
    )?
    .query_map([], |row| row.get(0))?
    .collect()
}

/// Removes the revision folder `name` and the files in it, without following
/// a link. A folder that holds anything but regular files is left in place.
fn remove_revision_folder(sources: &Dir, name: &str) -> io::Result<()> {
    let folder = sources.open_dir_nofollow(name)?;
    for file in folder.entries()? {
        let file = file?;
        let file = file
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "name is not UTF-8"))?;
        folder.remove_file(file)?;
    }
    sources.remove_dir(name)
}

/// The cached file `name` in `thumbs/sources/<leaf>`, read without following
/// a link.
pub(super) fn read_cached(data: &Path, leaf: &str, name: &str) -> io::Result<Vec<u8>> {
    let (folder, _) = cache_folder(data, leaf, false)?;
    let mut file = folder.open_read_nofollow(name)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Writes `bytes` as `name` in `thumbs/sources/<leaf>` through a temporary
/// file in the same folder, synced before the rename so a crash leaves the old
/// state or the whole file. Every step is relative to the folder handle, so
/// nothing is written through a link.
pub(super) fn store(data: &Path, leaf: &str, name: &str, bytes: &[u8]) -> io::Result<Folder> {
    let (folder, made) = cache_folder(data, leaf, true)?;
    let temporary = format!(".{:016x}.tmp", rand::random::<u64>());
    let written = folder
        .create_new_nofollow(&temporary)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| folder.rename_replace(&temporary, name))
        .and_then(|()| folder.sync());
    if written.is_err() {
        let _ = folder.remove_file(&temporary);
    }
    written.map(|()| made)
}
