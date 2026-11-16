use super::*;
use crate::{
    auth::Secret,
    build_graph::{create_with_brief::tests::IDLE, source_roles::tests::Fixture},
    read_model::planning::SourceRole,
    write::test_support::WAIT,
};
use serde_json::{Value, json};

/// Node's id for `printpartner:source:3`:
/// `createHash("sha256").update(url).digest("hex").slice(0, 20)`.
const S3_EVIDENCE: &str = "0c6feba9972dd1e4accf";

/// The Source-roles brief (evidence `e1` and `e2`, draft 5) plus Library
/// Source 3, which the brief does not name yet.
fn fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture
        .database()
        .execute(
            "INSERT INTO projects(id,tenant_id,name,url,role) \
             VALUES(3,'default','S3','https://github.com/example/s3','mod')",
            [],
        )
        .unwrap();
    fixture
}

fn source(id: u64) -> ImportInput {
    ImportInput {
        target: Target::Source {
            id: PositiveId::new(id).unwrap(),
            filenames: Vec::new(),
        },
        kind: None,
        title: None,
        extract: None,
        derived_from_evidence_id: None,
    }
}

/// A URL input; `normalized` stands in for what the caller's rules give.
fn page(url: &str, normalized: &str, page: Page) -> ImportInput {
    ImportInput {
        target: Target::Url {
            url: url.into(),
            normalized: normalized.into(),
            page,
        },
        ..source(1)
    }
}

fn import(fixture: &Fixture, inputs: Vec<ImportInput>) -> Result<Vec<Imported>, ImportInputsError> {
    fixture
        .client
        .import_inputs(fixture.session(), fixture.build, inputs, &IDLE, WAIT)
}

fn check(fixture: &Fixture, inputs: Vec<ImportInput>) -> Result<(), ImportInputsError> {
    fixture
        .client
        .check_import_inputs(fixture.session(), fixture.build, inputs, WAIT)
}

/// Every Library Source row, as JSON text.
fn library(fixture: &Fixture) -> String {
    fixture
        .database()
        .query_row(
            "SELECT json_group_array(json_array(id,name,url,role,branch,local_path,last_synced_at)) \
             FROM (SELECT * FROM projects ORDER BY id)",
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn import_inputs_attaches_a_source_and_clears_the_selected_draft() {
    let fixture = fixture();
    let (before, sources) = (fixture.brief(), library(&fixture));
    let input = ImportInput {
        target: Target::Source {
            id: PositiveId::new(3).unwrap(),
            filenames: vec![" a.stl ".into(), "a.stl".into(), " ".into(), "b.stl".into()],
        },
        kind: Some(EvidenceKind::Mod),
        derived_from_evidence_id: Some(" e1 ".into()),
        ..source(3)
    };
    check(&fixture, vec![input.clone()]).unwrap();
    assert_eq!(fixture.brief(), before, "the check writes nothing");

    let imported = import(&fixture, vec![input]).unwrap();
    assert_eq!(
        imported,
        [Imported {
            evidence_id: S3_EVIDENCE.into(),
            kind: EvidenceKind::Mod,
            source_id: Some(3),
            url: "printpartner:source:3".into(),
            upload_required: false,
        }]
    );
    let after = fixture.brief();
    assert_eq!(
        after["evidence"][2],
        json!({
            "id": S3_EVIDENCE,
            "url": "printpartner:source:3",
            "normalized_url": "printpartner:source:3",
            "kind": "mod",
            "input_kind": "upload",
            "filenames": ["a.stl", "b.stl"],
            "derived_from_evidence_id": "e1",
            "source_id": 3,
            "title": "S3",
            "sync_status": "pending",
        })
    );
    assert!(after.get("draft_id").is_none(), "{after}");
    assert_ne!(after["updated_at"], before["updated_at"]);
    // Only the new item, the draft and the stamp change.
    let mut unchanged = after.clone();
    unchanged["evidence"].as_array_mut().unwrap().pop();
    unchanged["updated_at"] = before["updated_at"].clone();
    unchanged["draft_id"] = json!(5);
    assert_eq!(unchanged, before);
    assert_eq!(library(&fixture), sources, "no Source changes");
}

#[test]
fn import_inputs_again_updates_the_one_item_and_keeps_its_role() {
    let fixture = fixture();
    import(&fixture, vec![source(3)]).unwrap();
    assert_eq!(fixture.brief()["evidence"][2]["kind"], "model_source");
    fixture.set(&[(S3_EVIDENCE, SourceRole::Overlay)]).unwrap();

    let again = ImportInput {
        kind: Some(EvidenceKind::Component),
        title: Some("Kit".into()),
        ..source(3)
    };
    import(&fixture, vec![again.clone(), again]).unwrap();
    let evidence = fixture.brief()["evidence"].clone();
    let ids: Vec<&Value> = evidence
        .as_array()
        .unwrap()
        .iter()
        .map(|item| &item["id"])
        .collect();
    assert_eq!(ids, [&json!("e1"), &json!("e2"), &json!(S3_EVIDENCE)]);
    assert_eq!(evidence[2]["kind"], "component");
    assert_eq!(evidence[2]["title"], "Kit");
    assert_eq!(evidence[2]["source_role"], "overlay", "{evidence}");
}

#[test]
fn import_inputs_refuses_and_stores_nothing() {
    let fixture = fixture();
    let before = fixture.brief();
    let unknown_derived = ImportInput {
        derived_from_evidence_id: Some("e9".into()),
        ..source(3)
    };
    for (inputs, expected) in [
        (vec![source(3), source(99)], "source 99"),
        (vec![source(3), unknown_derived], "derived e9"),
    ] {
        let found = |error: &ImportInputsError| match error {
            ImportInputsError::SourceNotFound(99) => "source 99",
            ImportInputsError::DerivedEvidenceNotFound(id) if id == "e9" => "derived e9",
            _ => "other",
        };
        let checked = check(&fixture, inputs.clone()).unwrap_err();
        assert_eq!(found(&checked), expected, "{checked:?}");
        let imported = import(&fixture, inputs).unwrap_err();
        assert_eq!(found(&imported), expected, "{imported:?}");
        assert_eq!(fixture.brief(), before);
    }

    let other = PositiveId::new(fixture.build.get() + 1).unwrap();
    let missing =
        fixture
            .client
            .import_inputs(fixture.session(), other, vec![source(3)], &IDLE, WAIT);
    assert!(matches!(missing, Err(ImportInputsError::NoBrief)));
    let stranger = Credential::Session(Secret::new("not-a-session".into()));
    let refused =
        fixture
            .client
            .check_import_inputs(stranger, fixture.build, vec![source(3)], WAIT);
    assert!(matches!(
        refused,
        Err(ImportInputsError::Writer(WriterFailure::Auth(_)))
    ));
    assert_eq!(fixture.brief(), before);
}

#[test]
fn import_inputs_attaches_model_pages_and_links_by_url() {
    let fixture = fixture();
    let sources = library(&fixture);
    let printables = "https://www.printables.com/model/12345-kit/files";
    let extract = ImportInput {
        title: Some("Guide".into()),
        extract: Some("  Print the frame first.  ".into()),
        derived_from_evidence_id: Some("e1".into()),
        ..page(
            "https://Example.COM/Docs/Guide///?q=Ab#frag",
            "https://example.com/Docs/Guide?q=Ab",
            Page::Link,
        )
    };
    let imported = import(
        &fixture,
        vec![
            page(&format!("{printables}#comments"), printables, Page::Model),
            extract,
        ],
    )
    .unwrap();
    // Ids from Node's analyzeBuildRequest for the same URLs (pp-services
    // build_url pins the normalized strings).
    let ids: Vec<_> = imported
        .iter()
        .map(|item| item.evidence_id.as_str())
        .collect();
    assert_eq!(ids, ["e23eb2aaca54431c33ba", "73c6eab824d58a05cc41"]);
    assert!(imported[0].upload_required && !imported[1].upload_required);
    assert_eq!(imported[0].source_id, None);

    let evidence = fixture.brief()["evidence"].clone();
    assert_eq!(
        evidence[2],
        json!({
            "id": "e23eb2aaca54431c33ba",
            "url": format!("{printables}#comments"),
            "normalized_url": printables,
            "kind": "model_source",
            "input_kind": "model_page",
            "upload_required": true,
        })
    );
    let link = evidence[3].as_object().unwrap();
    assert_eq!(link["kind"], "informational_evidence");
    assert_eq!(link["input_kind"], "url");
    assert_eq!(link["url"], "https://Example.COM/Docs/Guide///?q=Ab#frag");
    assert_eq!(link["title"], "Guide");
    assert_eq!(link["extract"], "Print the frame first.");
    // sha256("Print the frame first.")
    assert_eq!(
        link["content_hash"],
        "8a59b5bfc58ba8baf8e26fece6b3fd9bbe768c98773b1cdf772da15e06bdb1a4"
    );
    assert!(
        link["retrieved_at"].as_str().unwrap().ends_with('Z'),
        "{link:?}"
    );
    assert!(!link.contains_key("derived_from_evidence_id"), "{link:?}");
    assert_eq!(library(&fixture), sources, "a page creates no Source");
}
