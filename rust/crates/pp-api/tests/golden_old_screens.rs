#![cfg(feature = "desktop_rebuild")]
//! GRE-1116: Rust golden tests for the behaviour worth keeping from the old
//! screens (ADR 0008): the STL pack, managed Plate 3MF, HTML checklist,
//! Checkoff attribution of object names to Required units, and Plate packing.
//! Each test runs the Rust code and compares its output with a file in
//! `tests/golden/` that Rust wrote. A change to any of them fails here.
//! `pp-domain/tests/golden_print_files.rs` holds the 3MF member and
//! EXCLUDE_OBJECT name goldens, since `pp-api` may not depend on `pp-domain`.
//!
//! The checklist route stamps the wall clock into its `Generated` line, so
//! only that value is replaced before comparing. Every other output is
//! compared byte for byte, the STL pack archive included.
//!
//! After a deliberate change, rewrite the files and review the diff:
//! `PP_GOLDEN_UPDATE=1 cargo nextest run --manifest-path rust/Cargo.toml -p pp-api --test golden_old_screens`.
//! An update run writes the files and still fails; rerun without the
//! variable to pass. See `docs/development/golden-tests.md`.
use accepted_plate::{data_dir, initialize_fixture};
use axum::http::{Request, StatusCode, header};
use checkoff_http::{Fixture, loopback};
use pp_api::{accepted::AcceptedHttpConfig, exports::exports_router, plates::plates_router};
use pp_services::exports::{
    self, ChecklistPart, ChecklistPlan, Colour,
    pack::{self, Limits, PackPart, PackRefusal, PackUnit, Scope, ZipStream},
};
use pp_storage::{
    Limits as StorageLimits, WriterOwner,
    auth::{AuthPolicy, FirstUserTenant, RegistrationPolicy, SessionTenantPolicy},
    checkoff_progress::attribution::resolve,
    production::{
        layout::{
            GroupField, GroupKind, GroupRule, GroupUnit, PackResult, PackedUnit, PackingUnit,
            Placement, PlateInput, PrinterGeometry, UnitInput, ValidatedPlate, grouping_buckets,
            layout_digest, pack_units, pack_units_around,
        },
        model::{DimensionUm, PlateId, RequiredUnitToken},
    },
    read_model::{Artifact, Part, Profile, Provenance, Snapshot, Unit},
};
use pp_testkit::golden::Goldens;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::io::{Cursor, Read};

#[path = "support/accepted_plate.rs"]
mod accepted_plate;
#[path = "support/checkoff_http.rs"]
mod checkoff_http;
#[path = "../../pp-storage/tests/common/js_json.rs"]
mod js_json;

/// Every file in `tests/golden/`, each checked by one test below.
const FILES: &[&str] = &[
    "checklist.html",
    "checklist-route.html",
    "checkoff-attribution.json",
    "managed-plate-3mf.json",
    "plate-packing.json",
    "stl-pack.json",
    "stl-pack-route.json",
];

fn goldens() -> Goldens {
    Goldens::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden"), FILES)
}

fn token(n: u32) -> String {
    format!("ppu_{n:032x}")
}

#[test]
fn golden_every_file_is_checked() {
    goldens().assert_all_listed();
}

// --- STL pack -------------------------------------------------------------

fn pack_part(role: &str, relative_path: &str, size: u64, printed: &[bool]) -> PackPart {
    PackPart {
        role: role.into(),
        relative_path: relative_path.into(),
        filename: relative_path.rsplit('/').next().unwrap().into(),
        size,
        units: (1..)
            .zip(printed)
            .map(|(number, &printed)| PackUnit { number, printed })
            .collect(),
    }
}

fn layout_json(parts: &[PackPart], scope: Scope, limits: Limits) -> Value {
    match pack::layout(parts, scope, limits) {
        Ok(entries) => entries
            .iter()
            .map(|entry| json!([entry.name, entry.part]))
            .collect(),
        Err(PackRefusal::NothingToPack) => json!({"refused": "nothing_to_pack"}),
        Err(PackRefusal::TooManyEntries { entries, limit }) => {
            json!({"refused": "too_many_entries", "entries": entries, "limit": limit})
        }
        Err(PackRefusal::TooLarge { bytes, limit }) => {
            json!({"refused": "too_large", "bytes": bytes, "limit": limit})
        }
    }
}

#[test]
fn golden_stl_pack_layout_and_archive() {
    let parts = [
        pack_part(
            "primary",
            "Frame/Corners/frame_corner.stl",
            84,
            &[true, false, false],
        ),
        pack_part("primary", "frame_corner.STL", 84, &[false]),
        pack_part(
            "primary",
            "Frame/Corners/FRAME_CORNER.stl",
            84,
            &[false, false],
        ),
        pack_part("accent", "Frame/Corners/frame_corner.stl", 84, &[false]),
        pack_part("", "../Évil dir/weird name?.stl", 84, &[false]),
        pack_part("a/b\\c", "x/./..//.stl", 84, &[true]),
    ];
    let mut archive = ZipStream::new(Vec::new());
    for (name, data) in [
        (
            "primary/Frame_Corners/frame_corner_01.stl",
            &b"solid a\nendsolid a\n"[..],
        ),
        (
            "accent/_root/clip_02.stl",
            &b"solid clip\nendsolid clip\n"[..],
        ),
    ] {
        archive.entry(name, &mut &data[..]).unwrap();
    }
    let archive = archive.finish().unwrap();
    let tight = Limits {
        entries: 2,
        bytes: u64::MAX,
    };
    let small = Limits {
        entries: 100,
        bytes: 600,
    };
    let actual = json!({
        "all": layout_json(&parts, Scope::All, pack::LIMITS),
        "remaining": layout_json(&parts, Scope::Remaining, pack::LIMITS),
        "nothing_remaining": layout_json(&parts[5..], Scope::Remaining, pack::LIMITS),
        "too_many_entries": layout_json(&parts, Scope::All, tight),
        "too_large": layout_json(&parts[..1], Scope::All, small),
        "filenames": [
            pack::filename("Voron 2.4 — LDO kit!", 7),
            pack::filename("???", 1),
            pack::filename(&"A".repeat(80), 12),
        ],
        "archive_hex": hex::encode(&archive),
    });
    goldens().check_json("stl-pack.json", &actual);
    // The archive opens with a stock reader and holds the bytes it was given.
    let mut zip = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
    let mut data = String::new();
    zip.by_index(1).unwrap().read_to_string(&mut data).unwrap();
    assert_eq!(data, "solid clip\nendsolid clip\n");
}

#[tokio::test]
async fn golden_stl_pack_route_is_byte_identical() {
    let dir = data_dir();
    let root = dir.path();
    initialize_fixture(root);
    let owner = WriterOwner::open(root, StorageLimits::default()).unwrap().0;
    let config = AcceptedHttpConfig::from_startup(loopback::ORIGIN, "default", Some("1")).unwrap();
    let router = exports_router(config, owner.accepted_reads().unwrap());
    let request = Request::get("/api/v2/plans/1/exports/stl-pack")
        .header("cookie", "pp_session=read-fixture-secret");
    let response = loopback::respond(&router, request, Vec::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let disposition = response.headers()[header::CONTENT_DISPOSITION].clone();
    let archive = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(archive.to_vec())).unwrap();
    let entries: Vec<Value> = (0..zip.len())
        .map(|index| {
            let entry = zip.by_index(index).unwrap();
            json!({"name": entry.name().unwrap(), "size": entry.size(), "crc32": entry.crc32()})
        })
        .collect();
    let actual = json!({
        "content_disposition": disposition.to_str().unwrap(),
        "entries": entries,
        "archive_sha256": hex::encode(Sha256::digest(&archive)),
    });
    goldens().check_json("stl-pack-route.json", &actual);
    owner.shutdown().unwrap();
}

#[tokio::test]
async fn golden_managed_plate_3mf_members_and_placement() {
    let dir = data_dir();
    let init = initialize_fixture(dir.path());
    let owner = WriterOwner::open(dir.path(), StorageLimits::default())
        .unwrap()
        .0;
    let config = AcceptedHttpConfig::from_startup(loopback::ORIGIN, "default", Some("1")).unwrap();
    let plates = owner
        .accepted_plates(AuthPolicy {
            registration: RegistrationPolicy::Open,
            session_tenant: SessionTenantPolicy::AccountTenant,
            first_user: FirstUserTenant::NewUser,
        })
        .unwrap();
    let router = exports_router(config.clone(), owner.accepted_reads().unwrap())
        .merge(plates_router(config, plates));
    let cookie = "pp_session=read-fixture-secret";
    let (status, plate) = loopback::send(
        &router,
        "POST",
        "/api/v2/plans/1/plates/initialize",
        Some(&init.to_string()),
        Some(cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plate}");
    let revision = plate["plate_revision_id"].as_u64().unwrap();
    let request = Request::get(format!(
        "/api/v2/plans/1/exports/managed-3mf?plate_revision_id={revision}"
    ))
    .header("cookie", cookie);
    let response = loopback::respond(&router, request, Vec::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let archive = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    owner.shutdown().unwrap();

    let mut zip = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
    let mut package = Vec::new();
    zip.by_name("plates/0001.3mf")
        .unwrap()
        .read_to_end(&mut package)
        .unwrap();
    let mut package = zip::ZipArchive::new(Cursor::new(package)).unwrap();
    let entries: Vec<Value> = (0..package.len())
        .map(|index| {
            let mut entry = package.by_index(index).unwrap();
            let mut xml = String::new();
            entry.read_to_string(&mut xml).unwrap();
            json!({"name": entry.name().unwrap(), "xml": xml})
        })
        .collect();
    goldens().check_json("managed-plate-3mf.json", &json!(entries));
}

// --- HTML checklist -------------------------------------------------------

fn checklist_part(
    source: &str,
    path: &str,
    colour: (&str, Option<&str>),
    counts: (u64, u64),
) -> ChecklistPart {
    ChecklistPart {
        source: source.into(),
        relative_path: path.into(),
        filename: path.rsplit('/').next().unwrap().into(),
        role: if path.contains("accent") {
            "accent"
        } else {
            "primary"
        }
        .into(),
        colour: Colour {
            label: colour.0.into(),
            hex: colour.1.map(String::from),
        },
        quantity: counts.0,
        printed: counts.1,
        notes: String::new(),
        thumbnail_png: None,
    }
}

#[test]
fn golden_checklist_html() {
    let mut notes = checklist_part(
        "base:Voron 2.4",
        "Frame/z_belt_clip.stl",
        ("Black", Some("#111")),
        (4, 1),
    );
    notes.notes = "Print 2 spare.\n<b>not bold</b> & \"quoted\" 'single'".into();
    let plan = ChecklistPlan {
        build_name: "Voron <2.4> & \"Friends\"".into(),
        order_number: Some("LDO-1234".into()),
        plan_version: 3,
        parts: vec![
            notes,
            checklist_part(
                "base:Voron 2.4",
                "Frame/a_frame_corner.stl",
                ("Grey", Some("A0A0A0")),
                (8, 8),
            ),
            checklist_part("base:Voron 2.4", "top_level.stl", ("Natural", None), (1, 0)),
            checklist_part(
                "addon:Stealthburner",
                "accent/sb_front.stl",
                ("Red", Some("#red")),
                (1, 0),
            ),
            checklist_part(
                "addon:Stealthburner",
                "Main/sb_main.stl",
                ("PLA <Blue>", Some("#00F\" onload=x")),
                (2, 2),
            ),
        ],
    };
    let checklist = exports::checklist(&plan, "2026-01-02T03:04:05.000Z");
    assert_eq!(checklist.filename, "voron-2-4-friends-v3-checklist.html");
    goldens().check("checklist.html", &checklist.html);
    let blank = exports::checklist(
        &ChecklistPlan {
            build_name: "  ".into(),
            order_number: Some(" ".into()),
            plan_version: 1,
            parts: Vec::new(),
        },
        "now",
    );
    assert_eq!(blank.filename, "build-v1-checklist.html");
    assert!(!blank.html.contains("Order number"));
}

#[tokio::test]
async fn golden_checklist_route_normalizes_only_the_generated_time() {
    let fixture = Fixture::new();
    let (owner, router) = fixture.open_with("1", |config, owner| {
        exports_router(config, owner.accepted_reads().unwrap())
    });
    let request = Request::get("/api/v2/plans/1/exports/checklist")
        .header("cookie", "pp_session=required-unit-fixture-secret");
    let response = loopback::respond(&router, request, Vec::new()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    let (head, rest) = html.split_once("<dt>Generated</dt><dd>").unwrap();
    let (stamp, tail) = rest.split_once("</dd>").unwrap();
    let generated =
        time::OffsetDateTime::parse(stamp, &time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|error| panic!("{stamp:?} is not RFC 3339: {error}"));
    let age = time::OffsetDateTime::now_utc() - generated;
    assert!(age.abs() < time::Duration::minutes(5), "{stamp} is not now");
    let normalized = format!("{head}<dt>Generated</dt><dd>GENERATED</dd>{tail}");
    goldens().check("checklist-route.html", &normalized);
    drop(router);
    owner.shutdown().unwrap();
}

// --- Checkoff attribution -------------------------------------------------

fn snapshot_part(id: i64, path: &str, completed: &[bool], included: bool) -> Part {
    let filename = path.rsplit('/').next().unwrap();
    let stem = filename.strip_suffix(".stl").unwrap();
    Part {
        revision_part_id: id,
        projection_part_id: id,
        part_key: format!("key-{id}"),
        relative_path: path.into(),
        filename: filename.into(),
        source_layer: "base".into(),
        status: "included".into(),
        role_inferred: "primary".into(),
        role_override: None,
        effective_role: "primary".into(),
        filament_color_id: None,
        filament_custom_hex: None,
        spoolman_spool_id: None,
        quantity_inferred: completed.len() as i64,
        quantity_override: None,
        quantity_effective: completed.len() as i64,
        included,
        notes: String::new(),
        github_blob_url: None,
        geometry_same: None,
        requirement: None,
        option_group_id: None,
        manifest_source: None,
        manifest: Default::default(),
        artifact: Artifact::Unavailable { reason: "fixture" },
        units: (0..)
            .zip(completed)
            .map(|(index, &completed)| {
                let token = token((id * 10 + index) as u32);
                Unit {
                    unit_index: index,
                    required: true,
                    object_name: format!("{stem}__{token}"),
                    token,
                    completed,
                    assembled: completed,
                }
            })
            .collect(),
    }
}

fn checkoff_snapshot() -> Snapshot {
    Snapshot {
        format: "accepted-plan-snapshot-v1",
        profile: Profile {
            id: 1,
            name: "Golden".into(),
            order_number: None,
            special_request: None,
            archived_at: None,
        },
        plan_version: 4,
        revision_id: 9,
        revision_number: 4,
        revision_digest: "a".repeat(64),
        accepted_at: "2026-01-02T03:04:05.000Z".into(),
        provenance: Provenance::Legacy,
        required_unit_mapping_digest: "b".repeat(64),
        parts: vec![
            snapshot_part(
                1,
                "Frame/frame_corner.stl",
                &[true, false, false, false],
                true,
            ),
            snapshot_part(2, "Panels/panel_clip.stl", &[false, false, false], true),
            snapshot_part(3, "Toolhead/fan_duct.stl", &[false], true),
            snapshot_part(4, "Spare/fan_duct.stl", &[false], true),
            snapshot_part(5, "Extras/handle.stl", &[false], false),
        ],
    }
}

#[test]
fn golden_checkoff_attribution() {
    let snapshot = checkoff_snapshot();
    let required = |part: usize, unit: usize| snapshot.parts[part].units[unit].object_name.clone();
    // Object names as EXCLUDE_OBJECT (OrcaSlicer and quoted PrusaSlicer), M486
    // and objects_info report them.
    let names: Vec<String> = vec![
        format!("{}_id_0_copy_0", required(0, 1)),
        format!("'{}'", required(0, 0)),
        format!("{}_id_1_copy_0", required(0, 1)),
        "panel_clip_02.stl_id_2_copy_0".into(),
        "'panel_clip_03.stl'".into(),
        "panel_clip_01.stl (instance 2)".into(),
        "panel_clip_04.stl".into(),
        "fan_duct.stl_id_4_copy_1".into(),
        "Frame/frame_corner.stl".into(),
        "handle.stl".into(),
        "mystery_widget".into(),
        // A misspelling only the fuzzy step matches; positive-only drops it.
        "frame_cornr.stl_id_9_copy_0".into(),
    ];
    let actual = json!({
        "names": names,
        "attribution": resolve(&snapshot, &names, None, false),
        "attribution_positive_only": resolve(&snapshot, &names, None, true),
        "fallback_only": resolve(&snapshot, &[], Some("Panels/panel_clip.stl"), true),
        "fallback_after_unmatched": resolve(&snapshot, &["mystery_widget".into()], Some("Panels/panel_clip.stl"), false),
    });
    goldens().check_json("checkoff-attribution.json", &actual);
}

// --- Plate packing ----------------------------------------------------------

fn packing_unit(n: u32, width_mm: u32, depth_mm: u32, height_mm: u32) -> PackingUnit {
    let um = |mm: u32| DimensionUm::parse(i64::from(mm) * 1000).unwrap();
    PackingUnit {
        token: RequiredUnitToken::parse(token(n)).unwrap(),
        width_um: um(width_mm),
        depth_um: um(depth_mm),
        height_um: um(height_mm),
    }
}

fn packed_json(result: &PackResult) -> Value {
    match result {
        PackResult::Packed(plates) => plates
            .iter()
            .map(|plate| {
                plate
                    .iter()
                    .map(|unit| json!([unit.unit.token.as_str(), unit.x_um.get(), unit.y_um.get()]))
                    .collect::<Vec<_>>()
            })
            .collect(),
        PackResult::UnitTooLarge(token) => json!({"unit_too_large": token.as_str()}),
    }
}

fn validated(
    printer: &PrinterGeometry,
    plates: &[Vec<PackedUnit>],
    pinned: &[u32],
) -> Vec<ValidatedPlate> {
    (1..)
        .zip(plates)
        .map(|(ordinal, units)| ValidatedPlate {
            ordinal,
            input: PlateInput {
                plate_id: PlateId::parse(format!("plate_{ordinal:032x}")).unwrap(),
                printer_id: "printer-1".into(),
                printer_name: "Voron \"2.4\"".into(),
                printer_model: "voron-2.4-350".into(),
                printer: printer.clone(),
                units: units
                    .iter()
                    .map(|unit| UnitInput {
                        token: unit.unit.token.clone(),
                        x_um: unit.x_um,
                        y_um: unit.y_um,
                        width_um: unit.unit.width_um,
                        depth_um: unit.unit.depth_um,
                        height_um: unit.unit.height_um,
                        placement: if pinned.iter().any(|n| unit.unit.token.as_str() == token(*n)) {
                            Placement::Manual
                        } else {
                            Placement::Auto
                        },
                        pinned: pinned.iter().any(|n| unit.unit.token.as_str() == token(*n)),
                    })
                    .collect(),
            },
        })
        .collect()
}

#[test]
fn golden_plate_packing() {
    let printer = PrinterGeometry::from_millimetres(120.0, 100.0, 80.0, 5.0).unwrap();
    // Out of packing order on purpose: the packer sorts by longest side,
    // then footprint, then token.
    let units = [
        packing_unit(1, 30, 20, 10),
        packing_unit(2, 50, 40, 20),
        packing_unit(3, 20, 50, 30),
        packing_unit(4, 30, 20, 10),
        packing_unit(5, 100, 30, 5),
        packing_unit(6, 45, 45, 70),
        packing_unit(7, 10, 10, 10),
    ];
    let PackResult::Packed(plates) = pack_units(&printer, &units) else {
        panic!("the fixture fits");
    };
    let pinned = PackedUnit {
        unit: packing_unit(8, 40, 40, 10),
        x_um: pp_storage::production::model::OffsetUm::parse(40_000).unwrap(),
        y_um: pp_storage::production::model::OffsetUm::parse(30_000).unwrap(),
    };
    let around = pack_units_around(&printer, std::slice::from_ref(&pinned), &units[..4]);
    let PackResult::Packed(around_plates) = &around else {
        panic!("the fixture fits around the pinned unit");
    };
    let group = |n: u32, material: &str, colour: &str, role: &str| GroupUnit {
        value: n,
        token: RequiredUnitToken::parse(token(n)).unwrap(),
        object_name: format!("part_{n}"),
        filename: format!("part_{n}.stl"),
        source_directory: "Frame".into(),
        source_layer: "base".into(),
        role: role.into(),
        filament_color_id: None,
        filament_custom_hex: Some(colour.into()),
        material_type: Some(material.into()).filter(|m: &String| !m.is_empty()),
    };
    let grouped = [
        group(1, "PLA", "#000000", "primary"),
        group(2, "ABS", "#FF0000", "accent"),
        group(3, "PLA", "#ff0000", "accent"),
        group(4, "", "#000000", "primary"),
        group(5, "ABS", "#000000", "primary"),
    ];
    let rule = |id: &str, kind, field, value: Option<&str>, material: Option<&str>| GroupRule {
        id: id.into(),
        enabled: true,
        kind,
        field,
        value: value.map(String::from),
        material_type: material.map(String::from),
    };
    let rules = [
        rule(
            "by-material",
            GroupKind::SeparateBy,
            GroupField::Material,
            None,
            None,
        ),
        rule(
            "red-together",
            GroupKind::KeepTogether,
            GroupField::Color,
            Some("FF0000"),
            None,
        ),
        rule(
            "unset-is-pla",
            GroupKind::SetMaterial,
            GroupField::Material,
            Some("unassigned"),
            Some("PLA"),
        ),
    ];
    let actual = json!({
        "printer_um": [120_000, 100_000, 80_000, 5_000],
        "pack": packed_json(&PackResult::Packed(plates.clone())),
        "too_large": packed_json(&pack_units(&printer, &[packing_unit(9, 111, 10, 10)])),
        "too_tall": packed_json(&pack_units(&printer, &[packing_unit(10, 10, 10, 81)])),
        "around_pinned": packed_json(&around),
        "buckets_without_rules": grouping_buckets(&grouped, &[]),
        "buckets": grouping_buckets(&grouped, &rules),
        "layout_digest_v1": layout_digest(&validated(&printer, &plates, &[]), 1).as_str(),
        "layout_digest_v2": layout_digest(&validated(&printer, around_plates, &[8]), 2).as_str(),
    });
    goldens().check_json("plate-packing.json", &actual);
}
