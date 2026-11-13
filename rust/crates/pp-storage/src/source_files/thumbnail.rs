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

use super::{SourceFilesClient, SourceFilesError};
use crate::{
    production::{
        geometry::GeometryLimits,
        thumbnail::{RENDERER_VERSION, render},
    },
    read_model::{Credential, graph::safe_path},
};
use pp_os::Dir;
use sha2::{Digest, Sha256};
use std::{
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
        if !self.requestable(path) {
            return Err(SourceFilesError::FileNotFound);
        }
        let name = "source_files.thumbnail";
        let Some((revision_id, root)) =
            self.snapshot(name, credential, source_id, cancelled, wait)?
        else {
            return Err(SourceFilesError::FileNotFound);
        };
        let digest = hex::encode(Sha256::digest(path.as_bytes()));
        let tag = format!("{revision_id}-v{RENDERER_VERSION}-{}", &digest[..16]);
        let cached = format!("{digest}-v{RENDERER_VERSION}.png");
        // A file that is not a whole PNG, as a crash between write and sync
        // could leave, is rendered again and replaced.
        if let Ok(png) = read_cached(&self.data, revision_id, &cached)
            && png.starts_with(PNG_SIGNATURE)
        {
            return Ok(SourceThumbnail {
                revision_id,
                tag,
                png,
            });
        }
        let limits = GeometryLimits::default();
        let stl = read_stl(&stl_file(&root, path)?, limits.max_bytes)?;
        let png = render(&stl, limits).map_err(|_| SourceFilesError::Unrenderable)?;
        if let Err(error) = store(&self.data, revision_id, &cached, &png) {
            tracing::warn!(error = %error, "source_thumbnail_cache_write_failed");
        }
        Ok(SourceThumbnail {
            revision_id,
            tag,
            png,
        })
    }

    /// A path the file list could show for an STL. No name may hold a `:`:
    /// on Windows a later `D:` segment would replace the whole path.
    fn requestable(&self, path: &str) -> bool {
        let first = path.split('/').next().unwrap_or_default();
        path.len() <= 4096
            && safe_path(path)
            && !path.contains(':')
            && path.to_ascii_lowercase().ends_with(".stl")
            && !self.internal.iter().any(|name| name == first)
    }
}

/// The STL at `path` under `root`, reached through real folders only. Each
/// name must be one plain path component, and the result stays under `root`.
fn stl_file(root: &Path, path: &str) -> Result<PathBuf, SourceFilesError> {
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

/// `thumbs/sources/<revision id>` under the data folder, each folder opened
/// relative to the one above without following a link, so a symbolic link or
/// a file at any of those names fails instead of leading outside. Missing
/// folders are made only when `create` is set.
fn cache_folder(data: &Path, revision_id: i64, create: bool) -> io::Result<Dir> {
    let revision = revision_id.to_string();
    let mut folder = Dir::open(data)?;
    // ast-grep-ignore: no-writer-family-literal -- Rust's own thumbnail cache, only inside thumbs/sources, a folder Node never creates; Node writes flat files directly in thumbs/.
    for name in ["thumbs", "sources", revision.as_str()] {
        folder = match folder.open_dir_nofollow(name) {
            Ok(next) => next,
            Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                match folder.create_dir(name) {
                    Ok(()) => folder.sync()?,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                folder.open_dir_nofollow(name)?
            }
            Err(error) => return Err(error),
        };
    }
    Ok(folder)
}

/// The cached file `name`, read without following a link.
fn read_cached(data: &Path, revision_id: i64, name: &str) -> io::Result<Vec<u8>> {
    let mut file = cache_folder(data, revision_id, false)?.open_read_nofollow(name)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Writes `png` as `name` through a temporary file in the same folder, synced
/// before the rename so a crash leaves the old state or the whole file. Every
/// step is relative to the folder handle, so nothing is written through a link.
fn store(data: &Path, revision_id: i64, name: &str, png: &[u8]) -> io::Result<()> {
    let folder = cache_folder(data, revision_id, true)?;
    let temporary = format!(".{:016x}.tmp", rand::random::<u64>());
    let written = folder
        .create_new_nofollow(&temporary)
        .and_then(|mut file| {
            file.write_all(png)?;
            file.sync_all()
        })
        .and_then(|()| folder.rename_replace(&temporary, name))
        .and_then(|()| folder.sync());
    if written.is_err() {
        let _ = folder.remove_file(&temporary);
    }
    written
}
