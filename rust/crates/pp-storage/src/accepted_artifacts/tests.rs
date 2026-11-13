use super::*;
use std::io::{Seek, SeekFrom};

/// A Source snapshot at `repos/1/revisions/r` in a temporary directory.
pub(crate) struct Snapshot {
    pub(crate) dir: tempfile::TempDir,
}

impl Snapshot {
    pub(crate) fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("repos/1/revisions/r")).unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join("repos/1/revisions/r").join(name), text).unwrap();
        }
        Self { dir }
    }

    pub(crate) fn repos(&self) -> PathBuf {
        self.dir.path().join("repos")
    }

    /// The accepted record of `relative`, pinned to the SHA-256 of `text`.
    pub(crate) fn artifact(&self, relative: &str, text: &str) -> Artifact {
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

    fn open(&self, artifact: &Artifact) -> Result<(File, u64), ArtifactFailure> {
        open_verified(&self.repos(), artifact)
    }
}

/// The failure's code, or `ok` when the file opened.
fn code(result: Result<(File, u64), ArtifactFailure>) -> &'static str {
    result.map_or_else(|failure| failure.code(), |_| "ok")
}

#[test]
fn opens_the_accepted_bytes_and_their_length() {
    let snapshot = Snapshot::new(&[("box.stl", "solid box")]);
    let (mut file, length) = snapshot
        .open(&snapshot.artifact("BOX.stl", "solid box"))
        .unwrap();
    let mut bytes = String::new();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.read_to_string(&mut bytes).unwrap();
    assert_eq!((bytes.as_str(), length), ("solid box", 9));
}

#[test]
fn unavailable_keeps_the_plans_reason() {
    let snapshot = Snapshot::new(&[]);
    let legacy = Artifact::Unavailable { reason: "legacy" };
    assert!(matches!(
        snapshot.open(&legacy),
        Err(ArtifactFailure::Unavailable("legacy"))
    ));
}

#[test]
fn unsafe_path_refuses_leaving_the_snapshot() {
    let snapshot = Snapshot::new(&[("box.stl", "solid box")]);
    let escaping = snapshot.artifact("../box.stl", "solid box");
    assert!(matches!(
        snapshot.open(&escaping),
        Err(ArtifactFailure::UnsafePath)
    ));
    let root = snapshot.dir.path().to_string_lossy().into_owned();
    let outside = Artifact::Tracked {
        source_id: 1,
        source_revision_id: 1,
        snapshot_root: root.clone(),
        snapshot_path: root,
        relative_path: "box.stl".into(),
        expected_sha256: "0".repeat(64),
    };
    assert!(matches!(
        snapshot.open(&outside),
        Err(ArtifactFailure::UnsafePath)
    ));
}

#[test]
fn missing_empty_and_mismatched_files_are_refused() {
    let snapshot = Snapshot::new(&[("box.stl", "solid box"), ("empty.stl", "")]);
    let open = |relative, text| code(snapshot.open(&snapshot.artifact(relative, text)));
    assert_eq!(open("gone.stl", "solid box"), "missing");
    assert_eq!(open("empty.stl", ""), "empty");
    assert_eq!(open("box.stl", "other"), "digest_mismatch");
    assert!(matches!(
        snapshot.open(&snapshot.artifact("gone.stl", "solid box")),
        Err(ArtifactFailure::Missing)
    ));
    assert!(matches!(
        snapshot.open(&snapshot.artifact("empty.stl", "")),
        Err(ArtifactFailure::Empty)
    ));
    assert!(matches!(
        snapshot.open(&snapshot.artifact("box.stl", "other")),
        Err(ArtifactFailure::DigestMismatch)
    ));
}

#[test]
fn not_file_refuses_a_directory_in_the_files_place() {
    let snapshot = Snapshot::new(&[]);
    std::fs::create_dir(snapshot.repos().join("1/revisions/r/dir.stl")).unwrap();
    let artifact = snapshot.artifact("dir.stl", "");
    assert!(matches!(
        snapshot.open(&artifact),
        Err(ArtifactFailure::NotFile)
    ));
}

#[cfg(unix)]
#[test]
fn link_refuses_a_linked_file_and_a_linked_snapshot() {
    let snapshot = Snapshot::new(&[("box.stl", "solid box")]);
    let revisions = snapshot.repos().join("1/revisions");
    std::os::unix::fs::symlink("box.stl", revisions.join("r/link.stl")).unwrap();
    let linked_file = snapshot.artifact("link.stl", "solid box");
    assert!(matches!(
        snapshot.open(&linked_file),
        Err(ArtifactFailure::Link)
    ));
    std::os::unix::fs::symlink("r", revisions.join("link")).unwrap();
    let mut linked_root = snapshot.artifact("box.stl", "solid box");
    if let Artifact::Tracked { snapshot_path, .. } = &mut linked_root {
        *snapshot_path = revisions.join("link").to_string_lossy().into_owned();
    }
    assert!(matches!(
        snapshot.open(&linked_root),
        Err(ArtifactFailure::Link)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn ambiguous_case_refuses_two_matching_names() {
    let snapshot = Snapshot::new(&[("box.stl", "solid box"), ("BOX.stl", "solid box")]);
    let artifact = snapshot.artifact("box.stl", "solid box");
    assert!(matches!(
        snapshot.open(&artifact),
        Err(ArtifactFailure::AmbiguousCase)
    ));
}

#[cfg(unix)]
#[test]
fn read_reports_a_file_it_cannot_open() {
    use std::os::unix::fs::PermissionsExt;
    let snapshot = Snapshot::new(&[("box.stl", "solid box")]);
    let path = snapshot.repos().join("1/revisions/r/box.stl");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root still reads a mode-000 file, so there is nothing to refuse.
    if File::open(&path).is_ok() {
        return;
    }
    let result = snapshot.open(&snapshot.artifact("box.stl", "solid box"));
    assert!(
        matches!(&result, Err(ArtifactFailure::Read(error)) if error.kind() == io::ErrorKind::PermissionDenied),
        "{result:?}"
    );
    assert_eq!(code(result), "io_error");
}
