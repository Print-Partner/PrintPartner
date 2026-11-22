use super::*;
use std::io::Write;
use std::time::{Duration, Instant};

fn names(text: &str) -> Vec<String> {
    match object_names(text.as_bytes()) {
        FileObjects::Read(names) => names,
        other => panic!("text G-code read as {other:?}"),
    }
}

#[test]
fn gre_1896_text_gcode_labels_come_in_node_order_each_once() {
    let text = [
        "EXCLUDE_OBJECT_DEFINE NAME=bracket.stl_id_0_copy_0 CENTER=10,10",
        "EXCLUDE_OBJECT_DEFINE NAME='lid part.stl' CENTER=20,20",
        "EXCLUDE_OBJECT_DEFINE NAME=\"Bracket.stl_id_0_copy_0\"",
        "EXCLUDE_OBJECT_DEFINE {\"name\": \"json.stl\"}",
        "M486 S0 A\"knob.stl\"",
        "M486 A1",
        "M486 Aspacer.stl",
        "; printing object clip.stl id:3 copy 0",
        "; printing object not this one",
        ";MESH:foot.stl\r",
        ";MESH:NONMESH",
    ]
    .join("\n");
    assert_eq!(
        names(&text),
        [
            "json.stl",
            "bracket.stl_id_0_copy_0",
            "lid part.stl",
            "knob.stl",
            "spacer.stl",
            "clip.stl",
            "foot.stl"
        ]
    );
}

#[test]
fn gre_1896_objects_info_wins_and_lists_every_copy() {
    let text = "M486 A\"ignored.stl\"\n; objects_info = {\"objects\":[{\"name\":\"bracket.stl (Instance 1)\"},{\"name\":\"bracket.stl (Instance 1)\"},{\"name\":\" \"}]}\n";
    assert_eq!(
        names(text),
        ["bracket.stl (Instance 1)", "bracket.stl (Instance 1)"]
    );
    assert_eq!(names("G28\nG1 X10 Y10\n"), Vec::<String>::new());
}

#[test]
fn gre_1896_a_malformed_200_kib_file_is_scanned_in_linear_time() {
    let line =
        |prefix: &str, unit: &str| format!("{prefix}{}", unit.repeat(200 * 1024 / unit.len()));
    let files = [
        line("EXCLUDE_OBJECT_DEFINE ", "\"name\" :"),
        line("EXCLUDE_OBJECT_DEFINE ", "NAME = "),
        line("M486 ", "a "),
        line("M486 ", "a\""),
        line("", "; printing object "),
        line(";MESH:", " "),
        line("; objects_info = {", "\"objects\":["),
        "EXCLUDE_OBJECT_DEFINE NAME=x\n".repeat(200 * 1024 / 29),
    ];
    for file in files {
        let started = Instant::now();
        let _ = object_names(file.as_bytes());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{}… took {:?}",
            &file[..24],
            started.elapsed()
        );
    }
}

// ---- Binary G-code --------------------------------------------------------------------------

/// A block: header, parameters, data and its CRC32 over all three.
fn block(kind: u16, compression: u16, encoding: u16, text: &[u8]) -> Vec<u8> {
    // Heatshrink (2, 3) is not decoded, so its bytes only have to be there.
    let data = if compression == 1 {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(text).unwrap();
        encoder.finish().unwrap()
    } else {
        text.to_vec()
    };
    let mut out = Vec::new();
    out.extend(kind.to_le_bytes());
    out.extend(compression.to_le_bytes());
    out.extend((text.len() as u32).to_le_bytes());
    if compression != 0 {
        out.extend((data.len() as u32).to_le_bytes());
    }
    if kind == THUMBNAIL_BLOCK {
        out.extend([0; 6]);
    } else {
        out.extend(encoding.to_le_bytes());
    }
    out.extend(&data);
    let mut crc = flate2::Crc::new();
    crc.update(&out);
    out.extend(crc.sum().to_le_bytes());
    out
}

/// Recomputes a block's CRC after a test edits its header.
fn recrc(block: &mut Vec<u8>) {
    block.truncate(block.len() - 4);
    let mut crc = flate2::Crc::new();
    crc.update(block);
    let sum = crc.sum();
    block.extend(sum.to_le_bytes());
}

fn bgcode(blocks: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"GCDE".to_vec();
    out.extend(1u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    blocks.iter().for_each(|b| out.extend(b));
    out
}

const PRINTER_METADATA: u16 = 3;
const LABELS: &[u8] = b"EXCLUDE_OBJECT_DEFINE NAME=bracket.stl_id_0_copy_0\nEXCLUDE_OBJECT_DEFINE NAME=bracket.stl_id_0_copy_1\nG28\n";

#[test]
fn gre_1896_binary_gcode_reads_uncompressed_and_deflate_blocks() {
    let both = ["bracket.stl_id_0_copy_0", "bracket.stl_id_0_copy_1"].map(String::from);
    for compression in [0, 1] {
        let file = bgcode(&[
            block(PRINTER_METADATA, compression, 0, b"printer_model=MK4"),
            block(GCODE_BLOCK, compression, 0, LABELS),
        ]);
        assert_eq!(object_names(&file), FileObjects::Read(both.to_vec()));
    }
    // A label split across two G-code blocks is still one line.
    let file = bgcode(&[
        block(GCODE_BLOCK, 1, 0, &LABELS[..40]),
        block(GCODE_BLOCK, 0, 0, &LABELS[40..]),
    ]);
    assert_eq!(object_names(&file), FileObjects::Read(both.to_vec()));
    let info = br#"objects_info={"objects":[{"name":"lid.stl"},{"name":"lid.stl"}]}"#;
    let file = bgcode(&[block(PRINTER_METADATA, 1, 0, info)]);
    assert_eq!(
        object_names(&file),
        FileObjects::Read(vec!["lid.stl".into(), "lid.stl".into()])
    );
}

#[test]
fn gre_1896_binary_gcode_reports_blocks_it_cannot_decode() {
    for (compression, encoding) in [(2, 0), (3, 0), (0, 1), (0, 2)] {
        let file = bgcode(&[
            block(PRINTER_METADATA, 0, 0, b"printer_model=MK4"),
            block(GCODE_BLOCK, compression, encoding, LABELS),
        ]);
        assert_eq!(
            object_names(&file),
            FileObjects::Unsupported,
            "{compression}/{encoding}"
        );
    }
    // A full inventory in readable metadata still counts.
    let info = br#"objects_info={"objects":[{"name":"lid.stl"}]}"#;
    let file = bgcode(&[
        block(PRINTER_METADATA, 0, 0, info),
        block(GCODE_BLOCK, 3, 1, LABELS),
    ]);
    assert_eq!(
        object_names(&file),
        FileObjects::Read(vec!["lid.stl".into()])
    );
}

#[test]
fn gre_1896_binary_gcode_reads_no_labels_from_thumbnails() {
    let file = bgcode(&[
        block(
            THUMBNAIL_BLOCK,
            0,
            0,
            b"\x89PNG EXCLUDE_OBJECT_DEFINE NAME=evil.stl\n",
        ),
        block(GCODE_BLOCK, 0, 0, b"G28\n"),
    ]);
    assert_eq!(object_names(&file), FileObjects::Read(Vec::new()));
}

#[test]
fn gre_1896_malformed_binary_gcode_is_unreadable() {
    let good = bgcode(&[block(GCODE_BLOCK, 1, 0, LABELS)]);
    let mut bad_crc = good.clone();
    let last = bad_crc.len() - 1;
    bad_crc[last] ^= 0xff;
    let mut corrupt = good.clone();
    corrupt[30] ^= 0xff;
    // Deflate data that inflates past its declared size: a small bomb.
    let mut bomb = block(GCODE_BLOCK, 1, 0, &[b'G'; 100_000]);
    bomb[4..8].copy_from_slice(&10u32.to_le_bytes());
    recrc(&mut bomb);
    let mut too_big = block(GCODE_BLOCK, 0, 0, b"G28\n");
    too_big[4..8].copy_from_slice(&(MAX_BLOCK_BYTES + 1).to_le_bytes());
    let mut unknown = block(GCODE_BLOCK, 0, 0, b"G28\n");
    unknown[0..2].copy_from_slice(&9u16.to_le_bytes());
    for (what, file) in [
        ("truncated", good[..good.len() - 6].to_vec()),
        ("checksum", bad_crc),
        ("corrupt data", corrupt),
        ("bomb", bgcode(&[bomb])),
        ("declared size", bgcode(&[too_big])),
        ("block type", bgcode(&[unknown])),
        ("header", b"GCDE\x01\x00".to_vec()),
    ] {
        assert_eq!(object_names(&file), FileObjects::Unreadable, "{what}");
    }
}

// ---- Proposal -------------------------------------------------------------------------------

fn part(part_id: i64, filename: &str, units: &[u32]) -> RemainingPart {
    let stem = filename.trim_end_matches(".stl");
    RemainingPart {
        part_id,
        filename: filename.into(),
        units: units
            .iter()
            .map(|&index| RemainingUnit {
                index,
                object_name: format!("{stem}__ppu_{part_id}_{index}"),
            })
            .collect(),
    }
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).into()).collect()
}

fn coordinates(proposal: &Proposal) -> Vec<(i64, u32, &str)> {
    proposal
        .units
        .iter()
        .map(|u| (u.part_id, u.unit_index, u.object_name.as_str()))
        .collect()
}

#[test]
fn gre_1896_each_copy_takes_the_next_unit_still_to_print() {
    let names = strings(&[
        "bracket.stl_id_0_copy_0",
        "bracket.stl_id_0_copy_1",
        "bracket.stl_id_0_copy_2",
    ]);
    let proposal = propose_units(&names, &[part(11, "bracket.stl", &[1, 2])]);
    assert_eq!(
        coordinates(&proposal),
        [
            (11, 1, "bracket.stl_id_0_copy_0"),
            (11, 2, "bracket.stl_id_0_copy_1")
        ]
    );
    assert_eq!(proposal.unmatched, ["bracket.stl_id_0_copy_2"]);
}

#[test]
fn gre_1896_an_export_name_takes_its_own_unit_before_file_name_matches() {
    let proposal = propose_units(
        &strings(&["bracket.stl", "bracket_03", " lid_01 "]),
        &[
            part(11, "bracket.stl", &[0, 1, 2]),
            part(13, "lid.stl", &[0]),
        ],
    );
    assert_eq!(
        coordinates(&proposal),
        [
            (11, 2, "bracket_03"),
            (13, 0, "lid_01"),
            (11, 0, "bracket.stl")
        ]
    );
}

#[test]
fn gre_1896_a_name_for_no_part_or_two_parts_takes_no_unit() {
    let parts = [
        part(11, "bracket.stl", &[0]),
        part(12, "bracket.stl", &[0]),
        part(13, "z_tensioner_left.stl", &[0]),
        part(14, "z_tensioner_right.stl", &[0]),
    ];
    let names = strings(&[
        "bracket.stl",
        "spoon.stl",
        "z_tensionr_left.stl",
        "z_tensioner_rght.stl",
    ]);
    let proposal = propose_units(&names, &parts);
    assert_eq!(coordinates(&proposal), [(13, 0, "z_tensionr_left.stl")]);
    assert_eq!(
        proposal.unmatched,
        ["bracket.stl", "spoon.stl", "z_tensioner_rght.stl"]
    );
}

#[test]
fn gre_1896_a_required_unit_name_from_the_3mf_export_takes_that_unit() {
    let parts = [part(11, "bracket.stl", &[0, 1, 2])];
    let names = strings(&[
        "bracket__ppu_11_2_id_0_copy_0",
        "'bracket__ppu_11_1'",
        "bracket__ppu_11_7",
        "bracket.stl",
    ]);
    let proposal = propose_units(&names, &parts);
    assert_eq!(
        coordinates(&proposal),
        [
            (11, 2, "bracket__ppu_11_2_id_0_copy_0"),
            (11, 1, "'bracket__ppu_11_1'"),
            (11, 0, "bracket.stl")
        ]
    );
    assert_eq!(proposal.unmatched, ["bracket__ppu_11_7"]);
}
