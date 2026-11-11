use pp_source::{
    ArtifactBudget, LocalFiles, SelectedFile, Selection, SnapshotRequest, SourcePath, TenantRepos,
    media::{MediaError, MediaLimits, discover_import_rules},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use zip::{ZipWriter, write::SimpleFileOptions};
#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

/// Removed when the scratch drops, including while a test unwinds from a panic.
struct Fixture(Scratch);
impl Fixture {
    fn new() -> Self {
        let root = Scratch::new("pp-source-media-");
        // Pin modes so source roots accept them under a permissive umask.
        for dir in ["repos", "input"] {
            let builder = &mut fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(builder, 0o700);
            builder.create(root.join(dir)).unwrap();
        }
        Self(root)
    }
    fn model(&self, triangles: usize) {
        self.model_named("part", triangles);
    }
    fn model_named(&self, name: &str, triangles: usize) {
        let mut zip = ZipWriter::new(File::create(self.0.join("input/model.3mf")).unwrap());
        zip.start_file("3D/main.model", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(format!("<model><object name='{name}'><mesh><vertex x='0' y='0' z='0'/><vertex x='1' y='0' z='0'/><vertex x='0' y='1' z='0'/>").as_bytes()).unwrap();
        for _ in 0..triangles {
            zip.write_all(b"<triangle v1='0' v2='1' v3='2'/>").unwrap();
        }
        zip.write_all(b"</mesh></object></model>").unwrap();
        zip.finish().unwrap();
    }
    fn model_with_large_padding(&self) {
        self.model(1);
        let path = self.0.join("input/model.3mf");
        let bytes = fs::read(&path).unwrap();
        let end = bytes.len() - 22;
        let central_offset =
            u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap()) as usize;
        let original_central = &bytes[central_offset..end];
        let name = b"padding.bin";
        let padding = pp_source::archive::MAX_COMPRESSED_BYTES;
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(&bytes[..central_offset]).unwrap();
        let padding_offset = central_offset as u32;
        file.write_all(b"PK\x03\x04").unwrap();
        file.write_all(&20u16.to_le_bytes()).unwrap();
        file.write_all(&[0; 8]).unwrap();
        file.write_all(&0u32.to_le_bytes()).unwrap();
        file.write_all(&(padding as u32).to_le_bytes()).unwrap();
        file.write_all(&(padding as u32).to_le_bytes()).unwrap();
        file.write_all(&(name.len() as u16).to_le_bytes()).unwrap();
        file.write_all(&0u16.to_le_bytes()).unwrap();
        file.write_all(name).unwrap();
        let central_start = central_offset as u64 + 30 + name.len() as u64 + padding;
        file.set_len(central_start).unwrap();
        file.seek(SeekFrom::Start(central_start)).unwrap();
        file.write_all(original_central).unwrap();
        file.write_all(b"PK\x01\x02").unwrap();
        file.write_all(&20u16.to_le_bytes()).unwrap();
        file.write_all(&20u16.to_le_bytes()).unwrap();
        file.write_all(&[0; 8]).unwrap();
        file.write_all(&0u32.to_le_bytes()).unwrap();
        file.write_all(&(padding as u32).to_le_bytes()).unwrap();
        file.write_all(&(padding as u32).to_le_bytes()).unwrap();
        file.write_all(&(name.len() as u16).to_le_bytes()).unwrap();
        file.write_all(&[0; 8]).unwrap();
        file.write_all(&0u32.to_le_bytes()).unwrap();
        file.write_all(&padding_offset.to_le_bytes()).unwrap();
        file.write_all(name).unwrap();
        let central_size = original_central.len() as u32 + 46 + name.len() as u32;
        file.write_all(b"PK\x05\x06").unwrap();
        file.write_all(&[0; 4]).unwrap();
        file.write_all(&2u16.to_le_bytes()).unwrap();
        file.write_all(&2u16.to_le_bytes()).unwrap();
        file.write_all(&central_size.to_le_bytes()).unwrap();
        file.write_all(&(central_start as u32).to_le_bytes())
            .unwrap();
        file.write_all(&0u16.to_le_bytes()).unwrap();
    }
    fn local(&self) -> LocalFiles {
        LocalFiles::open(&self.0.join("input")).unwrap()
    }
    fn tenant(&self) -> TenantRepos {
        TenantRepos::open("tenant".into(), &self.0.join("repos")).unwrap()
    }
    fn candidate(&self) -> PathBuf {
        self.0
            .join("repos/42/revisions/.pp-source-media/.candidate")
    }
}
fn path(value: &str) -> SourcePath {
    SourcePath::try_from(value.to_owned()).unwrap()
}
fn prepare_model(
    f: &Fixture,
    source: &mut pp_source::SourceRoot,
) -> pp_source::media::PreparedMedia {
    source
        .prepare_media(
            &f.local(),
            &[path("model.3mf")],
            &[],
            MediaLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
}
fn media_request(key: &str, files: Vec<SelectedFile>) -> SnapshotRequest {
    SnapshotRequest {
        upstream_revision_key: key.into(),
        files,
        selection: Selection {
            max_stl_files: 500,
            max_documentation_bytes: 10000,
            omitted_files: vec![],
        },
    }
}

#[test]
fn conversion_owns_candidate_and_published_original_and_derived_bytes_survive_reuse() {
    let f = Fixture::new();
    f.model(1);
    let original = fs::read(f.0.join("input/model.3mf")).unwrap();
    let tenant = f.tenant();
    let mut source = tenant.source(42).unwrap();
    let prepared = prepare_model(&f, &mut source);
    assert_eq!(prepared.receipt().conversions[0].result.object_count, 1);
    let mut second = tenant.source(42).unwrap();
    assert!(matches!(
        second.prepare_media(
            &f.local(),
            &[path("model.3mf")],
            &[],
            MediaLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(MediaError::Busy)
    ));
    assert!(f.candidate().join("_3mf/model/part.stl").is_file());
    let request = media_request("revision", prepared.receipt().selected_files.clone());
    let snapshot = source
        .materialize(
            request.clone(),
            prepared.files(),
            ArtifactBudget::new(100000, 10000).unwrap(),
        )
        .unwrap();
    let derived = fs::read(
        f.0.join("repos")
            .join(&snapshot.snapshot_locator)
            .join("_3mf/model/part.stl"),
    )
    .unwrap();
    prepared.discard().unwrap();
    assert!(!f.candidate().exists());
    f.model(2);
    let prepared = source
        .prepare_media(
            &f.local(),
            &[path("model.3mf")],
            &[],
            MediaLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let reused = source
        .materialize(
            request,
            prepared.files(),
            ArtifactBudget::new(100000, 10000).unwrap(),
        )
        .unwrap();
    assert_eq!(reused.manifest_digest, snapshot.manifest_digest);
    assert_eq!(reused.publication, pp_source::Publication::Reused);
    assert_eq!(
        fs::read(
            f.0.join("repos")
                .join(&snapshot.snapshot_locator)
                .join("model.3mf")
        )
        .unwrap(),
        original
    );
    assert_eq!(
        fs::read(
            f.0.join("repos")
                .join(&snapshot.snapshot_locator)
                .join("_3mf/model/part.stl")
        )
        .unwrap(),
        derived
    );
    drop(prepared);
    assert!(!f.candidate().exists());
}

/// A mesh named after a Windows device converts as before on Unix. Windows
/// appends `_` to the generated stem, because pp-source picks that name.
#[test]
fn device_named_mesh_imports_with_a_portable_name_only_on_windows() {
    let f = Fixture::new();
    f.model_named("CON", 1);
    let mut source = f.tenant().source(42).unwrap();
    let prepared = prepare_model(&f, &mut source);
    let derived = if cfg!(windows) {
        "_3mf/model/con_.stl"
    } else {
        "_3mf/model/con.stl"
    };
    let selected = &prepared.receipt().selected_files;
    assert!(
        selected.iter().any(|file| file.path.as_str() == derived),
        "{selected:?}"
    );
    let snapshot = source
        .materialize(
            media_request("device-mesh", selected.clone()),
            prepared.files(),
            ArtifactBudget::new(100000, 10000).unwrap(),
        )
        .unwrap();
    let published = f.0.join("repos").join(&snapshot.snapshot_locator);
    assert!(published.join(derived).is_file());
    prepared.discard().unwrap();
}

#[test]
fn package_larger_than_archive_cap_uses_media_limits() {
    let f = Fixture::new();
    f.model_with_large_padding();
    let mut source = f.tenant().source(42).unwrap();
    let prepared = source
        .prepare_media(
            &f.local(),
            &[path("model.3mf")],
            &[],
            MediaLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(prepared.receipt().original_bytes > pp_source::archive::MAX_COMPRESSED_BYTES);
    assert_eq!(prepared.receipt().conversions[0].result.object_count, 1);
    prepared.discard().unwrap();
}
#[test]
fn failure_after_writing_a_facet_cleans_all_private_files_and_retry_succeeds() {
    let f = Fixture::new();
    f.model(2);
    let mut source = f.tenant().source(42).unwrap();
    let result = source.prepare_media(
        &f.local(),
        &[path("model.3mf")],
        &[],
        MediaLimits {
            max_output_bytes: 170,
            ..MediaLimits::default()
        },
        &AtomicBool::new(false),
    );
    assert!(matches!(
        result,
        Err(MediaError::Invalid(
            "3MF derived STL output exceeds the size limit"
        ))
    ));
    assert!(!f.candidate().exists());
    assert!(f.0.join("input/model.3mf").exists());
    source
        .prepare_media(
            &f.local(),
            &[path("model.3mf")],
            &[],
            MediaLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .discard()
        .unwrap();
}
#[test]
fn cancelled_live_conversion_cleans_derived_files_and_retries() {
    let f = Fixture::new();
    f.model(20000);
    let mut source = f.tenant().source(42).unwrap();
    let cancelled = AtomicBool::new(false);
    let output = f.candidate().join("_3mf/model/part.stl");
    // Cancel once the header and a facet are written; the next write's check sees it.
    let mut cancelled_at = None;
    let result = source.prepare_media_on_write(
        &f.local(),
        &[path("model.3mf")],
        &[],
        MediaLimits::default(),
        &cancelled,
        |written: u64| {
            if written > 100 && cancelled_at.is_none() {
                assert!(output.is_file());
                cancelled.store(true, Ordering::Relaxed);
                cancelled_at = Some(written);
            }
        },
    );
    assert!(matches!(
        result,
        Err(MediaError::Archive(
            pp_source::archive::ArchiveError::Cancelled
        ))
    ));
    // One header and one facet: well short of 20,000 facets.
    assert!(cancelled_at.is_some_and(|written| written < 1_000));
    assert!(!f.candidate().exists());
    cancelled.store(false, Ordering::Relaxed);
    source
        .prepare_media(
            &f.local(),
            &[path("model.3mf")],
            &[],
            MediaLimits::default(),
            &cancelled,
        )
        .unwrap()
        .discard()
        .unwrap();
}
#[cfg(unix)]
#[test]
fn original_capability_survives_path_replacement_and_output_symlinks_are_confined() {
    let f = Fixture::new();
    f.model(1);
    let local = f.local();
    fs::rename(f.0.join("input"), f.0.join("moved")).unwrap();
    std::os::unix::fs::symlink(f.0.join("repos"), f.0.join("input")).unwrap();
    let mut source = f.tenant().source(42).unwrap();
    let prepared = source
        .prepare_media(
            &local,
            &[path("model.3mf")],
            &[],
            MediaLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(prepared.receipt().conversions[0].result.object_count, 1);
    prepared.discard().unwrap();
    fs::remove_dir(f.0.join("repos/42/revisions/.pp-source-media")).unwrap();
    std::os::unix::fs::symlink(
        f.0.join("moved"),
        f.0.join("repos/42/revisions/.pp-source-media"),
    )
    .unwrap();
    assert!(
        source
            .prepare_media(
                &local,
                &[path("model.3mf")],
                &[],
                MediaLimits::default(),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert!(f.0.join("moved/model.3mf").is_file());
}
#[test]
fn discovery_keeps_empty_directories_and_uses_filesystem_name_order_with_printables_after_directories()
 {
    let f = Fixture::new();
    fs::create_dir(f.0.join("input/wrapper")).unwrap();
    assert_eq!(discover_import_rules(&f.local()).unwrap(), vec!["wrapper/"]);
    fs::create_dir(f.0.join("input/𐀀")).unwrap();
    fs::create_dir(f.0.join("input/\u{e000}")).unwrap();
    fs::write(f.0.join("input/a.STL"), b"stl").unwrap();
    fs::write(f.0.join("input/B.3MF"), b"opaque").unwrap();
    fs::write(f.0.join("input/ignored.zip"), b"zip").unwrap();
    assert_eq!(
        discover_import_rules(&f.local()).unwrap(),
        vec!["wrapper/", "\u{e000}/", "𐀀/", "B.3MF", "a.STL"]
    );
}
#[test]
fn original_derived_collisions_refuse_without_overwrite() {
    let f = Fixture::new();
    f.model(1);
    fs::create_dir_all(f.0.join("input/_3mf/model")).unwrap();
    fs::write(f.0.join("input/_3mf/model/part.stl"), b"immutable original").unwrap();
    let mut source = f.tenant().source(42).unwrap();
    assert!(
        source
            .prepare_media(
                &f.local(),
                &[path("model.3mf"), path("_3mf/model/part.stl")],
                &[],
                MediaLimits::default(),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(
        fs::read(f.0.join("input/_3mf/model/part.stl")).unwrap(),
        b"immutable original"
    );
    assert!(!f.candidate().exists());
}
