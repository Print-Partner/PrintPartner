//! T-10-draftreads (GRE-1052): the production `SourceReads` adapter replays
//! the Node preparation fixtures on real checkouts, walks like Node's
//! `scanRepo`, and refuses locator escapes and exhausted budgets.
use super::observation::*;
use super::preparation::*;
use super::preparation_tests::{cases, database, options, projection};
use super::source_reads::SourceReads;
use rusqlite::params;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

struct Data(PathBuf);
impl Data {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pp-draftreads-{}", rand::random::<u64>()));
        fs::create_dir_all(root.join("repos/1")).unwrap();
        Self(root)
    }
    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
    fn reads(&self) -> SourceReads {
        SourceReads::new(&self.0)
    }
}
impl Drop for Data {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}
fn stl(purpose: StlPurpose, source: i64, path: &str) -> OwnedStlReadRequest {
    OwnedStlReadRequest {
        tenant: "default".into(),
        source_id: source,
        path: Some(path.into()),
        purpose,
    }
}
fn checkout(source: i64, path: &Path) -> OwnedCheckoutReadRequest {
    OwnedCheckoutReadRequest {
        tenant: "default".into(),
        source_id: source,
        path: path.to_str().unwrap().into(),
    }
}
fn catalog(purpose: CatalogPurpose) -> OwnedCatalogReadRequest {
    OwnedCatalogReadRequest { purpose }
}
fn document(read: ReadResult<DocumentRead>) -> String {
    match read {
        Ok(DocumentRead::Missing) => "missing".into(),
        Ok(DocumentRead::Skipped(kind)) => format!("skipped {kind:?}"),
        Ok(DocumentRead::Bytes(bytes)) => String::from_utf8(bytes).unwrap(),
        Err(error) => format!("{error:?}"),
    }
}
fn failure<T>(result: ReadResult<T>) -> String {
    format!("{:?}", result.err().expect("read refused"))
}
fn scan(
    reads: &SourceReads,
    request: &OwnedStlReadRequest,
    b: &mut PreparationBudget<'_>,
) -> ReadResult<(Option<String>, Box<dyn StlInventory>)> {
    let mut root = reads.resolve_stl_root(request, b)?;
    let logical = root.logical_resolved_path().map(str::to_owned);
    Ok((logical, root.scan_stls(b)?))
}
fn paths(inventory: &dyn StlInventory) -> Vec<(String, usize)> {
    inventory
        .entries()
        .iter()
        .map(|e| (e.physical_relative_path.clone(), e.traversal_ordinal))
        .collect()
}

#[test]
fn ticket_t_10_draftreads_preparation_matches_node_on_real_checkouts() {
    let mut replayed = 0;
    for case in cases().into_iter().filter(|case| case["capture"] != true) {
        let (root, mut connection) = database(&case);
        let sources: &[i64] = if case["second"] == true {
            &[1, 2]
        } else {
            &[1]
        };
        for &id in sources {
            let checkout = root.path().join(format!("repos/{id}/checkout"));
            fs::create_dir_all(&checkout).unwrap();
            if case["name"] != "no-root" {
                connection
                    .execute(
                        "UPDATE projects SET local_path=?1 WHERE id=?2",
                        params![checkout.to_str().unwrap(), id],
                    )
                    .unwrap();
            }
            for file in case["files"].as_array().unwrap() {
                if file["source"] != id {
                    continue;
                }
                let (path, content) = (file["path"].as_str().unwrap(), file["content"].as_str());
                write(&checkout.join(path), content.unwrap().as_bytes());
                if id == 1 {
                    let revision = root.path().join("repos/1/revisions/accepted").join(path);
                    write(&revision, content.unwrap().as_bytes());
                }
            }
            if let Some(doc) = case["docs"][id.to_string()].as_str() {
                write(
                    &checkout.join("print-partner.manifest.yaml"),
                    doc.as_bytes(),
                );
            }
        }
        if let Some(hints) = case["hints"].as_str() {
            write(&root.path().join("path-hints.yaml"), hints.as_bytes());
        }
        if case["unresolvableCheckout"] == true {
            fs::remove_dir_all(root.path().join("repos/1/checkout")).unwrap();
        }
        let reads = SourceReads::new(root.path());
        let cancelled = AtomicBool::new(false);
        let mut b = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
        let result = prepare(
            &mut connection,
            "default",
            1,
            case["base"].clone(),
            PreparationContext {
                options: &options(&case),
                repos: &root.path().join("repos"),
            },
            &reads,
            &mut b,
        );
        let actual = match result {
            Ok(prepared) => projection(prepared),
            Err(e) => match e.downcast_ref::<Failure>() {
                Some(Failure::NoLayers) => json!({"kind":"no_layers"}),
                Some(Failure::NoStls) => json!({"kind":"no_stls"}),
                Some(Failure::WouldWipe) => json!({"kind":"would_wipe"}),
                None => json!({"error":e.to_string()}),
            },
        };
        assert_eq!(actual, case["result"], "{}", case["name"]);
        replayed += 1;
    }
    assert_eq!(replayed, 42);
}

#[test]
fn ticket_t_10_draftreads_scan_walks_like_node() {
    let data = Data::new();
    let root = data.path("repos/1/checkout");
    for (path, body) in [
        ("b.STL", "b"),
        ("a.stl", "solid a"),
        (".hidden/x.stl", "x"),
        ("dir/z.stl", "z"),
        ("dir/sub/y.Stl", "y"),
        ("dir.stl/inner.stl", "i"),
        ("notes.txt", "n"),
        ("print-partner.manifest.yaml", "option_groups: {}\n"),
    ] {
        write(&root.join(path), body.as_bytes());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("a.stl"), root.join("link.stl")).unwrap();
        std::os::unix::fs::symlink(root.join("dir"), root.join("linked")).unwrap();
    }
    let reads = data.reads();
    let cancelled = AtomicBool::new(false);
    let mut b = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
    let stored = root.to_str().unwrap();
    for purpose in [StlPurpose::Untracked, StlPurpose::CheckoutOptions] {
        let (logical, mut inventory) = scan(&reads, &stl(purpose, 1, stored), &mut b).unwrap();
        assert_eq!(logical.as_deref(), Some(stored));
        assert_eq!(
            paths(&*inventory),
            [
                (".hidden/x.stl".into(), 0),
                ("a.stl".into(), 1),
                ("b.STL".into(), 2),
                ("dir/sub/y.Stl".into(), 3),
                ("dir/z.stl".into(), 4),
                ("dir.stl/inner.stl".into(), 5),
            ]
        );
        let artifact = inventory.hash_tracked_winner(1, &mut b).unwrap();
        assert_eq!(artifact.byte_count, 7);
        assert_eq!(
            artifact.byte_sha256,
            hex::encode(Sha256::digest(b"solid a"))
        );
    }
    assert_eq!(
        document(reads.read_checkout_manifest(
            &checkout(1, &root),
            CheckoutManifestUse::OptionGroups,
            &mut b
        )),
        "option_groups: {}\n"
    );
    // An absent checkout inside the workspace scans nothing, as Node's
    // `listStlRelativePaths` returns `[]`.
    let absent = data.path("repos/1/absent");
    let (logical, inventory) = scan(
        &reads,
        &stl(StlPurpose::Untracked, 1, absent.to_str().unwrap()),
        &mut b,
    )
    .unwrap();
    assert_eq!(logical.as_deref(), absent.to_str());
    assert!(inventory.entries().is_empty());
    assert_eq!(
        document(reads.read_checkout_manifest(
            &checkout(1, &absent),
            CheckoutManifestUse::PartRulesAndDefaults,
            &mut b
        )),
        "missing"
    );
    // A directory where the manifest belongs is skipped, not read.
    write(
        &data.path("repos/1/dir-manifest/print-partner.manifest.yaml/x"),
        b"",
    );
    assert_eq!(
        document(reads.read_checkout_manifest(
            &checkout(1, &data.path("repos/1/dir-manifest")),
            CheckoutManifestUse::OptionGroups,
            &mut b
        )),
        "skipped IsADirectory"
    );
    let revision = data.path("repos/1/revisions/r1");
    write(&revision.join("part.stl"), b"rev");
    let (logical, mut inventory) = scan(
        &reads,
        &stl(StlPurpose::Revision, 1, "1/revisions/r1"),
        &mut b,
    )
    .unwrap();
    assert_eq!(
        logical.as_deref(),
        pp_os::display_path(&revision.canonicalize().unwrap()).to_str()
    );
    assert_eq!(paths(&*inventory), [("part.stl".into(), 0)]);
    assert_eq!(
        inventory
            .hash_tracked_winner(0, &mut b)
            .unwrap()
            .byte_sha256,
        hex::encode(Sha256::digest(b"rev"))
    );
}

#[test]
fn ticket_t_10_draftreads_catalog_documents_match_node_locations() {
    let data = Data::new();
    let reads = data.reads();
    let cancelled = AtomicBool::new(false);
    let mut b = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
    assert_eq!(
        document(reads.read_catalog_document(&catalog(CatalogPurpose::ShippedHintsFirst), &mut b)),
        include_str!("../../../../../../web/apps/server/src/data/path-hints.yaml")
    );
    for purpose in [
        CatalogPurpose::ShippedHintsSecond,
        CatalogPurpose::CustomHints,
        CatalogPurpose::Community(CommunitySlug::parse("kit").unwrap()),
    ] {
        assert_eq!(
            document(reads.read_catalog_document(&catalog(purpose), &mut b)),
            "missing"
        );
    }
    write(&data.path("path-hints.yaml"), b"version: 1\nrules: []\n");
    assert_eq!(
        document(reads.read_catalog_document(&catalog(CatalogPurpose::CustomHints), &mut b)),
        "version: 1\nrules: []\n"
    );
    fs::remove_file(data.path("path-hints.yaml")).unwrap();
    fs::create_dir(data.path("path-hints.yaml")).unwrap();
    assert_eq!(
        document(reads.read_catalog_document(&catalog(CatalogPurpose::CustomHints), &mut b)),
        "skipped IsADirectory"
    );
    write(&data.path("manifests/kit.yaml"), b"selections: {}\n");
    let reads = reads.with_community_manifests(data.path("manifests"));
    let community = |slug| {
        catalog(CatalogPurpose::Community(
            CommunitySlug::parse(slug).unwrap(),
        ))
    };
    assert_eq!(
        document(reads.read_catalog_document(&community("kit"), &mut b)),
        "selections: {}\n"
    );
    assert_eq!(
        document(reads.read_catalog_document(&community("other"), &mut b)),
        "missing"
    );
}

#[test]
fn ticket_t_10_draftreads_refuses_locator_escapes() {
    let data = Data::new();
    write(&data.path("repos/1/checkout/a.stl"), b"a");
    write(&data.path("repos/1/revisions/r1/a.stl"), b"a");
    write(
        &data.path("repos/2/checkout/print-partner.manifest.yaml"),
        b"x: 1\n",
    );
    write(
        &data.path("outside/print-partner.manifest.yaml"),
        b"secret: 1\n",
    );
    write(&data.path("outside/secret.stl"), b"secret");
    let reads = data.reads();
    let cancelled = AtomicBool::new(false);
    let mut b = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
    for locator in [
        "1/../2/checkout",
        "2/checkout",
        "1/revisions/missing",
        "1/revisions/r1/a.stl",
        "/1/revisions/r1",
        "1/./revisions/r1",
        "1",
    ] {
        assert_eq!(
            failure(reads.resolve_stl_root(&stl(StlPurpose::Revision, 1, locator), &mut b)),
            "UnsafeLocator",
            "{locator}"
        );
    }
    let repos = data.path("repos");
    for stored in [
        repos.join("1/../2/checkout"),
        repos.join("2/checkout"),
        data.path("outside"),
        PathBuf::from("repos/1/checkout"),
    ] {
        for purpose in [StlPurpose::Untracked, StlPurpose::CheckoutOptions] {
            let root = reads
                .resolve_stl_root(&stl(purpose, 1, stored.to_str().unwrap()), &mut b)
                .unwrap();
            assert_eq!(root.logical_resolved_path(), None, "{stored:?}");
        }
        assert_eq!(
            document(reads.read_checkout_manifest(
                &checkout(1, &stored),
                CheckoutManifestUse::OptionGroups,
                &mut b
            )),
            "skipped PermissionDenied",
            "{stored:?}"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let checkout_root = data.path("repos/1/checkout");
        // The checkout itself, its manifest, and a revision leave the workspace.
        symlink(data.path("outside"), data.path("repos/1/escape")).unwrap();
        symlink(data.path("outside"), data.path("repos/1/revisions/out")).unwrap();
        symlink(data.path("repos/1"), data.path("repos/3")).unwrap();
        let escape = data.path("repos/1/escape");
        let root = reads
            .resolve_stl_root(
                &stl(StlPurpose::Untracked, 1, escape.to_str().unwrap()),
                &mut b,
            )
            .unwrap();
        assert_eq!(root.logical_resolved_path(), None);
        assert_eq!(
            document(reads.read_checkout_manifest(
                &checkout(1, &escape),
                CheckoutManifestUse::OptionGroups,
                &mut b
            )),
            "skipped PermissionDenied"
        );
        for (source, locator) in [(1, "1/revisions/out"), (3, "3/revisions/r1")] {
            assert_eq!(
                failure(
                    reads.resolve_stl_root(&stl(StlPurpose::Revision, source, locator), &mut b)
                ),
                "UnsafeLocator",
                "{locator}"
            );
        }
        let workspace_link = data.path("repos/3/checkout");
        let root = reads
            .resolve_stl_root(
                &stl(StlPurpose::Untracked, 3, workspace_link.to_str().unwrap()),
                &mut b,
            )
            .unwrap();
        assert_eq!(root.logical_resolved_path(), None);
        symlink(
            data.path("outside/print-partner.manifest.yaml"),
            checkout_root.join("print-partner.manifest.yaml"),
        )
        .unwrap();
        assert_eq!(
            document(reads.read_checkout_manifest(
                &checkout(1, &checkout_root),
                CheckoutManifestUse::OptionGroups,
                &mut b
            )),
            "skipped PermissionDenied"
        );
        fs::create_dir(data.path("manifests")).unwrap();
        symlink(
            data.path("outside/secret.stl"),
            data.path("manifests/kit.yaml"),
        )
        .unwrap();
        let community = reads.with_community_manifests(data.path("manifests"));
        assert_eq!(
            document(community.read_catalog_document(
                &catalog(CatalogPurpose::Community(
                    CommunitySlug::parse("kit").unwrap()
                )),
                &mut b
            )),
            "skipped PermissionDenied"
        );
        // A scanned winner swapped for an escaping symlink is not hashed.
        let reads = data.reads();
        let (_, mut inventory) = scan(
            &reads,
            &stl(StlPurpose::Revision, 1, "1/revisions/r1"),
            &mut b,
        )
        .unwrap();
        fs::remove_file(data.path("repos/1/revisions/r1/a.stl")).unwrap();
        symlink(
            data.path("outside/secret.stl"),
            data.path("repos/1/revisions/r1/a.stl"),
        )
        .unwrap();
        assert_eq!(
            failure(inventory.hash_tracked_winner(0, &mut b)),
            "UnsafeLocator"
        );
    }
}

#[test]
fn ticket_t_10_draftreads_charges_the_budget_before_reads() {
    let data = Data::new();
    let root = data.path("repos/1/checkout");
    for path in ["a.stl", "b.stl", "c.stl", "deep/er/d.stl"] {
        write(&root.join(path), b"0123456789");
    }
    write(&root.join("print-partner.manifest.yaml"), b"0123456789");
    write(&data.path("path-hints.yaml"), b"0123456789");
    let reads = data.reads();
    let stored = root.to_str().unwrap();
    let request = stl(StlPurpose::Untracked, 1, stored);
    let refused = |limits: PreparationLimits,
                   cancelled: bool,
                   read: &dyn Fn(&mut PreparationBudget<'_>) -> String| {
        let cancelled = AtomicBool::new(cancelled);
        let mut b = PreparationBudget::new(&cancelled, limits).unwrap();
        read(&mut b)
    };
    let scanned = |b: &mut PreparationBudget<'_>| failure(scan(&reads, &request, b));
    let defaults = PreparationLimits::default();
    assert_eq!(
        refused(
            PreparationLimits {
                entries: 2,
                ..defaults
            },
            false,
            &scanned
        ),
        "LimitExceeded(Entries)"
    );
    assert_eq!(
        refused(
            PreparationLimits {
                depth: 1,
                ..defaults
            },
            false,
            &scanned
        ),
        "LimitExceeded(Depth)"
    );
    assert_eq!(
        refused(
            PreparationLimits {
                visits: 3,
                ..defaults
            },
            false,
            &scanned
        ),
        "LimitExceeded(Visits)"
    );
    assert_eq!(
        refused(
            PreparationLimits {
                path_bytes: 8,
                ..defaults
            },
            false,
            &scanned
        ),
        "LimitExceeded(PathBytes)"
    );
    let manifest = |b: &mut PreparationBudget<'_>| {
        document(reads.read_checkout_manifest(
            &checkout(1, &root),
            CheckoutManifestUse::OptionGroups,
            b,
        ))
    };
    let custom = |b: &mut PreparationBudget<'_>| {
        document(reads.read_catalog_document(&catalog(CatalogPurpose::CustomHints), b))
    };
    let shipped = |b: &mut PreparationBudget<'_>| {
        document(reads.read_catalog_document(&catalog(CatalogPurpose::ShippedHintsFirst), b))
    };
    for read in [
        &manifest as &dyn Fn(&mut PreparationBudget<'_>) -> String,
        &custom,
        &shipped,
    ] {
        assert_eq!(
            refused(
                PreparationLimits {
                    document_bytes: 4,
                    ..defaults
                },
                false,
                read
            ),
            "LimitExceeded(DocumentBytes)"
        );
        assert_eq!(refused(defaults, true, read), "Cancelled");
    }
    let hashed = |b: &mut PreparationBudget<'_>| {
        let (_, mut inventory) = scan(&reads, &request, b).unwrap();
        failure(inventory.hash_tracked_winner(0, b))
    };
    assert_eq!(
        refused(
            PreparationLimits {
                artifact_bytes: 4,
                ..defaults
            },
            false,
            &hashed
        ),
        "LimitExceeded(ArtifactBytes)"
    );
    let cancelled_root =
        |b: &mut PreparationBudget<'_>| failure(reads.resolve_stl_root(&request, b));
    assert_eq!(refused(defaults, true, &cancelled_root), "Cancelled");
    let revision = |b: &mut PreparationBudget<'_>| {
        failure(reads.resolve_stl_root(&stl(StlPurpose::Revision, 1, "1/checkout"), b))
    };
    assert_eq!(refused(defaults, true, &revision), "Cancelled");
    // Cancellation during the walk refuses the scan.
    let cancelled = AtomicBool::new(false);
    let mut b = PreparationBudget::new(&cancelled, defaults).unwrap();
    let mut root = reads.resolve_stl_root(&request, &mut b).unwrap();
    b.cancel_after_checks(2);
    assert_eq!(failure(root.scan_stls(&mut b)), "Cancelled");
}

/// GRE-1056: Node's trusted single-user policy reads a stored folder anywhere;
/// the isolated policy refuses it. Relative and dotted paths stay refused, and
/// snapshot locators stay under the workspace in both modes.
#[test]
fn ticket_t_10_draftreads_trusted_policy_reads_a_user_folder() {
    use super::source_reads::SourceFilesystemPolicy;
    let data = Data::new();
    write(
        &data.path("outside/print-partner.manifest.yaml"),
        b"user: 1\n",
    );
    write(&data.path("outside/part.stl"), b"stl");
    let outside = data.path("outside");
    let cancelled = AtomicBool::new(false);
    let mut b = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
    let trusted = data
        .reads()
        .with_filesystem_policy(SourceFilesystemPolicy::TrustedSingleUser);
    let isolated = data.reads();

    let (logical, inventory) = scan(
        &trusted,
        &stl(StlPurpose::Untracked, 1, outside.to_str().unwrap()),
        &mut b,
    )
    .unwrap();
    // Node reports the stored spelling, not the canonical path. Windows temp
    // paths can hold 8.3 short names, so the two differ there.
    assert_eq!(logical.as_deref(), outside.to_str());
    assert_eq!(paths(inventory.as_ref()), [("part.stl".to_owned(), 0)]);
    assert_eq!(
        document(trusted.read_checkout_manifest(
            &checkout(1, &outside),
            CheckoutManifestUse::OptionGroups,
            &mut b
        )),
        "user: 1\n"
    );

    let root = isolated
        .resolve_stl_root(
            &stl(StlPurpose::Untracked, 1, outside.to_str().unwrap()),
            &mut b,
        )
        .unwrap();
    assert_eq!(root.logical_resolved_path(), None);
    assert_eq!(
        document(isolated.read_checkout_manifest(
            &checkout(1, &outside),
            CheckoutManifestUse::OptionGroups,
            &mut b
        )),
        "skipped PermissionDenied"
    );

    let dotted = format!("{}/../outside", data.path("repos").display());
    for stored in ["outside", "repos/1/checkout", dotted.as_str()] {
        let root = trusted
            .resolve_stl_root(&stl(StlPurpose::Untracked, 1, stored), &mut b)
            .unwrap();
        assert_eq!(root.logical_resolved_path(), None, "{stored}");
    }
    // An absent folder scans nothing, as in the workspace case.
    let missing = data.path("gone");
    let (logical, inventory) = scan(
        &trusted,
        &stl(StlPurpose::Untracked, 1, missing.to_str().unwrap()),
        &mut b,
    )
    .unwrap();
    assert_eq!(logical.as_deref(), missing.to_str());
    assert!(inventory.entries().is_empty());
    assert_eq!(
        failure(trusted.resolve_stl_root(
            &stl(StlPurpose::Revision, 1, outside.to_str().unwrap()),
            &mut b
        )),
        "UnsafeLocator"
    );
}

#[test]
fn ticket_t_10_draftreads_filesystem_policy_follows_node_multi_user() {
    use super::source_reads::SourceFilesystemPolicy;
    use crate::auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy};
    let policy = |session_tenant| AuthPolicy {
        registration: RegistrationPolicy::Open,
        session_tenant,
        first_user: FirstUserTenant::NewUser,
    };
    assert_eq!(
        SourceFilesystemPolicy::for_auth(&policy(SessionTenantPolicy::SingleAccountDefault)),
        SourceFilesystemPolicy::TrustedSingleUser
    );
    assert_eq!(
        SourceFilesystemPolicy::for_auth(&policy(SessionTenantPolicy::AccountTenant)),
        SourceFilesystemPolicy::Isolated
    );
}

#[test]
fn path_hints_partial_and_corrupt_documents_preserve_last_good_read_and_recover() {
    let data = Data::new();
    let reads = data.reads();
    let cancelled = AtomicBool::new(false);
    let read = || {
        let mut budget = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
        match reads.read_catalog_document(&catalog(CatalogPurpose::CustomHints), &mut budget)? {
            DocumentRead::Bytes(bytes) => super::manifest::hints(&bytes, &mut budget),
            DocumentRead::Missing => Ok(Vec::new()),
            DocumentRead::Skipped(kind) => Err(ReadFailure::Io(kind)),
        }
    };
    assert!(read().unwrap().is_empty());
    let good = b"version: 1\nrules:\n  - path: chosen.stl\n    option_group: color\n    variant_id: blue\n    label: Blue\n";
    let path = data.path("path-hints.yaml");
    write(&path, good);
    let saved = read().unwrap();
    assert_eq!(saved.len(), 1);
    for invalid in [
        b"version: 1\nrules: [".as_slice(),
        b"not yaml hints",
        b"version: 1\nrules: null",
    ] {
        write(&data.path("hints-staged"), invalid);
        assert_eq!(
            read().unwrap(),
            saved,
            "a partial unpublished replacement leaves the last good read intact"
        );
        write(&path, invalid);
        assert!(read().is_err());
        write(&data.path("hints-staged"), good);
        fs::rename(data.path("hints-staged"), &path).unwrap();
        assert_eq!(read().unwrap(), saved);
    }
    fs::remove_file(path).unwrap();
    assert!(read().unwrap().is_empty());
}
