use pp_source::{
    ArtifactBudget, FileKind, LocalFiles, Publication, SelectedFile, Selection, SnapshotRequest,
    SourcePath, TenantRepos,
};
use std::{fs, path::PathBuf};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;
#[path = "support/pinned_repos.rs"]
mod pinned_repos;

/// Removed when the scratch drops, including while a test unwinds from a panic.
struct Fixture(Scratch);
impl Fixture {
    fn new() -> Self {
        let path = Scratch::new("pp-source-");
        fs::create_dir(path.join("repos")).unwrap();
        fs::create_dir(path.join("input")).unwrap();
        fs::write(path.join("input/part.stl"), b"solid bytes").unwrap();
        // Pin modes so root ancestor checks do not depend on the caller's umask.
        for dir in [path.to_path_buf(), path.join("repos"), path.join("input")] {
            modes::pin_mode(dir, 0o755);
        }
        Self(path)
    }
    fn repos(&self) -> TenantRepos {
        TenantRepos::open("tenant-fixture".into(), &self.0.join("repos")).unwrap()
    }
}
/// Windows leaves the Source lock file in the revisions directory.
fn revisions_empty(fixture: &Fixture) -> bool {
    fs::read_dir(fixture.0.join("repos/42/revisions"))
        .unwrap()
        .all(|entry| entry.unwrap().file_name() == pp_os::DIR_LOCK_FILE)
}
fn request() -> SnapshotRequest {
    SnapshotRequest {
        upstream_revision_key: "caller-uuid".into(),
        files: vec![SelectedFile {
            path: SourcePath::try_from("part.stl".to_owned()).unwrap(),
            kind: FileKind::Stl,
            size_hint_bytes: Some(999),
        }],
        selection: Selection {
            max_stl_files: 500,
            max_documentation_bytes: 1000,
            omitted_files: vec![],
        },
    }
}
fn budget() -> ArtifactBudget {
    ArtifactBudget::new(1024 * 1024, 1024 * 1024).unwrap()
}

#[test]
fn configured_repository_roots_reject_non_normalized_spellings() {
    let fixture = Fixture::new();
    for spelling in [
        format!("{}/./repos", fixture.0.display()),
        format!("{}//repos", fixture.0.display()),
        format!("{}/repos/", fixture.0.display()),
        format!("{}/repos/.", fixture.0.display()),
        format!("/{}/repos", fixture.0.display()),
        format!("{}/missing/./repos", fixture.0.display()),
    ] {
        let result = TenantRepos::open("tenant-fixture".into(), std::path::Path::new(&spelling));
        assert!(
            matches!(result, Err(pp_source::Error::UnsafePath)),
            "configured root must reject {spelling:?} as unsafe-path"
        );
    }
}

#[test]
fn configured_repository_roots_accept_normalized_utf8_and_filesystem_root() {
    #[cfg(unix)]
    TenantRepos::open("tenant-fixture".into(), std::path::Path::new("/")).unwrap();
    let fixture = Fixture::new();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    for name in ["repos", "répos"] {
        let root = fixture.0.join(name);
        if name != "repos" {
            fs::create_dir(&root).unwrap();
            modes::pin_mode(&root, 0o755);
        }
        let repos = TenantRepos::open("tenant-fixture".into(), &root).unwrap();
        let mut source = repos.source(42).unwrap();
        assert_eq!(
            source
                .materialize(request(), &input, budget())
                .unwrap()
                .publication,
            Publication::Created
        );
        assert_eq!(
            fs::read(root.join("42/revisions/caller-uuid/part.stl")).unwrap(),
            b"solid bytes"
        );
    }
}

#[cfg(unix)]
#[test]
fn searchable_ancestor_without_read_access_allows_publication_and_reuse() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    struct RestorePermissions<'a>(&'a std::path::Path, fs::Permissions);
    impl Drop for RestorePermissions<'_> {
        fn drop(&mut self) {
            fs::set_permissions(self.0, self.1.clone()).unwrap();
        }
    }
    let _restore = RestorePermissions(&fixture.0, fs::metadata(&fixture.0).unwrap().permissions());
    fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o111)).unwrap();
    // Root and processes with DAC overrides cannot exercise this restriction.
    if fs::read_dir(&fixture.0).is_ok() {
        eprintln!("Skipping permission restriction with privileged filesystem access");
        return;
    }
    assert_eq!(
        fs::read_dir(&fixture.0).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut source = fixture.repos().source(42).unwrap();
    assert_eq!(
        source
            .materialize(request(), &input, budget())
            .unwrap()
            .publication,
        Publication::Created
    );
    assert_eq!(
        source
            .materialize(request(), &input, budget())
            .unwrap()
            .publication,
        Publication::Reused
    );
    assert_eq!(
        fs::read(fixture.0.join("repos/42/revisions/caller-uuid/part.stl")).unwrap(),
        b"solid bytes"
    );
}

#[cfg(unix)]
#[test]
fn root_walk_rejects_symlink_ancestors_and_selected_roots() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let alias = fixture.0.with_extension("alias");
    symlink(&fixture.0, &alias).unwrap();
    let repos = TenantRepos::open("tenant-fixture".into(), &alias.join("repos"));
    let input = LocalFiles::open(&alias.join("input"));
    fs::remove_file(&alias).unwrap();
    assert!(repos.is_err());
    assert!(input.is_err());

    symlink(fixture.0.join("repos"), fixture.0.join("repos-alias")).unwrap();
    symlink(fixture.0.join("input"), fixture.0.join("input-alias")).unwrap();
    assert!(TenantRepos::open("tenant-fixture".into(), &fixture.0.join("repos-alias")).is_err());
    assert!(LocalFiles::open(&fixture.0.join("input-alias")).is_err());
}

#[test]
fn source_handle_survives_repos_path_replacement_without_following_it() {
    let fixture = Fixture::new();
    let mut source = fixture.repos().source(42).unwrap();
    let pinned = pinned_repos::move_repos_aside(&fixture.0, "original");
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let receipt = source.materialize(request(), &input, budget()).unwrap();
    assert_eq!(receipt.publication, Publication::Created);
    assert!(
        fixture
            .0
            .join(pinned)
            .join("42/revisions/caller-uuid/part.stl")
            .is_file()
    );
    if pinned == "original" {
        assert!(
            fs::read_dir(fixture.0.join("repos"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}

#[test]
fn input_handle_survives_input_path_replacement() {
    let fixture = Fixture::new();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    fs::rename(fixture.0.join("input"), fixture.0.join("old-input")).unwrap();
    fs::create_dir(fixture.0.join("input")).unwrap();
    fs::write(fixture.0.join("input/part.stl"), b"replacement bytes").unwrap();
    fixture
        .repos()
        .source(42)
        .unwrap()
        .materialize(request(), &input, budget())
        .unwrap();
    assert_eq!(
        fs::read(fixture.0.join("repos/42/revisions/caller-uuid/part.stl")).unwrap(),
        b"solid bytes"
    );
}

#[test]
fn zero_source_and_unsafe_revision_do_not_publish() {
    let fixture = Fixture::new();
    assert!(fixture.repos().source(0).is_err());
    assert!(fixture.repos().source(9_007_199_254_740_992).is_err());
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut invalid = request();
    invalid.upstream_revision_key = "../other".into();
    assert!(
        fixture
            .repos()
            .source(42)
            .unwrap()
            .materialize(invalid, &input, budget())
            .is_err()
    );
    assert!(revisions_empty(&fixture));
}

#[test]
fn changed_policy_and_bytes_reuse_original_publication() {
    let fixture = Fixture::new();
    let mut source = fixture.repos().source(42).unwrap();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let first = source.materialize(request(), &input, budget()).unwrap();
    fs::write(fixture.0.join("input/part.stl"), b"changed").unwrap();
    let mut next = request();
    next.selection.max_stl_files = 0;
    let second = source
        .materialize(next, &input, ArtifactBudget::new(0, 0).unwrap())
        .unwrap();
    assert_eq!(second.publication, Publication::Reused);
    assert_eq!(second.manifest_digest, first.manifest_digest);
    assert_eq!(second.selection, first.selection);
}

#[test]
fn source_paths_require_nfc_for_files_and_directories() {
    for (nfc, nfd) in [
        ("café.stl", "cafe\u{301}.stl"),
        ("pièces/part.stl", "pie\u{300}ces/part.stl"),
    ] {
        assert_eq!(SourcePath::try_from(nfc.to_owned()).unwrap().as_str(), nfc);
        assert_eq!(
            SourcePath::try_from(nfd.to_owned()).unwrap_err().category(),
            "unsafe-path"
        );
        // Deserialization must enforce the same boundary as direct construction.
        assert!(serde_json::from_value::<SourcePath>(serde_json::json!(nfd)).is_err());
    }
}

#[test]
fn filename_case_collisions_are_rejected_before_copying_in_either_order() {
    // Request validation runs before filesystem access, including on volumes
    // that cannot represent both spellings as separate files.
    for paths in [["Part.stl", "part.stl"], ["CAFÉ.stl", "café.stl"]] {
        for reverse in [false, true] {
            let fixture = Fixture::new();
            let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
            let mut selected = request();
            selected.files = paths
                .into_iter()
                .map(|path| SelectedFile {
                    path: SourcePath::try_from(path.to_owned()).unwrap(),
                    kind: FileKind::Stl,
                    size_hint_bytes: None,
                })
                .collect();
            if reverse {
                selected.files.reverse();
            }
            let error = fixture
                .repos()
                .source(42)
                .unwrap()
                .materialize(selected, &input, budget())
                .unwrap_err();
            assert_eq!(error.category(), "duplicate-path");
            assert!(revisions_empty(&fixture));
        }
    }
}

#[test]
fn conflicting_directory_case_is_rejected_before_copying() {
    let fixture = Fixture::new();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut selected = request();
    selected.files = ["Part.stl", "part.stl/child.stl"]
        .into_iter()
        .map(|path| SelectedFile {
            path: SourcePath::try_from(path.to_owned()).unwrap(),
            kind: FileKind::Stl,
            size_hint_bytes: None,
        })
        .collect();
    let error = fixture
        .repos()
        .source(42)
        .unwrap()
        .materialize(selected, &input, budget())
        .unwrap_err();
    assert_eq!(error.category(), "duplicate-path");
    assert!(revisions_empty(&fixture));
}

#[test]
fn max_flat_snapshot_publishes_and_retries_cleanly() {
    let fixture = Fixture::new();
    let input_dir = fixture.0.join("input");
    let mut files = Vec::new();
    for i in 0..pp_source::MAX_ENTRIES {
        let name = format!("f{i:05}.stl");
        fs::write(input_dir.join(&name), b"x").unwrap();
        files.push(SelectedFile {
            path: SourcePath::try_from(name).unwrap(),
            kind: FileKind::Stl,
            size_hint_bytes: Some(1),
        });
    }
    let mut req = request();
    req.files = files;
    req.selection.max_stl_files = pp_source::MAX_ENTRIES as u64;
    let budget = ArtifactBudget::new(16 * 1024 * 1024, 16 * 1024 * 1024).unwrap();
    let input = LocalFiles::open(&input_dir).unwrap();
    let mut source = fixture.repos().source(42).unwrap();
    let first = source.materialize(req.clone(), &input, budget).unwrap();
    assert_eq!(first.publication, Publication::Created);
    assert!(
        !fixture
            .0
            .join("repos/42/revisions/.pp-source-candidate")
            .exists()
    );
    let second = source.materialize(req, &input, budget).unwrap();
    assert_eq!(second.publication, Publication::Reused);
}

/// `PATH_MAX` and these `/`-joined byte counts are Unix rules.
#[cfg(unix)]
#[test]
fn published_paths_stop_one_byte_short_of_path_max() {
    let path_max = if cfg!(target_os = "macos") {
        1024
    } else {
        4096
    };
    let fixture = Fixture::new();
    let root = fixture.0.join("répos");
    fs::rename(fixture.0.join("repos"), &root).unwrap();
    // Revision keys "fits" and "fits2" give the same file stored paths one
    // byte apart.
    let prefix = root.join("42/revisions/fits/");
    let mut label = String::new();
    while path_max - 1 - prefix.as_os_str().len() - label.len() > 250 {
        label.push_str(&"d".repeat(99));
        label.push('/');
    }
    let file = path_max - 1 - prefix.as_os_str().len() - label.len();
    label.push_str(&format!("{}.stl", "f".repeat(file - 4)));
    assert_eq!(prefix.as_os_str().len() + label.len(), path_max - 1);
    let input = fixture.0.join("input").join(&label);
    fs::create_dir_all(input.parent().unwrap()).unwrap();
    fs::write(&input, b"solid long").unwrap();
    let mut req = request();
    req.files[0].path = SourcePath::try_from(label.clone()).unwrap();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let repos = TenantRepos::open("tenant-fixture".into(), &root).unwrap();
    let mut source = repos.source(42).unwrap();

    req.upstream_revision_key = "fits2".into();
    let error = source
        .materialize(req.clone(), &input, budget())
        .unwrap_err();
    assert_eq!(error.category(), "stored-path-too-long");
    assert_eq!(
        error.to_string(),
        format!(
            "stored-path-too-long: {label} would be stored at an absolute path of {} bytes; \
             this system opens paths of at most {} bytes; \
             use a shorter repository or folder path and retry the import",
            path_max,
            path_max - 1
        )
    );
    let revisions = root.join("42/revisions");
    assert!(!revisions.join("fits2").exists());
    assert!(!revisions.join(".pp-source-candidate").exists());

    req.upstream_revision_key = "fits".into();
    let published = source.materialize(req, &input, budget()).unwrap();
    assert_eq!(published.publication, Publication::Created);
    assert_eq!(fs::read(prefix.join(&label)).unwrap(), b"solid long");
}

/// Rust readers on Windows open long paths through `std`, so pp-source's
/// limit there is the 32,767-unit extended-length maximum, not 4096 bytes.
#[cfg(windows)]
#[test]
fn windows_publishes_paths_longer_than_unix_path_max() {
    let fixture = Fixture::new();
    let mut root = fixture.0.join("repos");
    while root.as_os_str().len() <= 4096 {
        root.push("d".repeat(200));
    }
    fs::create_dir_all(&root).unwrap();
    let repos = TenantRepos::open("tenant-fixture".into(), &root).unwrap();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut source = repos.source(42).unwrap();
    let published = source.materialize(request(), &input, budget()).unwrap();
    assert_eq!(published.publication, Publication::Created);
    let stored = ["42", "revisions", "caller-uuid", "part.stl"]
        .iter()
        .fold(root, |path, part| path.join(part));
    assert_eq!(fs::read(stored).unwrap(), b"solid bytes");
}

/// `PATH_MAX` and these `/`-joined byte counts are Unix rules.
#[cfg(unix)]
#[test]
fn published_manifest_also_stops_one_byte_short_of_path_max() {
    let path_max = if cfg!(target_os = "macos") {
        1024
    } else {
        4096
    };
    let fixture = Fixture::new();
    // Under revision key "k2", `<root>/42/revisions/k2/<manifest>` is
    // exactly `path_max` bytes, while `part.stl` still fits.
    let root_bytes = path_max - "/42/revisions/k2/".len() - pp_source::MANIFEST.len();
    let mut root = fixture.0.to_path_buf();
    while root_bytes - root.as_os_str().len() > 101 {
        root.push("d".repeat(99));
        fs::create_dir(&root).unwrap();
        modes::pin_mode(&root, 0o755);
    }
    root.push("e".repeat(root_bytes - root.as_os_str().len() - 1));
    fs::create_dir(&root).unwrap();
    modes::pin_mode(&root, 0o755);
    assert_eq!(root.as_os_str().len(), root_bytes);
    let repos = TenantRepos::open("tenant-fixture".into(), &root).unwrap();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut source = repos.source(42).unwrap();
    let mut req = request();

    req.upstream_revision_key = "k2".into();
    let error = source
        .materialize(req.clone(), &input, budget())
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        format!(
            "stored-path-too-long: {} would be stored at an absolute path of {path_max} \
             bytes; this system opens paths of at most {} bytes; \
             use a shorter repository or folder path and retry the import",
            pp_source::MANIFEST,
            path_max - 1
        )
    );
    assert!(!root.join("42/revisions/k2").exists());

    req.upstream_revision_key = "k".into();
    source.materialize(req, &input, budget()).unwrap();
    let manifest = root.join("42/revisions/k").join(pp_source::MANIFEST);
    assert_eq!(manifest.as_os_str().len(), path_max - 1);
    assert!(fs::read(manifest).unwrap().starts_with(b"{"));
}

#[test]
fn cli_retry_reuses_revision_after_inputs_are_deleted() {
    use std::io::Write;
    use std::process::Stdio;

    #[path = "support/process.rs"]
    mod test_process;

    let fixture = Fixture::new();
    let command = serde_json::json!({
        "tenantId": "tenant-fixture", "sourceId": 42,
        "reposDir": fixture.0.join("repos"), "inputDir": fixture.0.join("input"),
        "reservedStoredBytes": 1024 * 1024, "snapshot": request(),
    });
    let invoke = || {
        let mut child = test_process::command(test_process::bin_exe("pp-source"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(command.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let first = invoke();
    assert_eq!(first["publication"], "created");
    fs::remove_dir_all(fixture.0.join("input")).unwrap();
    let reused = invoke();
    assert_eq!(reused["publication"], "reused");
    for key in [
        "manifestDigest",
        "files",
        "selection",
        "storedBytes",
        "snapshotLocator",
    ] {
        assert_eq!(reused[key], first[key]);
    }
}

#[test]
fn shared_directory_case_publishes_with_one_spelling_and_reuses() {
    let fixture = Fixture::new();
    let paths = ["Parts/Nested/a.stl", "parts/nested/b.stl"];
    for (path, bytes) in paths
        .iter()
        .zip([b"first".as_slice(), b"second".as_slice()])
    {
        let full = fixture.0.join("input").join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, bytes).unwrap();
    }
    let mut selected = request();
    selected.files = paths
        .into_iter()
        .rev()
        .map(|path| SelectedFile {
            path: SourcePath::try_from(path.to_owned()).unwrap(),
            kind: FileKind::Stl,
            size_hint_bytes: None,
        })
        .collect();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut source = fixture.repos().source(42).unwrap();
    let first = source
        .materialize(selected.clone(), &input, budget())
        .unwrap();
    assert_eq!(first.publication, Publication::Created);
    assert_eq!(
        first
            .files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        ["Parts/Nested/a.stl", "Parts/Nested/b.stl"]
    );
    let published = fixture.0.join("repos/42/revisions/caller-uuid");
    assert_eq!(
        fs::read(published.join("Parts/Nested/a.stl")).unwrap(),
        b"first"
    );
    assert_eq!(
        fs::read(published.join("Parts/Nested/b.stl")).unwrap(),
        b"second"
    );
    assert_eq!(fs::read_dir(published.join("Parts")).unwrap().count(), 1);
    let second = source.materialize(selected, &input, budget()).unwrap();
    assert_eq!(second.publication, Publication::Reused);
    assert_eq!(second.manifest_digest, first.manifest_digest);
    assert_eq!(second.files, first.files);
}

#[test]
fn accented_shared_folder_case_publishes_and_reuses_with_stable_manifest() {
    let fixture = Fixture::new();
    let paths = [
        "Pièces/Nested/a.stl",
        "pièces/nested/b.stl",
        "PIÈCES/NESTED/c.stl",
    ];
    for (path, bytes) in paths.iter().zip([b"first".as_slice(), b"second", b"third"]) {
        let full = fixture.0.join("input").join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, bytes).unwrap();
    }
    let mut selected = request();
    selected.files = paths
        .into_iter()
        .rev()
        .map(|path| SelectedFile {
            path: SourcePath::try_from(path.to_owned()).unwrap(),
            kind: FileKind::Stl,
            size_hint_bytes: None,
        })
        .collect();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut source = fixture.repos().source(42).unwrap();
    let first = source
        .materialize(selected.clone(), &input, budget())
        .unwrap();
    assert_eq!(first.publication, Publication::Created);
    assert_eq!(
        first
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        [
            "PIÈCES/NESTED/a.stl",
            "PIÈCES/NESTED/b.stl",
            "PIÈCES/NESTED/c.stl",
        ]
    );
    let published = fixture.0.join("repos/42/revisions/caller-uuid");
    assert_eq!(fs::read_dir(&published).unwrap().count(), 2); // folder + manifest
    for (name, bytes) in
        ["a.stl", "b.stl", "c.stl"]
            .into_iter()
            .zip([b"first".as_slice(), b"second", b"third"])
    {
        assert_eq!(
            fs::read(published.join("PIÈCES/NESTED").join(name)).unwrap(),
            bytes
        );
    }
    let second = source.materialize(selected, &input, budget()).unwrap();
    assert_eq!(second.publication, Publication::Reused);
    assert_eq!(second.files, first.files);
    assert_eq!(second.manifest_digest, first.manifest_digest);
}

#[test]
fn selected_omitted_case_pair_publishes_while_exact_and_omitted_collisions_reject() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("input/README.md"), b"# Notes\n").unwrap();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut source = fixture.repos().source(42).unwrap();
    let mut selected = request();
    selected.files.push(SelectedFile {
        path: SourcePath::try_from("README.md".to_owned()).unwrap(),
        kind: FileKind::Readme,
        size_hint_bytes: None,
    });
    selected.selection.omitted_files = vec![pp_source::OmittedFile {
        path: SourcePath::try_from("readme.md".to_owned()).unwrap(),
        kind: FileKind::Md,
        size_hint_bytes: None,
        reason: pp_source::OmissionReason::UnknownDocumentSize,
    }];
    let published = source
        .materialize(selected.clone(), &input, budget())
        .unwrap();
    assert_eq!(published.publication, Publication::Created);
    assert!(
        published
            .files
            .iter()
            .any(|file| file.path.as_str() == "README.md")
    );
    selected.upstream_revision_key = "exact-duplicate".into();
    selected.selection.omitted_files[0].path =
        SourcePath::try_from("README.md".to_owned()).unwrap();
    assert!(matches!(
        source.materialize(selected.clone(), &input, budget()),
        Err(pp_source::Error::DuplicatePath)
    ));
    selected.upstream_revision_key = "omitted-case-collision".into();
    selected.selection.omitted_files = ["omitted.md", "OMITTED.md"]
        .into_iter()
        .map(|path| pp_source::OmittedFile {
            path: SourcePath::try_from(path.to_owned()).unwrap(),
            kind: FileKind::Md,
            size_hint_bytes: None,
            reason: pp_source::OmissionReason::UnknownDocumentSize,
        })
        .collect();
    assert!(matches!(
        source.materialize(selected, &input, budget()),
        Err(pp_source::Error::DuplicatePath)
    ));
}

#[test]
fn canonical_folder_overlap_is_rejected_without_publishing_and_retry_can_recover() {
    let fixture = Fixture::new();
    let paths = ["Parts/a.stl", "parts/notes.md"];
    for path in paths {
        let full = fixture.0.join("input").join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, b"content").unwrap();
    }
    let mut selected = request();
    selected.files = paths
        .into_iter()
        .map(|path| SelectedFile {
            path: SourcePath::try_from(path.to_owned()).unwrap(),
            kind: if path.ends_with(".stl") {
                FileKind::Stl
            } else {
                FileKind::Md
            },
            size_hint_bytes: None,
        })
        .collect();
    selected.selection.omitted_files = vec![pp_source::OmittedFile {
        path: SourcePath::try_from("Parts/notes.md".to_owned()).unwrap(),
        kind: FileKind::Md,
        size_hint_bytes: None,
        reason: pp_source::OmissionReason::UnknownDocumentSize,
    }];
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut source = fixture.repos().source(42).unwrap();
    assert!(matches!(
        source.materialize(selected.clone(), &input, budget()),
        Err(pp_source::Error::DuplicatePath)
    ));
    let revisions = fixture.0.join("repos/42/revisions");
    assert!(!revisions.join("caller-uuid").exists());
    assert!(!revisions.join(".pp-source-candidate").exists());
    selected.selection.omitted_files[0].path =
        SourcePath::try_from("Parts/other.md".to_owned()).unwrap();
    let first = source
        .materialize(selected.clone(), &input, budget())
        .unwrap();
    assert_eq!(first.publication, Publication::Created);
    let second = source.materialize(selected, &input, budget()).unwrap();
    assert_eq!(second.publication, Publication::Reused);
    assert_eq!(second.files, first.files);
    assert_eq!(second.manifest_digest, first.manifest_digest);
}

/// Writes a revision the way the earlier lowercase path rule published it.
fn write_legacy_revision(fixture: &Fixture, key: &str, names: &[&str]) -> PathBuf {
    write_legacy_revision_with_omissions(fixture, key, names, serde_json::json!([]))
}
fn write_legacy_revision_with_omissions(
    fixture: &Fixture,
    key: &str,
    names: &[&str],
    omitted: serde_json::Value,
) -> PathBuf {
    use sha2::{Digest, Sha256};

    let revision = fixture.0.join("repos/42/revisions").join(key);
    let mut files: Vec<_> = names
        .iter()
        .map(|name| {
            let path = revision.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, name.as_bytes()).unwrap();
            pp_source::ContentFile {
                path: SourcePath::try_from((*name).to_owned()).unwrap(),
                kind: FileKind::Stl,
                size_bytes: name.len() as u64,
                sha256: hex::encode(Sha256::digest(name.as_bytes())),
            }
        })
        .collect();
    files.sort_by(|a, b| {
        a.path
            .as_str()
            .encode_utf16()
            .cmp(b.path.as_str().encode_utf16())
    });
    let manifest = serde_json::json!({
        "version": 1,
        "upstreamRevisionKey": key,
        "manifestDigest": hex::encode(Sha256::digest(serde_json::to_vec(&files).unwrap())),
        "selection": {"maxStlFiles": 500, "maxDocumentationBytes": 1000, "omittedFiles": omitted},
        "files": files,
    });
    fs::write(
        revision.join(pp_source::MANIFEST),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    revision
}

/// Device names and trailing dots or spaces are refused only on Windows,
/// where absolute-path readers would reinterpret them. Unix publishes them
/// exactly as before.
#[test]
fn device_names_are_refused_only_on_windows() {
    let fixture = Fixture::new();
    let repos = fixture.repos();
    let materialize = |req: SnapshotRequest| {
        let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
        repos
            .source(42)
            .and_then(|mut source| source.materialize(req, &input, budget()))
    };
    let cases = [
        "NUL.stl",
        "parts/con/part.stl",
        "COM1.stl",
        "part.stl.",
        "parts /a.stl",
    ];
    for (index, path) in cases.iter().enumerate() {
        #[cfg(unix)]
        {
            let stored = fixture.0.join("input").join(path);
            fs::create_dir_all(stored.parent().unwrap()).unwrap();
            fs::write(stored, b"solid bytes").unwrap();
        }
        let mut req = request();
        req.upstream_revision_key = format!("device-{index}");
        req.files[0].path = SourcePath::try_from((*path).to_owned()).unwrap();
        let result = materialize(req);
        if cfg!(windows) {
            assert_eq!(result.unwrap_err().category(), "unsafe-path", "{path:?}");
        } else {
            assert_eq!(
                result.unwrap().publication,
                Publication::Created,
                "{path:?}"
            );
        }
    }
    let mut req = request();
    req.upstream_revision_key = "CON".into();
    let result = materialize(req);
    if cfg!(windows) {
        assert_eq!(result.unwrap_err().category(), "invalid-identity");
    } else {
        assert_eq!(result.unwrap().publication, Publication::Created);
    }
}

#[test]
fn legacy_ambiguous_revision_reports_permanent_migration_error_and_is_retained() {
    let fixture = Fixture::new();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut source = fixture.repos().source(42).unwrap();
    // Device names are refused only for new Windows names, so an intact Unix
    // legacy revision holding one keeps this classification. Windows cannot
    // store it: `NUL.stl` names the device there.
    let device = cfg!(unix).then_some(&["NUL.stl", "Straße.stl", "STRASSE.stl"][..]);
    for names in [
        &["Straße.stl", "STRASSE.stl"][..],
        &["Straße/a.stl", "STRASSE/b.stl"][..],
        &["Σ.stl", "ς.stl"][..],
    ]
    .into_iter()
    .chain(device)
    {
        let revision = write_legacy_revision(&fixture, "legacy-ambiguous", names);
        let inventory = || {
            let mut entries: Vec<_> = names
                .iter()
                .map(|name| fs::read(revision.join(name)).unwrap())
                .collect();
            entries.push(fs::read(revision.join(pp_source::MANIFEST)).unwrap());
            entries
        };
        let before = inventory();
        // Case-insensitive volumes (NTFS by default) fold some pairs, such as
        // Σ and ς, into one entry: the second write replaces the first. That
        // revision no longer matches its manifest, so load reports corruption.
        let distinct = names
            .iter()
            .all(|name| fs::read(revision.join(name)).unwrap() == name.as_bytes());
        let mut retry = request();
        retry.upstream_revision_key = "legacy-ambiguous".into();
        for _ in 0..2 {
            let error = source
                .materialize(retry.clone(), &input, budget())
                .unwrap_err();
            if !distinct {
                assert!(
                    matches!(error, pp_source::Error::CorruptSnapshot),
                    "{names:?}: {error:?}"
                );
                continue;
            }
            assert!(
                matches!(&error, pp_source::Error::AmbiguousRevision(key) if key == "legacy-ambiguous"),
                "{names:?}: {error:?}"
            );
            assert_eq!(error.category(), "ambiguous-revision");
            let message = error.to_string();
            assert!(message.starts_with("ambiguous-revision: "), "{message}");
            assert!(message.contains("legacy-ambiguous"), "{message}");
            assert!(message.contains("new revision key"), "{message}");
        }
        assert_eq!(inventory(), before);
        assert!(
            !revision
                .parent()
                .unwrap()
                .join(".pp-source-candidate")
                .exists()
        );
        fs::remove_dir_all(&revision).unwrap();
    }
}

#[test]
fn ambiguous_names_keep_generic_errors_unless_the_revision_is_an_intact_legacy_publication() {
    let fixture = Fixture::new();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut source = fixture.repos().source(42).unwrap();
    let mut retry = request();
    retry.upstream_revision_key = "legacy-ambiguous".into();
    // The earlier rule also rejected this pair, so it was never a publication.
    let revision = write_legacy_revision(&fixture, "legacy-ambiguous", &["a.stl", "A.stl"]);
    assert!(matches!(
        source.materialize(retry.clone(), &input, budget()),
        Err(pp_source::Error::DuplicatePath)
    ));
    fs::remove_dir_all(&revision).unwrap();
    // A tampered legacy-ambiguous revision is corruption, not a policy break.
    let revision =
        write_legacy_revision(&fixture, "legacy-ambiguous", &["Straße.stl", "STRASSE.stl"]);
    fs::write(revision.join("Straße.stl"), b"tampered!!").unwrap();
    assert!(matches!(
        source.materialize(retry, &input, budget()),
        Err(pp_source::Error::CorruptSnapshot)
    ));
}

#[test]
fn legacy_ambiguous_revision_still_enforces_the_other_manifest_constraints() {
    let fixture = Fixture::new();
    let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
    let mut source = fixture.repos().source(42).unwrap();
    // Omitted STL entries are corrupt regardless of the path collision.
    write_legacy_revision_with_omissions(
        &fixture,
        "legacy-ambiguous",
        &["Straße.stl", "STRASSE.stl"],
        serde_json::json!([{
            "path": "notes.md",
            "kind": "stl",
            "sizeHintBytes": null,
            "reason": "unknown-document-size",
        }]),
    );
    let mut retry = request();
    retry.upstream_revision_key = "legacy-ambiguous".into();
    assert!(matches!(
        source.materialize(retry, &input, budget()),
        Err(pp_source::Error::CorruptSnapshot)
    ));
}

#[path = "../../pp-storage/tests/common/cleanup.rs"]
mod cleanup;

#[test]
fn scratch_root_is_empty_after_tests() {
    cleanup::assert_test_run_clean(&["--skip", "scratch_root_is_empty_after_tests"]);
}

#[path = "support/modes.rs"]
mod modes;
