use pp_source::{
    ArtifactBudget, FileKind, LocalFiles, SelectedFile, Selection, SnapshotRequest, SourcePath,
    TenantRepos,
    archive::{ArchiveError, ArchiveLimits, ZipInput},
};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;
#[path = "support/pinned_repos.rs"]
mod pinned_repos;

/// Removed when the scratch drops, including while a test unwinds from a panic.
struct Fixture(Scratch);
impl Fixture {
    fn new() -> Self {
        let root = Scratch::new("pp-source-archive-");
        fs::create_dir(root.join("repos")).unwrap();
        fs::create_dir(root.join("input")).unwrap();
        #[cfg(unix)]
        for dir in [root.to_path_buf(), root.join("repos"), root.join("input")] {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(root)
    }
    fn zip(&self, size: usize) {
        let mut zip = ZipWriter::new(File::create(self.0.join("input/upload.zip")).unwrap());
        zip.start_file(
            "part.stl",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .unwrap();
        for _ in 0..size {
            zip.write_all(&[7; 64 * 1024]).unwrap();
        }
        zip.finish().unwrap();
    }
    fn input(&self) -> ZipInput {
        ZipInput::open(
            &LocalFiles::open(&self.0.join("input")).unwrap(),
            &SourcePath::try_from("upload.zip".to_owned()).unwrap(),
        )
        .unwrap()
    }
    fn tenant(&self) -> TenantRepos {
        TenantRepos::open("fixture-tenant".into(), &self.0.join("repos")).unwrap()
    }
    fn candidate(&self) -> PathBuf {
        self.0
            .join("repos/42/revisions/.pp-source-archives/.candidate")
    }
}

fn zip64_disk_sentinels(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    let end = bytes.len() - 22;
    assert_eq!(&bytes[end..end + 4], b"PK\x05\x06");
    let count = u16::from_le_bytes(bytes[end + 10..end + 12].try_into().unwrap()) as u64;
    let central_size = u32::from_le_bytes(bytes[end + 12..end + 16].try_into().unwrap()) as u64;
    let central_offset = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as u64;
    bytes.truncate(end);
    let zip64_offset = bytes.len() as u64;
    bytes.extend_from_slice(b"PK\x06\x06");
    bytes.extend_from_slice(&44u64.to_le_bytes());
    bytes.extend_from_slice(&45u16.to_le_bytes());
    bytes.extend_from_slice(&45u16.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&central_size.to_le_bytes());
    bytes.extend_from_slice(&central_offset.to_le_bytes());
    bytes.extend_from_slice(b"PK\x06\x07");
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&zip64_offset.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(b"PK\x05\x06");
    bytes.extend_from_slice(&u16::MAX.to_le_bytes());
    bytes.extend_from_slice(&u16::MAX.to_le_bytes());
    bytes.extend_from_slice(&(count as u16).to_le_bytes());
    bytes.extend_from_slice(&(count as u16).to_le_bytes());
    bytes.extend_from_slice(&(central_size as u32).to_le_bytes());
    bytes.extend_from_slice(&(central_offset as u32).to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    fs::write(path, bytes).unwrap();
}

#[test]
fn owned_extraction_feeds_snapshot_and_cleanup_preserves_published_bytes() {
    let fixture = Fixture::new();
    fixture.zip(1);
    let mut source = fixture.tenant().source(42).unwrap();
    let extracted = source
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let receipt = extracted.receipt();
    assert_eq!(receipt.inflated_bytes, 65536);
    assert!(fixture.candidate().join("files/part.stl").is_file());
    let snapshot = source
        .materialize(
            SnapshotRequest {
                upstream_revision_key: "uuid-owned-by-caller".into(),
                files: receipt
                    .files
                    .iter()
                    .map(|f| SelectedFile {
                        path: f.path.clone(),
                        kind: FileKind::Stl,
                        size_hint_bytes: Some(f.size_bytes),
                    })
                    .collect(),
                selection: Selection {
                    max_stl_files: 500,
                    max_documentation_bytes: 1024,
                    omitted_files: vec![],
                },
            },
            extracted.files(),
            ArtifactBudget::new(1_000_000, 1_000_000).unwrap(),
        )
        .unwrap();
    extracted.discard().unwrap();
    assert!(!fixture.candidate().exists());
    assert_eq!(
        fs::read(
            fixture
                .0
                .join("repos")
                .join(snapshot.snapshot_locator)
                .join("part.stl")
        )
        .unwrap(),
        vec![7; 65536]
    );
}

#[test]
fn zip64_disk_sentinels_resolve_to_single_disk() {
    let fixture = Fixture::new();
    fixture.zip(1);
    zip64_disk_sentinels(&fixture.0.join("input/upload.zip"));
    fixture
        .tenant()
        .source(42)
        .unwrap()
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .discard()
        .unwrap();
}

#[test]
fn disk_sentinels_without_zip64_metadata_are_rejected() {
    let fixture = Fixture::new();
    fixture.zip(1);
    let path = fixture.0.join("input/upload.zip");
    let mut bytes = fs::read(&path).unwrap();
    let end = bytes.len() - 22;
    bytes[end + 4..end + 8].fill(0xff);
    fs::write(path, bytes).unwrap();
    assert!(matches!(
        fixture.tenant().source(42).unwrap().extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        ),
        Err(ArchiveError::Unsupported)
    ));
}

#[test]
fn input_file_and_source_roots_remain_pinned_after_path_replacement() {
    let fixture = Fixture::new();
    fixture.zip(1);
    let input = fixture.input();
    let mut source = fixture.tenant().source(42).unwrap();
    fs::rename(
        fixture.0.join("input/upload.zip"),
        fixture.0.join("input/original.zip"),
    )
    .unwrap();
    fs::write(fixture.0.join("input/upload.zip"), b"not a zip").unwrap();
    let pinned = pinned_repos::move_repos_aside(&fixture.0, "original-repos");
    let candidate = fixture
        .0
        .join(pinned)
        .join("42/revisions/.pp-source-archives/.candidate");
    let extracted = source
        .extract_zip(input, ArchiveLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(extracted.receipt().inflated_bytes, 65536);
    assert!(candidate.is_dir());
    if pinned == "original-repos" {
        assert!(
            fs::read_dir(fixture.0.join("repos"))
                .unwrap()
                .next()
                .is_none()
        );
    }
    extracted.discard().unwrap();
    assert!(!candidate.exists());
}

#[cfg(unix)]
#[test]
fn symlinked_archive_parent_cannot_redirect_output() {
    let fixture = Fixture::new();
    fixture.zip(1);
    let mut source = fixture.tenant().source(42).unwrap();
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"keep").unwrap();
    std::os::unix::fs::symlink(
        &outside,
        fixture.0.join("repos/42/revisions/.pp-source-archives"),
    )
    .unwrap();
    assert!(
        source
            .extract_zip(
                fixture.input(),
                ArchiveLimits::default(),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
}

#[cfg(unix)]
#[test]
fn abandoned_candidate_symlink_is_unlinked_without_following_it() {
    let fixture = Fixture::new();
    fixture.zip(1);
    let mut source = fixture.tenant().source(42).unwrap();
    fs::create_dir(fixture.candidate().parent().unwrap()).unwrap();
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("sentinel"), b"keep").unwrap();
    std::os::unix::fs::symlink(&outside, fixture.candidate()).unwrap();
    source
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .discard()
        .unwrap();
    assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"keep");
    assert!(!fixture.candidate().exists());
}

#[test]
fn cancellation_during_input_copy_cleans_private_stage() {
    let fixture = Fixture::new();
    fixture.zip(512);
    let mut source = fixture.tenant().source(42).unwrap();
    let input = fixture.input();
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let path = fixture.candidate().join("archive.zip");
    let watcher = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if fs::metadata(&path).is_ok_and(|m| m.len() > 0) {
                signal.store(true, Ordering::Relaxed);
                return true;
            }
            thread::sleep(Duration::from_micros(100));
        }
        signal.store(true, Ordering::Relaxed);
        false
    });
    let result = source.extract_zip(input, ArchiveLimits::default(), &cancel);
    assert!(watcher.join().unwrap());
    assert!(matches!(result, Err(ArchiveError::Cancelled)));
    assert!(!fixture.candidate().exists());
    assert!(!fixture.0.join("repos/42/revisions/accepted").exists());
}

#[test]
fn dropping_unused_readiness_cleans_and_releases_the_stage() {
    let fixture = Fixture::new();
    fixture.zip(1);
    let mut source = fixture.tenant().source(42).unwrap();
    let extracted = source
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    drop(extracted);
    assert!(!fixture.candidate().exists());
    source
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .discard()
        .unwrap();
}

#[cfg(unix)]
#[test]
fn input_symlink_is_rejected_before_extraction() {
    let fixture = Fixture::new();
    fixture.zip(1);
    std::os::unix::fs::symlink(
        fixture.0.join("input/upload.zip"),
        fixture.0.join("input/alias.zip"),
    )
    .unwrap();
    let files = LocalFiles::open(&fixture.0.join("input")).unwrap();
    assert!(
        ZipInput::open(
            &files,
            &SourcePath::try_from("alias.zip".to_owned()).unwrap()
        )
        .is_err()
    );
}

#[test]
fn live_readiness_rejects_another_extraction_without_consuming_its_stage() {
    let fixture = Fixture::new();
    fixture.zip(1);
    let mut source = fixture.tenant().source(42).unwrap();
    let first = source
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(matches!(
        source.extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(ArchiveError::Busy)
    ));
    assert!(fixture.candidate().join("files/part.stl").is_file());
    first.discard().unwrap();
    source
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .discard()
        .unwrap();
}

#[test]
fn archive_and_snapshot_reject_the_same_unicode_tree_collisions() {
    for names in [
        ["Straße.stl", "STRASSE.stl"],
        ["Σ.stl", "ς.stl"],
        ["σ.stl", "ς.stl"],
        ["ﬃ.stl", "FFI.stl"],
        ["Straße/a.stl", "STRASSE/b.stl"],
        ["parts/Σ/a.stl", "parts/ς/b.stl"],
        ["Straße", "STRASSE/part.stl"],
    ] {
        let fixture = Fixture::new();
        let mut zip = ZipWriter::new(File::create(fixture.0.join("input/upload.zip")).unwrap());
        for name in names {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(b"x").unwrap();
        }
        zip.finish().unwrap();
        let mut source = fixture.tenant().source(42).unwrap();
        assert!(
            matches!(
                source.extract_zip(
                    fixture.input(),
                    ArchiveLimits::default(),
                    &AtomicBool::new(false)
                ),
                Err(ArchiveError::DuplicateEntry)
            ),
            "{names:?}"
        );
        assert!(!fixture.candidate().exists());
        let request = SnapshotRequest {
            upstream_revision_key: "unicode-conflict".into(),
            files: names
                .into_iter()
                .map(|name| SelectedFile {
                    path: SourcePath::try_from(name.to_owned()).unwrap(),
                    kind: FileKind::Stl,
                    size_hint_bytes: Some(1),
                })
                .collect(),
            selection: Selection {
                max_stl_files: 100,
                max_documentation_bytes: 100,
                omitted_files: vec![],
            },
        };
        assert!(
            matches!(
                source.materialize(
                    request,
                    &LocalFiles::open(&fixture.0.join("input")).unwrap(),
                    ArtifactBudget::new(10000, 10000).unwrap()
                ),
                Err(pp_source::Error::DuplicatePath)
            ),
            "{names:?}"
        );
        assert!(
            !fixture
                .0
                .join("repos/42/revisions/unicode-conflict")
                .exists()
        );
    }
}

#[test]
fn decomposed_archive_names_reject_without_normalizing_published_paths() {
    let fixture = Fixture::new();
    let mut zip = ZipWriter::new(File::create(fixture.0.join("input/upload.zip")).unwrap());
    for name in ["café.stl", "cafe\u{301}.stl"] {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        zip.write_all(b"x").unwrap();
    }
    zip.finish().unwrap();
    let mut source = fixture.tenant().source(42).unwrap();
    assert!(matches!(
        source.extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(ArchiveError::UnsafeEntry)
    ));
    assert!(!fixture.candidate().exists());
}

#[cfg(feature = "desktop_rebuild")]
mod acquisition_labels {
    use super::*;
    use pp_source::archive::ArchiveLabelPolicy;
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use unicode_normalization::UnicodeNormalization;

    fn write_members(fixture: &Fixture, members: &[(&str, &[u8])]) {
        let mut data = Vec::new();
        let mut central = Vec::new();
        for &(name, payload) in members {
            let offset = data.len() as u32;
            let crc = crc32(payload);
            let size = payload.len() as u32;
            let mut local = stored_local_header(crc, size, 0);
            local.truncate(30);
            local[6..8].copy_from_slice(&0x800u16.to_le_bytes());
            local[26..28].copy_from_slice(&(name.len() as u16).to_le_bytes());
            data.extend(local);
            data.extend_from_slice(name.as_bytes());
            data.extend_from_slice(payload);
            let mut header = stored_central_header(crc, size, offset, &[]);
            header.truncate(46);
            header[8..10].copy_from_slice(&0x800u16.to_le_bytes());
            header[28..30].copy_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend(header);
            central.extend_from_slice(name.as_bytes());
        }
        let mut end = end_record(data.len(), central.len());
        let count = (members.len() as u16).to_le_bytes();
        end[8..10].copy_from_slice(&count);
        end[10..12].copy_from_slice(&count);
        data.extend(central);
        data.extend(end);
        let mut reader = open_unchecked(data.clone());
        for (index, &(name, _)) in members.iter().enumerate() {
            assert_eq!(reader.by_index(index).unwrap().name_raw(), name.as_bytes());
        }
        fs::write(fixture.0.join("input/upload.zip"), data).unwrap();
    }

    #[test]
    fn explicit_strict_rejects_decomposed_labels_and_releases_stage() {
        let fixture = Fixture::new();
        write_members(&fixture, &[("cafe\u{301}.stl", b"x")]);
        let mut source = fixture.tenant().source(42).unwrap();
        assert!(matches!(
            source.extract_zip_with_label_policy(
                fixture.input(),
                ArchiveLimits::default(),
                ArchiveLabelPolicy::Strict,
                &AtomicBool::new(false),
            ),
            Err(ArchiveError::UnsafeEntry)
        ));
        assert!(!fixture.candidate().exists());
        write_members(&fixture, &[("café.stl", b"x")]);
        source
            .extract_zip_with_label_policy(
                fixture.input(),
                ArchiveLimits::default(),
                ArchiveLabelPolicy::Strict,
                &AtomicBool::new(false),
            )
            .unwrap()
            .discard()
            .unwrap();
    }

    // The contracted paths are longer than macOS PATH_MAX (1024 bytes), so open them one
    // component at a time, as the extractor writes them, instead of by absolute path.
    fn open_beneath(root: &Path, relative: &str) -> File {
        let (parents, name) = relative.rsplit_once('/').unwrap_or(("", relative));
        parents
            .split('/')
            .filter(|segment| !segment.is_empty())
            .fold(pp_os::Dir::open(root).unwrap(), |dir, segment| {
                dir.open_dir_nofollow(segment).unwrap()
            })
            .open_file(name, pp_os::FileOpen::Read)
            .unwrap()
    }

    #[test]
    fn raw_long_labels_contract_to_valid_file_and_directory_paths() {
        let raw_dir = vec!["e\u{301}".repeat(70); 20].join("/");
        let normalized_dir = vec!["é".repeat(70); 20].join("/");
        for (suffix, payload, raw_len, normalized_len) in [
            ("", b"".as_slice(), 4220, 2819),
            ("part.stl", b"x".as_slice(), 4228, 2828),
        ] {
            let raw = format!("{raw_dir}/{suffix}");
            let expected = if suffix.is_empty() {
                normalized_dir.clone()
            } else {
                format!("{normalized_dir}/{suffix}")
            };
            assert_eq!(raw.len(), raw_len);
            assert_eq!(expected.len(), normalized_len);
            let fixture = Fixture::new();
            write_members(&fixture, &[(&raw, payload)]);
            let mut source = fixture.tenant().source(42).unwrap();
            assert!(matches!(
                source.extract_zip(
                    fixture.input(),
                    ArchiveLimits::default(),
                    &AtomicBool::new(false),
                ),
                Err(ArchiveError::UnsafeEntry)
            ));
            assert!(!fixture.candidate().exists());
            assert!(matches!(
                source.extract_zip_with_label_policy(
                    fixture.input(),
                    ArchiveLimits::default(),
                    ArchiveLabelPolicy::Strict,
                    &AtomicBool::new(false),
                ),
                Err(ArchiveError::UnsafeEntry)
            ));
            assert!(!fixture.candidate().exists());
            let ready = source
                .extract_zip_with_label_policy(
                    fixture.input(),
                    ArchiveLimits::default(),
                    ArchiveLabelPolicy::NfcAtAcquisition,
                    &AtomicBool::new(false),
                )
                .unwrap();
            let mut destination = open_beneath(&fixture.candidate().join("files"), &expected);
            if suffix.is_empty() {
                assert!(
                    ready
                        .receipt()
                        .directories
                        .iter()
                        .any(|path| path.as_str() == expected)
                );
                assert!(destination.metadata().unwrap().is_dir());
                assert!(ready.receipt().files.is_empty());
            } else {
                assert_eq!(ready.receipt().files.len(), 1);
                assert_eq!(ready.receipt().files[0].path.as_str(), expected);
                let mut payload = Vec::new();
                destination.read_to_end(&mut payload).unwrap();
                assert_eq!(payload, b"x");
            }
            ready.discard().unwrap();
            assert!(!fixture.candidate().exists());
        }
    }

    #[test]
    fn contracted_paths_still_reject_post_nfc_overflow_and_release_stage() {
        let raw_dir = vec!["e\u{301}".repeat(70); 30].join("/");
        for suffix in ["", "part.stl"] {
            let raw = format!("{raw_dir}/{suffix}");
            let label = raw.trim_end_matches('/');
            assert!(label.nfc().collect::<String>().len() > 4096);
            let fixture = Fixture::new();
            write_members(&fixture, &[(&raw, b"")]);
            assert!(matches!(
                fixture
                    .tenant()
                    .source(42)
                    .unwrap()
                    .extract_zip_with_label_policy(
                        fixture.input(),
                        ArchiveLimits::default(),
                        ArchiveLabelPolicy::NfcAtAcquisition,
                        &AtomicBool::new(false),
                    ),
                Err(ArchiveError::UnsafeEntry)
            ));
            assert!(!fixture.candidate().exists());
        }
    }

    #[test]
    fn normalized_readiness_publishes_exact_payloads_and_preserves_original_zip() {
        let fixture = Fixture::new();
        write_members(
            &fixture,
            &[
                ("cafe\u{301}/", b""),
                ("cafe\u{301}/re\u{301}sume\u{301}/pie\u{300}ce.stl", b"x"),
                ("café/résumé/autre.stl", b"x"),
                ("cafe\u{301}/vide\u{301}/", b""),
            ],
        );
        let path = fixture.0.join("input/upload.zip");
        let mut original = fs::read(&path).unwrap();
        set_archive_comment(&mut original, b"acquisition fixture note");
        fs::write(&path, &original).unwrap();
        let original_hash = hex::encode(Sha256::digest(&original));
        let mut source = fixture.tenant().source(42).unwrap();
        let ready = source
            .extract_zip_with_label_policy(
                fixture.input(),
                ArchiveLimits::default(),
                ArchiveLabelPolicy::NfcAtAcquisition,
                &AtomicBool::new(false),
            )
            .unwrap();
        let receipt = ready.receipt();
        assert_eq!(receipt.archive_sha256, original_hash);
        assert_eq!(receipt.compressed_bytes, original.len() as u64);
        assert_eq!(receipt.inflated_bytes, 2);
        assert_eq!(receipt.entry_count, 4);
        assert_eq!(receipt.stl_count, 2);
        assert_eq!(
            receipt
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["café/résumé/autre.stl", "café/résumé/pièce.stl"]
        );
        assert_eq!(
            receipt
                .directories
                .iter()
                .map(SourcePath::as_str)
                .collect::<Vec<_>>(),
            ["café", "café/résumé", "café/vidé"]
        );
        for file in &receipt.files {
            assert_eq!(file.size_bytes, 1);
            assert_eq!(
                file.sha256,
                "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881"
            );
            assert_eq!(
                fs::read(fixture.candidate().join("files").join(file.path.as_str())).unwrap(),
                b"x"
            );
        }
        let snapshot = source
            .materialize(
                SnapshotRequest {
                    upstream_revision_key: "normalized-acquisition".into(),
                    files: receipt
                        .files
                        .iter()
                        .map(|file| SelectedFile {
                            path: file.path.clone(),
                            kind: FileKind::Stl,
                            size_hint_bytes: Some(file.size_bytes),
                        })
                        .collect(),
                    selection: Selection {
                        max_stl_files: 10,
                        max_documentation_bytes: 1024,
                        omitted_files: vec![],
                    },
                },
                ready.files(),
                ArtifactBudget::new(10000, 10000).unwrap(),
            )
            .unwrap();
        ready.discard().unwrap();
        assert!(!fixture.candidate().exists());
        for name in ["café/résumé/autre.stl", "café/résumé/pièce.stl"] {
            assert_eq!(
                fs::read(
                    fixture
                        .0
                        .join("repos")
                        .join(&snapshot.snapshot_locator)
                        .join(name)
                )
                .unwrap(),
                b"x"
            );
        }
        let after = fs::read(&path).unwrap();
        assert_eq!(after, original);
        assert_eq!(after.len(), original.len());
        assert_eq!(hex::encode(Sha256::digest(&after)), original_hash);
    }

    #[test]
    fn normalized_collisions_reject_in_both_orders_and_allow_retry() {
        for pair in [
            ["café.stl", "cafe\u{301}.stl"],
            ["cafe\u{301}/Straße.stl", "café/STRASSE.stl"],
            ["cafe\u{301}/a.stl", "CAFÉ/b.stl"],
            ["cafe\u{301}", "café/part.stl"],
            ["cafe\u{301}/", "café/"],
            ["cafe\u{301}", "café/"],
        ] {
            for names in [pair, [pair[1], pair[0]]] {
                let fixture = Fixture::new();
                let members = names.map(|name| {
                    (
                        name,
                        if name.ends_with('/') {
                            &b""[..]
                        } else {
                            &b"x"[..]
                        },
                    )
                });
                write_members(&fixture, &members);
                let mut source = fixture.tenant().source(42).unwrap();
                assert!(
                    matches!(
                        source.extract_zip_with_label_policy(
                            fixture.input(),
                            ArchiveLimits::default(),
                            ArchiveLabelPolicy::NfcAtAcquisition,
                            &AtomicBool::new(false),
                        ),
                        Err(ArchiveError::DuplicateEntry)
                    ),
                    "{names:?}"
                );
                assert!(!fixture.candidate().exists());
                write_members(&fixture, &[("cafe\u{301}.stl", b"x")]);
                source
                    .extract_zip_with_label_policy(
                        fixture.input(),
                        ArchiveLimits::default(),
                        ArchiveLabelPolicy::NfcAtAcquisition,
                        &AtomicBool::new(false),
                    )
                    .unwrap()
                    .discard()
                    .unwrap();
            }
        }
    }

    #[test]
    fn normalization_does_not_make_unsafe_labels_safe() {
        let expanded = format!("{}.stl", "\u{344}".repeat(1024));
        assert!(expanded.len() <= 4096);
        assert_eq!(expanded.nfc().collect::<String>().len(), 4100);
        for name in [
            "../cafe\u{301}.stl",
            "/cafe\u{301}.stl",
            "C:/cafe\u{301}.stl",
            "cafe\u{301}\\part.stl",
            "cafe\u{301}//part.stl",
            "./cafe\u{301}.stl",
            "cafe\u{301}/./part.stl",
            "cafe\u{301}/../part.stl",
            "cafe\u{301}/part\n.stl",
            "cafe\u{301}/part\0.stl",
            "/",
            "cafe\u{301}//",
            ".printpartner-source-snapshot.json/cafe\u{301}.stl",
            expanded.as_str(),
        ] {
            let fixture = Fixture::new();
            write_members(&fixture, &[(name, b"")]);
            assert!(
                matches!(
                    fixture
                        .tenant()
                        .source(42)
                        .unwrap()
                        .extract_zip_with_label_policy(
                            fixture.input(),
                            ArchiveLimits::default(),
                            ArchiveLabelPolicy::NfcAtAcquisition,
                            &AtomicBool::new(false),
                        ),
                    Err(ArchiveError::UnsafeEntry)
                ),
                "{name:?}"
            );
            assert!(!fixture.candidate().exists());
        }
    }

    #[test]
    fn opt_in_extraction_preserves_limits_and_ratio_guard() {
        let fixture = Fixture::new();
        write_members(
            &fixture,
            &[("cafe\u{301}.stl", b"xy"), ("autre.stl", b"xy")],
        );
        let compressed = fs::metadata(fixture.0.join("input/upload.zip"))
            .unwrap()
            .len();
        let mut source = fixture.tenant().source(42).unwrap();
        for limits in [
            ArchiveLimits {
                max_compressed_bytes: compressed - 1,
                ..ArchiveLimits::default()
            },
            ArchiveLimits {
                max_inflated_bytes: 1,
                ..ArchiveLimits::default()
            },
            ArchiveLimits {
                max_inflated_bytes: 3,
                ..ArchiveLimits::default()
            },
            ArchiveLimits {
                max_entries: 1,
                ..ArchiveLimits::default()
            },
        ] {
            assert!(matches!(
                source.extract_zip_with_label_policy(
                    fixture.input(),
                    limits,
                    ArchiveLabelPolicy::NfcAtAcquisition,
                    &AtomicBool::new(false),
                ),
                Err(ArchiveError::Limit)
            ));
            assert!(!fixture.candidate().exists());
        }
        deflated_zip(&fixture, &vec![0; 2 * 1024 * 1024]);
        assert!(matches!(
            source.extract_zip_with_label_policy(
                fixture.input(),
                ArchiveLimits::default(),
                ArchiveLabelPolicy::NfcAtAcquisition,
                &AtomicBool::new(false),
            ),
            Err(ArchiveError::CompressionRatio)
        ));
        assert!(!fixture.candidate().exists());
    }

    #[test]
    fn opt_in_cancellation_cleans_stage_and_allows_retry() {
        let fixture = Fixture::new();
        fixture.zip(512);
        let mut source = fixture.tenant().source(42).unwrap();
        let cancel = Arc::new(AtomicBool::new(true));
        assert!(matches!(
            source.extract_zip_with_label_policy(
                fixture.input(),
                ArchiveLimits::default(),
                ArchiveLabelPolicy::NfcAtAcquisition,
                &cancel,
            ),
            Err(ArchiveError::Cancelled)
        ));
        assert!(!fixture.candidate().exists());
        cancel.store(false, Ordering::Relaxed);
        let signal = cancel.clone();
        let path = fixture.candidate().join("archive.zip");
        let input_len = fs::metadata(fixture.0.join("input/upload.zip"))
            .unwrap()
            .len();
        let watcher = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if fs::metadata(&path).is_ok_and(|m| m.len() > 0 && m.len() < input_len) {
                    signal.store(true, Ordering::Relaxed);
                    return true;
                }
                thread::sleep(Duration::from_micros(100));
            }
            signal.store(true, Ordering::Relaxed);
            false
        });
        let result = source.extract_zip_with_label_policy(
            fixture.input(),
            ArchiveLimits::default(),
            ArchiveLabelPolicy::NfcAtAcquisition,
            &cancel,
        );
        assert!(watcher.join().unwrap());
        assert!(matches!(result, Err(ArchiveError::Cancelled)));
        assert!(!fixture.candidate().exists());
        cancel.store(false, Ordering::Relaxed);
        write_members(&fixture, &[("cafe\u{301}.stl", b"x")]);
        source
            .extract_zip_with_label_policy(
                fixture.input(),
                ArchiveLimits::default(),
                ArchiveLabelPolicy::NfcAtAcquisition,
                &cancel,
            )
            .unwrap()
            .discard()
            .unwrap();
        assert!(!fixture.candidate().exists());
    }

    #[test]
    fn canonically_equivalent_local_and_central_names_must_match_raw_bytes() {
        let fixture = Fixture::new();
        let central = "a\u{315}\u{300}.stl";
        let local = "a\u{300}\u{315}.stl";
        assert_eq!(central.len(), local.len());
        assert_ne!(central.as_bytes(), local.as_bytes());
        assert_eq!(
            central.nfc().collect::<String>(),
            local.nfc().collect::<String>()
        );
        write_members(&fixture, &[(central, b"x")]);
        let mut source = fixture.tenant().source(42).unwrap();
        source
            .extract_zip_with_label_policy(
                fixture.input(),
                ArchiveLimits::default(),
                ArchiveLabelPolicy::NfcAtAcquisition,
                &AtomicBool::new(false),
            )
            .unwrap()
            .discard()
            .unwrap();
        let path = fixture.0.join("input/upload.zip");
        let mut data = fs::read(&path).unwrap();
        data[30..30 + local.len()].copy_from_slice(local.as_bytes());
        fs::write(path, data).unwrap();
        assert!(matches!(
            source.extract_zip_with_label_policy(
                fixture.input(),
                ArchiveLimits::default(),
                ArchiveLabelPolicy::NfcAtAcquisition,
                &AtomicBool::new(false),
            ),
            Err(ArchiveError::InvalidArchive)
        ));
        assert!(!fixture.candidate().exists());
    }
}

fn local_request(key: &str) -> SnapshotRequest {
    SnapshotRequest {
        upstream_revision_key: key.into(),
        files: vec![SelectedFile {
            path: SourcePath::try_from("part.stl".to_owned()).unwrap(),
            kind: FileKind::Stl,
            size_hint_bytes: None,
        }],
        selection: Selection {
            max_stl_files: 500,
            max_documentation_bytes: 1024,
            omitted_files: vec![],
        },
    }
}

#[test]
fn peer_materialize_waits_until_live_archive_readiness_is_discarded() {
    let fixture = Fixture::new();
    fixture.zip(1);
    fs::write(fixture.0.join("input/part.stl"), b"peer bytes").unwrap();
    let mut owner = fixture.tenant().source(42).unwrap();
    let extracted = owner
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let revisions = fixture.0.join("repos/42/revisions");
    let finished = Arc::new(AtomicBool::new(false));
    let peer = {
        let mut peer = fixture.tenant().source(42).unwrap();
        let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
        let finished = finished.clone();
        thread::spawn(move || {
            let result = peer.materialize(
                local_request("peer"),
                &input,
                ArtifactBudget::new(1_000_000, 1_000_000).unwrap(),
            );
            finished.store(true, Ordering::Release);
            result
        })
    };
    // The peer's candidate must not stack on top of the live archive stage.
    thread::sleep(Duration::from_millis(500));
    assert!(!finished.load(Ordering::Acquire));
    assert!(!revisions.join(".pp-source-candidate").exists());
    assert!(!revisions.join("peer").exists());
    // The owning handle materializes under the lock it already holds.
    let owned = owner
        .materialize(
            local_request("owned"),
            extracted.files(),
            ArtifactBudget::new(1_000_000, 1_000_000).unwrap(),
        )
        .unwrap();
    assert_eq!(owned.files[0].size_bytes, 65536);
    assert!(!finished.load(Ordering::Acquire));
    extracted.discard().unwrap();
    let peer = peer.join().unwrap().unwrap();
    assert!(!fixture.candidate().exists());
    assert_eq!(
        fs::read(revisions.join("peer/part.stl")).unwrap(),
        b"peer bytes"
    );
    assert_eq!(peer.files[0].size_bytes, 10);
}

#[test]
fn extraction_is_busy_while_a_peer_holds_the_source_revisions_lock() {
    let fixture = Fixture::new();
    fixture.zip(1);
    let mut source = fixture.tenant().source(42).unwrap();
    // A peer process mid-materialize holds this lock on its own descriptor.
    let peer = pp_os::Dir::open(&fixture.0.join("repos/42/revisions")).unwrap();
    peer.lock().unwrap();
    assert!(matches!(
        source.extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(ArchiveError::Busy)
    ));
    assert!(!fixture.candidate().exists());
    peer.unlock().unwrap();
    let extracted = source
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(peer.try_lock().is_err());
    drop(extracted);
    assert!(!fixture.candidate().exists());
    peer.try_lock().unwrap();
}

fn deflated_zip(fixture: &Fixture, data: &[u8]) {
    let mut zip = ZipWriter::new(File::create(fixture.0.join("input/upload.zip")).unwrap());
    zip.start_file(
        "part.stl",
        SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(data).unwrap();
    zip.finish().unwrap();
}
fn extract(fixture: &Fixture) -> Result<(), ArchiveError> {
    fixture
        .tenant()
        .source(42)
        .unwrap()
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )?
        .discard()
}

#[test]
fn high_compression_ratio_entry_is_rejected_before_inflating() {
    use pp_source::archive::MAX_COMPRESSION_RATIO;

    let fixture = Fixture::new();
    // 8 MiB of zeros deflates to roughly 1000:1, far above the 100:1 cap.
    deflated_zip(&fixture, &vec![0; 8 * 1024 * 1024]);
    let compressed = fs::metadata(fixture.0.join("input/upload.zip"))
        .unwrap()
        .len();
    assert!(8 * 1024 * 1024 > compressed * MAX_COMPRESSION_RATIO);
    assert!(matches!(
        extract(&fixture),
        Err(ArchiveError::CompressionRatio)
    ));
    assert_eq!(
        ArchiveError::CompressionRatio.to_string(),
        "compression-ratio"
    );
    assert!(!fixture.candidate().exists());
    assert!(
        !fixture
            .0
            .join("repos/42/revisions/archive-revision")
            .exists()
    );
}

#[test]
fn understated_sizes_cannot_smuggle_a_high_ratio_entry_past_the_cap() {
    use pp_source::archive::COMPRESSION_RATIO_FLOOR_BYTES;

    let fixture = Fixture::new();
    deflated_zip(&fixture, &vec![0; 8 * 1024 * 1024]);
    // Advertise exactly the floor so the central-directory check passes. The
    // ZIP reader fails the real 8 MiB payload once it passes the advertised
    // size, before the streaming ratio cap is reached.
    let path = fixture.0.join("input/upload.zip");
    let mut data = fs::read(&path).unwrap();
    let central = data.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let floor = (COMPRESSION_RATIO_FLOOR_BYTES as u32).to_le_bytes();
    data[22..26].copy_from_slice(&floor);
    data[central + 24..central + 28].copy_from_slice(&floor);
    fs::write(&path, data).unwrap();
    assert_eq!(
        extract(&fixture).unwrap_err().to_string(),
        "io: File is larger than its declared uncompressed size"
    );
    assert!(!fixture.candidate().exists());
}

#[test]
fn ratio_cap_exempts_small_entries_and_accepts_ordinary_compression() {
    let fixture = Fixture::new();
    // Below the 1 MiB floor even an extreme ratio is accepted.
    deflated_zip(&fixture, &vec![0; 512 * 1024]);
    extract(&fixture).unwrap();
    // Above the floor, ordinary low-entropy data (~2:1) is accepted.
    let mut state = 0x2545_f491_u32;
    let data: Vec<u8> = (0..4 * 1024 * 1024)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 28) as u8
        })
        .collect();
    deflated_zip(&fixture, &data);
    extract(&fixture).unwrap();
    assert!(!fixture.candidate().exists());
}

/// Writes a valid ZIP of empty stored files whose central directory is
/// exactly `central_bytes` long, padding it with per-entry comments.
fn zip_with_central_directory(fixture: &Fixture, central_bytes: usize) {
    const NAME: usize = 9;
    let mut comments = vec![];
    let mut remaining = central_bytes;
    while remaining > 0 {
        let entry = remaining.min(46 + NAME + 65_535);
        assert!(entry >= 46 + NAME);
        comments.push(entry - 46 - NAME);
        remaining -= entry;
    }
    let mut local = vec![];
    let mut central = vec![];
    for (index, comment) in comments.iter().enumerate() {
        let name = format!("f{index:04}.stl");
        assert_eq!(name.len(), NAME);
        let offset = local.len() as u32;
        local.extend_from_slice(b"PK\x03\x04");
        local.extend_from_slice(&20u16.to_le_bytes());
        local.extend_from_slice(&[0; 20]);
        local.extend_from_slice(&(NAME as u16).to_le_bytes());
        local.extend_from_slice(&0u16.to_le_bytes());
        local.extend_from_slice(name.as_bytes());
        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&[0; 20]);
        central.extend_from_slice(&(NAME as u16).to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&(*comment as u16).to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
        central.extend(std::iter::repeat_n(b'c', *comment));
    }
    assert_eq!(central.len(), central_bytes);
    let count = (comments.len() as u16).to_le_bytes();
    let mut data = local;
    let central_offset = (data.len() as u32).to_le_bytes();
    data.extend_from_slice(&central);
    data.extend_from_slice(b"PK\x05\x06");
    data.extend_from_slice(&[0; 4]);
    data.extend_from_slice(&count);
    data.extend_from_slice(&count);
    data.extend_from_slice(&(central_bytes as u32).to_le_bytes());
    data.extend_from_slice(&central_offset);
    data.extend_from_slice(&0u16.to_le_bytes());
    fs::write(fixture.0.join("input/upload.zip"), data).unwrap();
}

#[test]
fn central_directory_metadata_is_bounded_at_eight_mebibytes() {
    use pp_source::archive::MAX_METADATA_BYTES;

    let fixture = Fixture::new();
    zip_with_central_directory(&fixture, MAX_METADATA_BYTES as usize);
    extract(&fixture).unwrap();
    zip_with_central_directory(&fixture, MAX_METADATA_BYTES as usize + 1);
    assert!(matches!(extract(&fixture), Err(ArchiveError::Limit)));
    assert!(!fixture.candidate().exists());
}

#[test]
fn zip64_extra_sizes_cannot_override_the_checked_central_sizes() {
    let fixture = Fixture::new();
    // Take a real ~1000:1 deflate stream for 8 MiB of zeros.
    deflated_zip(&fixture, &vec![0; 8 * 1024 * 1024]);
    let original = fs::read(fixture.0.join("input/upload.zip")).unwrap();
    let u16_at = |at: usize| u16::from_le_bytes([original[at], original[at + 1]]) as usize;
    let central = original
        .windows(4)
        .position(|w| w == b"PK\x01\x02")
        .unwrap();
    let crc = &original[central + 16..central + 20];
    let start = 30 + u16_at(26) + u16_at(28);
    let compressed = &original[start
        ..start
            + u32::from_le_bytes(original[central + 20..central + 24].try_into().unwrap())
                as usize];
    // Ordinary sizes claim a harmless 10:1 entry padded to 100,000 bytes,
    // while a ZIP64 extra field no sentinel asked for carries the real
    // 8 MiB / ~8 KiB sizes.
    let (claimed_compressed, claimed_inflated) = (100_000u32, 1024 * 1024u32);
    let name = b"part.stl";
    let mut data = vec![];
    data.extend_from_slice(b"PK\x03\x04");
    data.extend_from_slice(&20u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&8u16.to_le_bytes());
    data.extend_from_slice(&[0; 4]);
    data.extend_from_slice(crc);
    data.extend_from_slice(&claimed_compressed.to_le_bytes());
    data.extend_from_slice(&claimed_inflated.to_le_bytes());
    data.extend_from_slice(&(name.len() as u16).to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(name);
    data.extend_from_slice(compressed);
    data.resize(30 + name.len() + claimed_compressed as usize, 0);
    let central_offset = data.len() as u32;
    data.extend_from_slice(b"PK\x01\x02");
    data.extend_from_slice(&45u16.to_le_bytes());
    data.extend_from_slice(&45u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&8u16.to_le_bytes());
    data.extend_from_slice(&[0; 4]);
    data.extend_from_slice(crc);
    data.extend_from_slice(&claimed_compressed.to_le_bytes());
    data.extend_from_slice(&claimed_inflated.to_le_bytes());
    data.extend_from_slice(&(name.len() as u16).to_le_bytes());
    data.extend_from_slice(&28u16.to_le_bytes());
    data.extend_from_slice(&[0; 10]);
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(name);
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&24u16.to_le_bytes());
    data.extend_from_slice(&(8u64 * 1024 * 1024).to_le_bytes());
    data.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    let central_size = data.len() as u32 - central_offset;
    data.extend_from_slice(b"PK\x05\x06");
    data.extend_from_slice(&[0; 4]);
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&1u16.to_le_bytes());
    data.extend_from_slice(&central_size.to_le_bytes());
    data.extend_from_slice(&central_offset.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    fs::write(fixture.0.join("input/upload.zip"), &data).unwrap();
    let mut reader = open_unchecked(data);
    let member = reader.by_index(0).unwrap();
    assert_eq!(
        (member.compressed_size(), member.size()),
        (claimed_compressed.into(), claimed_inflated.into())
    );
    drop(member);
    assert_eq!(
        extract(&fixture).unwrap_err().to_string(),
        "io: File is larger than its declared uncompressed size"
    );
    assert!(!fixture.candidate().exists());
}

fn find_validated_eocd_offset(data: &[u8]) -> usize {
    let tail_size = data.len().min(65_557);
    let tail_start = data.len() - tail_size;
    let tail = &data[tail_start..];
    for i in (0..tail_size.saturating_sub(21)).rev() {
        if tail[i..i + 4] != *b"PK\x05\x06" {
            continue;
        }
        let comment_len = u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as usize;
        if i + 22 + comment_len != tail_size {
            continue;
        }
        let footer = &tail[i..];
        let central_size = u32::from_le_bytes(footer[12..16].try_into().unwrap()) as u64;
        let central_offset = u32::from_le_bytes(footer[16..20].try_into().unwrap()) as u64;
        let eocd_offset = tail_start + i;
        if central_offset + central_size == eocd_offset as u64 {
            return eocd_offset;
        }
    }
    panic!("validated EOCD");
}

fn set_archive_comment(data: &mut Vec<u8>, comment: &[u8]) {
    let o = find_validated_eocd_offset(data);
    data.truncate(o + 22);
    data.extend_from_slice(comment);
    data[o + 20..o + 22].copy_from_slice(&(comment.len() as u16).to_le_bytes());
}

fn fake_eocd_comment(decoy_offset: u32, decoy_size: u32, entries: u16) -> Vec<u8> {
    let mut comment = vec![b' '; 4096];
    let mut fake = Vec::from(*b"PK\x05\x06");
    fake.extend_from_slice(&0u16.to_le_bytes());
    fake.extend_from_slice(&0u16.to_le_bytes());
    fake.extend_from_slice(&entries.to_le_bytes());
    fake.extend_from_slice(&entries.to_le_bytes());
    fake.extend_from_slice(&decoy_size.to_le_bytes());
    fake.extend_from_slice(&decoy_offset.to_le_bytes());
    fake.extend_from_slice(&0u16.to_le_bytes());
    assert_eq!(fake.len(), 22);
    comment.extend(fake);
    comment
}

#[test]
fn archive_comment_cannot_embed_a_second_end_record() {
    let fixture = Fixture::new();
    fixture.zip(1);
    let path = fixture.0.join("input/upload.zip");
    let mut data = fs::read(&path).unwrap();
    // A decoy central directory large enough that the ZIP crate would over-allocate
    // if it indexed the fake EOCD placed at the end of the archive comment.
    let o = find_validated_eocd_offset(&data);
    set_archive_comment(
        &mut data,
        &fake_eocd_comment(o as u32, 56 * 1024 * 1024, 10_000),
    );
    fs::write(&path, data).unwrap();
    assert!(matches!(
        extract(&fixture),
        Err(ArchiveError::InvalidArchive)
    ));
    assert!(!fixture.candidate().exists());
}

#[test]
fn ordinary_archive_comment_is_accepted() {
    let fixture = Fixture::new();
    fixture.zip(1);
    let path = fixture.0.join("input/upload.zip");
    let mut data = fs::read(&path).unwrap();
    set_archive_comment(&mut data, b"fixture note");
    fs::write(&path, data).unwrap();
    extract(&fixture).unwrap();
    assert!(!fixture.candidate().exists());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}
/// A stored local header for `part.stl` whose sizes are both `size`.
fn stored_local_header(crc: u32, size: u32, extra_len: u16) -> Vec<u8> {
    let mut header = Vec::from(*b"PK\x03\x04");
    header.extend_from_slice(&20u16.to_le_bytes());
    header.extend_from_slice(&[0; 8]);
    header.extend_from_slice(&crc.to_le_bytes());
    header.extend_from_slice(&size.to_le_bytes());
    header.extend_from_slice(&size.to_le_bytes());
    header.extend_from_slice(&8u16.to_le_bytes());
    header.extend_from_slice(&extra_len.to_le_bytes());
    header.extend_from_slice(b"part.stl");
    header
}
/// A stored central header for `part.stl` carrying `extra`.
fn stored_central_header(crc: u32, size: u32, local_offset: u32, extra: &[u8]) -> Vec<u8> {
    let mut header = Vec::from(*b"PK\x01\x02");
    header.extend_from_slice(&45u16.to_le_bytes());
    header.extend_from_slice(&45u16.to_le_bytes());
    header.extend_from_slice(&[0; 8]);
    header.extend_from_slice(&crc.to_le_bytes());
    header.extend_from_slice(&size.to_le_bytes());
    header.extend_from_slice(&size.to_le_bytes());
    header.extend_from_slice(&8u16.to_le_bytes());
    header.extend_from_slice(&(extra.len() as u16).to_le_bytes());
    header.extend_from_slice(&[0; 10]);
    header.extend_from_slice(&local_offset.to_le_bytes());
    header.extend_from_slice(b"part.stl");
    header.extend_from_slice(extra);
    header
}
fn end_record(central_offset: usize, central_size: usize) -> Vec<u8> {
    let mut record = Vec::from(*b"PK\x05\x06");
    record.extend_from_slice(&[0; 4]);
    record.extend_from_slice(&1u16.to_le_bytes());
    record.extend_from_slice(&1u16.to_le_bytes());
    record.extend_from_slice(&(central_size as u32).to_le_bytes());
    record.extend_from_slice(&(central_offset as u32).to_le_bytes());
    record.extend_from_slice(&0u16.to_le_bytes());
    record
}
fn open_unchecked(data: Vec<u8>) -> zip::ZipArchive<std::io::Cursor<Vec<u8>>> {
    zip::ZipArchive::with_config(
        zip::read::Config {
            archive_offset: zip::read::ArchiveOffset::Known(0),
        },
        std::io::Cursor::new(data),
    )
    .unwrap()
}

#[test]
fn zip64_extra_cannot_redirect_the_reader_to_an_unvalidated_local_header() {
    // part.stl's local extra field hides a second local header at offset 38.
    // The central ZIP64 field keeps the checked sizes and names that offset
    // without a sentinel asking for it. zip 8.6 followed it and streamed
    // bytes preflight never bound; the CRC is forged for that decoy view.
    let fixture = Fixture::new();
    let decoy = b"decoy bytes preflight never validated";
    let payload = vec![b'p'; 4096];
    let size = payload.len() as u32;
    let view_start = 38 + 38;
    let mut view = decoy.to_vec();
    view.extend_from_slice(&payload[..payload.len() - decoy.len()]);
    let crc = crc32(&view);
    let mut data = stored_local_header(crc, size, (38 + decoy.len()) as u16);
    data.extend(stored_local_header(crc, size, 0));
    data.extend_from_slice(decoy);
    assert_eq!(data.len(), view_start + decoy.len());
    data.extend_from_slice(&payload);
    let central_offset = data.len();
    let mut zip64 = vec![1, 0, 24, 0];
    zip64.extend_from_slice(&(size as u64).to_le_bytes());
    zip64.extend_from_slice(&(size as u64).to_le_bytes());
    zip64.extend_from_slice(&38u64.to_le_bytes());
    data.extend(stored_central_header(crc, size, 0, &zip64));
    let central_size = data.len() - central_offset;
    data.extend(end_record(central_offset, central_size));
    fs::write(fixture.0.join("input/upload.zip"), &data).unwrap();

    // The reader stays on the validated header, so it streams the payload and
    // the CRC forged for the decoy view rejects it.
    let mut reader = open_unchecked(data);
    assert_eq!(reader.central_directory_start(), central_offset as u64);
    let mut member = reader.by_index(0).unwrap();
    assert_eq!(member.header_start(), 0);
    let mut produced = vec![];
    let error = std::io::Read::read_to_end(&mut member, &mut produced).unwrap_err();
    assert_eq!(error.to_string(), "Invalid checksum");
    assert_eq!(produced, payload[..produced.len()]);

    assert_eq!(
        extract(&fixture).unwrap_err().to_string(),
        "io: Invalid checksum"
    );
    assert!(!fixture.candidate().exists());
}

#[test]
fn malformed_extra_field_cannot_divert_the_reader_to_an_earlier_directory() {
    // The validated central entry ends in a truncated ZIP64 extra header. zip
    // 9 fails to parse it and falls back to an end record hidden in
    // part.stl's local extra field, indexing a decoy directory (and a decoy
    // local header) that preflight never walked.
    let fixture = Fixture::new();
    let decoy = b"decoy bytes preflight never validated";
    let payload = vec![b'p'; 512];
    let size = payload.len() as u32;
    let mut view = decoy.to_vec();
    view.extend_from_slice(&payload[..payload.len() - decoy.len()]);
    let crc = crc32(&view);
    let decoy_offset = 38;
    let decoy_central = stored_central_header(crc, size, 38 + 54 + 22, &[]);
    let mut hidden = decoy_central.clone();
    hidden.extend(end_record(decoy_offset, decoy_central.len()));
    hidden.extend(stored_local_header(crc, size, 0));
    hidden.extend_from_slice(decoy);
    let mut data = stored_local_header(crc, size, hidden.len() as u16);
    data.extend(hidden);
    data.extend_from_slice(&payload);
    let central_offset = data.len();
    data.extend(stored_central_header(crc, size, 0, &[1, 0]));
    let central_size = data.len() - central_offset;
    data.extend(end_record(central_offset, central_size));
    fs::write(fixture.0.join("input/upload.zip"), &data).unwrap();

    // Pre-fix preflight treated extra fields as opaque and accepted this
    // directory; the opened reader then passed the old count/name/size checks
    // while indexing the decoy directory and streaming the decoy view.
    let mut reader = open_unchecked(data);
    assert_eq!(reader.central_directory_start(), decoy_offset as u64);
    assert_ne!(reader.central_directory_start(), central_offset as u64);
    assert_eq!(reader.len(), 1);
    let mut member = reader.by_index(0).unwrap();
    assert_eq!(
        (member.name_raw(), member.compressed_size(), member.size()),
        (&b"part.stl"[..], size as u64, size as u64)
    );
    let mut produced = vec![];
    std::io::Read::read_to_end(&mut member, &mut produced).unwrap();
    assert_eq!(produced, view);
    assert_ne!(produced, payload);
    drop(member);

    let error = extract(&fixture).unwrap_err();
    assert!(
        matches!(error, ArchiveError::MalformedExtraField),
        "{error:?}"
    );
    assert_eq!(error.to_string(), "invalid-archive: malformed extra field");
    assert!(!fixture.candidate().exists());
}

#[test]
fn well_formed_extra_fields_are_accepted() {
    let fixture = Fixture::new();
    let payload = b"solid part";
    let size = payload.len() as u32;
    let crc = crc32(payload);
    let mut data = stored_local_header(crc, size, 0);
    data.extend_from_slice(payload);
    let central_offset = data.len();
    // Extended timestamp (mtime only), Info-ZIP unix, then an unknown field.
    let mut extra = vec![0x55, 0x54, 5, 0, 1, 0, 0, 0, 0];
    extra.extend_from_slice(&[0x75, 0x78, 11, 0, 1, 4, 0xe8, 3, 0, 0, 4, 0xe8, 3, 0, 0]);
    extra.extend_from_slice(&[0xcd, 0xab, 2, 0, 9, 9]);
    data.extend(stored_central_header(crc, size, 0, &extra));
    let central_size = data.len() - central_offset;
    data.extend(end_record(central_offset, central_size));
    fs::write(fixture.0.join("input/upload.zip"), &data).unwrap();
    extract(&fixture).unwrap();
    assert!(!fixture.candidate().exists());
}

#[test]
fn truncated_or_overrunning_extra_records_are_rejected() {
    let payload = b"solid part";
    let size = payload.len() as u32;
    let crc = crc32(payload);
    let mut short_zip64 = vec![1, 0, 8, 0];
    short_zip64.extend_from_slice(&0u64.to_le_bytes());
    let cases: [&[u8]; 6] = [
        // A record header cut short.
        &[0xcd, 0xab, 2],
        // A record whose length overruns the extra field.
        &[0xcd, 0xab, 9, 0, 1, 2],
        // An empty extended timestamp.
        &[0x55, 0x54, 0, 0],
        // Extended timestamp flags that disagree with its length.
        &[0x55, 0x54, 9, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
        // NTFS with the wrong length.
        &[0x0a, 0, 4, 0, 0, 0, 0, 0],
        // ZIP64 too short to replace every saturated 32-bit value.
        &short_zip64,
    ];
    for (index, extra) in cases.iter().enumerate() {
        let fixture = Fixture::new();
        let local_size = if index == 5 { u32::MAX } else { size };
        let mut data = stored_local_header(crc, local_size, 0);
        data.extend_from_slice(payload);
        let central_offset = data.len();
        let mut central = stored_central_header(crc, size, 0, extra);
        if index == 5 {
            // Saturate the central sizes so a ZIP64 field needs 16 bytes.
            central[20..28].copy_from_slice(&[0xff; 8]);
            data[18..26].copy_from_slice(&[0xff; 8]);
        }
        data.extend(central);
        let central_size = data.len() - central_offset;
        data.extend(end_record(central_offset, central_size));
        fs::write(fixture.0.join("input/upload.zip"), &data).unwrap();
        let error = extract(&fixture).unwrap_err();
        assert!(
            matches!(error, ArchiveError::MalformedExtraField),
            "case {index}: {error:?}"
        );
        assert!(!fixture.candidate().exists());
    }
}

#[test]
fn discarding_readiness_keeps_the_lock_for_a_running_owner_materialization() {
    let fixture = Fixture::new();
    fixture.zip(1);
    // Large enough that copying and syncing outlasts the discard below.
    let mut big = File::create(fixture.0.join("input/part.stl")).unwrap();
    for _ in 0..256 {
        big.write_all(&[3; 1024 * 1024]).unwrap();
    }
    drop(big);
    let mut owner = fixture.tenant().source(42).unwrap();
    let extracted = owner
        .extract_zip(
            fixture.input(),
            ArchiveLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let revisions = fixture.0.join("repos/42/revisions");
    let finished = Arc::new(AtomicBool::new(false));
    let worker = {
        let input = LocalFiles::open(&fixture.0.join("input")).unwrap();
        let finished = finished.clone();
        thread::spawn(move || {
            let result = owner.materialize(
                local_request("owner-local"),
                &input,
                ArtifactBudget::new(1 << 30, 1 << 30).unwrap(),
            );
            finished.store(true, Ordering::Release);
            result
        })
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !revisions.join(".pp-source-candidate").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_micros(200));
    }
    extracted.discard().unwrap();
    let peer = pp_os::Dir::open(&revisions).unwrap();
    let acquired = peer.try_lock().is_ok();
    let running = !finished.load(Ordering::Acquire);
    assert!(running, "materialization finished before the check");
    assert!(!acquired, "a peer took the lock during the owner's copy");
    let snapshot = worker.join().unwrap().unwrap();
    assert_eq!(snapshot.files[0].size_bytes, 256 * 1024 * 1024);
    peer.try_lock().unwrap();
}

#[path = "../../pp-storage/tests/common/cleanup.rs"]
mod cleanup;

#[test]
fn scratch_root_is_empty_after_tests() {
    cleanup::assert_test_run_clean(&["--skip", "scratch_root_is_empty_after_tests"]);
}
