//! GRE-1007: STEP part trees, read once per file hash and answered from the
//! cache under `thumbs/sources/step`.
use super::*;
use pp_domain::step_assembly::PartNode;
use sha2::Digest;
use std::time::Instant;

const NESTED: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/step/nested-assembly.step"
));

impl Fixture {
    fn step(&self, path: &str) -> Result<StepConversion, SourceFilesError> {
        let session = Credential::Session(Secret::new(self.token.clone()));
        self.client.step_conversion(session, 1, path, &IDLE, WAIT)
    }

    /// Opens `path` until the worker finishes with it.
    fn settled(&self, path: &str) -> StepConversion {
        let started = Instant::now();
        loop {
            match self.step(path).unwrap() {
                StepConversion::Queued | StepConversion::Converting => {
                    assert!(started.elapsed() < WAIT, "{path} never settled");
                    std::thread::sleep(Duration::from_millis(5));
                }
                settled => return settled,
            }
        }
    }

    fn step_cache(&self) -> Vec<String> {
        let folder = self.directory.path().join("thumbs/sources/step");
        let Ok(entries) = fs::read_dir(folder) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

fn depth(node: &PartNode) -> usize {
    1 + node.children.iter().map(depth).max().unwrap_or(0)
}

fn done(conversion: StepConversion) -> (String, pp_domain::step_assembly::PartTree) {
    match conversion {
        StepConversion::Done { sha256, tree } => (sha256, tree),
        other => panic!("not done: {other:?}"),
    }
}

#[test]
fn the_same_bytes_are_converted_once_and_later_opens_load_from_the_cache() {
    let fixture = Fixture::new();
    fixture.snapshot(
        "1/revisions/r1",
        &[("cad/printer.step", NESTED), ("copy/Printer.STP", NESTED)],
    );
    fixture.sql(FIRST);

    // The first open only queues the file.
    assert_eq!(
        fixture.step("cad/printer.step").unwrap(),
        StepConversion::Queued
    );
    let (hash, tree) = done(fixture.settled("cad/printer.step"));
    assert_eq!(hash, sha256(NESTED));
    assert_eq!(tree.roots.len(), 1);
    assert_eq!(tree.roots[0].name, "Printer");
    assert_eq!(depth(&tree.roots[0]), 6);
    assert_eq!(fixture.client.step_files_converted(), 1);
    let name = format!("{hash}-v{PART_TREE_FORMAT}.json");
    assert_eq!(fixture.step_cache(), std::slice::from_ref(&name));

    // The cached file is the nested tree.
    let cached: serde_json::Value = serde_json::from_slice(
        &fs::read(
            fixture
                .directory
                .path()
                .join("thumbs/sources/step")
                .join(&name),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(cached["format"], 1);
    assert_eq!(cached["sha256"], hash.as_str());
    let nozzle = &cached["roots"][0]["children"][0]["children"][0]["children"][0]["children"][0]["children"]
        [0];
    assert_eq!(nozzle["name"], "D\u{fc}se 0.4");

    // Opening again, and opening a copy at another path, reads no STEP.
    assert_eq!(done(fixture.step("cad/printer.step").unwrap()).1, tree);
    assert_eq!(done(fixture.settled("copy/Printer.STP")).1, tree);
    assert_eq!(fixture.client.step_files_converted(), 1);

    // After a restart (a new client on the same data folder) the first open
    // is queued, then answered from the cache without a conversion.
    let restarted = fixture.owner.source_files(POLICY).unwrap();
    let fixture = Fixture {
        client: restarted.skipping_at_root(WRITER_ENTRY),
        ..fixture
    };
    assert_eq!(done(fixture.settled("cad/printer.step")).1, tree);
    assert_eq!(fixture.client.step_files_converted(), 0);
    assert_eq!(fixture.step_cache(), [name]);
}

#[test]
fn a_file_that_cannot_be_read_fails_and_caches_nothing() {
    let fixture = Fixture::new();
    fixture.snapshot(
        "1/revisions/r1",
        &[
            ("not-step.step", b"solid cube\nendsolid\n"),
            (
                "points.stp",
                b"ISO-10303-21;\nDATA;\n#1=CARTESIAN_POINT('',(0.,0.,0.));\nENDSEC;\n",
            ),
        ],
    );
    fixture.sql(FIRST);
    assert_eq!(
        fixture.settled("not-step.step"),
        StepConversion::Failed(StepFailure::NotStep)
    );
    assert_eq!(
        fixture.settled("points.stp"),
        StepConversion::Failed(StepFailure::NoProducts)
    );
    // The failure is kept: a later open does not queue the file again.
    assert_eq!(
        fixture.step("not-step.step").unwrap(),
        StepConversion::Failed(StepFailure::NotStep)
    );
    assert!(fixture.step_cache().is_empty());
    assert_eq!(fixture.client.step_files_converted(), 0);
}

#[test]
fn only_a_real_step_file_of_the_current_revision_is_opened() {
    let fixture = Fixture::new();
    let not_found = |result| matches!(result, Err(SourceFilesError::FileNotFound));
    assert!(not_found(fixture.step("part.step")), "no revision yet");
    let root = fixture.snapshot(
        "1/revisions/r1",
        &[
            ("part.step", NESTED),
            ("part.stl", b"solid"),
            ("dir.step/x", b""),
        ],
    );
    fixture.sql(FIRST);
    assert!(not_found(fixture.step("missing.step")));
    assert!(not_found(fixture.step("part.stl")), "not a STEP extension");
    assert!(not_found(fixture.step("dir.step")), "a folder");
    assert!(not_found(fixture.step("../r1/part.step")));
    assert!(not_found(fixture.step("/part.step")));
    assert!(not_found(
        fixture.step(&format!("{WRITER_ENTRY}/part.step"))
    ));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("part.step"), root.join("link.step")).unwrap();
        assert!(not_found(fixture.step("link.step")), "a link");
    }
    let session = Credential::Session(Secret::new(fixture.token.clone()));
    assert!(matches!(
        fixture
            .client
            .step_conversion(session, 2, "part.step", &IDLE, WAIT),
        Err(SourceFilesError::NotFound)
    ));
    assert!(fixture.step_cache().is_empty());
}

#[test]
fn a_deleted_or_damaged_cache_file_is_converted_again() {
    let fixture = Fixture::new();
    fixture.snapshot("1/revisions/r1", &[("printer.step", NESTED)]);
    fixture.sql(FIRST);
    let (hash, tree) = done(fixture.settled("printer.step"));
    let cached = fixture
        .directory
        .path()
        .join("thumbs/sources/step")
        .join(format!("{hash}-v{PART_TREE_FORMAT}.json"));
    fs::write(&cached, b"{\"format\":1").unwrap();

    assert_eq!(
        fixture.step("printer.step").unwrap(),
        StepConversion::Queued
    );
    assert_eq!(done(fixture.settled("printer.step")).1, tree);
    assert_eq!(fixture.client.step_files_converted(), 2);
}

#[test]
fn a_thumbnail_prune_keeps_the_step_cache() {
    let fixture = Fixture::new();
    fixture.snapshot(
        "1/revisions/r1",
        &[
            ("printer.step", NESTED),
            ("part.stl", &one_triangle_binary()),
        ],
    );
    fixture.sql(FIRST);
    done(fixture.settled("printer.step"));
    fixture.plant("99/stale-v1.png");
    // The revision's first thumbnail makes its folder and prunes.
    fixture.thumbnail("part.stl").unwrap();
    assert!(!fixture.revision_folders().contains(&"99".to_owned()));
    assert_eq!(fixture.step_cache().len(), 1);
}
