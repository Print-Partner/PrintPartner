//! The published `pp-source-media` CLI must apply the same reader binding as
//! ZIP extraction. An unsaturated 24-byte ZIP64 extra names a local header
//! hidden in the checked entry's extra field. zip 8.6 followed it; zip 9 and
//! preflight keep the central offset, so the CRC forged for the hidden view
//! rejects the entry before any model parse or snapshot publish.
use pp_source::{
    LocalFiles, SourcePath, TenantRepos,
    archive::{ArchiveLimits, ZipInput},
};
use serde_json::Value;
use std::{fs, io::Write, process::Stdio, sync::atomic::AtomicBool};

#[path = "support/process.rs"]
mod support;

const MODEL_NAME: &str = "3D/main.model";
const PART_MODEL: &str = "<model><object name='part'><mesh><vertex x='0' y='0' z='0'/><vertex x='1' y='0' z='0'/><vertex x='0' y='1' z='0'/><triangle v1='0' v2='1' v3='2'/></mesh></object></model>";
const INJECTED_MODEL: &str = "<model><object name='injected'><mesh><vertex x='0' y='0' z='0'/><vertex x='1' y='0' z='0'/><vertex x='0' y='1' z='0'/><triangle v1='0' v2='1' v3='2'/></mesh></object></model>";

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

/// Removed when the scratch drops, including while a test unwinds from a panic.
struct Fixture(Scratch);
impl Fixture {
    fn new() -> Self {
        let path = Scratch::new("pp-source-bind-");
        fs::create_dir(path.join("repos")).unwrap();
        fs::create_dir(path.join("input")).unwrap();
        for directory in [path.to_path_buf(), path.join("repos"), path.join("input")] {
            modes::pin_mode(directory, 0o755);
        }
        Self(path)
    }
    fn command(&self) -> Value {
        serde_json::json!({
            "tenantId": "review",
            "sourceId": 1,
            "reposDir": self.0.join("repos"),
            "inputDir": self.0.join("input"),
            "revisionKey": "review",
            "files": ["model.3mf"],
        })
    }
}

fn invoke(fixture: &Fixture) -> std::process::Output {
    let mut child = support::command(support::bin_exe("pp-source-media"))
        .current_dir(&fixture.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(fixture.command().to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

fn local_header(
    name: &str,
    crc: u32,
    compressed: u32,
    uncompressed: u32,
    extra_len: u16,
) -> Vec<u8> {
    let mut header = Vec::from(*b"PK\x03\x04");
    header.extend_from_slice(&20u16.to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&crc.to_le_bytes());
    header.extend_from_slice(&compressed.to_le_bytes());
    header.extend_from_slice(&uncompressed.to_le_bytes());
    header.extend_from_slice(&(name.len() as u16).to_le_bytes());
    header.extend_from_slice(&extra_len.to_le_bytes());
    header.extend_from_slice(name.as_bytes());
    header
}

fn central_header(
    name: &str,
    crc: u32,
    compressed: u32,
    uncompressed: u32,
    local_offset: u32,
    extra: &[u8],
) -> Vec<u8> {
    let mut header = Vec::from(*b"PK\x01\x02");
    header.extend_from_slice(&45u16.to_le_bytes());
    header.extend_from_slice(&45u16.to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&crc.to_le_bytes());
    header.extend_from_slice(&compressed.to_le_bytes());
    header.extend_from_slice(&uncompressed.to_le_bytes());
    header.extend_from_slice(&(name.len() as u16).to_le_bytes());
    header.extend_from_slice(&(extra.len() as u16).to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&local_offset.to_le_bytes());
    header.extend_from_slice(name.as_bytes());
    header.extend_from_slice(extra);
    header
}

fn end_record(count: u16, central_offset: usize, central_size: usize) -> Vec<u8> {
    let mut record = Vec::from(*b"PK\x05\x06");
    record.extend_from_slice(&0u32.to_le_bytes());
    record.extend_from_slice(&count.to_le_bytes());
    record.extend_from_slice(&count.to_le_bytes());
    record.extend_from_slice(&(central_size as u32).to_le_bytes());
    record.extend_from_slice(&(central_offset as u32).to_le_bytes());
    record.extend_from_slice(&0u16.to_le_bytes());
    record
}

fn stored_model(xml: &str, zip64: bool) -> Vec<u8> {
    let payload = xml.as_bytes();
    let size = u32::try_from(payload.len()).unwrap();
    let crc = crc32(payload);
    let mut data = local_header(MODEL_NAME, crc, size, size, 0);
    data.extend_from_slice(payload);
    let central_offset = data.len();
    let extra;
    let (compressed, uncompressed, local_offset) = if zip64 {
        extra = {
            let mut field = vec![1, 0, 24, 0];
            field.extend_from_slice(&u64::from(size).to_le_bytes());
            field.extend_from_slice(&u64::from(size).to_le_bytes());
            field.extend_from_slice(&0u64.to_le_bytes());
            field
        };
        (u32::MAX, u32::MAX, u32::MAX)
    } else {
        extra = Vec::new();
        (size, size, 0)
    };
    data.extend(central_header(
        MODEL_NAME,
        crc,
        compressed,
        uncompressed,
        local_offset,
        &extra,
    ));
    let central_size = data.len() - central_offset;
    data.extend(end_record(1, central_offset, central_size));
    data
}

fn stored_model_with_sibling(xml: &str, sibling_name: &str, sibling: &[u8]) -> Vec<u8> {
    let payload = xml.as_bytes();
    let size = u32::try_from(payload.len()).unwrap();
    let crc = crc32(payload);
    let mut data = local_header(MODEL_NAME, crc, size, size, 0);
    data.extend_from_slice(payload);
    let sibling_size = u32::try_from(sibling.len()).unwrap();
    let sibling_crc = crc32(sibling);
    let sibling_offset = u32::try_from(data.len()).unwrap();
    data.extend(local_header(
        sibling_name,
        sibling_crc,
        sibling_size,
        sibling_size,
        0,
    ));
    data.extend_from_slice(sibling);
    let central_offset = data.len();
    data.extend(central_header(MODEL_NAME, crc, size, size, 0, &[]));
    data.extend(central_header(
        sibling_name,
        sibling_crc,
        sibling_size,
        sibling_size,
        sibling_offset,
        &[],
    ));
    let central_size = data.len() - central_offset;
    data.extend(end_record(2, central_offset, central_size));
    data
}

/// Same redirect shape as the archive extraction regression, aimed at the
/// direct 3MF member: the checked payload is only `p`, and the XML for object
/// `injected` lives in a local header concealed by the central extra field.
fn redirected_model() -> Vec<u8> {
    let decoy = INJECTED_MODEL.as_bytes();
    let payload = vec![b'p'; 4096];
    let size = payload.len() as u32;
    let local_len = 30 + MODEL_NAME.len();
    let mut view = decoy.to_vec();
    view.extend_from_slice(&payload[..payload.len() - decoy.len()]);
    let crc = crc32(&view);
    let mut data = local_header(
        MODEL_NAME,
        crc,
        size,
        size,
        u16::try_from(local_len + decoy.len()).unwrap(),
    );
    data.extend(local_header(MODEL_NAME, crc, size, size, 0));
    data.extend_from_slice(decoy);
    assert_eq!(data.len(), local_len * 2 + decoy.len());
    data.extend_from_slice(&payload);
    let central_offset = data.len();
    let mut zip64 = vec![1, 0, 24, 0];
    zip64.extend_from_slice(&u64::from(size).to_le_bytes());
    zip64.extend_from_slice(&u64::from(size).to_le_bytes());
    zip64.extend_from_slice(&(local_len as u64).to_le_bytes());
    data.extend(central_header(MODEL_NAME, crc, size, size, 0, &zip64));
    let central_size = data.len() - central_offset;
    data.extend(end_record(1, central_offset, central_size));
    data
}

fn assert_published_part(output: &std::process::Output) {
    assert!(output.status.success(), "{output:?}");
    let parsed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(parsed.get("error").is_none(), "{parsed}");
    let files = parsed["snapshot"]["files"].as_array().unwrap();
    assert!(
        files.iter().any(|file| file["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("_3mf/model/part.stl"))),
        "{parsed}"
    );
    assert!(
        files.iter().all(|file| file["path"]
            .as_str()
            .is_none_or(|path| !path.contains("injected"))),
        "{parsed}"
    );
    assert_eq!(parsed["snapshot"]["publication"], "created");
}

#[test]
fn direct_model_redirect_is_rejected_before_publish_and_valid_models_still_convert() {
    let malicious = Fixture::new();
    let bytes = redirected_model();
    let model = malicious.0.join("input/model.3mf");
    fs::write(&model, &bytes).unwrap();
    let local = LocalFiles::open(&malicious.0.join("input")).unwrap();
    let mut source = TenantRepos::open("review".into(), &malicious.0.join("repos"))
        .unwrap()
        .source(2)
        .unwrap();
    match source.extract_zip(
        ZipInput::open(
            &local,
            &SourcePath::try_from("model.3mf".to_owned()).unwrap(),
        )
        .unwrap(),
        ArchiveLimits::default(),
        &AtomicBool::new(false),
    ) {
        Err(extracted) => assert_eq!(extracted.to_string(), "io: Invalid checksum"),
        Ok(_) => panic!("redirected archive was extracted"),
    }
    assert_eq!(fs::read(&model).unwrap(), bytes);

    let media = invoke(&malicious);
    assert!(!media.status.success(), "{media:?}");
    let parsed: Value = serde_json::from_slice(&media.stdout).unwrap();
    assert_eq!(parsed["error"], "io: Invalid checksum");
    assert_eq!(fs::read(&model).unwrap(), bytes);
    assert!(!malicious.0.join("repos/1/revisions/review").exists());
    assert!(
        !malicious
            .0
            .join("repos")
            .join("_3mf/model/injected.stl")
            .exists()
    );
    for entry in fs::read_dir(malicious.0.join("repos")).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            for nested in fs::read_dir(&path).unwrap().flatten() {
                let name = nested.file_name();
                assert!(
                    !name.to_string_lossy().contains("injected"),
                    "published {}",
                    nested.path().display()
                );
            }
        }
    }

    let direct = Fixture::new();
    fs::write(
        direct.0.join("input/model.3mf"),
        stored_model(PART_MODEL, false),
    )
    .unwrap();
    assert_published_part(&invoke(&direct));

    let zip64 = Fixture::new();
    fs::write(
        zip64.0.join("input/model.3mf"),
        stored_model(PART_MODEL, true),
    )
    .unwrap();
    assert_published_part(&invoke(&zip64));

    let sibling = Fixture::new();
    let package =
        stored_model_with_sibling(PART_MODEL, "Metadata/trailing.bin", &[0; 2 * 1024 * 1024]);
    let sibling_model = sibling.0.join("input/model.3mf");
    fs::write(&sibling_model, &package).unwrap();
    let published = invoke(&sibling);
    assert_published_part(&published);
    let parsed: Value = serde_json::from_slice(&published.stdout).unwrap();
    let read_bytes = parsed["media"]["conversions"][0]["modelRead"]["readBytes"]
        .as_u64()
        .unwrap();
    assert!(read_bytes < 200_000, "{read_bytes}");
    assert_eq!(fs::read(&sibling_model).unwrap(), package);
}

#[path = "support/modes.rs"]
mod modes;
