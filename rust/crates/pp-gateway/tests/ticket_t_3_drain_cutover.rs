use axum::{
    body::Body,
    http::{HeaderMap, Request, Response, StatusCode},
};
use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use pp_compat::{CompatExit, CompatHandle, Endpoint};
use pp_gateway::{
    Gateway,
    ownership::{
        Owner,
        cutover::{
            self, BootReceipt, CUTOVER_FILE, CutoverError, CutoverReceipt, CutoverRefusal,
            DrainEvidence, EVIDENCE_FILE, Evidence, OperationOwners,
        },
        manifest::{OwnershipIndex, admit_manifest_documents},
    },
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const CATALOG: &str = "filament.catalog";
const PROFILE_SYNC: &str = "profile.sync";
const SOURCE_SYNC: &str = "source.sync";
const EXPORT_STL_PACK: &str = "export.stl-pack";
const EXPORT_ACCEPTED_PLATE: &str = "export.accepted-plate-3mf";
// Each export job with its route, its /api/v1 alias and any other route that
// records a job of the same kind.
const EXPORT_OPERATIONS: [(&str, &[&str]); 5] = [
    (
        EXPORT_ACCEPTED_PLATE,
        &[
            "http.POST /api/v1/jobs/export-accepted-plate-3mf",
            "http.POST /api/v1/slicer-instances/:id/open-accepted-plates",
            "http.POST /jobs/export-accepted-plate-3mf",
            "http.POST /slicer-instances/:id/open-accepted-plates",
            "job.export-accepted-plate-3mf",
        ],
    ),
    (
        "export.checklist-html",
        &[
            "http.POST /api/v1/jobs/export-checklist-html",
            "http.POST /jobs/export-checklist-html",
            "job.export-checklist-html",
        ],
    ),
    (
        "export.direct-3mf",
        &[
            "http.POST /api/v1/jobs/export-direct-3mf",
            "http.POST /jobs/export-direct-3mf",
            "job.export-direct-3mf",
        ],
    ),
    (
        "export.kit-bundle",
        &[
            "http.POST /api/v1/jobs/export-kit-bundle",
            "http.POST /jobs/export-kit-bundle",
            "job.export-kit-bundle",
        ],
    ),
    (
        EXPORT_STL_PACK,
        &[
            "http.POST /api/v1/jobs/export-stl-pack",
            "http.POST /jobs/export-stl-pack",
            "job.export-stl-pack",
        ],
    ),
];
// Every writer of the printer send queue, with the upload job they start.
const PRINTER_SEND_QUEUE: &str = "printer.send-queue";
const PRINTER_SEND_QUEUE_PARTICIPANTS: [&str; 19] = [
    "http.DELETE /api/v1/printer-send-queue/:id",
    "http.DELETE /printer-send-queue/:id",
    "http.GET /api/v1/printer-send-queue",
    "http.GET /printer-send-queue",
    "http.HEAD /api/v1/printer-send-queue",
    "http.HEAD /printer-send-queue",
    "http.POST /api/v1/jobs/printer-upload",
    "http.POST /api/v1/printer-send-queue",
    "http.POST /api/v1/printer-send-queue/:id/dispatch",
    "http.POST /api/v1/printer-send-queue/drain",
    "http.POST /jobs/printer-upload",
    "http.POST /printer-send-queue",
    "http.POST /printer-send-queue/:id/dispatch",
    "http.POST /printer-send-queue/drain",
    "job.printer-upload",
    "migration.migrateLegacySelfHostExports",
    "reconcile.reconcileMissingPrinterUploadJob",
    "reconcile.reconcilePrinterSendsAfterProcessStart",
    "shutdown.interruptPrinterUploadsOnShutdown",
];
// Its own operation: Node cannot fence this startup hook yet.
const UNFENCED: &str = "startup.seedStockSlicerInstancesIfEmpty";
const PLANS_CREATE: &str = "http:POST /plans current";
const CATALOG_PARTICIPANTS: [&str; 4] = [
    "http.GET /api/v1/filaments/catalog",
    "http.GET /filaments/catalog",
    "http.HEAD /api/v1/filaments/catalog",
    "http.HEAD /filaments/catalog",
];
const DRAINED: DrainEvidence = DrainEvidence {
    tracked_idle: true,
    gateway_idle: true,
    compat: Some(CompatExit::Graceful),
};

fn source_sync_text() -> &'static str {
    include_str!("../../../../web/apps/server/src/data/source-sync-ownership.json")
}

fn index_from(edit: impl FnOnce(&mut serde_json::Value), operations: &str) -> OwnershipIndex {
    let mut manifest: serde_json::Value =
        serde_json::from_str(include_str!("../ownership-manifest.json")).unwrap();
    let groups = manifest["operations"].as_array_mut().unwrap();
    groups.push(json!({
        "operation": CATALOG,
        "owner": "node",
        "participants": ["http.GET /filaments/catalog", "http.GET /api/v1/filaments/catalog"],
    }));
    let unresolved = manifest["resources"]["unresolved"].as_array_mut().unwrap();
    unresolved.retain(|id| id != "http:GET /filaments/catalog current");
    unresolved.push(json!(CATALOG));
    edit(&mut manifest);
    admit_manifest_documents(&manifest.to_string(), operations, source_sync_text()).unwrap()
}

fn fixture_index() -> OwnershipIndex {
    index_from(|_| {}, include_str!("../operations.json"))
}

fn index_with_extra_tool() -> OwnershipIndex {
    index_from(
        |manifest| {
            manifest["node_participants"]["tool"]
                .as_array_mut()
                .unwrap()
                .push(json!("ticket_t_3_drain_extra_tool"));
            manifest["resources"]["unresolved"]
                .as_array_mut()
                .unwrap()
                .push(json!("tool.ticket_t_3_drain_extra_tool"));
        },
        include_str!("../operations.json"),
    )
}

fn parity(
    operation: &str,
    manifest_sha256: &str,
    fixtures: u32,
    passed: u32,
    failed: u32,
) -> String {
    json!({
        "schema": "pp-operation-parity-v1",
        "operation": operation,
        "manifest_sha256": manifest_sha256,
        "fixtures": fixtures,
        "passed": passed,
        "failed": failed,
    })
    .to_string()
}

fn sha256(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn listing(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().into_string().unwrap(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect()
}

fn evidence_lines(dir: &Path) -> Vec<serde_json::Value> {
    fs::read_to_string(dir.join(EVIDENCE_FILE))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn plan_for(
    index: &OwnershipIndex,
    owners: &OperationOwners,
    operation: &str,
) -> cutover::CutoverPlan {
    cutover::plan(
        Some(index),
        owners,
        operation,
        &parity(operation, index.digest(), 3, 3, 0),
    )
    .unwrap()
}

fn switch(index: &OwnershipIndex, dir: &Path, operation: &str) -> CutoverReceipt {
    let (owners, _) = cutover::recover(dir, Some(index)).unwrap();
    let receipt = plan_for(index, &owners, operation).commit(dir, DRAINED);
    assert!(matches!(receipt, CutoverReceipt::Committed { .. }));
    receipt
}

fn plant_record(dir: &Path, digest: &str, operation: &str, participants: &[&str]) {
    let record = json!({
        "schema": "pp-operation-cutover-v1",
        "manifest_sha256": digest,
        "operations": {
            operation: {
                "parity_sha256": "00",
                "fixtures": 3,
                "participants": participants,
            },
        },
    });
    fs::write(dir.join(CUTOVER_FILE), record.to_string()).unwrap();
}

#[test]
fn ticket_t_3_drain_cutover_switches_whole_operation() {
    let index = fixture_index();
    let dir = Scratch::new("pp-t3-cutover-whole-");
    assert_eq!(
        index.participants_of(CATALOG),
        BTreeSet::from(CATALOG_PARTICIPANTS)
    );
    assert_json(
        switch(&index, &dir, CATALOG),
        json!({"outcome": "committed", "operation": CATALOG, "participants": 4, "manifest_sha256": index.digest(), "parity_sha256": sha256(&parity(CATALOG, index.digest(), 3, 3, 0)), "fixtures": 3, "directory_synced": true}),
    );
    let (owners, boot) = cutover::recover(&dir, Some(&index)).unwrap();
    assert_json(
        boot,
        json!({"rust_operations": [CATALOG], "abandoned_temporaries": 0, "evidence_saved": true}),
    );
    let rust_owned: BTreeSet<&str> = index
        .claims()
        .keys()
        .map(String::as_str)
        .filter(|id| owners.owner_of(id) == Owner::Rust)
        .collect();
    assert_eq!(rust_owned, BTreeSet::from(CATALOG_PARTICIPANTS));
    assert_eq!(owners.owner_of("http.GET /filaments/custom"), Owner::Node);
}

async fn exchange(
    origin: &str,
    path: &str,
    method: &str,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let authority = origin.strip_prefix("http://").unwrap();
    let socket = tokio::net::TcpStream::connect(authority).await.unwrap();
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(socket))
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut builder = Request::builder()
        .uri(path)
        .method(method)
        .header("host", authority)
        .header("origin", origin);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    let body = if method == "POST" {
        builder = builder.header("content-type", "application/json");
        Body::from(r#"{"name":"cutover"}"#)
    } else {
        Body::empty()
    };
    let response = sender
        .send_request(builder.body(body).unwrap())
        .await
        .unwrap();
    let (head, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes().to_vec();
    (head.status, head.headers, bytes)
}

struct FakeNode {
    compat: CompatHandle,
    forwards: Arc<Mutex<Vec<String>>>,
    _publication: tokio::sync::watch::Sender<Option<Arc<Endpoint>>>,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

fn fake_node(dir: &Path) -> FakeNode {
    fake_node_with_hold(dir, false)
}

fn fake_node_with_hold(dir: &Path, hold: bool) -> FakeNode {
    let (endpoint, mut child) = Endpoint::fake(dir).unwrap();
    let forwards = Arc::new(Mutex::new(Vec::new()));
    let seen = forwards.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (notify_entered, notify_release) = (entered.clone(), release.clone());
    tokio::spawn(async move {
        while let Ok(stream) = child.accept().await {
            let seen = seen.clone();
            let (entered, release) = (notify_entered.clone(), notify_release.clone());
            tokio::spawn(async move {
                let service =
                    hyper::service::service_fn(move |request: Request<hyper::body::Incoming>| {
                        if request.uri().path() != "/__desktop/moonraker-hosts" {
                            seen.lock().unwrap().push(format!(
                                "{} {}",
                                request.method(),
                                request.uri().path()
                            ));
                        }
                        let (entered, release) = (entered.clone(), release.clone());
                        async move {
                            if hold && request.uri().path() == "/plans" {
                                entered.notify_one();
                                release.notified().await;
                            }
                            Ok::<_, std::convert::Infallible>(Response::new(Body::from("{}")))
                        }
                    });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            });
        }
    });
    let (compat, publication) = CompatHandle::replaceable(Some(endpoint));
    FakeNode {
        compat,
        forwards,
        _publication: publication,
        entered,
        release,
    }
}

async fn session(gateway: &mut Gateway) -> (String, String) {
    let origin = gateway.origin().to_owned();
    let launch = gateway.take_launch_target().unwrap().into_url();
    let (status, headers, _) =
        exchange(&origin, launch.strip_prefix(&origin).unwrap(), "GET", None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let cookie = headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (origin, cookie)
}

async fn rust_owned_gateway(dir: &Path, node: &FakeNode) -> Gateway {
    let index = fixture_index();
    for operation in [
        CATALOG,
        PLANS_CREATE,
        PROFILE_SYNC,
        EXPORT_STL_PACK,
        EXPORT_ACCEPTED_PLATE,
        PRINTER_SEND_QUEUE,
    ] {
        switch(&index, dir, operation);
    }
    let (owners, _) = cutover::recover(dir, Some(&index)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    Gateway::start(listener, PathBuf::new(), node.compat.clone(), &owners).unwrap()
}

fn assert_rust_owner_unavailable(status: StatusCode, headers: &HeaderMap, body: &[u8]) {
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!headers.contains_key("retry-after"));
    if !body.is_empty() {
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(body).unwrap(),
            json!({
                "detail": "Operation is owned by the Rust runtime and has no handler yet",
                "code": "rust_owner_unavailable",
            })
        );
    }
}

async fn assert_gateway_ownership(routes: &[(&str, &str)], node_method: &str) {
    let dir = Scratch::new("pp-t3-cutover-gateway-");
    let node = fake_node(&dir);
    let mut gateway = rust_owned_gateway(&dir, &node).await;
    let (origin, cookie) = session(&mut gateway).await;
    for &(method, path) in routes {
        let (status, headers, body) = exchange(&origin, path, method, Some(&cookie)).await;
        assert_eq!(body.is_empty(), method == "HEAD", "{method} {path}");
        assert_rust_owner_unavailable(status, &headers, &body);
    }
    assert!(node.forwards.lock().unwrap().is_empty());
    let (status, _, _) = exchange(&origin, "/filaments/custom", node_method, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        *node.forwards.lock().unwrap(),
        vec![format!("{node_method} /filaments/custom")]
    );
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_3_drain_cutover_gateway_routes_whole_operation() {
    assert_gateway_ownership(
        &[
            ("GET", "/filaments/catalog"),
            ("GET", "/api/v1/filaments/catalog"),
            ("HEAD", "/filaments/catalog"),
            ("HEAD", "/api/v1/filaments/catalog"),
        ],
        "GET",
    )
    .await;
}

#[tokio::test]
async fn ticket_t_3_drain_cutover_gateway_routes_worker_operation() {
    assert_gateway_ownership(
        &[("GET", "/ws/profile-sync"), ("HEAD", "/ws/profile-sync")],
        "GET",
    )
    .await;
}

#[tokio::test]
async fn ticket_t_3_drain_cutover_gateway_routes_export_operation() {
    assert_gateway_ownership(
        &[
            ("POST", "/jobs/export-stl-pack"),
            ("POST", "/api/v1/jobs/export-stl-pack"),
            ("POST", "/jobs/export-accepted-plate-3mf"),
            ("POST", "/slicer-instances/1/open-accepted-plates"),
            ("POST", "/api/v1/slicer-instances/1/open-accepted-plates"),
        ],
        "POST",
    )
    .await;
}

#[tokio::test]
async fn ticket_t_3_drain_cutover_gateway_routes_printer_send_queue_operation() {
    assert_gateway_ownership(
        &[
            ("GET", "/printer-send-queue"),
            ("HEAD", "/api/v1/printer-send-queue"),
            ("POST", "/printer-send-queue"),
            ("POST", "/api/v1/printer-send-queue/item-1/dispatch"),
            ("POST", "/printer-send-queue/drain"),
            ("DELETE", "/printer-send-queue/item-1"),
            ("DELETE", "/api/v1/printer-send-queue/item-1"),
            ("POST", "/jobs/printer-upload"),
            ("POST", "/api/v1/jobs/printer-upload"),
        ],
        "GET",
    )
    .await;
    // Suggestions only read the queue, so they stay with Node.
    let dir = Scratch::new("pp-t3-cutover-gateway-reader-");
    let node = fake_node(&dir);
    let mut gateway = rust_owned_gateway(&dir, &node).await;
    let (origin, cookie) = session(&mut gateway).await;
    let (status, _, _) = exchange(
        &origin,
        "/printer-send-queue/suggestions",
        "GET",
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        *node.forwards.lock().unwrap(),
        vec!["GET /printer-send-queue/suggestions".to_owned()]
    );
    gateway.stop().await.unwrap();
}

#[tokio::test]
async fn ticket_t_3_drain_cutover_no_mutation_failover() {
    assert_gateway_ownership(&[("POST", "/plans"), ("POST", "/api/v2/plans")], "POST").await;
}

#[tokio::test]
async fn ticket_t_3_drain_cutover_gateway_refuses_unregistered_rust_route() {
    let mut registry: serde_json::Value =
        serde_json::from_str(include_str!("../operations.json")).unwrap();
    registry["routes"].as_array_mut().unwrap().push(json!({
        "method": "POST",
        "path": "/ticket-t-3-drain/rust-only",
        "owner": "compat",
        "effect": "local_commit",
        "operation": "POST /ticket-t-3-drain/rust-only current",
    }));
    let index = index_from(
        |manifest| {
            manifest["resources"]["unresolved"]
                .as_array_mut()
                .unwrap()
                .push(json!("http:POST /ticket-t-3-drain/rust-only current"));
        },
        &registry.to_string(),
    );
    let dir = Scratch::new("pp-t3-cutover-unregistered-");
    let operation = "http:POST /ticket-t-3-drain/rust-only current";
    assert!(matches!(
        switch(&index, &dir, operation),
        CutoverReceipt::Committed {
            participants: 1,
            ..
        }
    ));
    let (owners, _) = cutover::recover(&dir, Some(&index)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let error = Gateway::start(
        listener,
        PathBuf::new(),
        CompatHandle::unpublished(),
        &owners,
    )
    .err()
    .unwrap();
    assert_eq!(
        error.to_string(),
        "Rust-owned participant http.POST /ticket-t-3-drain/rust-only is not a registered route"
    );
}

#[test]
fn ticket_t_3_drain_cutover_subset_record_fails_closed() {
    let index = fixture_index();
    let dir = Scratch::new("pp-t3-cutover-subset-");
    plant_record(
        &dir,
        index.digest(),
        CATALOG,
        &["http.GET /filaments/catalog"],
    );
    let error = cutover::recover(&dir, Some(&index)).unwrap_err();
    assert_json(
        error,
        json!({"reason": "participant_set_mismatch", "operation": CATALOG, "missing": [
                "http.GET /api/v1/filaments/catalog",
                "http.HEAD /api/v1/filaments/catalog",
                "http.HEAD /filaments/catalog",
            ], "unexpected": []}),
    );
    assert_eq!(
        evidence_lines(&dir),
        vec![json!({
            "event": "boot_failed",
            "reason": "participant_set_mismatch",
            "operation": CATALOG,
            "missing": [
                "http.GET /api/v1/filaments/catalog",
                "http.HEAD /api/v1/filaments/catalog",
                "http.HEAD /filaments/catalog",
            ],
            "unexpected": [],
        })]
    );
}

#[test]
fn ticket_t_3_drain_cutover_refuses_participant_node_cannot_fence() {
    let index = fixture_index();
    let owners = OperationOwners::node_only();
    assert_json(
        cutover::plan(
            Some(&index),
            &owners,
            UNFENCED,
            &parity(UNFENCED, index.digest(), 3, 3, 0),
        )
        .unwrap_err(),
        json!({"reason": "node_cannot_fence", "participant": UNFENCED}),
    );
    let dir = Scratch::new("pp-t3-cutover-unfenced-");
    plant_record(&dir, index.digest(), UNFENCED, &[UNFENCED]);
    assert_json(
        cutover::recover(&dir, Some(&index)).unwrap_err(),
        json!({"reason": "node_cannot_fence", "participant": UNFENCED}),
    );
}

#[test]
fn ticket_t_3_drain_cutover_switches_source_sync_as_one_operation() {
    let index = fixture_index();
    let participants = index.participants_of(SOURCE_SYNC);
    // 30 HTTP routes and aliases (GET routes bring their HEAD twins) plus nine others.
    assert_eq!(participants.len(), 39);
    let non_http: BTreeSet<&str> = participants
        .iter()
        .copied()
        .filter(|id| !id.starts_with("http."))
        .collect();
    assert_eq!(
        non_http,
        BTreeSet::from([
            "assistant.propose_import_source_files",
            "job.check-source-updates",
            "job.extract-source-docs",
            "job.import-scan",
            "job.sync",
            "mcp.start_sync",
            "migration.migrateLegacySourceManifestOverridesForTenant",
            "startup.startSourceWatcher",
            "worker.startSourceWatcher",
        ])
    );
    let dir = Scratch::new("pp-t3-cutover-source-sync-");
    assert_json(
        switch(&index, &dir, SOURCE_SYNC),
        json!({"outcome": "committed", "operation": SOURCE_SYNC,
            "participants": 39,
            "manifest_sha256": index.digest(),
            "parity_sha256": sha256(&parity(SOURCE_SYNC, index.digest(), 3, 3, 0)),
            "fixtures": 3, "directory_synced": true}),
    );
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join(CUTOVER_FILE)).unwrap()).unwrap();
    assert_eq!(
        record["operations"][SOURCE_SYNC]["participants"],
        json!(participants)
    );
    let (owners, _) = cutover::recover(&dir, Some(&index)).unwrap();
    let rust_owned: BTreeSet<&str> = index
        .claims()
        .keys()
        .map(String::as_str)
        .filter(|id| owners.owner_of(id) == Owner::Rust)
        .collect();
    assert_eq!(rust_owned, participants);

    // A record that leaves any job, worker or hook with Node is refused at boot.
    for kept in &non_http {
        let mut partial: Vec<&str> = participants.iter().copied().collect();
        partial.retain(|id| id != kept);
        let split = Scratch::new("pp-t3-cutover-source-sync-split-");
        plant_record(&split, index.digest(), SOURCE_SYNC, &partial);
        assert_json(
            cutover::recover(&split, Some(&index)).unwrap_err(),
            json!({"reason": "participant_set_mismatch", "operation": SOURCE_SYNC,
                "missing": [kept], "unexpected": []}),
        );
    }
}

#[test]
fn ticket_t_3_drain_cutover_switches_fenced_worker_with_aliases() {
    let index = fixture_index();
    let dir = Scratch::new("pp-t3-cutover-worker-");
    let participants = [
        "http.GET /ws/profile-sync",
        "http.HEAD /ws/profile-sync",
        "worker.startManagedProfileSync",
    ];
    assert_eq!(
        index.participants_of(PROFILE_SYNC),
        BTreeSet::from(participants)
    );
    assert_json(
        switch(&index, &dir, PROFILE_SYNC),
        json!({"outcome": "committed", "operation": PROFILE_SYNC, "participants": 3,
            "manifest_sha256": index.digest(),
            "parity_sha256": sha256(&parity(PROFILE_SYNC, index.digest(), 3, 3, 0)),
            "fixtures": 3, "directory_synced": true}),
    );
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.join(CUTOVER_FILE)).unwrap()).unwrap();
    assert_eq!(
        record["operations"][PROFILE_SYNC]["participants"],
        json!(participants)
    );
    let (owners, _) = cutover::recover(&dir, Some(&index)).unwrap();
    let rust_owned: BTreeSet<&str> = index
        .claims()
        .keys()
        .map(String::as_str)
        .filter(|id| owners.owner_of(id) == Owner::Rust)
        .collect();
    assert_eq!(rust_owned, BTreeSet::from(participants));

    let mut partial = participants.to_vec();
    partial.retain(|id| !id.starts_with("worker."));
    let split = Scratch::new("pp-t3-cutover-worker-split-");
    plant_record(&split, index.digest(), PROFILE_SYNC, &partial);
    assert_json(
        cutover::recover(&split, Some(&index)).unwrap_err(),
        json!({"reason": "participant_set_mismatch", "operation": PROFILE_SYNC,
            "missing": ["worker.startManagedProfileSync"], "unexpected": []}),
    );
}

#[test]
fn ticket_t_3_drain_cutover_switches_export_jobs_with_their_routes() {
    let index = fixture_index();
    for (operation, participants) in EXPORT_OPERATIONS {
        assert_eq!(
            index.participants_of(operation),
            participants.iter().copied().collect::<BTreeSet<_>>(),
            "{operation}"
        );
        let dir = Scratch::new("pp-t3-cutover-export-");
        assert_json(
            switch(&index, &dir, operation),
            json!({"outcome": "committed", "operation": operation,
                "participants": participants.len(),
                "manifest_sha256": index.digest(),
                "parity_sha256": sha256(&parity(operation, index.digest(), 3, 3, 0)),
                "fixtures": 3, "directory_synced": true}),
        );
        let (owners, _) = cutover::recover(&dir, Some(&index)).unwrap();
        let rust_owned: BTreeSet<&str> = index
            .claims()
            .keys()
            .map(String::as_str)
            .filter(|id| owners.owner_of(id) == Owner::Rust)
            .collect();
        assert_eq!(rust_owned, participants.iter().copied().collect());

        // Leaving the job with Node would split the operation.
        let job = participants
            .iter()
            .find(|id| id.starts_with("job."))
            .unwrap();
        let mut partial = participants.to_vec();
        partial.retain(|id| id != job);
        let split = Scratch::new("pp-t3-cutover-export-split-");
        plant_record(&split, index.digest(), operation, &partial);
        assert_json(
            cutover::recover(&split, Some(&index)).unwrap_err(),
            json!({"reason": "participant_set_mismatch", "operation": operation,
                "missing": [job], "unexpected": []}),
        );
    }
}

#[test]
fn ticket_t_3_drain_cutover_switches_printer_send_queue_with_every_writer() {
    let index = fixture_index();
    assert_eq!(
        index.participants_of(PRINTER_SEND_QUEUE),
        PRINTER_SEND_QUEUE_PARTICIPANTS.into_iter().collect()
    );
    let dir = Scratch::new("pp-t3-cutover-printer-");
    assert_json(
        switch(&index, &dir, PRINTER_SEND_QUEUE),
        json!({"outcome": "committed", "operation": PRINTER_SEND_QUEUE,
            "participants": PRINTER_SEND_QUEUE_PARTICIPANTS.len(),
            "manifest_sha256": index.digest(),
            "parity_sha256": sha256(&parity(PRINTER_SEND_QUEUE, index.digest(), 3, 3, 0)),
            "fixtures": 3, "directory_synced": true}),
    );
    let (owners, _) = cutover::recover(&dir, Some(&index)).unwrap();
    let rust_owned: BTreeSet<&str> = index
        .claims()
        .keys()
        .map(String::as_str)
        .filter(|id| owners.owner_of(id) == Owner::Rust)
        .collect();
    assert_eq!(
        rust_owned,
        PRINTER_SEND_QUEUE_PARTICIPANTS.into_iter().collect()
    );
    // The read-only suggestions route and the shared job lookup stay with Node.
    for node_owned in [
        "http.GET /printer-send-queue/suggestions",
        "http.GET /jobs/:id",
    ] {
        assert_eq!(owners.owner_of(node_owned), Owner::Node, "{node_owned}");
    }

    // Leaving the shutdown hook with Node would let it rewrite Rust's rows.
    let shutdown = "shutdown.interruptPrinterUploadsOnShutdown";
    let mut partial = PRINTER_SEND_QUEUE_PARTICIPANTS.to_vec();
    partial.retain(|id| *id != shutdown);
    let split = Scratch::new("pp-t3-cutover-printer-split-");
    plant_record(&split, index.digest(), PRINTER_SEND_QUEUE, &partial);
    assert_json(
        cutover::recover(&split, Some(&index)).unwrap_err(),
        json!({"reason": "participant_set_mismatch", "operation": PRINTER_SEND_QUEUE,
            "missing": [shutdown], "unexpected": []}),
    );
}

#[test]
fn ticket_t_3_drain_cutover_parity_refusals() {
    let index = fixture_index();
    let other = index_with_extra_tool();
    let owners = OperationOwners::node_only();
    let refuse = |text: &str| cutover::plan(Some(&index), &owners, CATALOG, text).unwrap_err();
    let mut unknown_field: serde_json::Value =
        serde_json::from_str(&parity(CATALOG, index.digest(), 3, 3, 0)).unwrap();
    unknown_field["skipped"] = json!(0);
    let mut wrong_schema = unknown_field.clone();
    wrong_schema.as_object_mut().unwrap().remove("skipped");
    wrong_schema["schema"] = json!("pp-operation-parity-v0");
    for malformed in [
        String::new(),
        "{".to_owned(),
        unknown_field.to_string(),
        wrong_schema.to_string(),
    ] {
        assert!(
            matches!(refuse(&malformed), CutoverRefusal::ParityMalformed { .. }),
            "{malformed}"
        );
    }
    assert_json(
        refuse(&parity(CATALOG, index.digest(), 0, 0, 0)),
        json!({"reason": "parity_no_fixtures"}),
    );
    for (passed, failed) in [(2, 1), (3, 1), (2, 0)] {
        assert_json(
            refuse(&parity(CATALOG, index.digest(), 3, passed, failed)),
            json!({"reason": "parity_failures", "fixtures": 3, "passed": passed, "failed": failed}),
        );
    }
    assert_json(
        refuse(&parity(PROFILE_SYNC, index.digest(), 3, 3, 0)),
        json!({"reason": "parity_operation_mismatch", "expected": CATALOG, "actual": PROFILE_SYNC}),
    );
    assert_ne!(index.digest(), other.digest());
    assert_json(
        refuse(&parity(CATALOG, other.digest(), 3, 3, 0)),
        json!({"reason": "parity_manifest_mismatch", "expected": index.digest(), "actual": other.digest()}),
    );
}

#[test]
fn ticket_t_3_drain_cutover_flag_off_refuses_and_writes_nothing() {
    let index = fixture_index();
    assert_json(
        cutover::plan(
            None,
            &OperationOwners::node_only(),
            CATALOG,
            &parity(CATALOG, index.digest(), 3, 3, 0),
        )
        .unwrap_err(),
        json!({"reason": "flag_off"}),
    );
    let empty = Scratch::new("pp-t3-cutover-flag-off-empty-");
    assert_eq!(
        cutover::recover(&empty, None).unwrap(),
        (
            OperationOwners::node_only(),
            BootReceipt {
                rust_operations: Vec::new(),
                abandoned_temporaries: 0,
                evidence_saved: true,
            }
        )
    );
    assert!(listing(&empty).is_empty());
    let recorded = Scratch::new("pp-t3-cutover-flag-off-record-");
    switch(&index, &recorded, CATALOG);
    assert_json(
        cutover::recover(&recorded, None).unwrap_err(),
        json!({"reason": "record_without_flag"}),
    );
}

fn assert_commit_changes_nothing(drained: DrainEvidence, expected: CutoverReceipt) {
    let index = fixture_index();
    let dir = Scratch::new("pp-t3-cutover-unchanged-");
    switch(&index, &dir, CATALOG);
    let (owners, _) = cutover::recover(&dir, Some(&index)).unwrap();
    let before = listing(&dir);
    let record = fs::read(dir.join(CUTOVER_FILE)).unwrap();
    assert_eq!(
        plan_for(&index, &owners, PLANS_CREATE).commit(&dir, drained),
        expected
    );
    assert_eq!(fs::read(dir.join(CUTOVER_FILE)).unwrap(), record);
    assert_eq!(listing(&dir), before);
}

#[test]
fn ticket_t_3_drain_cutover_drain_timeout_changes_nothing() {
    for (tracked_idle, gateway_idle) in [(true, false), (false, true), (false, false)] {
        assert_commit_changes_nothing(
            DrainEvidence {
                tracked_idle,
                gateway_idle,
                compat: Some(CompatExit::Graceful),
            },
            CutoverReceipt::DrainTimedOut {
                operation: PLANS_CREATE.into(),
                tracked_idle,
                gateway_idle,
            },
        );
    }
}

#[test]
fn ticket_t_3_drain_cutover_unreaped_compat_changes_nothing() {
    assert_commit_changes_nothing(
        DrainEvidence {
            compat: None,
            ..DRAINED
        },
        CutoverReceipt::CompatNotReaped {
            operation: PLANS_CREATE.into(),
        },
    );
}

#[test]
fn ticket_t_3_drain_cutover_interrupted_compat_changes_nothing() {
    let drained = DrainEvidence {
        compat: Some(CompatExit::Interrupted),
        ..DRAINED
    };
    let expected = CutoverReceipt::CompatInterrupted {
        operation: PLANS_CREATE.into(),
    };
    assert_commit_changes_nothing(drained, expected.clone());
    let index = fixture_index();
    let dir = Scratch::new("pp-t3-cutover-interrupted-");
    let (owners, _) = cutover::recover(&dir, Some(&index)).unwrap();
    assert_eq!(
        plan_for(&index, &owners, PLANS_CREATE).commit(&dir, drained),
        expected
    );
    assert!(!dir.join(CUTOVER_FILE).exists());
}

#[test]
fn ticket_t_3_drain_cutover_force_killed_compat_changes_nothing() {
    assert_commit_changes_nothing(
        DrainEvidence {
            compat: Some(CompatExit::Forced),
            ..DRAINED
        },
        CutoverReceipt::CompatForceKilled {
            operation: PLANS_CREATE.into(),
        },
    );
}

#[test]
fn ticket_t_3_drain_cutover_crash_before_rename_recovers_node() {
    let index = fixture_index();
    let committed = Scratch::new("pp-t3-cutover-committed-body-");
    switch(&index, &committed, CATALOG);
    let dir = Scratch::new("pp-t3-cutover-before-rename-");
    let temporary = dir.join(".operation-cutover.abc.tmp");
    fs::copy(committed.join(CUTOVER_FILE), &temporary).unwrap();
    let (owners, boot) = cutover::recover(&dir, Some(&index)).unwrap();
    assert_eq!(owners, OperationOwners::node_only());
    assert_json(
        boot,
        json!({"rust_operations": [], "abandoned_temporaries": 1, "evidence_saved": true}),
    );
    assert!(!temporary.exists());
    assert!(!dir.join(CUTOVER_FILE).exists());
    assert_eq!(
        evidence_lines(&dir),
        vec![
            json!({"event": "boot", "rust_operations": [], "abandoned_temporaries": 1, "evidence_saved": true})
        ]
    );
}

#[test]
fn ticket_t_3_drain_cutover_crash_after_rename_recovers_rust() {
    let index = fixture_index();
    let dir = Scratch::new("pp-t3-cutover-after-rename-");
    switch(&index, &dir, CATALOG);
    let temporary = dir.join(".operation-cutover.def.tmp");
    fs::copy(dir.join(CUTOVER_FILE), &temporary).unwrap();
    let (owners, boot) = cutover::recover(&dir, Some(&index)).unwrap();
    assert_json(
        boot,
        json!({"rust_operations": [CATALOG], "abandoned_temporaries": 1, "evidence_saved": true}),
    );
    assert_eq!(
        owners.owner_of("http.HEAD /api/v1/filaments/catalog"),
        Owner::Rust
    );
    assert!(!temporary.exists());
}

#[test]
fn ticket_t_3_drain_cutover_restart_is_idempotent() {
    let index = fixture_index();
    let dir = Scratch::new("pp-t3-cutover-idempotent-");
    switch(&index, &dir, CATALOG);
    let record = fs::read(dir.join(CUTOVER_FILE)).unwrap();
    let first = cutover::recover(&dir, Some(&index)).unwrap();
    let second = cutover::recover(&dir, Some(&index)).unwrap();
    assert_eq!(first, second);
    assert_json(
        second.1,
        json!({"rust_operations": [CATALOG], "abandoned_temporaries": 0, "evidence_saved": true}),
    );
    assert_json(
        cutover::plan(
            Some(&index),
            &second.0,
            CATALOG,
            &parity(CATALOG, index.digest(), 3, 3, 0),
        )
        .unwrap_err(),
        json!({"reason": "already_rust_owned"}),
    );
    assert_eq!(fs::read(dir.join(CUTOVER_FILE)).unwrap(), record);
}

#[test]
fn ticket_t_3_drain_cutover_changed_manifest_on_restart_fails_closed() {
    let bundled = fixture_index();
    let upgraded = index_with_extra_tool();
    let dir = Scratch::new("pp-t3-cutover-manifest-changed-");
    switch(&bundled, &dir, CATALOG);
    assert_json(
        cutover::recover(&dir, Some(&upgraded)).unwrap_err(),
        json!({"reason": "restart_manifest_changed", "recorded": bundled.digest(), "bundled": upgraded.digest()}),
    );
}

#[test]
fn ticket_t_11_routing_changed_manifest_refuses_planning_before_shutdown() {
    let original = fixture_index();
    let changed = index_with_extra_tool();
    let dir = Scratch::new("pp-t3-cutover-planning-manifest-changed-");
    switch(&original, &dir, CATALOG);
    let (owners, _) = cutover::recover(&dir, Some(&original)).unwrap();
    let before = listing(&dir);

    // Both a new operation and an already-owned operation must report the
    // authority mismatch, even with passing parity for the new manifest.
    for operation in [PLANS_CREATE, CATALOG] {
        let before_planning = listing(&dir);
        let refusal = cutover::plan(
            Some(&changed),
            &owners,
            operation,
            &parity(operation, changed.digest(), 3, 3, 0),
        )
        .unwrap_err();
        assert_json(
            &refusal,
            json!({"reason": "ownership_manifest_mismatch",
                "recorded": original.digest(), "current": changed.digest()}),
        );
        assert_eq!(listing(&dir), before_planning);
        cutover::append_evidence(
            &dir,
            &Evidence::Refused {
                operation,
                refusal: &refusal,
            },
        )
        .unwrap();
    }
    assert_eq!(
        fs::read(dir.join(CUTOVER_FILE)).unwrap(),
        before[CUTOVER_FILE],
    );
    assert_eq!(owners.owner_of("http.POST /plans"), Owner::Node);
    assert_eq!(owners.owner_of("http.GET /filaments/catalog"), Owner::Rust);
    assert!(matches!(
        cutover::recover(&dir, Some(&changed)),
        Err(CutoverError::RestartManifestChanged { .. }),
    ));
    let (recovered, _) = cutover::recover(&dir, Some(&original)).unwrap();
    assert_eq!(recovered, owners);
    assert_eq!(
        evidence_lines(&dir)[1],
        json!({
            "event": "refused", "operation": PLANS_CREATE,
            "reason": "ownership_manifest_mismatch",
            "recorded": original.digest(), "current": changed.digest(),
        })
    );
}

#[test]
fn ticket_t_11_routing_matching_manifest_preserves_prior_cutovers() {
    let index = fixture_index();
    let dir = Scratch::new("pp-t3-cutover-planning-manifest-matches-");
    switch(&index, &dir, CATALOG);
    let (owners, _) = cutover::recover(&dir, Some(&index)).unwrap();
    let receipt = plan_for(&index, &owners, PLANS_CREATE).commit(&dir, DRAINED);
    assert!(matches!(receipt, CutoverReceipt::Committed { .. }));
    let (recovered, _) = cutover::recover(&dir, Some(&index)).unwrap();
    assert_eq!(recovered.owner_of("http.POST /plans"), Owner::Rust);
    for participant in CATALOG_PARTICIPANTS {
        assert_eq!(recovered.owner_of(participant), Owner::Rust);
    }
    assert_eq!(recovered.rust_operations().count(), 2);

    let changed = index_with_extra_tool();
    let fresh = Scratch::new("pp-t3-cutover-planning-new-authority-");
    let receipt =
        plan_for(&changed, &OperationOwners::node_only(), CATALOG).commit(&fresh, DRAINED);
    assert!(matches!(receipt, CutoverReceipt::Committed { .. }));
    assert!(cutover::recover(&fresh, Some(&changed)).is_ok());
}

#[test]
fn ticket_t_3_drain_cutover_receipts_serialize_exactly() {
    let operation = || CATALOG.to_owned();
    for (receipt, expected) in [
        (
            CutoverReceipt::Committed {
                operation: operation(),
                participants: 4,
                manifest_sha256: "aa".into(),
                parity_sha256: "bb".into(),
                fixtures: 3,
                directory_synced: false,
            },
            json!({"outcome": "committed", "operation": CATALOG, "participants": 4,
                "manifest_sha256": "aa", "parity_sha256": "bb", "fixtures": 3,
                "directory_synced": false}),
        ),
        (
            CutoverReceipt::DrainTimedOut {
                operation: operation(),
                tracked_idle: true,
                gateway_idle: false,
            },
            json!({"outcome": "drain_timed_out", "operation": CATALOG,
                "tracked_idle": true, "gateway_idle": false}),
        ),
        (
            CutoverReceipt::CompatNotReaped {
                operation: operation(),
            },
            json!({"outcome": "compat_not_reaped", "operation": CATALOG}),
        ),
        (
            CutoverReceipt::CompatInterrupted {
                operation: operation(),
            },
            json!({"outcome": "compat_interrupted", "operation": CATALOG}),
        ),
        (
            CutoverReceipt::CompatForceKilled {
                operation: operation(),
            },
            json!({"outcome": "compat_force_killed", "operation": CATALOG}),
        ),
        (
            CutoverReceipt::CommitUnproved {
                operation: operation(),
            },
            json!({"outcome": "commit_unproved", "operation": CATALOG}),
        ),
        (
            CutoverReceipt::CommitFailed {
                operation: operation(),
                detail: "disk full".into(),
            },
            json!({"outcome": "commit_failed", "operation": CATALOG, "detail": "disk full"}),
        ),
    ] {
        assert_json(&receipt, expected.clone());
        let mut line = expected;
        line["event"] = json!("cutover");
        assert_json(Evidence::Cutover(&receipt), line);
    }
    assert_json(
        Evidence::Refused {
            operation: CATALOG,
            refusal: &CutoverRefusal::ParityFailures {
                fixtures: 3,
                passed: 2,
                failed: 1,
            },
        },
        json!({"event": "refused", "operation": CATALOG, "reason": "parity_failures",
            "fixtures": 3, "passed": 2, "failed": 1}),
    );
    assert_json(
        Evidence::Refused {
            operation: CATALOG,
            refusal: &CutoverRefusal::FlagOff,
        },
        json!({"event": "refused", "operation": CATALOG, "reason": "flag_off"}),
    );
    assert_json(
        Evidence::Boot(&BootReceipt {
            rust_operations: vec![CATALOG.into()],
            abandoned_temporaries: 1,
            evidence_saved: false,
        }),
        json!({"event": "boot", "rust_operations": [CATALOG], "abandoned_temporaries": 1, "evidence_saved": false}),
    );
    assert_json(
        Evidence::BootFailed(&CutoverError::RestartManifestChanged {
            recorded: "aa".into(),
            bundled: "bb".into(),
        }),
        json!({"event": "boot_failed", "reason": "restart_manifest_changed",
            "recorded": "aa", "bundled": "bb"}),
    );
}

#[test]
fn ticket_t_3_drain_cutover_evidence_log_appends_exact_lines() {
    let index = fixture_index();
    let dir = Scratch::new("pp-t3-cutover-evidence-");
    let owners = OperationOwners::node_only();
    let refusal = cutover::plan(Some(&index), &owners, UNFENCED, "{}").unwrap_err();
    cutover::append_evidence(
        &dir,
        &Evidence::Refused {
            operation: UNFENCED,
            refusal: &refusal,
        },
    )
    .unwrap();
    let timed_out = plan_for(&index, &owners, CATALOG).commit(
        &dir,
        DrainEvidence {
            gateway_idle: false,
            ..DRAINED
        },
    );
    cutover::append_evidence(&dir, &Evidence::Cutover(&timed_out)).unwrap();
    let committed = plan_for(&index, &owners, CATALOG).commit(&dir, DRAINED);
    cutover::append_evidence(&dir, &Evidence::Cutover(&committed)).unwrap();
    assert_eq!(
        evidence_lines(&dir),
        vec![
            json!({"event": "refused", "operation": UNFENCED, "reason": "node_cannot_fence",
                "participant": UNFENCED}),
            json!({"event": "cutover", "outcome": "drain_timed_out", "operation": CATALOG,
                "tracked_idle": true, "gateway_idle": false}),
            json!({"event": "cutover", "outcome": "committed", "operation": CATALOG,
                "participants": 4, "manifest_sha256": index.digest(),
                "parity_sha256": sha256(&parity(CATALOG, index.digest(), 3, 3, 0)),
                "fixtures": 3, "directory_synced": true}),
        ]
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(dir.join(EVIDENCE_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[tokio::test]
async fn ticket_t_3_drain_cutover_gateway_drain_waits_for_inflight_forward() {
    let dir = Scratch::new("pp-t3-cutover-inflight-");
    let node = fake_node_with_hold(&dir, true);
    let owners = OperationOwners::node_only();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut gateway =
        Gateway::start(listener, PathBuf::new(), node.compat.clone(), &owners).unwrap();
    let (origin, cookie) = session(&mut gateway).await;
    let request =
        tokio::spawn(async move { exchange(&origin, "/plans", "POST", Some(&cookie)).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), node.entered.notified())
        .await
        .unwrap();
    gateway.begin_drain();
    let gateway_idle = gateway
        .wait_idle(std::time::Duration::from_millis(50))
        .await;
    assert!(!gateway_idle);
    let index = fixture_index();
    assert_json(
        plan_for(&index, &owners, PLANS_CREATE).commit(
            &dir,
            DrainEvidence {
                gateway_idle,
                ..DRAINED
            },
        ),
        json!({"outcome": "drain_timed_out", "operation": PLANS_CREATE, "tracked_idle": true, "gateway_idle": false}),
    );
    assert!(!dir.join(CUTOVER_FILE).exists());
    node.release.notify_one();
    assert_eq!(request.await.unwrap().0, StatusCode::OK);
    let gateway_idle = gateway.wait_idle(std::time::Duration::from_secs(1)).await;
    assert!(gateway_idle);
    assert!(matches!(
        plan_for(&index, &owners, PLANS_CREATE).commit(
            &dir,
            DrainEvidence {
                gateway_idle,
                ..DRAINED
            }
        ),
        CutoverReceipt::Committed { .. }
    ));
    gateway.stop().await.unwrap();
}

#[test]
fn ticket_t_3_drain_cutover_record_and_evidence_fail_closed() {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_file as symlink;
    let index = fixture_index();
    let blocked = Scratch::new("pp-t3-cutover-failed-rename-");
    fs::create_dir(blocked.join(CUTOVER_FILE)).unwrap();
    assert!(matches!(
        plan_for(&index, &OperationOwners::node_only(), CATALOG).commit(&blocked, DRAINED),
        CutoverReceipt::CommitFailed { .. }
    ));
    assert!(blocked.join(CUTOVER_FILE).is_dir());
    assert_eq!(fs::read_dir(&blocked).unwrap().count(), 1);
    let dir = Scratch::new("pp-t3-cutover-unsafe-record-");
    let path = dir.join(CUTOVER_FILE);
    fs::write(&path, vec![b' '; 256 * 1024 + 1]).unwrap();
    assert_eq!(
        cutover::recover(&dir, Some(&index)).unwrap_err(),
        CutoverError::Malformed {
            detail: "cutover record exceeds 262144 bytes".into()
        }
    );
    fs::remove_file(&path).unwrap();
    let target = dir.join("target");
    fs::write(&target, "{}").unwrap();
    symlink(&target, &path).unwrap();
    assert!(matches!(
        cutover::recover(&dir, Some(&index)),
        Err(CutoverError::Malformed { .. })
    ));
    fs::remove_file(&path).unwrap();
    switch(&index, &dir, CATALOG);
    fs::remove_file(dir.join(EVIDENCE_FILE)).unwrap();
    symlink(&target, dir.join(EVIDENCE_FILE)).unwrap();
    let (owners, boot) = cutover::recover(&dir, Some(&index)).unwrap();
    assert!(!boot.evidence_saved);
    assert_eq!(owners.owner_of(CATALOG_PARTICIPANTS[0]), Owner::Rust);
    assert_eq!(fs::read_to_string(target).unwrap(), "{}");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["operations"][CATALOG]["fixtures"] = json!(0);
    fs::write(path, record.to_string()).unwrap();
    assert_json(
        cutover::recover(&dir, Some(&index)).unwrap_err(),
        json!({"reason": "no_parity_fixtures", "operation": CATALOG}),
    );
}

#[test]
fn ticket_t_3_drain_cutover_all_bundled_http_operations_recover() {
    use pp_gateway::ownership::manifest::{Disposition, admit_bundled_manifest};
    let index = admit_bundled_manifest().unwrap();
    let operations: BTreeSet<_> = index
        .claims()
        .values()
        .map(|claim| claim.operation.as_str())
        .filter(|operation| {
            index.participants_of(operation).iter().all(|id| {
                id.starts_with("http.")
                    && index.lookup(id).unwrap().disposition == Disposition::Owned(Owner::Node)
            })
        })
        .collect();
    assert!(!operations.is_empty());
    let expected: BTreeSet<_> = operations
        .iter()
        .flat_map(|op| index.participants_of(op))
        .collect();
    let dir = Scratch::new("pp-t3-cutover-all-http-");
    let mut owners = OperationOwners::node_only();
    for operation in &operations {
        let receipt = plan_for(&index, &owners, operation).commit(&dir, DRAINED);
        assert!(
            matches!(receipt, CutoverReceipt::Committed { .. }),
            "{receipt:?}"
        );
        let (recovered, boot) = cutover::recover(&dir, Some(&index)).unwrap();
        assert!(boot.evidence_saved);
        owners = recovered;
    }
    assert_eq!(
        owners.rust_operations().collect::<BTreeSet<_>>(),
        operations
    );
    assert_eq!(
        owners.rust_participants().collect::<BTreeSet<_>>(),
        expected
    );
    for id in index.claims().keys() {
        assert_eq!(
            owners.owner_of(id),
            if expected.contains(id.as_str()) {
                Owner::Rust
            } else {
                Owner::Node
            },
            "{id}"
        );
    }
    let bytes = fs::read(dir.join(CUTOVER_FILE)).unwrap().len();
    println!(
        "bundled cutover operations={}, participants={}, serialized_bytes={bytes}",
        operations.len(),
        expected.len()
    );
    assert!(bytes > 64 * 1024 && bytes <= 256 * 1024);
}

fn assert_json(value: impl serde::Serialize, expected: serde_json::Value) {
    assert_eq!(serde_json::to_value(value).unwrap(), expected);
}

#[path = "../../pp-storage/tests/common/scratch.rs"]
mod scratch;
use scratch::Scratch;
#[path = "../../pp-storage/tests/common/cleanup.rs"]
mod cleanup;

#[test]
fn scratch_root_is_empty_after_tests() {
    cleanup::assert_test_run_clean(&["--skip", "scratch_root_is_empty_after_tests"]);
}
