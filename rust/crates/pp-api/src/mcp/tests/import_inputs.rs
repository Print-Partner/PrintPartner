//! `propose_import_build_inputs` and its `confirm_apply`: `source_id` inputs
//! (GRE-1300 part 2, GRE-1521) and model-page and link `url` inputs (part 3a,
//! GRE-1526), through the MCP router with an API key, on the Build the
//! Source-roles tests seed.
use super::create_build::{confirm, database, execute, pending};
use super::source_roles::Seeded;
use super::*;
use pp_storage::read_model::planning::EvidenceKind;

const TOOL: &str = "propose_import_build_inputs";
/// Library Source S3, synced, which B's brief does not name yet.
const S3: i64 = 30;
/// Node's evidence id for `printpartner:source:30`.
const S3_EVIDENCE: &str = "9e0e0c90973d2846b2be";
const PRINTABLES: &str = "https://www.printables.com/model/12345-kit/files";

async fn seeded() -> Seeded {
    let seeded = Seeded::new().await;
    execute(
        &seeded.server,
        &format!(
            "INSERT INTO projects(id,tenant_id,name,url,role,local_path,last_synced_at,last_commit_sha) \
             VALUES({S3},'default','S3','https://github.com/example/s3','mod','/sources/s3', \
             '2026-11-01T10:00:00.000Z','abc123');"
        ),
    );
    seeded
}

fn args(seeded: &Seeded, inputs: Value) -> Value {
    json!({ "plan_id": seeded.build, "inputs": inputs })
}

/// Every Library Source row, as JSON text.
fn library(seeded: &Seeded) -> String {
    database(&seeded.server)
        .query_row(
            "SELECT json_group_array(json_array(id,name,url,role,branch,local_path,last_synced_at,last_commit_sha)) \
             FROM (SELECT * FROM projects ORDER BY id)",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

/// Proposes and confirms `inputs`, and returns the confirm result.
async fn imported(seeded: &Seeded, inputs: Value) -> Value {
    let id = seeded.proposed_call(TOOL, args(seeded, inputs)).await;
    let (confirmed, error) = confirm(&seeded.server, &seeded.headers, &id).await;
    assert!(!error, "{confirmed}");
    confirmed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_import_inputs_attach_a_source_and_clear_the_selected_draft() {
    let seeded = seeded().await;
    let (sources, library_roles) = (library(&seeded), seeded.untouched());
    let layers = seeded.state()[0].clone();

    let confirmed = imported(&seeded, json!([{ "source_id": S3, "kind": "mod" }])).await;
    assert_eq!(
        confirmed,
        json!({ "ok": true, "result": {
            "evidence": [{
                "id": S3_EVIDENCE, "kind": "mod", "source_id": S3,
                "url": format!("printpartner:source:{S3}"), "upload_required": false,
            }],
            "created_source_ids": [],
        } })
    );
    assert!(pending(&seeded.server, &seeded.headers).await.is_empty());

    let brief = seeded.planning().await.brief;
    let items: Vec<_> = brief
        .evidence
        .iter()
        .map(|item| (item.id.as_str(), item.source_id, item.kind))
        .collect();
    assert_eq!(
        items,
        [
            ("E1", Some(seeded.sources[0]), EvidenceKind::CanonicalDesign),
            ("E2", Some(seeded.sources[1]), EvidenceKind::CanonicalDesign),
            (S3_EVIDENCE, Some(S3), EvidenceKind::Mod),
        ]
    );
    assert_eq!(brief.draft_id, None);
    assert_eq!(
        library(&seeded),
        sources,
        "no Source was created or changed"
    );
    assert_eq!(
        seeded.untouched(),
        library_roles,
        "Library roles and Accepted Plan"
    );
    assert_eq!(seeded.state()[0], layers, "B's layers");

    // Importing S3 again leaves one evidence item for it.
    imported(&seeded, json!([{ "source_id": S3, "title": "Kit" }])).await;
    let evidence = seeded.brief()["evidence"].clone();
    let for_s3: Vec<_> = evidence
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["source_id"] == S3)
        .collect();
    assert_eq!(for_s3.len(), 1, "{evidence}");
    assert_eq!(for_s3[0]["title"], "Kit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_import_inputs_attach_model_pages_and_links_by_url() {
    let seeded = seeded().await;
    let sources = library(&seeded);
    let confirmed = imported(
        &seeded,
        json!([
            { "url": format!("{PRINTABLES}#comments") },
            { "url": "  HTTPS://MakerWorld.com/en/models/678/  " },
            { "url": "https://example.com/guide/", "title": "Guide", "extract": " Print the frame first. " },
        ]),
    )
    .await;
    let evidence = &confirmed["result"]["evidence"];
    // Ids from Node's analyzeBuildRequest for the first two URLs; the
    // classifier test in pp-services pins them.
    assert_eq!(evidence[0]["id"], "e23eb2aaca54431c33ba", "{confirmed}");
    assert_eq!(evidence[1]["id"], "e16aee20b7489c02899c", "{confirmed}");
    let fields: Vec<_> = evidence
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["kind"].clone(),
                item["source_id"].clone(),
                item["upload_required"].clone(),
            )
        })
        .collect();
    let model = (json!("model_source"), Value::Null, json!(true));
    let link = (json!("informational_evidence"), Value::Null, json!(false));
    assert_eq!(fields, [model.clone(), model, link]);
    assert_eq!(confirmed["result"]["created_source_ids"], json!([]));
    assert_eq!(library(&seeded), sources, "a page creates no Source");

    let stored = seeded.brief()["evidence"].clone();
    assert_eq!(stored.as_array().unwrap().len(), 5, "{stored}");
    let link = &stored[4];
    assert_eq!(link["normalized_url"], "https://example.com/guide");
    assert_eq!(link["extract"], "Print the frame first.");
    // sha256("Print the frame first.")
    assert_eq!(
        link["content_hash"],
        "8a59b5bfc58ba8baf8e26fece6b3fd9bbe768c98773b1cdf772da15e06bdb1a4"
    );
    assert!(link["retrieved_at"].is_string(), "{link}");
    assert_eq!(seeded.planning().await.brief.draft_id, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_import_inputs_propose_refuses_with_a_code() {
    let seeded = seeded().await;
    let before = (seeded.state(), library(&seeded));
    let s3 = json!([{ "source_id": S3 }]);
    let cases = vec![
        (json!({ "inputs": s3 }), "invalid_plan_id"),
        (json!({ "plan_id": seeded.build }), "invalid_inputs"),
        (args(&seeded, json!([])), "invalid_inputs"),
        (args(&seeded, json!({ "source_id": S3 })), "invalid_inputs"),
        (args(&seeded, json!([S3])), "invalid_input"),
        (
            args(&seeded, json!([{ "source_id": "30" }])),
            "invalid_input",
        ),
        (
            args(
                &seeded,
                json!([{ "url": "https://github.com/example/kit" }]),
            ),
            "url_inputs_not_supported",
        ),
        (
            args(&seeded, json!([{ "url": "ftp://example.com/kit.zip" }])),
            "invalid_url",
        ),
        (args(&seeded, json!([{ "url": 7 }])), "invalid_url"),
        (
            args(&seeded, json!([{ "source_id": S3, "url": PRINTABLES }])),
            "invalid_input",
        ),
        (args(&seeded, json!([{ "kind": "mod" }])), "invalid_input"),
        (
            args(&seeded, json!([{ "source_id": S3, "kind": "part" }])),
            "invalid_evidence_kind",
        ),
        (
            args(&seeded, json!([{ "source_id": 999 }])),
            "source_not_found",
        ),
        (
            args(
                &seeded,
                json!([{ "source_id": S3, "derived_from_evidence_id": "E9" }]),
            ),
            "derived_evidence_not_found",
        ),
        (
            json!({ "plan_id": seeded.build + 1, "inputs": s3 }),
            "no_planning_brief",
        ),
    ];
    seeded.refuses_propose(TOOL, cases).await;
    assert!(pending(&seeded.server, &seeded.headers).await.is_empty());
    assert_eq!((seeded.state(), library(&seeded)), before);
}

/// Confirms proposal `id`, checks the refusal's `code` and the id it names,
/// and that nothing changed.
async fn refused(seeded: &Seeded, id: &str, code: &str, (field, named): (&str, Value)) {
    let before = (seeded.state(), library(seeded));
    let (refused, error) = confirm(&seeded.server, &seeded.headers, id).await;
    assert!(error, "{refused}");
    assert_eq!(refused["ok"], false, "{refused}");
    assert_eq!(refused["code"], code, "{refused}");
    assert_eq!(refused[field], named, "{refused}");
    assert_eq!((seeded.state(), library(seeded)), before, "{code}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_import_inputs_confirm_refuses_with_a_code_and_changes_nothing() {
    let seeded = seeded().await;
    // Each proposal is valid at propose and made stale just before its
    // confirm: E1 leaves the brief, then S3 leaves the Library.
    // The link in the first proposal is not attached either: the inputs of
    // one confirm are attached together or not at all.
    let derived = json!([
        { "url": "https://example.com/guide" },
        { "source_id": S3, "derived_from_evidence_id": "E1" },
    ]);
    let derived = seeded.proposed_call(TOOL, args(&seeded, derived)).await;
    let source = seeded
        .proposed_call(TOOL, args(&seeded, json!([{ "source_id": S3 }])))
        .await;
    let mut brief = seeded.brief();
    brief["evidence"].as_array_mut().unwrap().remove(0);
    seeded.store(&brief);
    let e1 = json!("E1");
    refused(
        &seeded,
        &derived,
        "derived_evidence_not_found",
        ("evidence_id", e1),
    )
    .await;
    execute(
        &seeded.server,
        &format!("DELETE FROM projects WHERE id={S3};"),
    );
    refused(
        &seeded,
        &source,
        "source_not_found",
        ("source_id", json!(S3)),
    )
    .await;
    // A refused confirm keeps its proposal pending.
    assert_eq!(pending(&seeded.server, &seeded.headers).await.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_import_inputs_without_a_key_is_refused_and_proposes_nothing() {
    let seeded = seeded().await;
    let arguments = args(&seeded, json!([{ "source_id": S3 }]));
    seeded.unkeyed_proposes_nothing(TOOL, arguments).await;
}
