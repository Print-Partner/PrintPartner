#![cfg(feature = "desktop_rebuild")]
//! A captured ZIP is unpacked by the import worker before media preparation,
//! and files of every type stay in the revision (GRE-1444). Synthetic files
//! only.

use pp_core::source_acquisition::{CaptureRequest, CapturedFile, SourceAcquisitions};
use pp_source::SourcePath;
use pp_storage::{
    Limits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    catalog::CreateSource,
    jobs::Credential,
    uploads::{AdmissionLimits, CapturedPayloadV1, State, Target},
};
use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use zip::{ZipWriter, write::SimpleFileOptions};

fn policy() -> AuthPolicy {
    AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant: SessionTenantPolicy::AccountTenant,
        first_user: FirstUserTenant::NewUser,
    }
}

/// A ZIP holding `entries`, written in memory.
fn zipped(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        zip.start_file(*name, SimpleFileOptions::default()).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// A 3MF with one triangle in an object named `part`.
fn model() -> Vec<u8> {
    zipped(&[(
        "3D/main.model",
        b"<model><object name='part'><mesh><vertex x='0' y='0' z='0'/><vertex x='1' y='0' z='0'/>\
          <vertex x='0' y='1' z='0'/><triangle v1='0' v2='1' v3='2'/></mesh></object></model>",
    )])
}

struct Imports {
    _root: tempfile::TempDir,
    data: PathBuf,
    owner: WriterOwner,
    acquisitions: SourceAcquisitions,
}

impl Imports {
    fn open() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = std::fs::canonicalize(root.path()).unwrap().join("data");
        let owner = WriterOwner::open(&data, Limits::default()).unwrap().0;
        let acquisitions = SourceAcquisitions::new(&owner, policy(), 1 << 34, 1).unwrap();
        acquisitions.recover_before_accepting().unwrap();
        Self {
            _root: root,
            data,
            owner,
            acquisitions,
        }
    }

    /// Captures `bytes` as the upload `name` into `target`, ZIP or not, and
    /// waits for the import to settle.
    fn import(&self, key: &str, target: Target, name: &str, bytes: Vec<u8>) -> State {
        let payload = if name.ends_with(".zip") {
            CapturedPayloadV1::zip(name.into())
        } else {
            CapturedPayloadV1::files(vec![name.into()])
        };
        let request = CaptureRequest {
            credential: Credential::PhysicalOwner(self.owner.job_physical_owner()),
            operation_key: key.into(),
            target,
            payload,
            limits: AdmissionLimits {
                reserved_bytes: 3_506_438_144,
                max_input_bytes: 268_435_456,
                max_prepared_bytes: 1_073_741_824,
            },
            files: vec![CapturedFile {
                path: SourcePath::try_from(name.to_owned()).unwrap(),
                input: Box::new(Cursor::new(bytes)),
            }],
        };
        let reply = self
            .acquisitions
            .acquire(request, &AtomicBool::new(false))
            .unwrap();
        match reply.wait() {
            Ok(operation) => operation.state,
            Err(_) => State::Failed,
        }
    }

    /// The Source's current revision, read from storage.
    fn current_revision(&self) -> Option<i64> {
        rusqlite::Connection::open(self.data.join("print-partner.db"))
            .unwrap()
            .query_row(
                "SELECT current_source_revision_id FROM projects ORDER BY id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
}

fn create() -> Target {
    Target::Create {
        metadata: Box::new(CreateSource {
            name: "Kit".into(),
            source_kind: Some("local".into()),
            source_type: Some("local".into()),
            ..Default::default()
        }),
    }
}

/// Every published file below `repos`, relative to its revision folder.
fn published(data: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![data.join("repos")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if let Some((_, rest)) = path.to_string_lossy().split_once("/revisions/") {
                found.push(
                    rest.split_once('/')
                        .map_or(rest, |(_, file)| file)
                        .to_owned(),
                );
            }
        }
    }
    found.sort();
    found
}

#[test]
fn a_zip_publishes_folders_the_derived_stl_and_every_other_file() {
    let imports = Imports::open();
    let kit = zipped(&[
        ("body/frame.stl", b"solid frame\nendsolid frame\n"),
        ("body/arms/left.stl", b"solid left\nendsolid left\n"),
        ("model.3mf", &model()),
        ("guide.pdf", b"%PDF-1.4 synthetic"),
        ("photo.png", b"not really a png"),
    ]);
    assert_eq!(
        imports.import("kit", create(), "kit.zip", kit),
        State::Activated
    );
    let files = published(&imports.data);
    for expected in [
        "body/frame.stl",
        "body/arms/left.stl",
        "model.3mf",
        "_3mf/model/part.stl",
        "guide.pdf",
        "photo.png",
    ] {
        assert!(
            files.iter().any(|file| file == expected),
            "{expected} in {files:?}"
        );
    }
    assert!(
        !files.iter().any(|file| file.ends_with("kit.zip")),
        "{files:?}"
    );
}

#[test]
fn a_refused_zip_fails_its_job_and_keeps_the_previous_revision() {
    let imports = Imports::open();
    let first = zipped(&[("a.stl", b"solid a\nendsolid a\n")]);
    assert_eq!(
        imports.import("first", create(), "first.zip", first),
        State::Activated
    );
    let revision = imports.current_revision();
    assert!(revision.is_some());
    let existing = || Target::Existing { source_id: 1 };

    let escaping = zipped(&[("../outside.stl", b"solid o\nendsolid o\n")]);
    let names = (0..10_001)
        .map(|index| format!("f{index}.stl"))
        .collect::<Vec<_>>();
    let many = zipped(
        &names
            .iter()
            .map(|name| (name.as_str(), b"x".as_slice()))
            .collect::<Vec<_>>(),
    );
    for (key, bytes) in [
        ("escaping", escaping),
        ("many", many),
        ("not-a-zip", b"this is not a zip archive".to_vec()),
    ] {
        assert_eq!(
            imports.import(key, existing(), "upload.zip", bytes),
            State::Failed,
            "{key}"
        );
        assert_eq!(imports.current_revision(), revision, "{key}");
    }
}
