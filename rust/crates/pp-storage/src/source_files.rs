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

use crate::{
    SettingsClient, WriterFailure, WriterOwner,
    auth::AuthPolicy,
    read_model::{Credential, graph::safe_path},
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
    /// The files of Source `source_id`'s current revision.
    pub fn list(
        &self,
        credential: Credential,
        source_id: i64,
        cancelled: &AtomicBool,
        wait: Duration,
    ) -> Result<SourceFiles, SourceFilesError> {
        let current = self
            .readers
            .read("source_files.list", cancelled, wait, |tx| {
                let installation = authorize(tx, &credential, self.policy)?;
                current(tx, &installation, source_id)
            })??;
        let Some(revision_id) = current.revision_id else {
            return Ok(SourceFiles {
                source_id,
                revision_id: None,
                files: Vec::new(),
            });
        };
        let locator = current
            .locator
            .ok_or(SourceFilesError::SnapshotUnavailable)?;
        let root = self.snapshot(source_id, &locator)?;
        Ok(SourceFiles {
            source_id,
            revision_id: Some(revision_id),
            files: walk(&root)?,
        })
    }

    /// The snapshot directory, which must resolve inside `repos/<source_id>`.
    fn snapshot(&self, source_id: i64, locator: &str) -> Result<PathBuf, SourceFilesError> {
        let unavailable = SourceFilesError::SnapshotUnavailable;
        if !safe_path(locator) || !locator.starts_with(&format!("{source_id}/")) {
            return Err(unavailable);
        }
        let workspace = self.repos.join(source_id.to_string());
        if !fs::symlink_metadata(&workspace).is_ok_and(|meta| meta.is_dir()) {
            return Err(unavailable);
        }
        match (
            fs::canonicalize(&workspace),
            fs::canonicalize(self.repos.join(locator)),
        ) {
            (Ok(workspace), Ok(root)) if root.starts_with(&workspace) && root.is_dir() => Ok(root),
            _ => Err(unavailable),
        }
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

/// Every regular file under `root`, sorted by path.
fn walk(root: &Path) -> Result<Vec<SourceFile>, SourceFilesError> {
    let unavailable = |_| SourceFilesError::SnapshotUnavailable;
    let mut files = Vec::new();
    let mut pending = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(unavailable)? {
            let entry = entry.map_err(unavailable)?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
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

#[cfg(test)]
mod tests;
