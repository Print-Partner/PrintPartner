use super::*;
use crate::{
    SettingSnapshot,
    build_graph::{create_with_brief::tests::IDLE, source_roles::tests::Fixture},
    write::test_support::{WAIT, seed_setting},
};
use serde_json::{Value, json};

/// A Build whose brief has an unverified `color_accent` requirement and
/// selects draft 5.
fn fixture() -> Fixture {
    let fixture = Fixture::new();
    let mut brief = fixture.brief();
    brief["requirements"] = json!([
        { "key": "color_accent", "value": "Accent colour", "status": "unverified" },
        { "key": "color_base", "value": "Base colour", "status": "unverified" },
    ]);
    seed_setting(&fixture.owner, &brief_key(&fixture), &brief.to_string());
    fixture
}

fn brief_key(fixture: &Fixture) -> String {
    format!("build_planning.v1.{}", fixture.build.get())
}

fn defaults_key(fixture: &Fixture) -> String {
    format!("role_filaments_{}", fixture.build.get())
}

fn setting(fixture: &Fixture, key: &str) -> Option<Value> {
    let reader = fixture.owner.client().reader(WAIT).unwrap();
    match reader.snapshot("default", key).unwrap() {
        SettingSnapshot::Stored { value } => Some(serde_json::from_str(&value).unwrap()),
        SettingSnapshot::Missing => None,
    }
}

fn assignment(role: &str, kind: FilamentInventoryKind, id: Option<&str>) -> RoleFilamentAssignment {
    RoleFilamentAssignment {
        role: role.into(),
        requested_brand: None,
        requested_name: None,
        inventory_kind: kind,
        inventory_id: id.map(str::to_owned),
        color_hex: "#1a2b3c".into(),
        substitution_confirmed: false,
    }
}

fn assign(
    fixture: &Fixture,
    build: PositiveId,
    assignment: RoleFilamentAssignment,
) -> Result<RoleFilamentAssignment, RoleFilamentError> {
    fixture
        .client
        .assign_role_filament(fixture.session(), build, assignment, &IDLE, WAIT)
}

#[test]
fn assigning_saves_the_default_and_records_it_in_the_brief() {
    let fixture = fixture();
    let catalog = assignment(" accent ", FilamentInventoryKind::Catalog, Some("c1"));
    let saved = assign(&fixture, fixture.build, catalog).unwrap();
    assert_eq!(saved.role, "accent");
    assert_eq!(
        setting(&fixture, &defaults_key(&fixture)).unwrap(),
        json!({ "accent": { "filament_color_id": "c1", "filament_custom_hex": null, "spoolman_spool_id": null } })
    );
    let brief = fixture.brief();
    assert_eq!(brief["role_filaments"].as_array().unwrap().len(), 1);
    assert_eq!(brief["role_filaments"][0]["inventory_id"], "c1");
    assert_eq!(brief["requirements"][0]["status"], "satisfied");
    assert_eq!(
        brief["requirements"][0]["detail"],
        "Assigned catalog filament c1"
    );
    assert_eq!(brief["requirements"][1]["status"], "unverified");
    assert!(
        brief.get("draft_id").is_none(),
        "the selected draft is cleared"
    );
    assert_ne!(brief["updated_at"], "2020-01-01T00:00:00.000Z");
}

#[test]
fn a_later_assignment_replaces_the_role_and_keeps_other_roles() {
    let fixture = fixture();
    // Node's own document: another role stays, a non-object entry is dropped.
    seed_setting(
        &fixture.owner,
        &defaults_key(&fixture),
        &json!({ "base": { "filament_color_id": "b1", "filament_custom_hex": null, "spoolman_spool_id": "7" }, "junk": 3 })
            .to_string(),
    );
    assign(
        &fixture,
        fixture.build,
        assignment("accent", FilamentInventoryKind::Catalog, Some("c1")),
    )
    .unwrap();
    let mut custom = assignment("Accent", FilamentInventoryKind::Custom, None);
    custom.substitution_confirmed = true;
    assign(&fixture, fixture.build, custom).unwrap();
    assert_eq!(
        setting(&fixture, &defaults_key(&fixture)).unwrap(),
        json!({
            "accent": { "filament_color_id": null, "filament_custom_hex": "#1a2b3c", "spoolman_spool_id": null },
            "base": { "filament_color_id": "b1", "filament_custom_hex": null, "spoolman_spool_id": "7" },
        })
    );
    let brief = fixture.brief();
    let filaments = brief["role_filaments"].as_array().unwrap();
    assert_eq!(filaments.len(), 1, "{filaments:?}");
    assert_eq!(filaments[0]["role"], "Accent");
    assert_eq!(filaments[0]["inventory_kind"], "custom");
    assert_eq!(
        brief["requirements"][0]["detail"],
        "Assigned custom filament #1a2b3c"
    );
}

#[test]
fn refusals_store_nothing() {
    let fixture = fixture();
    let before = (fixture.brief(), setting(&fixture, &defaults_key(&fixture)));
    let missing = PositiveId::new(9_999).unwrap();
    let catalog = || assignment("accent", FilamentInventoryKind::Catalog, Some("c1"));
    assert!(matches!(
        assign(&fixture, missing, catalog()),
        Err(RoleFilamentError::NoBrief)
    ));
    let mut bad_hex = catalog();
    bad_hex.color_hex = "#12345".into();
    for invalid in [
        assignment("  ", FilamentInventoryKind::Catalog, Some("c1")),
        bad_hex,
    ] {
        assert!(matches!(
            assign(&fixture, fixture.build, invalid),
            Err(RoleFilamentError::InvalidAssignment)
        ));
    }
    assert_eq!(
        (fixture.brief(), setting(&fixture, &defaults_key(&fixture))),
        before
    );
    assert!(
        fixture
            .client
            .check_role_filament(fixture.session(), fixture.build, WAIT)
            .is_ok()
    );
    assert!(matches!(
        fixture
            .client
            .check_role_filament(fixture.session(), missing, WAIT),
        Err(RoleFilamentError::NoBrief)
    ));
}

#[test]
fn role_filaments_reads_node_documents_leniently() {
    assert_eq!(RoleFilaments::parse("not json"), RoleFilaments::default());
    assert_eq!(RoleFilaments::parse("[1]"), RoleFilaments::default());
    let parsed = RoleFilaments::parse(
        r##"{"Accent":{"filament_color_id":5,"filament_custom_hex":"#000000"}}"##,
    );
    assert_eq!(
        parsed.get("accent"),
        Some(&RoleFilamentDefault {
            filament_color_id: None,
            filament_custom_hex: Some("#000000".into()),
            spoolman_spool_id: None,
        })
    );
    assert_eq!(requirement_key("Top Panel-2"), "color_top_panel_2");
    assert_eq!(requirement_key("a  b"), "color_a_b");
}
