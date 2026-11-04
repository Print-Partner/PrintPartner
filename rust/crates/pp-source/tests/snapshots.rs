use pp_source::{ArtifactBudget, Publication, SnapshotRequest, TenantRepos};
use serde::Deserialize;
use std::{collections::BTreeMap, fs};

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;

#[derive(Deserialize)]
struct Fixture {
    tree: BTreeMap<String, String>,
    request: SnapshotRequest,
}

fn assert_fixture(name: &str, fixture: &str) {
    let fixture: Fixture = serde_json::from_str(fixture).unwrap();
    let root = Scratch::new("pp-source-snapshot-");
    let input = root.join("input");
    let repos = root.join("repos");
    // Pin modes so source roots accept them under a permissive umask.
    for dir in [&input, &repos] {
        let builder = &mut fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(builder, 0o700);
        builder.create(dir).unwrap();
    }
    for (path, content) in fixture.tree {
        let path = input.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    let repos = TenantRepos::open("tenant-fixture".into(), &repos).unwrap();
    let mut source = repos.source(42).unwrap();
    let budget = ArtifactBudget::new(1024 * 1024, 1024 * 1024).unwrap();
    let created = source
        .materialize_from_directory(fixture.request.clone(), &input, budget)
        .unwrap();
    assert_eq!(created.publication, Publication::Created);
    // Reuse must return the persisted selection even when inputs disappear.
    fs::remove_dir_all(&input).unwrap();
    let reused = source
        .materialize_from_directory(fixture.request, &input, budget)
        .unwrap();
    assert_eq!(reused.publication, Publication::Reused);
    assert_eq!(created.selection, reused.selection);

    let mut settings = insta::Settings::clone_current();
    settings.set_prepend_module_to_snapshot(false);
    settings.add_redaction(".manifestDigest", "[hash]");
    settings.add_redaction(".files[].sha256", "[hash]");
    // These DTOs currently have no timestamps; keep future volatile fields stable.
    settings.add_redaction(".**.createdAt", "[timestamp]");
    settings.add_redaction(".**.updatedAt", "[timestamp]");
    settings.bind(|| {
        insta::assert_json_snapshot!(format!("{name}_published_created"), created);
        insta::assert_json_snapshot!(format!("{name}_published_reused"), reused);
        insta::assert_json_snapshot!(format!("{name}_selection"), created.selection);
    });
}

#[test]
fn single_stl() {
    assert_fixture("single_stl", include_str!("fixtures/single_stl.json"));
}

#[test]
fn nested_mixed() {
    assert_fixture("nested_mixed", include_str!("fixtures/nested_mixed.json"));
}

#[test]
fn omitted_docs() {
    assert_fixture("omitted_docs", include_str!("fixtures/omitted_docs.json"));
}
