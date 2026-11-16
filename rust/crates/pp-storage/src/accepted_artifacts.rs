//! The verified open of an accepted Part's STL, Node's
//! `openVerifiedAcceptedArtifact` (`services/accepted-artifacts.ts`): the file
//! is found inside its Source snapshot without following links and checked
//! against its accepted SHA-256. Plate initialize measures through it.

use crate::read_model::Artifact;
use sha2::{Digest as _, Sha256};
use std::{
    fmt,
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

/// Why an accepted STL could not be opened as accepted.
#[derive(Debug)]
pub enum ArtifactFailure {
    /// The accepted Plan names no usable file; the reason it records.
    Unavailable(&'static str),
    /// The snapshot or relative path would leave the Source snapshot.
    UnsafePath,
    Missing,
    /// A snapshot directory or the file itself is a link.
    Link,
    /// A path segment is not a directory, or the file is not a regular file.
    NotFile,
    /// Two names in one directory match a segment ignoring case.
    AmbiguousCase,
    Empty,
    /// The bytes do not hash to the accepted SHA-256, or none is recorded.
    DigestMismatch,
    /// Reading failed, or the file changed between the check and the open.
    Read(io::Error),
}

impl ArtifactFailure {
    /// The reason code Node reports for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(reason) => reason,
            Self::UnsafePath => "unsafe_path",
            Self::Missing => "missing",
            Self::Link => "symlink",
            Self::NotFile => "not_file",
            Self::AmbiguousCase => "ambiguous_case",
            Self::Empty => "empty",
            Self::DigestMismatch => "digest_mismatch",
            Self::Read(_) => "io_error",
        }
    }
}

impl From<io::Error> for ArtifactFailure {
    /// Node's `fileErrorReason`.
    fn from(error: io::Error) -> Self {
        if matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
        ) {
            return Self::Missing;
        }
        #[cfg(unix)]
        if error.raw_os_error() == Some(libc::ELOOP) {
            return Self::Link;
        }
        Self::Read(error)
    }
}

impl fmt::Display for ArtifactFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for ArtifactFailure {}

/// Opens `artifact` under `repos` without following links and returns the
/// open file and its hashed length once its SHA-256 matches. The file's
/// position is at its end.
pub fn open_verified(repos: &Path, artifact: &Artifact) -> Result<(File, u64), ArtifactFailure> {
    let (root, relative, expected) = match artifact {
        Artifact::Unavailable { reason } => return Err(ArtifactFailure::Unavailable(reason)),
        Artifact::Tracked {
            snapshot_path,
            relative_path,
            expected_sha256,
            ..
        } => (snapshot_path, relative_path, expected_sha256),
    };
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(ArtifactFailure::DigestMismatch);
    }
    let path = artifact_path(repos, root, relative)?;
    if is_link(&path)? {
        return Err(ArtifactFailure::Link);
    }
    let before = pp_os::status_nofollow(&path)?;
    let mut file = pp_os::open_read_nofollow(&path)?;
    let opened = pp_os::status_of(&file)?;
    if !opened.is_file() {
        return Err(ArtifactFailure::NotFile);
    }
    if before.identity() != opened.identity() {
        return Err(ArtifactFailure::Read(io::Error::other(
            "file changed between check and open",
        )));
    }
    if file.metadata()?.len() == 0 {
        return Err(ArtifactFailure::Empty);
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut hashed = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        hashed += read as u64;
    }
    if hashed == 0 {
        return Err(ArtifactFailure::Empty);
    }
    if hex::encode(hash.finalize()) != *expected {
        return Err(ArtifactFailure::DigestMismatch);
    }
    Ok((file, hashed))
}

fn is_link(path: &Path) -> io::Result<bool> {
    Ok(std::fs::symlink_metadata(path)?.file_type().is_symlink())
}

/// Node's `path.resolve` for an absolute path: `.` and `..` removed lexically.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Node's `isSafeRelativePath`.
fn safe_relative(path: &str) -> bool {
    let drive = path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic();
    !path.is_empty()
        && !path.contains(['\\', '\0'])
        && !path.starts_with('/')
        && !drive
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// Node's `resolveSnapshotRoot`: a real directory strictly inside `repos`,
/// reached without links. `root` is the stored locator under `repos` before
/// links resolve, so a linked snapshot directory is refused.
fn snapshot_root(repos: &Path, root: &str) -> Result<PathBuf, ArtifactFailure> {
    let configured = lexical(repos);
    let snapshot = lexical(Path::new(root));
    let Ok(relative) = snapshot.strip_prefix(&configured) else {
        return Err(ArtifactFailure::UnsafePath);
    };
    if relative.as_os_str().is_empty() {
        return Err(ArtifactFailure::UnsafePath);
    }
    let mut cursor = configured.clone();
    for segment in relative.components() {
        cursor.push(segment);
        let meta = std::fs::symlink_metadata(&cursor)?;
        if meta.file_type().is_symlink() {
            return Err(ArtifactFailure::Link);
        }
        if !meta.is_dir() {
            return Err(ArtifactFailure::NotFile);
        }
    }
    let real_repos = configured.canonicalize()?;
    let real_root = cursor.canonicalize()?;
    if real_root == real_repos || !real_root.starts_with(&real_repos) {
        return Err(ArtifactFailure::UnsafePath);
    }
    Ok(real_root)
}

/// Node's `resolveArtifactPath`: each segment matched case-insensitively
/// against the directory, refusing a link or an ambiguous match.
fn artifact_path(repos: &Path, root: &str, relative: &str) -> Result<PathBuf, ArtifactFailure> {
    if !safe_relative(relative) {
        return Err(ArtifactFailure::UnsafePath);
    }
    let mut current = snapshot_root(repos, root)?;
    let segments: Vec<&str> = relative.split('/').collect();
    for (index, segment) in segments.iter().enumerate() {
        let wanted = segment.to_lowercase();
        let mut matches = Vec::new();
        for entry in std::fs::read_dir(&current)? {
            let name = entry?.file_name();
            if name.to_string_lossy().to_lowercase() == wanted {
                matches.push(name);
            }
        }
        match matches.len() {
            0 => return Err(ArtifactFailure::Missing),
            1 => current.push(&matches[0]),
            _ => return Err(ArtifactFailure::AmbiguousCase),
        }
        let meta = std::fs::symlink_metadata(&current)?;
        if meta.file_type().is_symlink() {
            return Err(ArtifactFailure::Link);
        }
        if index + 1 < segments.len() && !meta.is_dir() {
            return Err(ArtifactFailure::NotFile);
        }
    }
    Ok(current)
}

#[cfg(test)]
pub(crate) mod tests;
