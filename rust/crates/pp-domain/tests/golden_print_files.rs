#![cfg(feature = "desktop_rebuild")]
//! GRE-1116: Rust golden tests for the print-file behaviour the old screens
//! relied on: which 3MF members make a package print-ready, and how object
//! names from EXCLUDE_OBJECT, M486 and objects_info group per Part and match
//! library STLs, `stem_NN` unit names included. Outputs are compared byte for
//! byte with `tests/golden/`; `pp-api/tests/golden_old_screens.rs` holds the
//! export, Checkoff attribution and Plate packing goldens. Rewrite after a
//! deliberate change with `PP_GOLDEN_UPDATE=1` and review the diff; see
//! `docs/development/golden-tests.md`.
use pp_domain::{
    gcode_objects::{group_objects_by_part, match_objects_to_filenames},
    print_file::classify,
};
use serde_json::{Value, json};
use std::io::{Cursor, Write};

#[path = "../../pp-testkit/src/golden.rs"]
mod golden;

const FILES: &[&str] = &["sliced-object-names.json", "three-mf-members.json"];

fn goldens() -> golden::Goldens {
    golden::Goldens::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden"), FILES)
}

#[test]
fn golden_every_file_is_checked() {
    goldens().assert_all_listed();
}

/// A stored ZIP holding a one-byte entry per name, or a directory for a name
/// ending in `/`.
fn three_mf(members: &[&str]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for member in members {
        if let Some(dir) = member.strip_suffix('/') {
            zip.add_directory(dir, options).unwrap();
        } else {
            zip.start_file(*member, options).unwrap();
            zip.write_all(b"x").unwrap();
        }
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn golden_three_mf_members() {
    let package = ["[Content_Types].xml", "_rels/.rels"];
    let cases: [(&str, &[&str]); 8] = [
        ("model only", &["3D/3dmodel.model"]),
        (
            "model and slicer state",
            &["3D/3dmodel.model", "Metadata/model_settings.config"],
        ),
        (
            "PrusaSlicer project",
            &["3D/3dmodel.model", "Metadata/Slic3r_PE.config"],
        ),
        (
            "toolpath plate",
            &[
                "3D/3dmodel.model",
                "Metadata/plate_1.gcode",
                "Metadata/plate_1.json",
            ],
        ),
        (
            "upper-case members",
            &["3D/Object.MODEL", "METADATA/Project_Settings.CONFIG"],
        ),
        ("state without a model", &["Metadata/model_settings.config"]),
        ("model as a directory", &["3D/3dmodel.model/"]),
        (
            "model outside 3D/",
            &["3dmodel.model", "Metadata/notes.ini"],
        ),
    ];
    let mut actual: Vec<Value> = cases
        .iter()
        .map(|(label, members)| {
            let all: Vec<&str> = package.iter().chain(members.iter()).copied().collect();
            let classified = classify(&three_mf(&all)).unwrap();
            json!({
                "case": label,
                "members": members,
                "classification": classified.classification,
                "print_ready": classified.classification.is_print_ready(),
                "next_action": classified.classification.next_action(),
            })
        })
        .collect();
    let refused = classify(&three_mf(&["3D/3dmodel.model", "../evil.gcode"])).unwrap_err();
    actual.push(json!({
        "case": "member outside the package",
        "refused": refused,
        "message": refused.message(),
    }));
    goldens().check_json("three-mf-members.json", &Value::Array(actual));
}

#[test]
fn golden_sliced_object_names() {
    // As printers report them: OrcaSlicer and BambuStudio EXCLUDE_OBJECT,
    // quoted PrusaSlicer EXCLUDE_OBJECT, PrusaSlicer instances, M486 and
    // objects_info. The STL pack names units `<stem>_<NN>.stl`.
    let names = [
        "frame_corner_01.stl_id_0_copy_0",
        "frame_corner_02.stl_id_1_copy_0",
        "Frame_Corner_03.STL_id_2_copy_1",
        "'panel_clip_01.stl'",
        "panel_clip_01.stl_instance_2",
        "panel_clip.stl (instance 3)",
        "fan_duct.stl",
        "fan_duct_stl",
        "Toolhead%2Fsb_cowl_01.stl",
        "z_belt_clip_x2.stl_id_7_copy_0",
        "mystery_widget",
        "",
    ];
    let library = [
        "Frame/frame_corner.stl",
        "Panels/panel_clip.stl",
        "Toolhead/fan_duct.stl",
        "Spare/fan_duct.stl",
        "Toolhead/sb_cowl.stl",
        "Gantry/z_belt_clip_x2.stl",
    ];
    let groups = group_objects_by_part(&names);
    let actual = json!({
        "names": names,
        "library": library,
        "groups": groups
            .iter()
            .map(|group| json!({
                "stl_basename": group.stl_basename,
                "count": group.count,
                "objects": group.objects.iter().map(|object| json!({
                    "name": object.name,
                    "stl_basename": object.stl_basename,
                    "copy_index": object.copy_index,
                    "format": object.format.as_str(),
                })).collect::<Vec<_>>(),
            }))
            .collect::<Vec<_>>(),
        "matches": match_objects_to_filenames(&groups, &library),
    });
    goldens().check_json("sliced-object-names.json", &actual);
}
