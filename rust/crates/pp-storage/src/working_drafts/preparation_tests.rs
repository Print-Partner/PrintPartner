#![cfg(test)]

use super::observation::*;
use super::preparation::*;
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex, atomic::AtomicBool};

pub(super) fn cases() -> Vec<Value> {
    include_str!("../../tests/fixtures/working-draft-preparation-node.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
pub(super) struct FixtureRoot(std::path::PathBuf);
impl FixtureRoot {
    pub(super) fn path(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for FixtureRoot {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
pub(super) fn database(case: &Value) -> (FixtureRoot, Connection) {
    let root =
        FixtureRoot(std::env::temp_dir().join(format!("pp-preparation-{}", rand::random::<u64>())));
    std::fs::create_dir_all(root.path().join("repos/1/revisions/accepted")).unwrap();
    let path = root.path().join("fixture.db");
    std::fs::write(
        &path,
        include_bytes!("../../tests/fixtures/accepted-plan-node.db"),
    )
    .unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .pragma_update(None, "foreign_keys", false)
        .unwrap();
    let triggers = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='trigger'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    for trigger in triggers {
        connection
            .execute_batch(&format!("DROP TRIGGER \"{trigger}\""))
            .unwrap();
    }
    for step in case["setup"].as_array().unwrap() {
        connection.execute_batch(step.as_str().unwrap()).unwrap();
    }
    (root, connection)
}
struct Reads {
    case: Value,
    hashes: Arc<Mutex<Vec<String>>>,
}
struct Root {
    logical: Option<String>,
    files: Vec<Value>,
    hashes: Arc<Mutex<Vec<String>>>,
}
struct Inventory {
    entries: Vec<InventoryPath>,
    files: Vec<Value>,
    hashes: Arc<Mutex<Vec<String>>>,
}
impl DraftReads for Reads {
    fn resolve_stl_root(
        &self,
        request: &OwnedStlReadRequest,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlRootRead>> {
        b.check()?;
        let logical = if request.purpose() == StlPurpose::CheckoutOptions
            && self.case["unresolvableCheckout"] == true
        {
            None
        } else {
            request.stored_path().map(|p| {
                if request.purpose() == StlPurpose::Revision {
                    format!("/logical/{p}")
                } else {
                    p.into()
                }
            })
        };
        let files = self.case["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["source"] == request.source_id())
            .cloned()
            .collect();
        Ok(Box::new(Root {
            logical,
            files,
            hashes: self.hashes.clone(),
        }))
    }
    fn read_checkout_manifest(
        &self,
        request: &OwnedCheckoutReadRequest,
        _: CheckoutManifestUse,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        b.check()?;
        match self.case["docs"][request.source_id().to_string()].as_str() {
            Some(s) => {
                b.document(s.len(), s.len())?;
                Ok(DocumentRead::Bytes(s.as_bytes().to_vec()))
            }
            None => Ok(DocumentRead::Missing),
        }
    }
    fn read_catalog_document(
        &self,
        request: &OwnedCatalogReadRequest,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        b.check()?;
        if matches!(request.purpose(), CatalogPurpose::CustomHints)
            && let Some(raw) = self.case["hints"].as_str()
        {
            b.document(raw.len(), raw.len())?;
            return Ok(DocumentRead::Bytes(raw.as_bytes().to_vec()));
        }
        Ok(DocumentRead::Missing)
    }
}
impl StlRootRead for Root {
    fn logical_resolved_path(&self) -> Option<&str> {
        self.logical.as_deref()
    }
    fn scan_stls(&mut self, b: &mut PreparationBudget<'_>) -> ReadResult<Box<dyn StlInventory>> {
        b.check()?;
        if self.logical.is_none() {
            return Err(ReadFailure::Unavailable);
        }
        let mut entries = Vec::new();
        for (i, file) in self.files.iter().enumerate() {
            let path = file["path"].as_str().unwrap();
            b.entry(path.split('/').count() - 1, path.len())?;
            entries.push(InventoryPath {
                physical_relative_path: path.into(),
                traversal_ordinal: i,
            });
        }
        Ok(Box::new(Inventory {
            entries,
            files: self.files.clone(),
            hashes: self.hashes.clone(),
        }))
    }
}
impl StlInventory for Inventory {
    fn entries(&self) -> &[InventoryPath] {
        &self.entries
    }
    fn hash_tracked_winner(
        &mut self,
        index: usize,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<ArtifactRead> {
        let bytes = self.files[index]["content"].as_str().unwrap().as_bytes();
        b.artifact(bytes.len() as u64, bytes.len())?;
        self.hashes
            .lock()
            .unwrap()
            .push(self.files[index]["path"].as_str().unwrap().into());
        Ok(ArtifactRead {
            byte_count: bytes.len() as u64,
            byte_sha256: hex::encode(Sha256::digest(bytes)),
        })
    }
}
pub(super) fn options(case: &Value) -> PreparationOptions {
    let o = &case["options"];
    PreparationOptions {
        apply_manifest: o["applyManifest"].as_bool().unwrap_or(true),
        prefer_accepted: o["preferAccepted"].as_bool().unwrap_or(false),
        excluded_paths_by_source_id: serde_json::from_value(o["excluded"].clone()).unwrap(),
        included_paths_by_source_id: serde_json::from_value(o["included"].clone()).unwrap(),
        part_choices_by_source_id: serde_json::from_value(o["choices"].clone()).unwrap(),
    }
}
pub(super) fn projection(prepared: PreparedDraftSnapshot) -> Value {
    let fields = [
        vec!["baseRevisionPartId"],
        crate::required_units::model::PART_FIELDS.to_vec(),
    ]
    .concat();
    let parts: Vec<Value> = prepared
        .parts
        .iter()
        .map(|p| serde_json::from_slice(&p.json_fields(&fields, false)).unwrap())
        .collect();
    json!({"kind":"prepared","inputs":prepared.capture.inputs,"parts":parts,"digest":prepared.digest})
}

#[test]
fn preparation_matches_literal_node_results() {
    preparation_matches_cases(cases());
}
#[test]
fn preparation_large_quantity_digest_matches_node() {
    naming_override_case("large-filename-quantities");
}

fn large_quantity_snapshot() -> PreparedDraftSnapshot {
    let case = cases()
        .into_iter()
        .find(|c| c["name"] == "large-filename-quantities")
        .unwrap();
    let (root, mut connection) = database(&case);
    let reads = Reads {
        case: case.clone(),
        hashes: Default::default(),
    };
    let cancelled = AtomicBool::new(false);
    let mut budget = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
    prepare(
        &mut connection,
        "default",
        1,
        case["base"].clone(),
        PreparationContext {
            options: &options(&case),
            repos: &root.path().join("repos"),
        },
        &reads,
        &mut budget,
    )
    .unwrap()
}

#[test]
fn preparation_large_quantity_keeps_integer_spelling() {
    let prepared = large_quantity_snapshot();
    let expected = [
        "9007199254740991",
        "9007199254740992",
        "9007199254740992",
        "1000000000000000100",
        "100000000000000000000",
        "1e+21",
    ];
    assert_eq!(prepared.parts.len(), expected.len());
    for (part, expected) in prepared.parts.iter().zip(expected) {
        assert_eq!(
            String::from_utf8(part.json_fields(
                &["quantityInferred", "quantityOverride", "quantityEffective"],
                false
            ))
            .unwrap(),
            format!(
                "{{\"quantityInferred\":{expected},\"quantityOverride\":null,\"quantityEffective\":{expected}}}"
            ),
        );
    }
}
#[test]
fn full_naming_override_omits_role() {
    naming_override_case("naming-full-override-omits-role");
}
#[test]
fn full_naming_override_reorders_roles() {
    naming_override_case("naming-full-override-reorders-roles");
}
#[test]
fn full_naming_override_adds_role() {
    naming_override_case("naming-full-override-adds-role");
}
fn naming_override_case(name: &str) {
    let case = cases()
        .into_iter()
        .find(|case| case["name"] == name)
        .unwrap();
    preparation_matches_cases([case]);
}
fn preparation_matches_cases(cases: impl IntoIterator<Item = Value>) {
    for case in cases {
        let (root, mut connection) = database(&case);
        let writes = connection.total_changes();
        let reads = Reads {
            case: case.clone(),
            hashes: Default::default(),
        };
        let cancelled = AtomicBool::new(false);
        let mut b = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
        LAYER_NAMING_COMPILES.with(|count| count.set(0));
        let result = if case["capture"] == true {
            let tx = connection.transaction().unwrap();
            capture(&tx, "default", 1, &reads, &mut b)
                .map(|c| json!({"fingerprint":c.fingerprint,"inputs":c.inputs}))
        } else {
            prepare(
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
            )
            .map(|prepared| {
                assert_eq!(prepared.base, case["base"]);
                projection(prepared)
            })
        };
        let actual = match result {
            Ok(value) => value,
            Err(e) => match e.downcast_ref::<Failure>() {
                Some(Failure::NoLayers) => json!({"kind":"no_layers"}),
                Some(Failure::NoStls) => json!({"kind":"no_stls"}),
                Some(Failure::WouldWipe) => json!({"kind":"would_wipe"}),
                None => json!({"error":e.to_string()}),
            },
        };
        assert_eq!(actual, case["result"], "{}", case["name"]);
        assert_eq!(
            connection.total_changes(),
            writes,
            "{} wrote rows",
            case["name"]
        );
        if case["name"] == "reuse" || case["name"] == "untracked" {
            assert_eq!(*reads.hashes.lock().unwrap(), Vec::<String>::new());
        }
        if case["name"] == "layer-winners-and-conflicts" {
            // Seven paths across two layers compile each layer's regexes once.
            assert_eq!(LAYER_NAMING_COMPILES.with(|count| count.get()), 2);
            assert_eq!(
                *reads.hashes.lock().unwrap(),
                vec![
                    "[a]clip_x3.stl",
                    "excluded.stl",
                    "folder/bracket.stl",
                    "included.stl"
                ]
            );
        }
    }
}
#[test]
fn preparation_cancels_mid_attempt_and_propagates_resource_refusals() {
    let case = cases()
        .into_iter()
        .find(|c| c["name"] == "manifest")
        .unwrap();
    let (root, mut connection) = database(&case);
    let reads = Reads {
        case: case.clone(),
        hashes: Default::default(),
    };
    {
        let cancelled = AtomicBool::new(false);
        let mut b = PreparationBudget::new(
            &cancelled,
            PreparationLimits {
                visits: 0,
                ..Default::default()
            },
        )
        .unwrap();
        let tx = connection.transaction().unwrap();
        let error = capture(&tx, "default", 1, &reads, &mut b).err().unwrap();
        assert_eq!(error.to_string(), "LimitExceeded(Visits)");
    }
    for (limits, resource) in [
        (
            PreparationLimits {
                entries: 0,
                ..Default::default()
            },
            "Entries",
        ),
        (
            PreparationLimits {
                artifact_bytes: 0,
                ..Default::default()
            },
            "ArtifactBytes",
        ),
        (
            PreparationLimits {
                document_bytes: 0,
                ..Default::default()
            },
            "DocumentBytes",
        ),
        (
            PreparationLimits {
                comparison_work: 0,
                ..Default::default()
            },
            "ComparisonWork",
        ),
        (
            PreparationLimits {
                visits: 0,
                ..Default::default()
            },
            "Visits",
        ),
    ] {
        let cancelled = AtomicBool::new(false);
        let mut b = PreparationBudget::new(&cancelled, limits).unwrap();
        let e = prepare(
            &mut connection,
            "default",
            1,
            case["base"].clone(),
            PreparationContext {
                options: &options(&case),
                repos: root.path(),
            },
            &reads,
            &mut b,
        )
        .err()
        .unwrap();
        assert_eq!(e.to_string(), format!("LimitExceeded({resource})"));
    }
    let mut completed = false;
    for checks in 0..1000 {
        let cancelled = AtomicBool::new(false);
        let mut b = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
        b.cancel_after_checks(checks);
        let tx = connection.transaction().unwrap();
        let mut tx = tx;
        match prepare(
            &mut tx,
            "default",
            1,
            case["base"].clone(),
            PreparationContext {
                options: &options(&case),
                repos: root.path(),
            },
            &reads,
            &mut b,
        ) {
            Ok(prepared) => {
                assert_eq!(projection(prepared), case["result"]);
                completed = true;
                break;
            }
            Err(e) => assert!(
                matches!(
                    e.downcast_ref::<ReadFailure>(),
                    Some(ReadFailure::Cancelled)
                ),
                "{checks}: {e:?}"
            ),
        }
    }
    assert!(completed);
}

#[test]
fn naming_comparisons_are_cumulative_and_cancellable() {
    use crate::catalog::naming::{FolderRule, NamingProfile, RoleId};
    let profile = NamingProfile {
        folder_rules: (0..40)
            .map(|i| FolderRule {
                path_contains: format!("missing-{i}"),
                role_id: RoleId::Clear,
                functional_class: None,
            })
            .collect(),
        ..NamingProfile::default()
    };
    let deep = format!("{}gear.stl", "folder/".repeat(40));
    let profile = LayerNaming::new(Arc::new(profile));
    let default = LayerNaming::new(Arc::new(NamingProfile::default()));
    for (path, profile, cap) in [("gear.stl", &profile, 600), (deep.as_str(), &default, 700)] {
        let cancelled = AtomicBool::new(false);
        let mut b = PreparationBudget::new(
            &cancelled,
            PreparationLimits {
                comparison_work: cap,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            parse(path, profile, &mut b).unwrap_err().to_string(),
            "LimitExceeded(ComparisonWork)"
        );
        let mut b = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
        b.cancel_after_checks(10);
        assert_eq!(
            parse(path, profile, &mut b).unwrap_err().to_string(),
            "Cancelled"
        );
    }
}

#[test]
fn capture_processes_global_naming_once_across_layers() {
    let case = cases()
        .into_iter()
        .find(|case| case["name"] == "layer-winners-and-conflicts")
        .unwrap();
    let (_root, mut connection) = database(&case);
    let reads = Reads {
        case: case.clone(),
        hashes: Default::default(),
    };
    let cancelled = AtomicBool::new(false);
    let mut budget = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
    let tx = connection.transaction().unwrap();
    GLOBAL_NAMING_PROJECTIONS.with(|count| count.set(0));
    let captured = capture(&tx, "default", 1, &reads, &mut budget).unwrap();
    assert_eq!(json!(captured.inputs), case["result"]["inputs"]);
    assert_eq!(GLOBAL_NAMING_PROJECTIONS.with(|count| count.get()), 1);
}

#[test]
fn capture_charges_layer_override_projection_work() {
    let case = cases()
        .into_iter()
        .find(|case| case["name"] == "naming-layer-overrides")
        .unwrap();
    let (_root, mut connection) = database(&case);
    let reads = Reads {
        case: case.clone(),
        hashes: Default::default(),
    };
    let cancelled = AtomicBool::new(false);
    let tx = connection.transaction().unwrap();
    let mut budget = PreparationBudget::new(
        &cancelled,
        PreparationLimits {
            comparison_work: 100,
            ..Default::default()
        },
    )
    .unwrap();
    let error = capture(&tx, "default", 1, &reads, &mut budget)
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "LimitExceeded(ComparisonWork)");
    let mut budget = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
    let captured = capture(&tx, "default", 1, &reads, &mut budget).unwrap();
    assert_eq!(json!(captured.inputs), case["result"]["inputs"]);
}

#[test]
fn part_tracking_indexes_layers_once() {
    let case = cases()
        .into_iter()
        .find(|case| case["name"] == "layer-winners-and-conflicts")
        .unwrap();
    let (root, mut connection) = database(&case);
    let reads = Reads {
        case: case.clone(),
        hashes: Default::default(),
    };
    let cancelled = AtomicBool::new(false);
    let mut budget = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
    SOURCE_LAYER_VISITS.with(|count| count.set(0));
    let prepared = prepare(
        &mut connection,
        "default",
        1,
        case["base"].clone(),
        PreparationContext {
            options: &options(&case),
            repos: root.path(),
        },
        &reads,
        &mut budget,
    )
    .unwrap();
    assert_eq!(projection(prepared), case["result"]);
    assert_eq!(SOURCE_LAYER_VISITS.with(|count| count.get()), 2);
}

struct CommunityLog {
    inner: Reads,
    requests: Mutex<Vec<String>>,
}
impl DraftReads for CommunityLog {
    fn resolve_stl_root(
        &self,
        request: &OwnedStlReadRequest,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlRootRead>> {
        self.inner.resolve_stl_root(request, b)
    }
    fn read_checkout_manifest(
        &self,
        request: &OwnedCheckoutReadRequest,
        occurrence: CheckoutManifestUse,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        self.inner.read_checkout_manifest(request, occurrence, b)
    }
    fn read_catalog_document(
        &self,
        request: &OwnedCatalogReadRequest,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        if let CatalogPurpose::Community(slug) = request.purpose() {
            self.requests.lock().unwrap().push(slug.as_str().into());
        }
        self.inner.read_catalog_document(request, b)
    }
}

#[test]
fn invalid_stored_community_slugs_make_no_community_read() {
    let case = cases()
        .into_iter()
        .find(|case| case["capture"] != true && case["result"]["kind"] == "prepared")
        .unwrap();
    let prepare_with = |slug: Option<&str>| {
        let (root, mut connection) = database(&case);
        connection
            .execute("UPDATE projects SET manifest_community_slug=?1", [slug])
            .unwrap();
        let reads = CommunityLog {
            inner: Reads {
                case: case.clone(),
                hashes: Default::default(),
            },
            requests: Mutex::default(),
        };
        let cancelled = AtomicBool::new(false);
        let mut b = PreparationBudget::new(&cancelled, PreparationLimits::default()).unwrap();
        let prepared = prepare(
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
        )
        .unwrap();
        (projection(prepared), reads.requests.into_inner().unwrap())
    };
    let (without, requests) = prepare_with(None);
    assert!(requests.is_empty());
    let (_, requests) = prepare_with(Some("oracle-kit"));
    assert!(!requests.is_empty());
    assert!(requests.iter().all(|slug| slug == "oracle-kit"));
    for slug in [
        // ast-grep-ignore: no-writer-family-literal -- Test input that the slug check must refuse.
        "../kit-catalog",
        // ast-grep-ignore: no-writer-family-literal -- Test input that the slug check must refuse.
        "kit-catalog",
        "registry-index",
        "oracle-kit/..",
        "Oracle-Kit",
    ] {
        assert_eq!(
            prepare_with(Some(slug)),
            (without.clone(), vec![]),
            "{slug:?}"
        );
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ClientAttempt {
    Match,
    Cancel,
    Budget,
    SortBudget(usize),
    SortCancel(usize),
    SortMatch,
    CorruptHints(&'static [u8]),
}
struct ClientReads {
    inner: Reads,
    visits: Arc<std::sync::atomic::AtomicUsize>,
    attempt: ClientAttempt,
}
impl DraftReads for ClientReads {
    fn resolve_stl_root(
        &self,
        request: &OwnedStlReadRequest,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlRootRead>> {
        if request.purpose() == StlPurpose::Revision {
            let target = match self.attempt {
                ClientAttempt::SortBudget(sort) | ClientAttempt::SortCancel(sort) => Some(sort),
                _ => None,
            };
            if let Some(sort) = target {
                PATH_SORT_TEST.with(|probe| {
                    *probe.borrow_mut() = Some(PathSortProbe {
                        remaining: sort,
                        cancel: matches!(self.attempt, ClientAttempt::SortCancel(_)),
                        comparisons: self.visits.clone(),
                    })
                });
            }
        }
        self.inner.resolve_stl_root(request, b)
    }
    fn read_checkout_manifest(
        &self,
        request: &OwnedCheckoutReadRequest,
        occurrence: CheckoutManifestUse,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        self.inner.read_checkout_manifest(request, occurrence, b)
    }
    fn read_catalog_document(
        &self,
        request: &OwnedCatalogReadRequest,
        b: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        b.check()?;
        match request.purpose() {
            CatalogPurpose::ShippedHintsFirst => {
                let mut hints = self.inner.case["hints"].clone();
                if matches!(self.attempt, ClientAttempt::Cancel | ClientAttempt::Budget) {
                    for rule in hints["rules"].as_array_mut().unwrap().iter_mut().take(2) {
                        rule["path"] = json!("new.stl");
                    }
                }
                Ok(DocumentRead::Bytes(serde_json::to_vec(&hints).unwrap()))
            }
            CatalogPurpose::CustomHints => {
                HINT_PATH_VISITS.with(|counter| *counter.borrow_mut() = Some(self.visits.clone()));
                match self.attempt {
                    ClientAttempt::Cancel => b.cancel_after_checks(3),
                    ClientAttempt::Budget => b.limit_visits_after(1),
                    _ => {}
                }
                match self.attempt {
                    ClientAttempt::CorruptHints(bytes) => Ok(DocumentRead::Bytes(bytes.to_vec())),
                    _ => Ok(DocumentRead::Missing),
                }
            }
            _ => Ok(DocumentRead::Missing),
        }
    }
}

fn client_attempt(attempt: ClientAttempt) {
    use crate::{Limits, WriterOwner, auth, read_model::Credential};
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };
    let case: Value = serde_json::from_str(match attempt {
        ClientAttempt::SortBudget(_) | ClientAttempt::SortCancel(_) | ClientAttempt::SortMatch => {
            include_str!("../../tests/fixtures/preparation-path-sort-node.json")
        }
        _ => include_str!("../../tests/fixtures/preparation-hints-node.json"),
    })
    .unwrap();
    let root = FixtureRoot(
        std::env::temp_dir().join(format!("pp-preparation-client-{}", rand::random::<u64>())),
    );
    std::fs::create_dir_all(root.path()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let database = root.path().join("print-partner.db");
    std::fs::write(
        &database,
        include_bytes!("../../tests/fixtures/required-units/unchanged.db"),
    )
    .unwrap();
    let connection = Connection::open(&database).unwrap();
    let files: Vec<Value> = case["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| {
            let file = root
                .path()
                .join("repos/1/revisions/fixture")
                .join(path.as_str().unwrap());
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, path.as_str().unwrap()).unwrap();
            json!({"source":1,"path":path,"content":path})
        })
        .collect();
    let snapshot = || {
        [
            "plan_drafts",
            "plan_draft_inputs",
            "plan_draft_parts",
            "plan_draft_required_unit_reconciliations",
            "plan_draft_required_unit_assignments",
        ]
        .map(|table| {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            statement
                .query_map([], |row| {
                    Ok((0..columns)
                        .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                        .collect::<Vec<_>>())
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        })
    };
    let before = snapshot();
    let owner = WriterOwner::open(root.path(), Limits::default()).unwrap().0;
    let visits = Arc::new(AtomicUsize::new(0));
    let reads = Arc::new(ClientReads {
        inner: Reads {
            case: json!({"files":files,"hints":case["hints"]}),
            hashes: Default::default(),
        },
        visits: visits.clone(),
        attempt,
    });
    let client = owner
        .working_drafts_with_observations(
            auth::AuthPolicy {
                registration: auth::RegistrationPolicy::Open,
                session_tenant: auth::SessionTenantPolicy::AccountTenant,
                first_user: auth::FirstUserTenant::NewUser,
            },
            reads,
            PreparationLimits::default(),
        )
        .unwrap();
    let result = client.recompute(
        Credential::Session(auth::Secret::new("required-unit-fixture-secret".into())),
        super::PositiveId::new(1).unwrap(),
        super::RecomputeRequest::new("first", true).unwrap(),
        Arc::new(AtomicBool::new(false)),
        Duration::from_secs(60),
    );
    if !matches!(attempt, ClientAttempt::Match | ClientAttempt::SortMatch) {
        let error = result.unwrap_err();
        if matches!(
            attempt,
            ClientAttempt::Cancel | ClientAttempt::SortCancel(_)
        ) {
            assert!(
                matches!(
                    error.downcast_ref::<ReadFailure>(),
                    Some(ReadFailure::Cancelled)
                ),
                "{error:?}"
            );
            assert_eq!(error.to_string(), "Cancelled");
        } else {
            assert_eq!(
                error.to_string(),
                if matches!(attempt, ClientAttempt::CorruptHints(_)) {
                    "InvalidDocument"
                } else if matches!(attempt, ClientAttempt::SortBudget(_)) {
                    "LimitExceeded(ComparisonWork)"
                } else {
                    "LimitExceeded(Visits)"
                }
            );
        }
        assert_eq!(
            visits.load(Ordering::Relaxed),
            if matches!(attempt, ClientAttempt::CorruptHints(_)) {
                0
            } else if matches!(
                attempt,
                ClientAttempt::SortBudget(_) | ClientAttempt::SortCancel(_)
            ) {
                1
            } else {
                2
            }
        );
        assert_eq!(snapshot(), before);
    } else {
        let mut actual: Value =
            serde_json::from_slice(&result.unwrap().into_json_body().into_bytes()).unwrap();
        assert!(
            actual["draft"]["createdAt"]
                .as_str()
                .unwrap()
                .ends_with('Z')
        );
        actual["draft"]["createdAt"] = json!(true);
        assert_eq!(actual, case["result"]);
        // Two rules match the first path. The missing rule examines all four.
        if attempt == ClientAttempt::Match {
            assert_eq!(visits.load(Ordering::Relaxed), 6);
        }
    }
    for file in &files {
        assert_eq!(
            std::fs::read(
                root.path()
                    .join("repos/1/revisions/fixture")
                    .join(file["path"].as_str().unwrap())
            )
            .unwrap(),
            file["content"].as_str().unwrap().as_bytes()
        );
    }
    owner.shutdown().unwrap();
}

#[test]
fn preparation_hint_stops_after_first_match() {
    client_attempt(ClientAttempt::Match);
}
#[test]
fn preparation_hint_search_honors_cancellation() {
    client_attempt(ClientAttempt::Cancel);
}
#[test]
fn preparation_hint_refusals_preserve_rows_and_files() {
    client_attempt(ClientAttempt::Budget);
}

#[test]
fn preparation_path_sort_exhausts_comparison_budget() {
    for sort in [1, 2] {
        client_attempt(ClientAttempt::SortBudget(sort));
    }
}
#[test]
fn preparation_path_sort_honors_cancellation() {
    for sort in [1, 2] {
        client_attempt(ClientAttempt::SortCancel(sort));
    }
}
#[test]
fn preparation_path_sort_preserves_node_order() {
    client_attempt(ClientAttempt::SortMatch);
}

const PRESERVED_TABLES: &[&str] = &[
    "plan_drafts",
    "plan_draft_inputs",
    "plan_draft_parts",
    "plan_draft_required_unit_reconciliations",
    "plan_draft_required_unit_assignments",
    "plan_revision_parts",
    "parts",
];

fn table_snapshot(connection: &Connection) -> Vec<Vec<Vec<String>>> {
    PRESERVED_TABLES
        .iter()
        .map(|table| {
            let mut statement = connection
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            statement
                .query_map([], |row| {
                    Ok((0..columns)
                        .map(|i| format!("{:?}", row.get_ref(i).unwrap()))
                        .collect::<Vec<_>>())
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        })
        .collect()
}

struct ReportedRecompute {
    body: Vec<u8>,
    rows_before: Vec<Vec<Vec<String>>>,
    rows_after: Vec<Vec<Vec<String>>>,
}

fn recompute_reported(profile: u64, reported: &[&str], setup: &str) -> ReportedRecompute {
    use crate::{Limits, WriterOwner, auth, read_model::Credential};
    use std::time::Duration;
    let root = FixtureRoot(
        std::env::temp_dir().join(format!("pp-preparation-wipe-{}", rand::random::<u64>())),
    );
    std::fs::create_dir_all(root.path()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let database = root.path().join("print-partner.db");
    std::fs::write(
        &database,
        include_bytes!("../../tests/fixtures/required-units/unchanged.db"),
    )
    .unwrap();
    let connection = Connection::open(&database).unwrap();
    if !setup.is_empty() {
        connection.execute_batch(setup).unwrap();
    }
    let files: Vec<Value> = reported
        .iter()
        .map(|path| json!({"source":1,"path":path,"content":path}))
        .collect();
    let reads: Arc<dyn DraftReads> = Arc::new(Reads {
        case: json!({"files":files}),
        hashes: Default::default(),
    });
    let owner = WriterOwner::open(root.path(), Limits::default()).unwrap().0;
    let client = owner
        .working_drafts_with_observations(
            auth::AuthPolicy {
                registration: auth::RegistrationPolicy::Open,
                session_tenant: auth::SessionTenantPolicy::AccountTenant,
                first_user: auth::FirstUserTenant::NewUser,
            },
            reads,
            PreparationLimits::default(),
        )
        .unwrap();
    let rows_before = table_snapshot(&connection);
    let body = client
        .recompute(
            Credential::Session(auth::Secret::new("required-unit-fixture-secret".into())),
            super::PositiveId::new(profile).unwrap(),
            super::RecomputeRequest::new("would-wipe", true).unwrap(),
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(60),
        )
        .unwrap()
        .into_json_body()
        .into_bytes();
    owner.shutdown().unwrap();
    let rows_after = table_snapshot(&connection);
    ReportedRecompute {
        body,
        rows_before,
        rows_after,
    }
}

#[test]
fn would_wipe_serializes_without_a_scanner() {
    assert_eq!(
        super::RecomputeOutcome::WouldWipe
            .into_json_body()
            .into_bytes(),
        br#"{"kind":"would_wipe"}"#
    );
    assert_eq!(
        super::RebaseOutcome::WouldWipe
            .into_json_body()
            .unwrap()
            .into_bytes(),
        br#"{"kind":"would_wipe"}"#
    );
}

#[test]
fn non_stl_scans_are_no_stls() {
    let accepted = recompute_reported(1, &["captured-input.txt"], "");
    assert_eq!(accepted.body, br#"{"kind":"no_stls"}"#);
    assert_eq!(accepted.rows_before, accepted.rows_after);

    let empty_prior = recompute_reported(
        3,
        &["captured-input.txt"],
        "INSERT INTO build_profiles(id,tenant_id,name) VALUES(3,'default','Empty Build'); INSERT INTO profile_layers(tenant_id,profile_id,project_id,layer_type,layer_order) SELECT tenant_id,3,project_id,layer_type,layer_order FROM profile_layers WHERE profile_id=1",
    );
    assert_eq!(empty_prior.body, br#"{"kind":"no_stls"}"#);
    assert_eq!(empty_prior.rows_before, empty_prior.rows_after);
}

#[test]
fn valid_scans_do_not_report_would_wipe() {
    let mixed = recompute_reported(1, &["notes.txt", "kept.stl"], "");
    let body: Value = serde_json::from_slice(&mixed.body).unwrap();
    assert_eq!(body["kind"], "created");
    let parts = body["draft"]["parts"].as_array().unwrap();
    let keys: Vec<&str> = parts
        .iter()
        .map(|part| part["partKey"].as_str().unwrap())
        .collect();
    let paths: Vec<&str> = parts
        .iter()
        .map(|part| part["relativePath"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["kept.stl"]);
    assert_eq!(paths, ["kept.stl"]);
    client_attempt(ClientAttempt::Match);
}

#[test]
fn corrupt_path_hints_preserve_saved_draft_rows_and_files() {
    for bytes in [
        b"version: 1\nrules: [".as_slice(),
        b"corrupt hints",
        b"version: 1\nrules: null",
    ] {
        client_attempt(ClientAttempt::CorruptHints(bytes));
    }
}
