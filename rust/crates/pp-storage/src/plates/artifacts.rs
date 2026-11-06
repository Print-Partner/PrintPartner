//! Node's `loadAcceptedArtifactGeometry` (`services/accepted-artifact-geometry.ts`)
//! over `openVerifiedAcceptedArtifact` (`services/accepted-artifacts.ts`): each
//! Required unit's pinned STL is found inside its Source snapshot without
//! following links, checked against its accepted SHA-256, and measured.

use crate::production::geometry::{GeometryLimits, dimensions_um, parse_accepted_stl};
use crate::read_model::graph::Artifact;
use sha2::{Digest as _, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

/// Node's route limits (`WORKSPACE_LIMITS` in `routes/accepted-plates.ts`).
#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub total_source_bytes: u64,
    pub objects: usize,
    pub triangles: usize,
}
pub(super) const LIMITS: Limits = Limits {
    total_source_bytes: 256 * 1024 * 1024,
    objects: 10_000,
    triangles: 5_000_000,
};

/// Why a unit's geometry could not be loaded, with Node's codes.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure {
    Unavailable { token: String, reason: &'static str },
    InvalidStl(String),
    Degenerate(String),
    LimitExceeded(&'static str),
}

/// The width, depth and height of each unit's STL in micrometres by token, or the first failure in unit
/// order. An STL shared by several units is read once, and its triangles
/// count once per unit, as Node counts them.
pub(super) fn load<'a>(
    repos: &Path,
    units: impl ExactSizeIterator<Item = (&'a str, &'a Artifact)>,
    limits: Limits,
) -> Result<HashMap<String, [u64; 3]>, Failure> {
    if units.len() > limits.objects {
        return Err(Failure::LimitExceeded("objects"));
    }
    let mut by_descriptor: HashMap<&Artifact, ([u64; 3], usize)> = HashMap::new();
    let mut by_token = HashMap::new();
    let mut total_bytes = 0u64;
    let mut triangles = 0usize;
    for (token, artifact) in units {
        let unavailable = |reason| Failure::Unavailable {
            token: token.to_owned(),
            reason,
        };
        let (dimensions, count) = match by_descriptor.get(artifact) {
            Some(known) => *known,
            None => {
                let (mut file, size) = open_verified(repos, artifact).map_err(unavailable)?;
                total_bytes += size;
                if total_bytes > limits.total_source_bytes {
                    return Err(Failure::LimitExceeded("total_source_bytes"));
                }
                let mut bytes = Vec::new();
                file.seek(SeekFrom::Start(0))
                    .and_then(|_| file.take(size).read_to_end(&mut bytes))
                    .map_err(|_| unavailable("io_error"))?;
                let unbounded = GeometryLimits {
                    max_bytes: usize::MAX,
                    max_triangles: usize::MAX,
                };
                let mesh = parse_accepted_stl(&bytes, unbounded)
                    .map_err(|_| Failure::InvalidStl(token.to_owned()))?;
                let size =
                    dimensions_um(&mesh).map_err(|_| Failure::Degenerate(token.to_owned()))?;
                let dimensions =
                    [size.width_um, size.depth_um, size.height_um].map(|value| value.get());
                by_descriptor.insert(artifact, (dimensions, mesh.triangles));
                (dimensions, mesh.triangles)
            }
        };
        triangles = triangles.saturating_add(count);
        if triangles > limits.triangles {
            return Err(Failure::LimitExceeded("triangles"));
        }
        by_token.insert(token.to_owned(), dimensions);
    }
    Ok(by_token)
}

/// Node's `fileErrorReason`.
fn reason(error: &io::Error) -> &'static str {
    if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    ) {
        return "missing";
    }
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ELOOP) {
        return "symlink";
    }
    "io_error"
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
fn snapshot_root(repos: &Path, root: &str) -> Result<PathBuf, &'static str> {
    let configured = lexical(repos);
    let snapshot = lexical(Path::new(root));
    let Ok(relative) = snapshot.strip_prefix(&configured) else {
        return Err("unsafe_path");
    };
    if relative.as_os_str().is_empty() {
        return Err("unsafe_path");
    }
    let mut cursor = configured.clone();
    for segment in relative.components() {
        cursor.push(segment);
        let meta = std::fs::symlink_metadata(&cursor).map_err(|error| reason(&error))?;
        if meta.file_type().is_symlink() {
            return Err("symlink");
        }
        if !meta.is_dir() {
            return Err("not_file");
        }
    }
    let real_repos = configured.canonicalize().map_err(|error| reason(&error))?;
    let real_root = cursor.canonicalize().map_err(|error| reason(&error))?;
    if real_root == real_repos || !real_root.starts_with(&real_repos) {
        return Err("unsafe_path");
    }
    Ok(real_root)
}

/// Node's `resolveArtifactPath`: each segment matched case-insensitively
/// against the directory, refusing a link or an ambiguous match.
fn artifact_path(repos: &Path, root: &str, relative: &str) -> Result<PathBuf, &'static str> {
    if !safe_relative(relative) {
        return Err("unsafe_path");
    }
    let mut current = snapshot_root(repos, root)?;
    let segments: Vec<&str> = relative.split('/').collect();
    for (index, segment) in segments.iter().enumerate() {
        let wanted = segment.to_lowercase();
        let mut matches = Vec::new();
        for entry in std::fs::read_dir(&current).map_err(|error| reason(&error))? {
            let name = entry.map_err(|error| reason(&error))?.file_name();
            if name.to_string_lossy().to_lowercase() == wanted {
                matches.push(name);
            }
        }
        match matches.len() {
            0 => return Err("missing"),
            1 => current.push(&matches[0]),
            _ => return Err("ambiguous_case"),
        }
        let meta = std::fs::symlink_metadata(&current).map_err(|error| reason(&error))?;
        if meta.file_type().is_symlink() {
            return Err("symlink");
        }
        if index + 1 < segments.len() && !meta.is_dir() {
            return Err("not_file");
        }
    }
    Ok(current)
}

/// Node's `openVerifiedAcceptedArtifact` without a byte cap: the open file
/// and its hashed length once its SHA-256 matches.
fn open_verified(repos: &Path, artifact: &Artifact) -> Result<(File, u64), &'static str> {
    let (root, relative, expected) = match artifact {
        Artifact::Unavailable { reason } => return Err(reason),
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
        return Err("digest_mismatch");
    }
    let path = artifact_path(repos, root, relative)?;
    let io = |error: io::Error| reason(&error);
    if is_link(&path).map_err(io)? {
        return Err("symlink");
    }
    let before = pp_os::status_nofollow(&path).map_err(io)?;
    let mut file = pp_os::open_read_nofollow(&path).map_err(io)?;
    let opened = pp_os::status_of(&file).map_err(io)?;
    if !opened.is_file() {
        return Err("not_file");
    }
    if before.identity() != opened.identity() {
        return Err("io_error");
    }
    if file.metadata().map_err(io)?.len() == 0 {
        return Err("empty");
    }
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut hashed = 0u64;
    loop {
        let read = file.read(&mut buffer).map_err(io)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
        hashed += read as u64;
    }
    if hashed == 0 {
        return Err("empty");
    }
    if hex::encode(hash.finalize()) != *expected {
        return Err("digest_mismatch");
    }
    Ok((file, hashed))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: &str = "solid box
 facet normal 0 0 0
  outer loop
   vertex 0 0 0
   vertex 10 0 0
   vertex 10 20 5
  endloop
 endfacet
endsolid box
";

    struct Snapshot {
        dir: tempfile::TempDir,
    }

    impl Snapshot {
        fn new(files: &[(&str, &str)]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join("repos/1/revisions/r")).unwrap();
            for (name, text) in files {
                std::fs::write(dir.path().join("repos/1/revisions/r").join(name), text).unwrap();
            }
            Self { dir }
        }
        fn repos(&self) -> PathBuf {
            self.dir.path().join("repos")
        }
        fn artifact(&self, relative: &str, text: &str) -> Artifact {
            let root = self
                .repos()
                .join("1/revisions/r")
                .to_string_lossy()
                .into_owned();
            Artifact::Tracked {
                source_id: 1,
                source_revision_id: 1,
                snapshot_root: root.clone(),
                snapshot_path: root,
                relative_path: relative.into(),
                expected_sha256: hex::encode(Sha256::digest(text.as_bytes())),
            }
        }
        fn load(
            &self,
            units: &[(&str, &Artifact)],
            limits: Limits,
        ) -> Result<HashMap<String, [u64; 3]>, Failure> {
            load(&self.repos(), units.iter().copied(), limits)
        }
    }

    fn unavailable(reason: &'static str) -> Result<HashMap<String, [u64; 3]>, Failure> {
        Err(Failure::Unavailable {
            token: "t".into(),
            reason,
        })
    }

    #[test]
    fn measures_a_pinned_stl() {
        let snapshot = Snapshot::new(&[("box.stl", BOX)]);
        let artifact = snapshot.artifact("BOX.stl", BOX);
        let sizes = snapshot.load(&[("t", &artifact)], LIMITS).unwrap();
        assert_eq!(sizes["t"], [10_000, 20_000, 5_000]);
    }

    #[test]
    fn limits_count_objects_bytes_and_repeated_triangles() {
        let snapshot = Snapshot::new(&[("box.stl", BOX)]);
        let artifact = snapshot.artifact("box.stl", BOX);
        let units = [("t", &artifact), ("u", &artifact)];
        let limit = |objects, total_source_bytes, triangles| Limits {
            total_source_bytes,
            objects,
            triangles,
        };
        let bytes = BOX.len() as u64;
        assert_eq!(
            snapshot.load(&units, limit(1, bytes, 2)),
            Err(Failure::LimitExceeded("objects"))
        );
        assert_eq!(
            snapshot.load(&units, limit(2, bytes - 1, 2)),
            Err(Failure::LimitExceeded("total_source_bytes"))
        );
        assert_eq!(
            snapshot.load(&units, limit(2, bytes, 1)),
            Err(Failure::LimitExceeded("triangles"))
        );
        assert_eq!(snapshot.load(&units, limit(2, bytes, 2)).unwrap().len(), 2);
    }

    #[test]
    fn refuses_unsafe_missing_and_mismatched_files() {
        let snapshot = Snapshot::new(&[("box.stl", BOX), ("empty.stl", "")]);
        let check = |artifact: &Artifact| snapshot.load(&[("t", artifact)], LIMITS);
        assert_eq!(
            check(&snapshot.artifact("../box.stl", BOX)),
            unavailable("unsafe_path")
        );
        assert_eq!(
            check(&snapshot.artifact("gone.stl", BOX)),
            unavailable("missing")
        );
        assert_eq!(
            check(&snapshot.artifact("empty.stl", "")),
            unavailable("empty")
        );
        assert_eq!(
            check(&snapshot.artifact("box.stl", "other")),
            unavailable("digest_mismatch")
        );
        let outside = Artifact::Tracked {
            source_id: 1,
            source_revision_id: 1,
            snapshot_root: snapshot.dir.path().to_string_lossy().into_owned(),
            snapshot_path: snapshot.dir.path().to_string_lossy().into_owned(),
            relative_path: "box.stl".into(),
            expected_sha256: "0".repeat(64),
        };
        assert_eq!(check(&outside), unavailable("unsafe_path"));
        let legacy = Artifact::Unavailable { reason: "legacy" };
        assert_eq!(check(&legacy), unavailable("legacy"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_linked_snapshot_directory() {
        let snapshot = Snapshot::new(&[("box.stl", BOX)]);
        let link = snapshot.repos().join("1/revisions/link");
        std::os::unix::fs::symlink("r", &link).unwrap();
        let mut artifact = snapshot.artifact("box.stl", BOX);
        if let Artifact::Tracked { snapshot_path, .. } = &mut artifact {
            *snapshot_path = link.to_string_lossy().into_owned();
        }
        assert_eq!(
            snapshot.load(&[("t", &artifact)], LIMITS),
            unavailable("symlink")
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_linked_file() {
        let snapshot = Snapshot::new(&[("box.stl", BOX)]);
        std::os::unix::fs::symlink("box.stl", snapshot.repos().join("1/revisions/r/link.stl"))
            .unwrap();
        let artifact = snapshot.artifact("link.stl", BOX);
        assert_eq!(
            snapshot.load(&[("t", &artifact)], LIMITS),
            unavailable("symlink")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refuses_an_ambiguous_case() {
        let snapshot = Snapshot::new(&[("box.stl", BOX), ("BOX.stl", BOX)]);
        let artifact = snapshot.artifact("box.stl", BOX);
        assert_eq!(
            snapshot.load(&[("t", &artifact)], LIMITS),
            unavailable("ambiguous_case")
        );
    }
}
