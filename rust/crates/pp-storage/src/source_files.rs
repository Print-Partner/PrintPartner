//! The files of a Source's current revision, which Library lists (GRE-430
//! part 1).
//!
//! One read transaction finds the Source and its current revision's
//! snapshot locator. A revision never changes, so the snapshot directory
//! under `<data>/repos` is walked after the transaction ends.
//!
//! Paths are relative to the snapshot root, joined with `/`, and sorted by
//! their bytes. Symbolic links and names that are not UTF-8 are left out: a
//! later read by path (the thumbnail route) could not open them as files of
//! the revision. A revision with more than [`MAX_FILES`] files is refused
//! rather than listed in part.
//!
//! The snapshot writer keeps its own entries at a revision's root. Storage may
//! not depend on `pp-source`, so the caller names them with
//! [`SourceFilesClient::skipping_at_root`]; `pp-core` passes
//! `pp_source::MANIFEST`.

use crate::{
    SettingsClient, WriterFailure, WriterOwner,
    auth::AuthPolicy,
    read_model::Credential,
    working_drafts::source_reads::revision_root,
    write::{Installation, authorize},
};
use rusqlite::{OptionalExtension, Transaction};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

/// The most files one revision may list.
pub const MAX_FILES: usize = 20_000;

/// What a file is, by its extension, for Library: an STL gets a thumbnail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Stl,
    /// Another 3D or CAD format: 3MF, STEP, OBJ, OpenSCAD, Fusion.
    Model,
    /// PDF, Markdown or plain text.
    Document,
    Image,
    Other,
}

impl FileKind {
    fn of(name: &str) -> Self {
        let extension = name
            .rsplit_once('.')
            .map(|(_, ext)| ext.to_ascii_lowercase());
        match extension.as_deref() {
            Some("stl") => Self::Stl,
            Some("3mf" | "step" | "stp" | "obj" | "scad" | "f3d") => Self::Model,
            Some("pdf" | "md" | "markdown" | "txt") => Self::Document,
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "svg") => Self::Image,
            _ => Self::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    pub path: String,
    pub kind: FileKind,
    pub size_bytes: u64,
}

/// `revision_id` is `None`, and `files` empty, for a Source with no revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFiles {
    pub source_id: i64,
    pub revision_id: Option<i64>,
    pub files: Vec<SourceFile>,
}

#[derive(Debug, thiserror::Error)]
pub enum SourceFilesError {
    #[error("no Source has that id")]
    NotFound,
    /// The current revision's row or snapshot directory is missing, or the
    /// locator points outside the Source's workspace.
    #[error("the current revision's snapshot cannot be read")]
    SnapshotUnavailable,
    #[error("the revision holds more than {MAX_FILES} files")]
    TooManyFiles,
    /// The path names no regular STL file of the current revision, or the
    /// Source has no revision.
    #[error("the current revision has no STL file at that path")]
    FileNotFound,
    /// The STL is corrupt or over the reader's caps. Nothing was cached.
    #[error("the STL cannot be rendered")]
    Unrenderable,
    #[error(transparent)]
    Writer(#[from] WriterFailure),
}

impl From<rusqlite::Error> for SourceFilesError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Writer(error.into())
    }
}

#[derive(Clone)]
pub struct SourceFilesClient {
    readers: SettingsClient,
    policy: AuthPolicy,
    repos: PathBuf,
    /// The data folder; the thumbnail cache is under `thumbs/sources`.
    data: PathBuf,
    /// Names at a revision's root that belong to the snapshot writer.
    internal: Vec<String>,
}

impl WriterOwner {
    pub fn source_files(&self, policy: AuthPolicy) -> anyhow::Result<SourceFilesClient> {
        crate::auth::validate_policy(policy)?;
        let lease = self
            .lease
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Storage owner is closed"))?;
        Ok(SourceFilesClient {
            readers: self.client(),
            policy,
            repos: lease.data_dir().join("repos"),
            data: lease.data_dir().to_path_buf(),
            internal: Vec::new(),
        })
    }
}

/// The current revision as stored: its id and, when its row belongs to the
/// Source, its snapshot locator.
struct Current {
    revision_id: Option<i64>,
    locator: Option<String>,
}

impl SourceFilesClient {
    /// Leaves `name` out of the list when it sits at a revision's root: an
    /// entry the snapshot writer keeps there, such as `pp_source::MANIFEST`.
    pub fn skipping_at_root(mut self, name: &str) -> Self {
        self.internal.push(name.to_owned());
        self
    }

    /// The files of Source `source_id`'s current revision.
    pub fn list(
        &self,
        credential: Credential,
        source_id: i64,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<SourceFiles, SourceFilesError> {
        let snapshot =
            self.snapshot("source_files.list", credential, source_id, cancelled, wait)?;
        let Some((revision_id, root)) = snapshot else {
            return Ok(SourceFiles {
                source_id,
                revision_id: None,
                files: Vec::new(),
            });
        };
        Ok(SourceFiles {
            source_id,
            revision_id: Some(revision_id),
            files: walk(&root, &self.internal)?,
        })
    }

    /// The current revision's id and snapshot directory, or `None` for a
    /// Source with no revision. One read transaction checks the credential
    /// and reads the Source; the directory is resolved after it.
    fn snapshot(
        &self,
        name: &'static str,
        credential: Credential,
        source_id: i64,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<Option<(i64, PathBuf)>, SourceFilesError> {
        let current = self.readers.read(name, cancelled, wait, |tx| {
            let installation = authorize(tx, &credential, self.policy)?;
            current(tx, &installation, source_id)
        })??;
        let Some(revision_id) = current.revision_id else {
            return Ok(None);
        };
        let locator = current
            .locator
            .ok_or(SourceFilesError::SnapshotUnavailable)?;
        let root = revision_root(&self.repos, source_id, &locator)
            .ok_or(SourceFilesError::SnapshotUnavailable)?;
        Ok(Some((revision_id, root)))
    }
}

fn current(
    tx: &Transaction<'_>,
    installation: &Installation,
    source_id: i64,
) -> Result<Current, SourceFilesError> {
    let row = tx
        .query_row(
            "SELECT p.current_source_revision_id, r.snapshot_locator FROM projects p \
             LEFT JOIN source_revisions r ON r.id = p.current_source_revision_id \
             AND r.project_id = p.id AND r.tenant_id = p.tenant_id \
             WHERE p.tenant_id = ?1 AND p.id = ?2",
            rusqlite::params![installation.id(), source_id],
            |row| {
                Ok(Current {
                    revision_id: row.get(0)?,
                    locator: row.get(1)?,
                })
            },
        )
        .optional()?;
    row.ok_or(SourceFilesError::NotFound)
}

/// Every regular file under `root`, sorted by path, without the `internal`
/// names at the root.
fn walk(root: &Path, internal: &[String]) -> Result<Vec<SourceFile>, SourceFilesError> {
    let unavailable = |_| SourceFilesError::SnapshotUnavailable;
    let mut files = Vec::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(unavailable)? {
            let entry = entry.map_err(unavailable)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if prefix.is_empty() && internal.contains(&name) {
                continue;
            }
            let path = format!("{prefix}{name}");
            // `file_type` does not follow a symbolic link, so links are skipped.
            let kind = entry.file_type().map_err(unavailable)?;
            if kind.is_dir() {
                pending.push((entry.path(), format!("{path}/")));
            } else if kind.is_file() {
                if files.len() == MAX_FILES {
                    return Err(SourceFilesError::TooManyFiles);
                }
                let size_bytes = entry.metadata().map_err(unavailable)?.len();
                files.push(SourceFile {
                    kind: FileKind::of(&name),
                    path,
                    size_bytes,
                });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

mod thumbnail;
pub use thumbnail::{SourceThumbnail, THUMBNAIL_CONCURRENCY};

#[cfg(test)]
mod tests;
