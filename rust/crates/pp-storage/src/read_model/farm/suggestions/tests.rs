use super::super::Farm;
use super::*;
use serde_json::{Value, json};

const T1: &str = "ppu_11111111111111111111111111111111";
const T2: &str = "ppu_22222222222222222222222222222222";
const T3: &str = "ppu_33333333333333333333333333333333";
const T9: &str = "ppu_99999999999999999999999999999999";

fn plate(id: &str, ordinal: i64, units: &[(&str, &str)]) -> Plate {
    Plate {
        plate_id: id.into(),
        ordinal,
        tokens: units.iter().map(|(token, _)| (*token).to_owned()).collect(),
        stems: units.iter().map(|(_, stem)| object_key(stem)).collect(),
    }
}

/// Builds 1 and 2 share "bracket.stl"; only Build 1 has "arm.stl". Build 1's
/// plate A holds T1 and T2, plate B holds T3. T9 belongs to Build 2.
fn library() -> MatchLibrary {
    MatchLibrary {
        builds: [(1, "Voron 2.4".into()), (2, "Trident".into())].into(),
        part_files: [
            (1, vec!["parts/bracket.stl".into(), "arm.stl".into()]),
            (2, vec!["bracket.stl".into(), "panel.stl".into()]),
        ]
        .into(),
        units: [
            (T1.into(), 1),
            (T2.into(), 1),
            (T3.into(), 1),
            (T9.into(), 2),
        ]
        .into(),
        plates: [(
            1,
            vec![
                plate("plate_a", 1, &[(T1, "bracket"), (T2, "arm")]),
                plate("plate_b", 2, &[(T3, "arm")]),
            ],
        )]
        .into(),
        job_files: [("old.gcode".into(), [2].into())].into(),
    }
}

fn print(id: &str, filename: &str, objects: &[&str]) -> Value {
    json!({"id": id, "integration_id": "moon", "printer_id": "p-moon", "host_name": "Micron",
        "filename": filename, "completed_at": "2026-11-05T12:00:00.000Z",
        "gcode_objects": objects, "candidates": []})
}

fn suggest(prints: Value, links: Value, queue: Value) -> Vec<MatchSuggestion> {
    let list = |value: Value| value.as_array().cloned().unwrap();
    Farm::parse(vec![], vec![], list(links), list(prints), list(queue))
        .suggest(&library())
        .suggestions
}

fn one(objects: &[&str]) -> MatchSuggestion {
    suggest(
        json!([print("x", "x.gcode", objects)]),
        json!([]),
        json!([]),
    )
    .remove(0)
}

fn sent(id: &str, profile: i64, filename: &str, at: &str) -> Value {
    json!({"id": id, "filename": filename, "artifact_path": "/disposable/a.gcode",
        "printer_id": "p-moon", "profile_id": profile, "state": "done", "created_at": at})
}

#[test]
fn match_suggestions_exact_needs_every_object_tokened_and_one_whole_plate() {
    let exact = one(&[
        &format!("bracket__{T1}.stl_id_0_copy_0"),
        &format!("'arm__{T2}_stl'"),
    ]);
    assert_eq!(exact.confidence, MatchConfidence::Exact);
    assert!(exact.starts_checked);
    assert_eq!(
        serde_json::to_value(&exact).unwrap(),
        json!({"print_id": "x", "printer_id": "p-moon", "filename": "x.gcode",
            "completed_at": "2026-11-05T12:00:00.000Z", "confidence": "exact",
            "starts_checked": true, "build": {"id": 1, "name": "Voron 2.4"},
            "plate": {"plate_id": "plate_a", "ordinal": 1},
            "evidence": {"basis": ["unit_tokens"], "file_name": {"build_ids": []},
                "object_list": {"object_count": 2, "unit_token_count": 2,
                    "unit_build_ids": [1], "part_build_ids": []},
                "time_window": {"start": "2026-11-02T12:00:00.000Z",
                    "end": "2026-11-05T12:00:00.000Z", "build_ids": []}}})
    );

    let part_of_plate = one(&[&format!("bracket__{T1}.stl_id_0_copy_0")]);
    assert_eq!(part_of_plate.confidence, MatchConfidence::Likely);
    assert!(!part_of_plate.starts_checked);
    assert_eq!(part_of_plate.plate.unwrap().plate_id, "plate_a");

    let untokened_extra = one(&[&format!("bracket__{T1}"), &format!("arm__{T2}"), "arm.stl"]);
    assert_eq!(untokened_extra.confidence, MatchConfidence::Likely);

    let unknown_token = one(&[
        &format!("bracket__{T1}"),
        &format!("arm__{T2}"),
        &format!("x__{}", "ppu_".to_owned() + &"a".repeat(32)),
    ]);
    assert_eq!(unknown_token.confidence, MatchConfidence::Likely);
    assert_eq!(unknown_token.evidence.object_list.unit_token_count, 3);

    let across_plates = one(&[&format!("bracket__{T1}"), &format!("arm__{T3}")]);
    assert_eq!(across_plates.confidence, MatchConfidence::Likely);
    assert_eq!(across_plates.plate, None);
}

#[test]
fn match_suggestions_tokens_from_two_builds_or_none_known_suggest_nothing() {
    let split = one(&[&format!("bracket__{T1}"), &format!("bracket__{T9}")]);
    assert_eq!(split.confidence, MatchConfidence::None);
    assert_eq!(split.build, None);
    assert!(split.evidence.basis.is_empty());
    assert_eq!(split.evidence.object_list.unit_build_ids, vec![1, 2]);

    let unknown = one(&[&format!("bracket__ppu_{}", "b".repeat(32))]);
    assert_eq!(unknown.confidence, MatchConfidence::None);
    assert!(unknown.evidence.object_list.part_build_ids.is_empty());
}

#[test]
fn match_suggestions_object_list_names_one_build_and_its_plate() {
    let likely = one(&["bracket.stl_id_0_copy_0", "arm.stl_id_1_copy_0"]);
    assert_eq!(likely.confidence, MatchConfidence::Likely);
    assert!(!likely.starts_checked);
    assert_eq!(likely.build.unwrap().id, 1);
    assert_eq!(likely.evidence.basis, vec![MatchBasis::ObjectList]);
    assert_eq!(likely.evidence.object_list.part_build_ids, vec![1]);
    assert_eq!(
        likely.plate.unwrap().plate_id,
        "plate_a",
        "same objects as plate A"
    );

    let arm_only = one(&["arm.stl_id_0_copy_0"]);
    assert_eq!(arm_only.plate.unwrap().plate_id, "plate_b");

    let two_arms = one(&["arm.stl_id_0_copy_0", "arm.stl_id_0_copy_1"]);
    assert_eq!(two_arms.confidence, MatchConfidence::Likely);
    assert_eq!(two_arms.plate, None, "no plate holds two arms");

    let unknown_part = one(&["arm.stl", "lid.stl"]);
    assert_eq!(unknown_part.confidence, MatchConfidence::None);
}

#[test]
fn match_suggestions_file_name_then_time_window_break_an_object_tie() {
    let tie = one(&["bracket.stl"]);
    assert_eq!(tie.confidence, MatchConfidence::None);
    assert_eq!(tie.evidence.object_list.part_build_ids, vec![1, 2]);

    let by_file = suggest(
        json!([print("x", "dir/OLD.gcode", &["bracket.stl"])]),
        json!([]),
        json!([sent("s1", 1, "fresh.gcode", "2026-11-05T11:00:00.000Z")]),
    )
    .remove(0);
    assert_eq!(
        by_file.build.as_ref().unwrap().id,
        2,
        "Build 2 printed old.gcode before"
    );
    assert_eq!(
        by_file.evidence.basis,
        vec![MatchBasis::ObjectList, MatchBasis::FileName]
    );
    assert_eq!(by_file.evidence.file_name.build_ids, vec![2]);
    assert_eq!(by_file.evidence.time_window.unwrap().build_ids, vec![1]);

    let by_window = suggest(
        json!([print("x", "x.gcode", &["bracket.stl"])]),
        json!([{"id": "l1", "profile_id": 2, "printer_id": "p-moon", "integration_id": "moon",
            "filename": "y.gcode", "state": "dismissed", "units": [],
            "created_at": "2026-11-04T12:00:00.000Z"}]),
        json!([
            sent("early", 1, "z.gcode", "2026-11-01T11:59:59.000Z"),
            sent("late", 1, "z.gcode", "2026-11-05T12:00:01.000Z"),
        ]),
    )
    .remove(0);
    assert_eq!(by_window.build.unwrap().id, 2);
    assert_eq!(
        by_window.evidence.basis,
        vec![MatchBasis::ObjectList, MatchBasis::TimeWindow]
    );
    assert_eq!(by_window.confidence, MatchConfidence::Likely);
}

#[test]
fn match_suggestions_without_objects_use_the_file_name_only() {
    let rows = suggest(
        json!([print("a", "old.gcode", &[]), print("b", "new.gcode", &[])]),
        json!([]),
        json!([
            sent("s", 1, "new.gcode", "2026-11-05T11:00:00.000Z"),
            sent("other", 2, "elsewhere.gcode", "2026-11-01T11:00:00.000Z"),
            sent("closed", 7, "old.gcode", "2026-11-05T11:00:00.000Z"),
        ]),
    );
    assert_eq!(
        rows[0].build.as_ref().unwrap().id,
        2,
        "unknown Build 7 is ignored"
    );
    assert_eq!(rows[0].evidence.basis, vec![MatchBasis::FileName]);
    assert_eq!(rows[1].build.as_ref().unwrap().id, 1);
    assert_eq!(
        rows[1].evidence.basis,
        vec![MatchBasis::FileName, MatchBasis::TimeWindow]
    );
    assert!(
        rows.iter()
            .all(|row| row.confidence == MatchConfidence::Likely && !row.starts_checked)
    );

    let file_tie =
        |sends: Value| suggest(json!([print("t", "old.gcode", &[])]), json!([]), sends).remove(0);
    let broken = file_tie(json!([sent(
        "s",
        1,
        "old.gcode",
        "2026-11-05T11:00:00.000Z"
    )]));
    assert_eq!(
        broken.build.unwrap().id,
        1,
        "the window leaves Build 1 of the file-name tie"
    );
    assert_eq!(broken.confidence, MatchConfidence::Likely);
    assert_eq!(
        broken.evidence.basis,
        vec![MatchBasis::FileName, MatchBasis::TimeWindow]
    );
    let tied = file_tie(json!([sent(
        "s",
        1,
        "old.gcode",
        "2026-10-01T11:00:00.000Z"
    )]));
    assert_eq!(tied.evidence.file_name.build_ids, vec![1, 2]);
    assert_eq!(
        tied.confidence,
        MatchConfidence::None,
        "no window: the tie stays"
    );

    let window_alone = suggest(
        json!([print("c", "c.gcode", &[])]),
        json!([]),
        json!([sent("s", 1, "other.gcode", "2026-11-05T11:00:00.000Z")]),
    )
    .remove(0);
    assert_eq!(window_alone.confidence, MatchConfidence::None);
    assert_eq!(
        window_alone.evidence.time_window.unwrap().build_ids,
        vec![1]
    );
}

#[test]
fn match_suggestions_follow_the_unmatched_list_and_tolerate_bad_times() {
    let mut bad = print("bad", "x.gcode", &["arm.stl"]);
    bad["completed_at"] = json!("yesterday");
    let rows = suggest(
        json!([
            bad,
            print("second", "x.gcode", &[]),
            print("ok", "x.gcode", &[])
        ]),
        json!([]),
        json!([]),
    );
    let ids: Vec<_> = rows.iter().map(|row| row.print_id.as_str()).collect();
    assert_eq!(ids, ["bad", "second", "ok"]);
    assert_eq!(rows[0].evidence.time_window, None);
    assert_eq!(rows[0].confidence, MatchConfidence::Likely);
}

#[test]
fn match_suggestions_token_scan_needs_exactly_32_lowercase_hex_digits() {
    assert_eq!(unit_token(&format!("a__{T1}.stl_id_0_copy_0")), Some(T1));
    assert_eq!(unit_token(&format!("'{T1}'")), Some(T1));
    assert_eq!(unit_token(&format!("{T1}0")), None);
    assert_eq!(unit_token(&format!("ppu_{}", "A".repeat(32))), None);
    assert_eq!(unit_token(&format!("ppu_{}", "a".repeat(31))), None);
    assert_eq!(
        unit_token(&format!("ppu_ppu_{}", "c".repeat(32))),
        Some(&*format!("ppu_{}", "c".repeat(32)))
    );
    assert_eq!(unit_token("é ppu_"), None);
}
