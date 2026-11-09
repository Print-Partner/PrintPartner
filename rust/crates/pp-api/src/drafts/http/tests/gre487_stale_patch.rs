//! GRE-487, the GRE-247 regression: rapid Plan edits keep every choice when a
//! PATCH held since the first workspace read arrives last.
//!
//! The fixture's open draft 2 starts with the bracket (draft Part 4) at
//! quantity 3 and `excluded.stl` (draft Part 5) left out. Three fresh edits
//! end at quantity 4 with Part 5 included; the held edit, sent last with the
//! starting digest, would put both back.
use super::*;
use pp_storage::read_model::{
    AcceptedRead, Credential, Provenance,
    views::{self, CatalogOnly, MediaObservation, ReviewObservations},
};

const PARTS: &str = "/api/v2/plans/1/drafts/2/parts";
const WORKSPACE: &str = "/plans/1/drafts/2";

/// One click: the bracket's quantity override and Part 5's inclusion together.
fn click(digest: &Value, quantity: u32, included: bool) -> Value {
    json!({"expected_snapshot_digest": digest, "decisions": [
        {"kind": "set_quantity_override", "draft_part_ids": [4], "value": quantity},
        {"kind": "set_included", "draft_part_ids": [5], "value": included}
    ]})
}

/// `(filename, quantity_override, quantity_effective, included)` for the
/// bracket and `excluded.stl`, from a workspace or Review Part list.
fn choices<'a>(parts: impl Iterator<Item = &'a Value>) -> Vec<Value> {
    parts
        .filter(|part| part["filename"] != "gear.stl")
        .map(|part| {
            json!([
                part["filename"],
                part["quantity_override"],
                part["quantity_effective"],
                part["included"]
            ])
        })
        .collect()
}

fn expected(quantity: u32, included: bool) -> Vec<Value> {
    vec![
        json!(["bracket.stl", quantity, quantity, true]),
        json!(["excluded.stl", 1, 1, included]),
    ]
}

fn workspace_choices(workspace: &Value) -> Vec<Value> {
    choices(workspace["parts"].as_array().unwrap().iter())
}

/// The Accepted Plan Review as the Rust read model projects it, with excluded
/// Parts listed.
fn accepted_review(server: &ReadServer) -> Value {
    let batch = server
        .owner
        .accepted_reads_with_policy(policy())
        .unwrap()
        .read(
            Credential::Session(pp_storage::auth::Secret::new(
                "required-unit-fixture-secret".into(),
            )),
            &[1],
            &AtomicBool::new(false),
            Duration::from_secs(5),
        )
        .unwrap();
    let accepted = &batch.builds[0].accepted;
    let AcceptedRead::Ready { snapshot } = accepted else {
        panic!("accepted Plan is not ready: {accepted:?}")
    };
    // Every input root is present and every Part's media is fine, so the
    // Review lists choices rather than file problems.
    let observations = ReviewObservations {
        available_input_roots: match &snapshot.provenance {
            Provenance::Tracked { inputs, .. } => inputs.iter().map(|i| i.input_id).collect(),
            _ => Default::default(),
        },
        media_by_part_id: snapshot
            .parts
            .iter()
            .map(|part| {
                let media = MediaObservation {
                    artifact_missing: false,
                    thumb_empty: false,
                };
                (part.projection_part_id, media)
            })
            .collect(),
    };
    let body = views::review_json(accepted, true, &observations, &CatalogOnly)
        .unwrap()
        .into_bytes();
    let review: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(review["kind"], "ready", "{review}");
    review["body"].clone()
}

#[tokio::test]
async fn gre487_held_stale_patch_arriving_last_keeps_every_edit() {
    let server = ReadServer::start(Some("true"), "default").await;
    // The Accepted Review reads the Source revision's snapshot directory.
    let builder = &mut std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(builder, 0o700);
    builder
        .recursive(true)
        .create(server.directory.join("repos/1/revisions/fixture"))
        .unwrap();
    let initial = json_reply(server.request(Method::GET, WORKSPACE), 200).await;
    assert_eq!(initial["draft"], open_draft());
    assert_eq!(workspace_choices(&initial), expected(3, false));
    // Built from the first read, as a slow click's request is, and held.
    let held = click(&initial["draft"]["snapshot_digest"], 3, false);

    let mut current = initial.clone();
    for (quantity, included) in [(2, true), (5, false), (4, true)] {
        let digest = current["draft"]["snapshot_digest"].clone();
        let next = json_reply(
            server
                .request(Method::PATCH, PARTS)
                .json(&click(&digest, quantity, included)),
            200,
        )
        .await;
        assert_eq!(
            workspace_choices(&next),
            expected(quantity, included),
            "fresh edit {quantity}/{included}"
        );
        assert_ne!(next["draft"]["snapshot_digest"], digest, "digest advanced");
        assert_eq!(next["draft"]["lifecycle_version"], 0);
        current = next;
    }

    let database = logical_database(&server.sql());
    let files = managed_files(&server.directory);
    let refused = json_reply(server.request(Method::PATCH, PARTS).json(&held), 409).await;
    assert_eq!(
        refused,
        json!({"code": "draft_changed", "workspace": current}),
        "the held PATCH is refused with the authoritative workspace"
    );
    assert_eq!(
        logical_database(&server.sql()),
        database,
        "stale PATCH wrote rows"
    );
    assert_eq!(
        managed_files(&server.directory),
        files,
        "stale PATCH wrote files"
    );

    let server = server.restart().await;
    assert_eq!(logical_database(&server.sql()), database);
    let reopened = json_reply(server.request(Method::GET, WORKSPACE), 200).await;
    assert_eq!(reopened, current, "reopened draft keeps every edit");

    let mut accept = revision_of(&reopened);
    accept.as_object_mut().unwrap().remove("draft_id");
    let receipt = json_reply(
        keyed(
            &server,
            "/api/v2/plans/1/drafts/2/apply",
            Some("gre487-save"),
        )
        .json(&accept),
        200,
    )
    .await;
    assert_eq!(receipt["revision_id"], 2);
    assert_eq!(receipt["plan_version"], 2);

    let server = server.restart().await;
    let saved = logical_database(&server.sql());
    let review = accepted_review(&server);
    let basis = &review["accepted_basis"];
    assert_eq!(basis["plan_revision_id"], receipt["revision_id"]);
    assert_eq!(basis["plan_version"], receipt["plan_version"]);
    let parts = review["part_groups"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|group| group["parts"].as_array().unwrap());
    assert_eq!(
        choices(parts),
        expected(4, true),
        "accepted Review keeps every edit"
    );
    assert_eq!(
        logical_database(&server.sql()),
        saved,
        "Review read wrote rows"
    );
    server.close().await;
}
