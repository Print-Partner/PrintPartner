#![cfg(test)]

use super::KitManifest;
use super::observation::{self, CapturedManifestGraph, ObservedBuilder};
use crate::working_drafts::observation::{
    ArtifactRead, CatalogPurpose, CommunitySlug, DocumentRead, DraftReads, InventoryPath,
    OwnedCatalogReadRequest, OwnedCheckoutReadRequest, OwnedStlReadRequest, PreparationBudget,
    PreparationLimits, ReadFailure, ReadResult, Resource, StlInventory, StlPurpose, StlRootRead,
    parse_builder_manifest,
};
use rusqlite::{Connection, params};
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

const TENANT: &str = "account-a";
const NODE_BUILDER: &[u8] = include_bytes!("../../tests/fixtures/manifest-builder-node.json");

struct Database {
    connection: Connection,
    directory: PathBuf,
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

impl Database {
    fn oracle() -> Self {
        let directory = crate::tests::directory("manifest-observation");
        let (connection, _) = crate::schema::initialize(
            &directory.join("print-partner.db"),
            0,
            "2026-10-16T00:00:00.000Z",
        )
        .unwrap();
        connection
            .execute(
                "INSERT INTO build_profiles(id,tenant_id,name,config_modified_at) VALUES(1,?1,'Oracle','2026-10-16T00:00:00.000Z')",
                [TENANT],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO app_settings(tenant_id,key,value) VALUES(?1,'kit_manifest_1','{\"selections\":{\"zeta\":\"z1\"}}')",
                [TENANT],
            )
            .unwrap();
        let sources = [
            (
                1,
                "Base Frame",
                "base",
                Some(r#"["﻿ toolhead", "/extras/skirt.stl", "", 7]"#),
                None,
            ),
            (2, "Addon Kit", "addon", None, Some("oracle-kit")),
            (3, "Fallback Source", "addon", None, None),
            (4, "Directory Manifest", "addon", None, None),
            (5, "No Checkout", "addon", None, None),
            (6, "Refused Root", "addon", None, None),
        ];
        for (id, name, layer_type, imported_paths, slug) in sources {
            let local_path = (id != 5).then(|| format!("/checkouts/{id}"));
            connection
                .execute(
                    "INSERT INTO projects(id,tenant_id,name,url,local_path,imported_paths,manifest_community_slug) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        id,
                        TENANT,
                        name,
                        format!("https://example.test/{}", name.replace(' ', "-")),
                        local_path,
                        imported_paths,
                        slug
                    ],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO profile_layers(tenant_id,profile_id,layer_order,layer_type,project_id) VALUES(?1,1,?2,?3,?2)",
                    params![TENANT, id, layer_type],
                )
                .unwrap();
        }
        Self {
            connection,
            directory,
        }
    }

    fn capture(&mut self, tenant: &str, build: i64) -> Option<CapturedManifestGraph> {
        let tx = self.connection.transaction().unwrap();
        observation::capture(&tx, tenant, build).unwrap()
    }

    fn basis_matches(&mut self, captured: &CapturedManifestGraph) -> bool {
        let tx = self.connection.transaction().unwrap();
        observation::basis_matches(&tx, captured).unwrap()
    }
}

#[derive(Default)]
struct Reads {
    manifests: BTreeMap<i64, Result<Vec<u8>, ErrorKind>>,
    allowed_roots: BTreeMap<i64, Vec<&'static str>>,
    catalog: BTreeMap<String, Result<Vec<u8>, ErrorKind>>,
    requests: Mutex<Vec<String>>,
}

impl Reads {
    fn oracle() -> Self {
        let mut addon = b"# caf\xff".to_vec();
        addon.extend_from_slice(
            b"\noption_groups:\n  toolhead:\n    label: Addon toolhead\n    variants:\n      - id: stock\n        label: Stock\n        parts: [\"toolhead/stock/**\", \"extra/**\"]\n      - id: mini\n        parts: [\"toolhead/mini/**\"]\n      - id: mini\n        label: Mini\n        parts: [\"toolhead/mini/*.stl\"]\n  alpha:\n    variants:\n      - id: enabled\n        parts: [\"nothing/**\"]\nselections:\n  toolhead: mini\n  alpha: [enabled]\n",
        );
        Self {
            manifests: BTreeMap::from([
                (1, Ok(b"format: print-partner-manifest-v2\nversion: 2\nproject: ignored-name\noption_groups:\n  toolhead:\n    label: Toolhead\n    variants:\n      - id: stock\n        parts: [\"toolhead/stock/**\"]\n      - id: custom\n        label: Custom\n        parts: [\"toolhead/custom/**\"]\n  \"7\":\n    rule: pick_any\n    max: 2\n    variants:\n      - id: skirt\n        parts: [skirt.stl]\nselections:\n  toolhead: stock\n  \"7\": [skirt]\n".to_vec())),
                (2, Ok(addon)),
                (3, Ok(b"not: [valid".to_vec())),
                (4, Err(ErrorKind::IsADirectory)),
                (6, Ok(b"selections:\n  refused: [never]\n".to_vec())),
            ]),
            allowed_roots: BTreeMap::from([
                (
                    1,
                    vec![
                        "toolhead/stock/a.stl",
                        "docs/ignored.stl",
                        "extras/skirt.stl",
                        "toolhead/custom/B.stl",
                    ],
                ),
                (2, vec!["toolhead/stock/c.stl", "toolhead/mini/m.stl"]),
                (
                    3,
                    vec![
                        "options/stock/a.stl",
                        "frame/rail_mount.stl",
                        "options/custom/b.stl",
                    ],
                ),
                (4, vec!["part.stl"]),
            ]),
            catalog: BTreeMap::from([
                ("shipped-first".into(), Ok(b"version: 2\nrules: []\n".to_vec())),
                (
                    "shipped-second".into(),
                    Ok(b"version: 1\nrules:\n  - path: \"**/electronics/**\"\n    option_group: controller\n    variant_id: stock\n".to_vec()),
                ),
                (
                    "custom".into(),
                    Ok(b"version: 1\nrules:\n  - path: \"**/rail_mount.stl\"\n    option_group: rail_mounts\n    variant_id: printed_mount\n".to_vec()),
                ),
                (
                    "community:oracle-kit".into(),
                    Ok(b"selections:\n  beta: x\n  toolhead: custom\n".to_vec()),
                ),
            ]),
            requests: Mutex::default(),
        }
    }

    fn log(&self, request: String) {
        self.requests.lock().unwrap().push(request);
    }
}

fn document(
    stored: Option<&Result<Vec<u8>, ErrorKind>>,
    budget: &mut PreparationBudget<'_>,
) -> ReadResult<DocumentRead> {
    match stored {
        None => Ok(DocumentRead::Missing),
        Some(Err(kind)) => Ok(DocumentRead::Skipped(*kind)),
        Some(Ok(bytes)) => {
            budget.document(bytes.len(), bytes.len())?;
            Ok(DocumentRead::Bytes(bytes.clone()))
        }
    }
}

impl DraftReads for Reads {
    fn resolve_stl_root(
        &self,
        request: &OwnedStlReadRequest,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlRootRead>> {
        budget.check()?;
        assert!(request.purpose() == StlPurpose::CheckoutOptions);
        self.log(format!(
            "root {} {} {}",
            request.tenant(),
            request.source_id(),
            request.stored_path().unwrap()
        ));
        Ok(Box::new(Root(
            self.allowed_roots
                .get(&request.source_id())
                .map(|paths| paths.iter().map(|path| path.to_string()).collect()),
        )))
    }

    fn read_checkout_manifest(
        &self,
        request: &OwnedCheckoutReadRequest,
        _occurrence: crate::working_drafts::observation::CheckoutManifestUse,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        self.log(format!(
            "manifest {} {} {}",
            request.tenant(),
            request.source_id(),
            request.stored_path()
        ));
        document(self.manifests.get(&request.source_id()), budget)
    }

    fn read_catalog_document(
        &self,
        request: &OwnedCatalogReadRequest,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        let key = match request.purpose() {
            CatalogPurpose::Community(slug) => format!("community:{}", slug.as_str()),
            CatalogPurpose::ShippedHintsFirst => "shipped-first".into(),
            CatalogPurpose::ShippedHintsSecond => "shipped-second".into(),
            CatalogPurpose::CustomHints => "custom".into(),
        };
        document(self.catalog.get(&key), budget)
    }
}

struct Root(Option<Vec<String>>);

impl StlRootRead for Root {
    fn logical_resolved_path(&self) -> Option<&str> {
        self.0.as_ref().map(|_| "/resolved")
    }

    fn scan_stls(
        &mut self,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlInventory>> {
        let mut entries = Vec::new();
        for (traversal_ordinal, path) in self.0.take().unwrap_or_default().into_iter().enumerate() {
            budget.entry(path.matches('/').count() + 1, path.len())?;
            entries.push(InventoryPath {
                physical_relative_path: path,
                traversal_ordinal,
            });
        }
        Ok(Box::new(Inventory(entries)))
    }
}

struct Inventory(Vec<InventoryPath>);

impl StlInventory for Inventory {
    fn entries(&self) -> &[InventoryPath] {
        &self.0
    }

    fn hash_tracked_winner(
        &mut self,
        _index: usize,
        _budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<ArtifactRead> {
        Err(ReadFailure::InvalidGrant)
    }
}

fn observe(
    captured: &CapturedManifestGraph,
    reads: Option<&dyn DraftReads>,
    limits: PreparationLimits,
    cancelled: bool,
) -> ReadResult<ObservedBuilder> {
    let cancelled = AtomicBool::new(cancelled);
    let mut budget = PreparationBudget::new(&cancelled, limits)?;
    observation::observe(captured, reads, &mut budget)
}

fn json(observed: &ObservedBuilder) -> String {
    let mut output = Vec::new();
    observed.builder.write_json(&mut output);
    String::from_utf8(output).unwrap()
}

#[test]
fn builder_bytes_equal_node_for_declared_inferred_fallback_and_community_sources() {
    let mut database = Database::oracle();
    let reads = Reads::oracle();
    let captured = database.capture(TENANT, 1).unwrap();
    let observed = observe(&captured, Some(&reads), PreparationLimits::default(), false).unwrap();
    assert_eq!(
        json(&observed),
        std::str::from_utf8(NODE_BUILDER).unwrap().trim_end()
    );
    let mut options = Vec::new();
    observed
        .builder
        .write_option_groups(&captured.stored_selections(), &mut options);
    let options: serde_json::Value = serde_json::from_slice(&options).unwrap();
    let node: serde_json::Value = serde_json::from_slice(NODE_BUILDER).unwrap();
    assert_eq!(options["plan_id"], 1);
    assert_eq!(options["selections"], serde_json::json!({"zeta": "z1"}));
    assert_eq!(options["option_groups"], node["merged_option_groups"]);
    assert_eq!(
        options["sources"][0],
        serde_json::json!({
            "source_id": 1, "name": "Base Frame", "layer_type": "base",
            "exists": true, "scanned_part_count": 3
        })
    );
    assert_eq!(options["sources"][2]["exists"], false);
    assert_eq!(
        observed
            .stamps
            .iter()
            .map(|(source, _)| *source)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    let mut requests = reads.requests.into_inner().unwrap();
    requests.sort();
    assert_eq!(
        requests,
        [
            "manifest account-a 1 /checkouts/1",
            "manifest account-a 2 /checkouts/2",
            "manifest account-a 3 /checkouts/3",
            "manifest account-a 4 /checkouts/4",
            "manifest account-a 6 /checkouts/6",
            "root account-a 1 /checkouts/1",
            "root account-a 2 /checkouts/2",
            "root account-a 3 /checkouts/3",
            "root account-a 4 /checkouts/4",
            "root account-a 6 /checkouts/6",
        ]
    );
}

#[test]
fn observation_refuses_missing_reader_exhausted_budget_cancellation_and_bad_custom_hints() {
    let mut database = Database::oracle();
    let captured = database.capture(TENANT, 1).unwrap();
    let reads = Reads::oracle();
    let failure = |reads: Option<&dyn DraftReads>, limits, cancelled| {
        observe(&captured, reads, limits, cancelled)
            .err()
            .map(|error| format!("{error:?}"))
    };
    assert_eq!(
        failure(None, PreparationLimits::default(), false).as_deref(),
        Some("Unavailable")
    );
    assert_eq!(
        failure(Some(&reads), PreparationLimits::default(), true).as_deref(),
        Some("Cancelled")
    );
    let few_nodes = PreparationLimits {
        nodes: 8,
        ..PreparationLimits::default()
    };
    assert!(matches!(
        observe(&captured, Some(&reads), few_nodes, false),
        Err(ReadFailure::LimitExceeded(Resource::Nodes))
    ));
    let few_entries = PreparationLimits {
        entries: 3,
        ..PreparationLimits::default()
    };
    assert!(matches!(
        observe(&captured, Some(&reads), few_entries, false),
        Err(ReadFailure::LimitExceeded(Resource::Entries))
    ));
    for (custom, expected) in [
        (
            Ok(b"version: 1\nrules: [{path: a}]\n".to_vec()),
            "InvalidDocument",
        ),
        (Err(ErrorKind::PermissionDenied), "Io(PermissionDenied)"),
    ] {
        let mut reads = Reads::oracle();
        reads.catalog.insert("custom".into(), custom);
        assert_eq!(
            failure(Some(&reads), PreparationLimits::default(), false).as_deref(),
            Some(expected)
        );
    }
}

#[test]
fn builder_without_checkouts_needs_no_reader_and_keeps_the_stored_kit() {
    let mut database = Database::oracle();
    database
        .connection
        .execute("UPDATE projects SET local_path=NULL", [])
        .unwrap();
    let captured = database.capture(TENANT, 1).unwrap();
    let observed = observe(&captured, None, PreparationLimits::default(), false).unwrap();
    assert_eq!(
        json(&observed),
        r#"{"profile_id":1,"sources":[],"resolved_selections":{"zeta":"z1"},"merged_option_groups":{}}"#
    );
    assert!(observed.stamps.is_empty());
}

#[test]
fn cancelled_observation_without_checkouts_refuses() {
    let mut database = Database::oracle();
    database
        .connection
        .execute("UPDATE projects SET local_path=NULL", [])
        .unwrap();
    let with_kit = database.capture(TENANT, 1).unwrap();
    database
        .connection
        .execute("DELETE FROM app_settings WHERE key='kit_manifest_1'", [])
        .unwrap();
    let without_kit = database.capture(TENANT, 1).unwrap();
    for captured in [with_kit, without_kit] {
        assert!(matches!(
            observe(&captured, None, PreparationLimits::default(), true),
            Err(ReadFailure::Cancelled)
        ));
    }
}

struct LongPaths {
    manifest: Vec<u8>,
    paths: Vec<String>,
    checks_before_cancel: usize,
}

impl DraftReads for LongPaths {
    fn resolve_stl_root(
        &self,
        _request: &OwnedStlReadRequest,
        _budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlRootRead>> {
        Ok(Box::new(Root(Some(self.paths.clone()))))
    }

    fn read_checkout_manifest(
        &self,
        _request: &OwnedCheckoutReadRequest,
        _occurrence: crate::working_drafts::observation::CheckoutManifestUse,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        document(Some(&Ok(self.manifest.clone())), budget)
    }

    fn read_catalog_document(
        &self,
        request: &OwnedCatalogReadRequest,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        // The community read is the last one before declared parts are matched.
        assert!(matches!(request.purpose(), CatalogPurpose::Community(_)));
        budget.cancel_after_checks(self.checks_before_cancel);
        Ok(DocumentRead::Missing)
    }
}

fn observe_long_paths(
    parts: &str,
    paths: Vec<String>,
    checks_before_cancel: usize,
) -> ReadResult<()> {
    let mut database = Database::oracle();
    database
        .connection
        .execute_batch(
            "DELETE FROM profile_layers WHERE project_id<>1; UPDATE projects SET imported_paths=NULL, manifest_community_slug='slow-kit' WHERE id=1",
        )
        .unwrap();
    let captured = database.capture(TENANT, 1).unwrap();
    let reads = LongPaths {
        manifest: format!(
            "option_groups:\n  slow:\n    variants:\n      - id: crafted\n        parts: {parts}\n"
        )
        .into_bytes(),
        paths,
        checks_before_cancel,
    };
    observe(&captured, Some(&reads), PreparationLimits::default(), false).map(drop)
}

#[test]
fn cancellation_while_matching_long_paths_refuses() {
    // The entry check passes; cancellation lands after the first path.
    let parts = format!("[\"*{}b\"]", "a".repeat(2048));
    assert!(matches!(
        observe_long_paths(&parts, vec!["a".repeat(4096); 256], 2),
        Err(ReadFailure::Cancelled)
    ));
}

#[test]
fn cancellation_before_matching_without_paths_or_parts_refuses() {
    for (parts, paths) in [("[\"*b\"]", Vec::new()), ("[]", vec!["b".to_owned()])] {
        assert!(matches!(
            observe_long_paths(parts, paths, 0),
            Err(ReadFailure::Cancelled)
        ));
    }
    let mut database = Database::oracle();
    database
        .connection
        .execute_batch(
            "DELETE FROM profile_layers WHERE project_id<>1; UPDATE projects SET imported_paths=NULL, manifest_community_slug='slow-kit' WHERE id=1",
        )
        .unwrap();
    let captured = database.capture(TENANT, 1).unwrap();
    let reads = LongPaths {
        manifest: b"option_groups: {slow: {variants: []}}".to_vec(),
        paths: Vec::new(),
        checks_before_cancel: 0,
    };
    assert!(matches!(
        observe(&captured, Some(&reads), PreparationLimits::default(), false),
        Err(ReadFailure::Cancelled)
    ));
}

fn import_only_source(imported_paths: &str) -> Database {
    let database = Database::oracle();
    database
        .connection
        .execute("DELETE FROM profile_layers WHERE project_id<>1", [])
        .unwrap();
    database
        .connection
        .execute(
            "UPDATE projects SET imported_paths=?1 WHERE id=1",
            [imported_paths],
        )
        .unwrap();
    database
}

fn part_reads() -> Reads {
    let mut reads = Reads::default();
    reads.allowed_roots.insert(1, vec!["part.stl"]);
    reads
}

#[test]
fn import_arrays_over_the_node_limit_refuse_instead_of_dropping_parts() {
    let reads = part_reads();
    let scanned = |numbers: usize| {
        let raw = format!("[{}\"part.stl\"]", "0,".repeat(numbers));
        let captured = import_only_source(&raw).capture(TENANT, 1).unwrap();
        observe(&captured, Some(&reads), PreparationLimits::default(), false)
            .map(|observed| observed.builder.sources[0].scanned_parts.clone())
            .map_err(|error| format!("{error:?}"))
    };
    // The stored kit charges 5 of the shared nodes before the import rules.
    assert_eq!(scanned(99_993), Ok(vec!["part.stl".to_owned()]));
    assert_eq!(scanned(99_994), Err("LimitExceeded(Nodes)".to_owned()));
    assert_eq!(scanned(100_000), Err("LimitExceeded(Nodes)".to_owned()));
}

#[test]
fn import_rules_charge_overwritten_nested_unknown_fields() {
    let mut database = import_only_source(r#"{"unknown":{"nested":[0,1,2]},"unknown":null}"#);
    let captured = database.capture(TENANT, 1).unwrap();
    assert!(matches!(
        observe(
            &captured,
            Some(&part_reads()),
            PreparationLimits {
                nodes: 8,
                ..PreparationLimits::default()
            },
            false,
        ),
        Err(ReadFailure::LimitExceeded(Resource::Nodes))
    ));
}

#[test]
fn zero_budgets_refuse_the_stored_kit_and_import_rules() {
    let mut database = import_only_source(r#"["part.stl"]"#);
    let both = database.capture(TENANT, 1).unwrap();
    let mut capture_after = |change: &str| {
        database.connection.execute_batch(change).unwrap();
        database.capture(TENANT, 1).unwrap()
    };
    let kit_only = capture_after("UPDATE projects SET local_path=NULL WHERE id=1");
    let imports_only = capture_after(
        "UPDATE projects SET local_path='/checkouts/1' WHERE id=1; DELETE FROM app_settings WHERE key='kit_manifest_1'",
    );
    let reads = part_reads();
    let outcome = |captured, limits| match observe(captured, Some(&reads), limits, false) {
        Ok(observed) => json(&observed),
        Err(error) => format!("{error:?}"),
    };
    let zero = PreparationLimits {
        document_bytes: 0,
        total_document_bytes: 0,
        nodes: 0,
        visits: 0,
        rules: 0,
        ..PreparationLimits::default()
    };
    let no_documents = PreparationLimits {
        document_bytes: 0,
        ..PreparationLimits::default()
    };
    assert_eq!(outcome(&both, zero), "LimitExceeded(DocumentBytes)");
    assert_eq!(
        outcome(&kit_only, no_documents),
        "LimitExceeded(DocumentBytes)"
    );
    for (limits, expected) in [
        (
            PreparationLimits {
                nodes: 0,
                ..PreparationLimits::default()
            },
            "LimitExceeded(Nodes)",
        ),
        (
            PreparationLimits {
                depth: 0,
                ..PreparationLimits::default()
            },
            "LimitExceeded(Depth)",
        ),
    ] {
        assert_eq!(outcome(&kit_only, limits), expected);
    }
    for (limits, expected) in [
        (no_documents, "LimitExceeded(DocumentBytes)"),
        (
            PreparationLimits {
                nodes: 0,
                ..PreparationLimits::default()
            },
            "LimitExceeded(Nodes)",
        ),
        (
            PreparationLimits {
                rules: 0,
                ..PreparationLimits::default()
            },
            "LimitExceeded(Rules)",
        ),
    ] {
        assert_eq!(outcome(&imports_only, limits), expected);
    }
    assert_eq!(
        outcome(&both, PreparationLimits::default()),
        r#"{"profile_id":1,"sources":[{"source_id":1,"layer_type":"base","name":"Base Frame","role":"unassigned","url":"https://example.test/Base-Frame","exists":false,"path":"print-partner.manifest.yaml","yaml":"format: print-partner-manifest-v2\nversion: 2\nproject: Base Frame\n","document":{"format":"print-partner-manifest-v2","version":2,"project":"Base Frame","option_groups":{}},"scanned_parts":[{"match":"part.stl","relative_path":"part.stl"}]}],"resolved_selections":{"zeta":"z1"},"merged_option_groups":{}}"#
    );
}

fn selections(observed: &ObservedBuilder) -> String {
    let mut output = Vec::new();
    observed
        .builder
        .resolved_selections
        .write_selections_json(&mut output);
    String::from_utf8(output).unwrap()
}

#[test]
fn stored_kits_over_the_json_limits_refuse_instead_of_dropping_selections() {
    let mut database = Database::oracle();
    database
        .connection
        .execute("UPDATE projects SET local_path=NULL", [])
        .unwrap();
    let mut observed = |kit: String| {
        database
            .connection
            .execute(
                "UPDATE app_settings SET value=?1 WHERE key='kit_manifest_1'",
                [kit],
            )
            .unwrap();
        let captured = database.capture(TENANT, 1).unwrap();
        observe(&captured, None, PreparationLimits::default(), false)
            .map(|observed| selections(&observed))
            .map_err(|error| format!("{error:?}"))
    };
    let nested = |depth| {
        format!(
            r#"{{"selections":{{"zeta":"z1"}},"choice_tree":[{}null{}]}}"#,
            "[".repeat(depth),
            "]".repeat(depth)
        )
    };
    let many = |count| {
        format!(
            r#"{{"selections":{{"zeta":"z1"}},"choice_tree":[{}0]}}"#,
            "0,".repeat(count)
        )
    };
    let kept = Ok(r#"{"zeta":"z1"}"#.to_owned());
    assert_eq!(observed(nested(62)), kept);
    assert_eq!(observed(nested(63)), Err("LimitExceeded(Depth)".to_owned()));
    assert_eq!(observed(many(99_992)), kept);
    assert_eq!(
        observed(many(99_993)),
        Err("LimitExceeded(Nodes)".to_owned())
    );
}

#[test]
fn stored_kit_charges_overwritten_nested_unknown_fields() {
    let cancelled = AtomicBool::new(false);
    let limits = PreparationLimits {
        nodes: 7,
        ..PreparationLimits::default()
    };
    let mut budget = PreparationBudget::new(&cancelled, limits).unwrap();
    assert!(
        observation::stored_kit(
            r#"{"selections":{"zeta":"z1"},"unknown":null}"#,
            &mut budget,
        )
        .is_ok()
    );

    let mut budget = PreparationBudget::new(&cancelled, limits).unwrap();
    assert!(matches!(
        observation::stored_kit(
            r#"{"selections":{"zeta":"z1"},"unknown":{"nested":[0,1,2]},"unknown":null}"#,
            &mut budget,
        ),
        Err(ReadFailure::LimitExceeded(Resource::Nodes))
    ));
}

#[test]
fn stored_kit_preserves_duplicate_free_node_counts_and_depth() {
    for (nodes, depth, expected) in [
        (6, 3, Ok(())),
        (5, 3, Err("LimitExceeded(Nodes)".to_owned())),
        (6, 2, Err("LimitExceeded(Depth)".to_owned())),
    ] {
        let cancelled = AtomicBool::new(false);
        let mut budget = PreparationBudget::new(
            &cancelled,
            PreparationLimits {
                nodes,
                depth,
                ..PreparationLimits::default()
            },
        )
        .unwrap();
        assert_eq!(
            observation::stored_kit(r#"{"unknown":{"leaf":[null]}}"#, &mut budget)
                .map(drop)
                .map_err(|error| format!("{error:?}")),
            expected,
        );
    }
}

#[test]
fn stored_kit_malformed_value_tokens_preserve_the_remaining_node_budget() {
    for (raw, nodes) in [
        ("", 0),
        (" \t\r\n", 0),
        ("[0,]", 2),
        (r#"{"key": }"#, 2),
        ("n", 0),
        ("tru", 0),
        ("fals", 0),
        (r#""\q""#, 0),
        (r#""unterminated"#, 0),
        ("1.", 0),
        ("1e", 0),
        ("1e+", 0),
        (r#"{"kept":0,"nested":[true,n]}"#, 6),
        (r#"{"kept":0,"nested":[true,"\q"]}"#, 6),
        (r#"{"kept":0,"nested":[true,"unterminated"#, 6),
        ("[0,1.]", 2),
        ("[0,1e+]", 2),
    ] {
        for remaining in [0, 1] {
            let cancelled = AtomicBool::new(false);
            let mut budget = PreparationBudget::new(
                &cancelled,
                PreparationLimits {
                    nodes: nodes + remaining,
                    ..PreparationLimits::default()
                },
            )
            .unwrap();
            let kit = observation::stored_kit(raw, &mut budget)
                .unwrap_or_else(|error| panic!("{raw:?}, {remaining} remaining: {error:?}"));
            let mut output = Vec::new();
            kit.write_selections_json(&mut output);
            assert_eq!(output, b"{}", "{raw:?}");
            if remaining == 1 {
                assert!(
                    observation::stored_kit("{}", &mut budget).is_ok(),
                    "remaining budget after {raw:?}",
                );
            }
            assert!(matches!(
                observation::stored_kit("{}", &mut budget),
                Err(ReadFailure::LimitExceeded(Resource::Nodes))
            ));
        }
    }
}

#[test]
fn stored_kit_cancels_during_parse_before_malformed_tail() {
    let cancelled = AtomicBool::new(false);
    let mut budget = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
    budget.cancel_after_checks(4);
    assert!(matches!(
        observation::stored_kit(r#"{"unknown":[0,1,2,3],"unknown":null,"#, &mut budget),
        Err(ReadFailure::Cancelled)
    ));
}

#[test]
fn malformed_stored_kits_read_as_the_empty_kit_as_in_node() {
    let mut database = Database::oracle();
    let reads = Reads::oracle();
    for kit in [
        "",
        "{",
        "null",
        "[]",
        r#"{"selections":{"zeta":"z1"},"name":7}"#,
        r#"{"selections":{"zeta":["z1"," z1 "]}}"#,
    ] {
        database
            .connection
            .execute(
                "UPDATE app_settings SET value=?1 WHERE key='kit_manifest_1'",
                [kit],
            )
            .unwrap();
        let captured = database.capture(TENANT, 1).unwrap();
        let observed =
            observe(&captured, Some(&reads), PreparationLimits::default(), false).unwrap();
        assert_eq!(
            selections(&observed),
            r#"{"7":["skirt"],"toolhead":"stock","alpha":["enabled"],"beta":"x"}"#,
            "stored kit {kit:?}"
        );
    }
}

#[test]
fn capture_scopes_to_the_tenant_and_detects_graph_changes() {
    let mut database = Database::oracle();
    assert!(database.capture("account-b", 1).is_none());
    assert!(database.capture(TENANT, 2).is_none());
    let captured = database.capture(TENANT, 1).unwrap();
    assert!(database.basis_matches(&captured));
    for change in [
        "UPDATE projects SET imported_paths='[]' WHERE id=1",
        "UPDATE projects SET manifest_community_slug=NULL WHERE id=2",
        "UPDATE profile_layers SET layer_order=9 WHERE project_id=3",
        "UPDATE app_settings SET value='{}' WHERE key='kit_manifest_1'",
        "UPDATE build_profiles SET config_modified_at=NULL WHERE id=1",
    ] {
        let tx = database.connection.transaction().unwrap();
        tx.execute(change, []).unwrap();
        assert!(
            !observation::basis_matches(&tx, &captured).unwrap(),
            "{change}"
        );
    }
}

#[test]
fn stamps_change_only_for_the_source_whose_reads_changed() {
    let mut database = Database::oracle();
    let captured = database.capture(TENANT, 1).unwrap();
    let first = observe(
        &captured,
        Some(&Reads::oracle()),
        PreparationLimits::default(),
        false,
    )
    .unwrap();
    let again = observe(
        &captured,
        Some(&Reads::oracle()),
        PreparationLimits::default(),
        false,
    )
    .unwrap();
    assert_eq!(first.stamps, again.stamps);
    let mut reads = Reads::oracle();
    reads
        .allowed_roots
        .get_mut(&3)
        .unwrap()
        .push("options/custom/c.stl");
    reads
        .catalog
        .insert("community:oracle-kit".into(), Err(ErrorKind::NotFound));
    let changed = observe(&captured, Some(&reads), PreparationLimits::default(), false).unwrap();
    let differing: Vec<_> = first
        .stamps
        .iter()
        .zip(&changed.stamps)
        .filter(|(left, right)| left != right)
        .map(|(left, _)| left.0)
        .collect();
    assert_eq!(differing, [2, 3]);
}

#[test]
fn kit_validation_reports_node_messages_for_merged_groups() {
    let cancelled = AtomicBool::new(false);
    let mut budget = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
    let groups = parse_builder_manifest(
        b"option_groups:\n  toolhead:\n    variants: [{id: stock}, {id: mini}]\n  extras:\n    rule: pick_any\n    max: 2\n  panels:\n    rule: pick_n\n    max: 1\n  open:\n    rule: pick_any\n",
        &mut budget,
    )
    .unwrap()
    .groups;
    let detail = |request: &str| {
        let kit = KitManifest::from_request(request.as_bytes()).unwrap();
        observation::validate_known_selections(&kit, &groups)
            .err()
            .map(|detail| String::from_utf8(detail.into_body().into_bytes()).unwrap())
    };
    assert_eq!(
        detail(r#"{"kit":{"selections":{"unknown":[],"toolhead":[]}}}"#).as_deref(),
        Some(r#"{"detail":"kit.selections.toolhead must contain at least one variant id"}"#)
    );
    assert_eq!(
        detail(r#"{"kit":{"selections":{"toolhead":["stock","mini"]}}}"#).as_deref(),
        Some(r#"{"detail":"kit.selections.toolhead must contain no more than 1 variant id"}"#)
    );
    assert_eq!(
        detail(r#"{"kit":{"selections":{"extras":["a","b","c"]}}}"#).as_deref(),
        Some(r#"{"detail":"kit.selections.extras must contain no more than 2 variant ids"}"#)
    );
    assert_eq!(
        detail(r#"{"kit":{"selections":{"panels":["a","b"]}}}"#).as_deref(),
        Some(r#"{"detail":"kit.selections.panels must contain no more than 1 variant id"}"#)
    );
    assert_eq!(
        detail(r#"{"kit":{"selections":{"toolhead":"retired","extras":[],"open":["a","b","c"]}}}"#),
        None
    );
}

#[test]
fn the_stored_kit_shares_the_node_allowance_with_import_rules() {
    let mut database = import_only_source(r#"["part.stl"]"#);
    let both = database.capture(TENANT, 1).unwrap();
    database
        .connection
        .execute("DELETE FROM app_settings WHERE key='kit_manifest_1'", [])
        .unwrap();
    let imports_only = database.capture(TENANT, 1).unwrap();
    let reads = part_reads();
    let outcome = |captured, nodes| {
        let limits = PreparationLimits {
            nodes,
            ..PreparationLimits::default()
        };
        observe(captured, Some(&reads), limits, false)
            .map(|_| ())
            .map_err(|error| format!("{error:?}"))
    };
    assert_eq!(outcome(&imports_only, 2), Ok(()));
    assert_eq!(
        outcome(&imports_only, 1),
        Err("LimitExceeded(Nodes)".to_owned())
    );
    // {"selections":{"zeta":"z1"}} charges its root, two keys, the inner object and one value.
    assert_eq!(outcome(&both, 7), Ok(()));
    assert_eq!(outcome(&both, 6), Err("LimitExceeded(Nodes)".to_owned()));

    database
        .connection
        .execute(
            "INSERT INTO app_settings (tenant_id,key,value) VALUES (?1,'kit_manifest_1',?2)",
            params![TENANT, r#"{"kept":0,"nested":[true,n]}"#],
        )
        .unwrap();
    let malformed = database.capture(TENANT, 1).unwrap();
    assert_eq!(outcome(&malformed, 8), Ok(()));
    assert_eq!(
        outcome(&malformed, 7),
        Err("LimitExceeded(Nodes)".to_owned())
    );
}

#[test]
fn community_slugs_follow_the_node_grammar_on_the_shared_corpus() {
    let corpus: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/parity/community-manifest-slug.json"
    ))
    .unwrap();
    for (key, valid) in [("valid", true), ("invalid", false)] {
        for slug in corpus[key].as_array().unwrap() {
            let slug = slug.as_str().unwrap();
            assert_eq!(CommunitySlug::parse(slug).is_some(), valid, "{slug:?}");
        }
    }
}

#[test]
fn invalid_stored_community_slugs_read_no_community_manifest() {
    let observe_with = |slug: Option<&str>| {
        let mut database = Database::oracle();
        database
            .connection
            .execute(
                "UPDATE projects SET manifest_community_slug=?1 WHERE id=2",
                [slug],
            )
            .unwrap();
        let mut reads = Reads::oracle();
        if let Some(slug) = slug {
            reads.catalog.insert(
                format!("community:{slug}"),
                Ok(b"selections:\n  beta: x\n  toolhead: custom\n".to_vec()),
            );
        }
        let captured = database.capture(TENANT, 1).unwrap();
        let observed =
            observe(&captured, Some(&reads), PreparationLimits::default(), false).unwrap();
        (
            (json(&observed), observed.stamps),
            observed.invalid_community_slugs,
        )
    };
    let (without, refused) = observe_with(None);
    assert!(refused.is_empty());
    let (valid, refused) = observe_with(Some("oracle-kit"));
    assert_ne!(valid, without);
    assert!(refused.is_empty());
    for slug in [
        "../oracle-kit",
        "oracle-kit/..",
        "..",
        " oracle-kit ",
        "Oracle-Kit",
    ] {
        assert_eq!(
            observe_with(Some(slug)),
            (without.clone(), vec![2]),
            "{slug:?}"
        );
    }
}

/// T-10-draftreads (GRE-1052): the oracle replayed through the production
/// `SourceReads` on real checkouts laid out as `manifest-builder-node.mts`.
mod ticket_t_10_draftreads {
    use super::*;
    use crate::working_drafts::source_reads::SourceReads;

    #[test]
    fn ticket_t_10_draftreads_builder_bytes_equal_node_on_real_checkouts() {
        let mut database = Database::oracle();
        let data = database.directory.clone();
        let write = |path: &str, bytes: &[u8]| {
            let path = data.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        let reads = Reads::oracle();
        for (id, paths) in &reads.allowed_roots {
            for path in paths {
                write(&format!("repos/{id}/{path}"), b"solid");
            }
        }
        for (id, manifest) in &reads.manifests {
            match manifest {
                Ok(bytes) => write(&format!("repos/{id}/print-partner.manifest.yaml"), bytes),
                Err(_) => write(
                    &format!("repos/{id}/print-partner.manifest.yaml/.keep"),
                    b"",
                ),
            }
        }
        write(
            // ast-grep-ignore: no-writer-family-literal -- Test fixture for Node's custom hints file; production only reads it.
            "path-hints.yaml",
            b"version: 1\nrules:\n  - path: \"**/rail_mount.stl\"\n    option_group: rail_mounts\n    variant_id: printed_mount\n",
        );
        write(
            // ast-grep-ignore: no-writer-family-literal -- Test fixture community manifest; production only reads it.
            "manifests/oracle-kit.yaml",
            b"selections:\n  beta: x\n  toolhead: custom\n",
        );
        for id in 1..=4 {
            let root = data.join(format!("repos/{id}"));
            database
                .connection
                .execute(
                    "UPDATE projects SET local_path=?1 WHERE id=?2",
                    params![root.to_str().unwrap(), id],
                )
                .unwrap();
        }
        // Source 6 points outside its workspace; the reader refuses it, so
        // observation skips it like a Source with no checkout.
        database
            .connection
            .execute(
                "UPDATE projects SET local_path=?1 WHERE id=6",
                [data.join("repos/6/../1").to_str().unwrap()],
            )
            .unwrap();
        // ast-grep-ignore: no-writer-family-literal -- Names the fixture community-manifest root to read.
        let reads = SourceReads::new(&data).with_community_manifests(data.join("manifests"));
        let captured = database.capture(TENANT, 1).unwrap();
        let observed =
            observe(&captured, Some(&reads), PreparationLimits::default(), false).unwrap();
        assert_eq!(
            json(&observed),
            std::str::from_utf8(NODE_BUILDER).unwrap().trim_end()
        );
        assert_eq!(
            observed
                .stamps
                .iter()
                .map(|(source, _)| *source)
                .collect::<Vec<_>>(),
            [1, 2, 3, 4]
        );
        assert!(observed.invalid_community_slugs.is_empty());
    }
}
