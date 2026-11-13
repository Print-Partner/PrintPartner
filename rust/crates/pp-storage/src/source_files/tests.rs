use super::*;
use crate::{
    Limits,
    auth::{FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    write::test_support::{first_session, opened},
};

const WAIT: Duration = Duration::from_secs(5);
/// The snapshot writer's root entry: the value of `pp_source::MANIFEST`,
/// which `pp-core` passes in production. `pp-storage` may not depend on
/// `pp-source`, not even for tests (`check-boundaries.py`).
const WRITER_ENTRY: &str = ".printpartner-source-snapshot.json";
static IDLE: AtomicBool = AtomicBool::new(false);
const POLICY: AuthPolicy = AuthPolicy {
    registration: RegistrationPolicy::FirstAccountOnly,
    first_user: FirstUserTenant::NewUser,
    session_tenant: SessionTenantPolicy::SingleAccountDefault,
};

struct Fixture {
    directory: tempfile::TempDir,
    owner: WriterOwner,
    client: SourceFilesClient,
    token: String,
}

impl Fixture {
    /// Source 1 "Kit" with no revision.
    fn new() -> Self {
        let (directory, owner) = opened(Limits::default());
        let token = first_session(&owner);
        let client = owner.source_files(POLICY).unwrap();
        let fixture = Self {
            client: client.skipping_at_root(WRITER_ENTRY),
            token,
            owner,
            directory,
        };
        fixture.sql("INSERT INTO projects(id,tenant_id,name,url,source_kind) VALUES(1,'default','Kit','','local')");
        fixture
    }

    fn sql(&self, sql: &'static str) {
        let done = self
            .owner
            .client
            .shared
            .write("test", &IDLE, WAIT, |tx, _| tx.execute_batch(sql));
        done.unwrap().unwrap();
    }

    /// Writes `files` (path, contents) under `repos/<locator>`.
    fn snapshot(&self, locator: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let root = self.directory.path().join("repos").join(locator);
        for (path, bytes) in files {
            let file = root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, bytes).unwrap();
        }
        root
    }

    fn list(&self, source_id: i64) -> Result<SourceFiles, SourceFilesError> {
        let session = Credential::Session(Secret::new(self.token.clone()));
        self.client.list(session, source_id, &IDLE, WAIT)
    }
}

fn file(path: &str, kind: FileKind, size_bytes: u64) -> SourceFile {
    SourceFile {
        path: path.into(),
        kind,
        size_bytes,
    }
}

const FIRST: &str = "INSERT INTO source_revisions(id,tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at) VALUES(10,'default',1,'r1','d1','1/revisions/r1','2026-11-12T00:00:00.000Z'); UPDATE projects SET current_source_revision_id=10 WHERE id=1";

#[test]
fn lists_the_current_revisions_files_sorted_by_path() {
    let fixture = Fixture::new();
    let root = fixture.snapshot(
        "1/revisions/r1",
        &[
            ("stl/Body.STL", b"solid body"),
            ("README.md", b"# Kit"),
            ("stl/arms/arm.stl", b"arm"),
            ("photos/front.jpg", b"jpeg"),
            ("cad/body.step", b"step"),
            ("LICENSE", b"MIT"),
        ],
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("LICENSE"), root.join("link.stl")).unwrap();
    fixture.sql(FIRST);

    let listed = fixture.list(1).unwrap();

    assert_eq!(listed.source_id, 1);
    assert_eq!(listed.revision_id, Some(10));
    assert_eq!(
        listed.files,
        [
            file("LICENSE", FileKind::Other, 3),
            file("README.md", FileKind::Document, 5),
            file("cad/body.step", FileKind::Model, 4),
            file("photos/front.jpg", FileKind::Image, 4),
            file("stl/Body.STL", FileKind::Stl, 10),
            file("stl/arms/arm.stl", FileKind::Stl, 3),
        ]
    );
}

#[test]
fn a_source_without_a_revision_lists_nothing_and_an_unknown_one_is_not_found() {
    let fixture = Fixture::new();
    let empty = fixture.list(1).unwrap();
    assert_eq!(
        empty,
        SourceFiles {
            source_id: 1,
            revision_id: None,
            files: Vec::new(),
        }
    );
    assert!(matches!(fixture.list(2), Err(SourceFilesError::NotFound)));
}

#[test]
fn the_next_read_after_activation_lists_the_new_revision() {
    let fixture = Fixture::new();
    fixture.snapshot("1/revisions/r1", &[("old.stl", b"old")]);
    fixture.snapshot("1/revisions/r2", &[("new.stl", b"newer")]);
    fixture.sql(FIRST);
    assert_eq!(
        fixture.list(1).unwrap().files,
        [file("old.stl", FileKind::Stl, 3)]
    );

    fixture.sql("INSERT INTO source_revisions(id,tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at) VALUES(11,'default',1,'r2','d2','1/revisions/r2','2026-11-13T00:00:00.000Z'); UPDATE projects SET current_source_revision_id=11 WHERE id=1");

    let listed = fixture.list(1).unwrap();
    assert_eq!(listed.revision_id, Some(11));
    assert_eq!(listed.files, [file("new.stl", FileKind::Stl, 5)]);
}

#[test]
fn a_missing_or_foreign_snapshot_is_unavailable() {
    let fixture = Fixture::new();
    // The row exists but its directory does not.
    fixture.sql(FIRST);
    assert!(matches!(
        fixture.list(1),
        Err(SourceFilesError::SnapshotUnavailable)
    ));

    // A locator under another Source's workspace.
    fixture.snapshot("1/revisions/r1", &[("a.stl", b"a")]);
    fixture.snapshot("2/revisions/x", &[("b.stl", b"b")]);
    fixture.sql("INSERT INTO source_revisions(id,tenant_id,project_id,upstream_revision_key,manifest_digest,snapshot_locator,synced_at) VALUES(12,'default',1,'x','d','2/revisions/x','2026-11-13T00:00:00.000Z'); UPDATE projects SET current_source_revision_id=12 WHERE id=1");
    assert!(matches!(
        fixture.list(1),
        Err(SourceFilesError::SnapshotUnavailable)
    ));
}

#[test]
fn a_refused_credential_reads_nothing() {
    let fixture = Fixture::new();
    let stranger = Credential::Session(Secret::new("not-a-session".into()));
    let refused = fixture.client.list(stranger, 1, &IDLE, WAIT);
    assert!(matches!(
        refused,
        Err(SourceFilesError::Writer(WriterFailure::Auth(_)))
    ));
}

#[test]
fn kinds_follow_the_extension_whatever_its_case() {
    for (name, kind) in [
        ("a.stl", FileKind::Stl),
        ("a.StL", FileKind::Stl),
        ("a.3mf", FileKind::Model),
        ("a.scad", FileKind::Model),
        ("a.pdf", FileKind::Document),
        ("a.txt", FileKind::Document),
        ("a.PNG", FileKind::Image),
        ("stl", FileKind::Other),
        ("a.stl.bak", FileKind::Other),
    ] {
        assert_eq!(FileKind::of(name), kind, "{name}");
    }
}

#[test]
fn the_snapshot_writers_manifest_is_not_one_of_the_owners_files() {
    let fixture = Fixture::new();
    let owner_files: [(&str, &[u8]); 3] = [
        ("README.md", b"# Kit"),
        ("stl/body.stl", b"solid body"),
        // Only the root entry is the writer's; the same name deeper down is
        // an owner's file.
        ("docs/.printpartner-source-snapshot.json", b"{}"),
    ];
    let root = fixture.snapshot("1/revisions/r1", &owner_files);
    fs::write(root.join(WRITER_ENTRY), b"{\"schema\":1}").unwrap();
    fixture.sql(FIRST);

    let listed = fixture.list(1).unwrap().files;

    assert_eq!(listed.len(), owner_files.len());
    assert!(!listed.iter().any(|file| file.path == WRITER_ENTRY));
    let mut paths: Vec<&str> = owner_files.iter().map(|(path, _)| *path).collect();
    paths.sort_unstable();
    let listed_paths: Vec<&str> = listed.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(listed_paths, paths);
}

/// One triangle, binary: 80-byte header, count, normal, three vertices, attribute.
fn one_triangle_binary() -> Vec<u8> {
    let mut bytes = vec![0u8; 80];
    bytes.extend(1u32.to_le_bytes());
    bytes.extend([0u8; 12]);
    for value in [0.0f32, 0.0, 0.0, 10.0, 0.0, 0.0, 0.0, 10.0, 5.0] {
        bytes.extend(value.to_le_bytes());
    }
    bytes.extend([0u8; 2]);
    bytes
}

const ONE_TRIANGLE_ASCII: &[u8] = b"solid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 10 0 0\nvertex 0 10 5\nendloop\nendfacet\nendsolid t";

impl Fixture {
    fn thumbnail(&self, path: &str) -> Result<SourceThumbnail, SourceFilesError> {
        let session = Credential::Session(Secret::new(self.token.clone()));
        self.client.thumbnail(session, 1, path, &IDLE, WAIT)
    }

    /// Every file under `thumbs/`, relative to it.
    fn cached(&self) -> Vec<String> {
        let thumbs = self.directory.path().join("thumbs");
        let mut found = Vec::new();
        let mut pending = vec![thumbs.clone()];
        while let Some(folder) = pending.pop() {
            for entry in fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.push(path.strip_prefix(&thumbs).unwrap().display().to_string());
                }
            }
        }
        found
    }
}

/// Width and height from a PNG's header.
fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let word = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    (word(16), word(20))
}

#[test]
fn a_thumbnail_is_rendered_once_then_served_from_thumbs_sources() {
    let fixture = Fixture::new();
    let root = fixture.snapshot(
        "1/revisions/r1",
        &[
            ("stl/part.stl", &one_triangle_binary()),
            ("text.stl", ONE_TRIANGLE_ASCII),
        ],
    );
    fixture.sql(FIRST);
    assert!(fixture.cached().is_empty());

    let first = fixture.thumbnail("stl/part.stl").unwrap();
    assert_eq!(png_size(&first.png), (256, 256));
    assert_eq!(first.revision_id, 10);
    let cached = fixture.cached();
    assert_eq!(cached.len(), 1, "{cached:?}");
    assert!(cached[0].starts_with("sources/10/"), "{cached:?}");

    // A hit never opens the STL: the corrupted file still answers the cached bytes.
    fs::write(root.join("stl/part.stl"), b"no longer an stl").unwrap();
    let second = fixture.thumbnail("stl/part.stl").unwrap();
    assert_eq!(second, first);

    let ascii = fixture.thumbnail("text.stl").unwrap();
    assert_eq!(png_size(&ascii.png), (256, 256));
    assert_ne!(ascii.tag, first.tag);
    assert_eq!(fixture.cached().len(), 2);
}

#[test]
fn only_a_real_stl_of_the_current_revision_has_a_thumbnail() {
    let fixture = Fixture::new();
    let not_found = |result| matches!(result, Err(SourceFilesError::FileNotFound));
    assert!(not_found(fixture.thumbnail("part.stl")), "no revision yet");
    let root = fixture.snapshot(
        "1/revisions/r1",
        &[
            ("part.stl", &one_triangle_binary()),
            ("README.md", b"# Kit"),
            ("dir.stl/x", b""),
        ],
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("part.stl"), root.join("link.stl")).unwrap();
    fixture.sql(FIRST);
    for path in [
        "missing.stl",
        "README.md",
        "dir.stl",
        "link.stl",
        WRITER_ENTRY,
    ] {
        assert!(not_found(fixture.thumbnail(path)), "{path}");
    }
    let session = Credential::Session(Secret::new(fixture.token.clone()));
    let unknown = fixture
        .client
        .thumbnail(session, 2, "part.stl", &IDLE, WAIT);
    assert!(matches!(unknown, Err(SourceFilesError::NotFound)));
    assert!(fixture.cached().is_empty());
}

#[test]
fn an_unsafe_path_is_refused_before_any_read_or_write() {
    let fixture = Fixture::new();
    let outside = fixture.directory.path().join("repos/1/x.stl");
    fixture.snapshot("1/revisions/r1", &[("part.stl", &one_triangle_binary())]);
    fs::write(&outside, one_triangle_binary()).unwrap();
    fixture.sql(FIRST);
    // A credential that would be refused: the path check comes first, so the
    // answer is "not found", not an authentication failure.
    let refused = || Credential::Session(Secret::new("not-a-session".into()));
    for path in [
        "../x.stl",
        "/etc/x.stl",
        "a/../x.stl",
        "./part.stl",
        "C:/x.stl",
        "a\\x.stl",
        "",
    ] {
        let answer = fixture.client.thumbnail(refused(), 1, path, &IDLE, WAIT);
        assert!(
            matches!(answer, Err(SourceFilesError::FileNotFound)),
            "{path}"
        );
    }
    assert!(fixture.cached().is_empty());
}

#[test]
fn a_corrupt_stl_is_unrenderable_and_nothing_is_cached() {
    let fixture = Fixture::new();
    fixture.snapshot(
        "1/revisions/r1",
        &[("broken.stl", b"solid broken\nendsolid broken")],
    );
    fixture.sql(FIRST);
    assert!(matches!(
        fixture.thumbnail("broken.stl"),
        Err(SourceFilesError::Unrenderable)
    ));
    assert!(fixture.cached().is_empty());
}
