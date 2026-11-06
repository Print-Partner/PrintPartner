#![cfg(feature = "desktop_rebuild")]
//! Profile overlay parity through the public working-draft client.
//! Prototype-name slugs and custom quantity regexes are outside this corpus.

use pp_storage::{
    Limits, WriterOwner,
    auth::{self, Secret},
    read_model::Credential,
    working_drafts::{
        PositiveId, RecomputeRequest,
        observation::{
            ArtifactRead, CheckoutManifestUse, DocumentRead, DraftReads, InventoryPath,
            OwnedCatalogReadRequest, OwnedCheckoutReadRequest, OwnedStlReadRequest,
            PreparationBudget, PreparationLimits, ReadResult, StlInventory, StlRootRead,
        },
    },
};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

#[path = "common/process.rs"]
mod child_process;

const FIXTURE: &str = include_str!("../../../../fixtures/parity/profile-overlay/node.json");
const PART_FIELDS: &[&str] = &[
    "partKey",
    "relativePath",
    "filename",
    "sourceLayer",
    "status",
    "roleInferred",
    "roleOverride",
    "quantityInferred",
    "quantityOverride",
    "quantityEffective",
    "included",
    "notes",
    "artifactDigest",
];
const INPUT_FIELDS: &[&str] = &[
    "sourceId",
    "sourceLayer",
    "layerOrder",
    "trackingKind",
    "sourceRevisionId",
    "manifestDigest",
    "effectiveNamingDigest",
];

fn session() -> Credential {
    Credential::Session(Secret::new("required-unit-fixture-secret".into()))
}
fn policy() -> auth::AuthPolicy {
    auth::AuthPolicy {
        registration: auth::RegistrationPolicy::Open,
        session_tenant: auth::SessionTenantPolicy::AccountTenant,
        first_user: auth::FirstUserTenant::NewUser,
    }
}
fn profile_id() -> PositiveId {
    PositiveId::new(3).unwrap()
}

struct Reads {
    files: BTreeMap<i64, Vec<String>>,
}
struct Root {
    files: Vec<String>,
}
struct Inventory(Vec<InventoryPath>);
impl DraftReads for Reads {
    fn resolve_stl_root(
        &self,
        request: &OwnedStlReadRequest,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlRootRead>> {
        budget.check()?;
        let files = self
            .files
            .get(&request.source_id())
            .cloned()
            .unwrap_or_default();
        Ok(Box::new(Root { files }))
    }
    fn read_checkout_manifest(
        &self,
        _: &OwnedCheckoutReadRequest,
        _: CheckoutManifestUse,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        budget.check()?;
        Ok(DocumentRead::Missing)
    }
    fn read_catalog_document(
        &self,
        _: &OwnedCatalogReadRequest,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<DocumentRead> {
        budget.check()?;
        Ok(DocumentRead::Missing)
    }
}
impl StlRootRead for Root {
    fn logical_resolved_path(&self) -> Option<&str> {
        Some("/logical/root")
    }
    fn scan_stls(
        &mut self,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<Box<dyn StlInventory>> {
        budget.check()?;
        let mut entries = Vec::new();
        for (traversal_ordinal, path) in self.files.iter().enumerate() {
            budget.entry(0, path.len())?;
            entries.push(InventoryPath {
                physical_relative_path: path.clone(),
                traversal_ordinal,
            });
        }
        Ok(Box::new(Inventory(entries)))
    }
}
impl StlInventory for Inventory {
    fn entries(&self) -> &[InventoryPath] {
        &self.0
    }
    fn hash_tracked_winner(
        &mut self,
        index: usize,
        budget: &mut PreparationBudget<'_>,
    ) -> ReadResult<ArtifactRead> {
        let bytes = self.0[index].physical_relative_path.as_bytes();
        budget.artifact(bytes.len() as u64, bytes.len())?;
        Ok(ArtifactRead {
            byte_count: bytes.len() as u64,
            byte_sha256: hex::encode(Sha256::digest(bytes)),
        })
    }
}

struct Fixture {
    path: PathBuf,
    owner: Option<WriterOwner>,
}
impl Fixture {
    fn open(setup: &[String]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pp-profile-overlay-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let database = path.join("print-partner.db");
        std::fs::write(
            &database,
            include_bytes!("fixtures/required-units/unchanged.db"),
        )
        .unwrap();
        let connection = Connection::open(&database).unwrap();
        for sql in setup {
            connection.execute_batch(sql).unwrap();
        }
        drop(connection);
        let owner = WriterOwner::open(&path, Limits::default()).unwrap().0;
        Self {
            path,
            owner: Some(owner),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let shutdown = self.owner.take().map(WriterOwner::shutdown).transpose();
        let removal = std::fs::remove_dir_all(&self.path);
        if shutdown.is_err() || removal.is_err() {
            let message = format!(
                "profile overlay fixture cleanup failed: shutdown {shutdown:?}, remove directory {removal:?}"
            );
            if std::thread::panicking() {
                let _ =
                    std::io::Write::write_fmt(&mut std::io::stderr(), format_args!("{message}\n"));
            } else {
                panic!("{message}");
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OverlayFixture {
    metadata: Metadata,
    cases: Vec<OverlayCase>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    node: String,
    generator: String,
    sources: Vec<SourceFile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    path: String,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OverlayCase {
    name: String,
    setup: Vec<String>,
    files: BTreeMap<String, Vec<String>>,
    parts: Vec<Value>,
    inputs: Vec<Value>,
}

fn load() -> OverlayFixture {
    serde_json::from_str(FIXTURE).unwrap()
}
fn project(row: &Value, fields: &[&str]) -> Value {
    let mut out = serde_json::Map::new();
    for field in fields {
        out.insert((*field).to_owned(), row[*field].clone());
    }
    Value::Object(out)
}
fn case_named<'a>(fixture: &'a OverlayFixture, name: &str) -> &'a OverlayCase {
    fixture
        .cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("missing case {name}"))
}
fn prepare(case: &OverlayCase) -> Value {
    let fixture = Fixture::open(&case.setup);
    let files = case
        .files
        .iter()
        .map(|(id, paths)| (id.parse().unwrap(), paths.clone()))
        .collect();
    let owner = fixture.owner.as_ref().unwrap();
    let client = owner
        .working_drafts_with_observations(
            policy(),
            Arc::new(Reads { files }),
            PreparationLimits::default(),
        )
        .unwrap();
    let outcome = client
        .recompute(
            session(),
            profile_id(),
            RecomputeRequest::new(&case.name, true).unwrap(),
            Arc::new(AtomicBool::new(false)),
            Duration::from_secs(60),
        )
        .unwrap();
    serde_json::from_slice(&outcome.into_json_body().into_bytes()).unwrap()
}

#[test]
fn node_overlay_fixture_is_current() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let node = std::env::var_os("PP_SCHEMA_PARITY_NODE").unwrap_or_else(|| "node".into());
    let output = child_process::command(&node)
        .current_dir(&root)
        .arg("--conditions=development")
        .arg("--import")
        .arg(
            pp_testkit::node_path::file_url(&root.join("web/node_modules/tsx/dist/loader.mjs"))
                .expect("UTF-8 repository path"),
        )
        .arg(root.join("fixtures/parity/profile-overlay/generate.mjs"))
        .arg("--check")
        .output()
        .expect("run Node profile-overlay generator");
    assert!(
        output.status.success(),
        "profile-overlay fixture drift. Regenerate with Node 22.23.3.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "profile-overlay: 9 Node cases verified"
    );
}

#[test]
fn public_client_matches_node_overlay_matrix() {
    let fixture = load();
    assert_eq!(fixture.metadata.node, "v22.23.3");
    assert_eq!(
        fixture.metadata.generator,
        "fixtures/parity/profile-overlay/generate.mjs"
    );
    assert_eq!(fixture.metadata.sources.len(), 5);
    for source in &fixture.metadata.sources {
        assert_eq!(source.sha256.len(), 64, "{}", source.path);
    }
    assert_eq!(fixture.cases.len(), 9);
    for case in &fixture.cases {
        let outcome = prepare(case);
        assert_eq!(outcome["kind"], "created", "{}", case.name);
        let draft = &outcome["draft"];
        let parts: Vec<_> = draft["parts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| project(part, PART_FIELDS))
            .collect();
        let inputs: Vec<_> = draft["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|input| project(input, INPUT_FIELDS))
            .collect();
        assert_eq!(parts, case.parts, "{}", case.name);
        assert_eq!(inputs, case.inputs, "{}", case.name);
    }
}

#[test]
fn permuting_an_addon_or_dropping_provenance_fails() {
    let fixture = load();
    let alpha = case_named(&fixture, "addon-order-alpha-then-beta");
    let beta = case_named(&fixture, "addon-order-beta-then-alpha");
    let winner = |case: &OverlayCase| {
        case.parts
            .iter()
            .find(|part| part["partKey"] == "shared.stl")
            .unwrap()["sourceLayer"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(winner(alpha), "addon:Beta");
    assert_eq!(winner(beta), "addon:Alpha");
    assert_ne!(alpha.parts, beta.parts);
    assert_ne!(alpha.inputs, beta.inputs);
    let without_source = |case: &OverlayCase| {
        case.parts
            .iter()
            .map(|part| {
                let mut part = part.clone();
                part.as_object_mut().unwrap().remove("sourceLayer");
                part
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(without_source(alpha), without_source(beta));
    let alpha_live = prepare(alpha);
    let beta_live = prepare(beta);
    let live_winner = |outcome: &Value| {
        outcome["draft"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|part| part["partKey"] == "shared.stl")
            .unwrap()["sourceLayer"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_ne!(live_winner(&alpha_live), live_winner(&beta_live));
    let included = case_named(&fixture, "included-and-excluded");
    assert!(included.parts.iter().any(|part| part["included"] == true));
    assert!(included.parts.iter().any(|part| part["included"] == false));
}
