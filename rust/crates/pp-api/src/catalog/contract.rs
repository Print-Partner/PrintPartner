//! Generated-client metadata for Library's Source create and job read
//! (GRE-1365 PR 1). It sits beside the catalog routes so `client_contract.rs`
//! does not grow; `export-rust-client` composes it with the base contract.
use crate::client_contract::{ClientAuth, ClientRoute};

/// `createSource` and `readJob`. `pp-core`'s mounted-route test checks them
/// against the composed routers with the base routes.
pub const CLIENT_ROUTES: [ClientRoute; 2] = [
    ClientRoute {
        name: "createSource",
        method: "POST",
        path: "/api/v1/sources",
        auth: ClientAuth::Credential,
        request: Some("CreateSourceRequest"),
        response: "SourceSummary",
        text_params: &[],
        headers: &[],
    },
    ClientRoute {
        name: "readJob",
        method: "GET",
        path: "/api/v1/jobs/{id}",
        auth: ClientAuth::Credential,
        request: None,
        response: "JobSnapshot",
        text_params: &["id"],
        headers: &[],
    },
];

/// Adds [`CLIENT_ROUTES`], their DTO schemas and fixtures to `document`.
#[cfg(feature = "codegen")]
pub fn client_document(
    mut document: serde_json::Value,
) -> Result<serde_json::Value, serde_json::Error> {
    use super::wire::CreateSourceRequest;
    use pp_storage::{
        catalog::SourceSummary,
        jobs::{JobKind, JobSnapshot, SourceSyncResult},
    };
    use serde_json::{Map, json, to_value};
    for (name, schema) in [
        (
            "CreateSourceRequest",
            schemars::schema_for!(CreateSourceRequest),
        ),
        ("SourceSummary", schemars::schema_for!(SourceSummary)),
        ("JobSnapshot", schemars::schema_for!(JobSnapshot)),
    ] {
        document["dtos"][name] = to_value(schema)?;
    }
    if let Some(routes) = document["routes"].as_array_mut() {
        for route in CLIENT_ROUTES {
            routes.push(to_value(route)?);
        }
    }
    let archive = CreateSourceRequest {
        name: "Rack".into(),
        url: None,
        branch: None,
        tag: None,
        source_kind: Some("archive".into()),
        role: None,
        metadata: None,
    };
    let github = CreateSourceRequest {
        name: "Rack \u{2014} \u{30e9}\u{30c3}\u{30af}".into(),
        url: Some("https://github.com/example/rack".into()),
        branch: Some("dev".into()),
        tag: Some("v1.0.0".into()),
        source_kind: Some("github".into()),
        role: Some("primary".into()),
        metadata: Some(Map::from_iter([("note".into(), json!("kit"))])),
    };
    // What `createSource` answers: a stored record with no revision yet.
    let created = SourceSummary {
        id: 9_007_199_254_740_991,
        name: "Rack".into(),
        url: String::new(),
        source_kind: "archive".into(),
        source_type: "kit".into(),
        role: "primary".into(),
        category: None,
        branch: "main".into(),
        tag: None,
        local_path: None,
        content_available: false,
        last_synced_at: None,
        last_commit_sha: None,
        current_source_revision_id: None,
        docs_url: None,
        manifest_community_slug: None,
        metadata: None,
        naming_use_defaults: true,
        update_status: None,
        update_checked_at: None,
        doc_count: 0,
        catalog_metadata_version: None,
    };
    let mut fixtures = vec![
        request_fixture("archive", &archive)?,
        request_fixture("github", &github)?,
        json!({"dto": "SourceSummary", "name": "created", "value": to_value(created)?,
            "rust_accepts": true}),
    ];
    // The read-one answer for each status. `pp_storage` keeps `JobSnapshot`'s
    // version private, so these are written out around Rust-serialized parts.
    let sync = to_value(JobKind::Sync)?;
    let synced = to_value(SourceSyncResult {
        synced: 1,
        failed: 0,
        results: vec![],
        failures: vec![],
    })?;
    for (status, message, progress, result, error, finished) in [
        (
            "pending",
            "Waiting for a compatible worker",
            json!(0),
            json!(null),
            json!(null),
            false,
        ),
        (
            "running",
            "Running",
            json!(40),
            json!(null),
            json!(null),
            false,
        ),
        ("done", "Complete", json!(100), synced, json!(null), true),
        (
            "error",
            "Job failed",
            json!(null),
            json!(null),
            json!("Job failed"),
            true,
        ),
        (
            "cancelled",
            "Cancelled before effects",
            json!(null),
            json!(null),
            json!(null),
            true,
        ),
    ] {
        let value = json!({
            "job_id": format!("job-{status}"),
            "kind": sync,
            "status": status,
            "message": message,
            "progress": progress,
            "result": result,
            "error": error,
            "created_at": "2026-11-13T00:00:00.000Z",
            "finished_at": finished.then_some("2026-11-13T00:01:00.000Z"),
        });
        fixtures.push(json!({"dto": "JobSnapshot", "name": status, "value": value,
            "rust_accepts": true}));
    }
    if let Some(all) = document["fixtures"].as_array_mut() {
        all.extend(fixtures);
    }
    Ok(document)
}

/// A request fixture Rust's own deserializer reads back.
#[cfg(feature = "codegen")]
fn request_fixture(
    name: &str,
    request: &super::wire::CreateSourceRequest,
) -> Result<serde_json::Value, serde_json::Error> {
    let value = serde_json::to_value(request)?;
    let rust_accepts =
        serde_json::from_value::<super::wire::CreateSourceRequest>(value.clone()).is_ok();
    Ok(
        serde_json::json!({"dto": "CreateSourceRequest", "name": name, "value": value,
        "rust_accepts": rust_accepts}),
    )
}
