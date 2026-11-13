use super::*;
use crate::{
    Limits,
    auth::{FirstUserTenant, RegistrationPolicy, Secret, SessionTenantPolicy},
    write::test_support::{first_session, opened},
};

const WAIT: Duration = Duration::from_secs(5);
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
            client: client.skipping_at_root(pp_source::MANIFEST),
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
    fs::write(root.join(pp_source::MANIFEST), b"{\"schema\":1}").unwrap();
    fixture.sql(FIRST);

    let listed = fixture.list(1).unwrap().files;

    assert_eq!(listed.len(), owner_files.len());
    assert!(!listed.iter().any(|file| file.path == pp_source::MANIFEST));
    let mut paths: Vec<&str> = owner_files.iter().map(|(path, _)| *path).collect();
    paths.sort_unstable();
    let listed_paths: Vec<&str> = listed.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(listed_paths, paths);
}
