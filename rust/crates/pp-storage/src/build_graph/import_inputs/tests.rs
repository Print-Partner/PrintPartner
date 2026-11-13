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

fn source(id: u64) -> SourceInput {
    SourceInput {
        source_id: PositiveId::new(id).unwrap(),
        kind: None,
        title: None,
        filenames: Vec::new(),
        derived_from_evidence_id: None,
    }
}

fn import(fixture: &Fixture, inputs: Vec<SourceInput>) -> Result<Vec<Imported>, ImportInputsError> {
    fixture
        .client
        .import_inputs(fixture.session(), fixture.build, inputs, &IDLE, WAIT)
}

fn check(fixture: &Fixture, inputs: Vec<SourceInput>) -> Result<(), ImportInputsError> {
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
    let input = SourceInput {
        kind: Some(EvidenceKind::Mod),
        filenames: vec![" a.stl ".into(), "a.stl".into(), " ".into(), "b.stl".into()],
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
            source_id: 3,
            url: "printpartner:source:3".into(),
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

    let again = SourceInput {
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
    let unknown_derived = SourceInput {
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
