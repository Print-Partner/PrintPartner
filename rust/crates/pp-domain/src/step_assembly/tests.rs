use super::*;

static IDLE: AtomicBool = AtomicBool::new(false);

const NESTED: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/step/nested-assembly.step"
));

fn node(name: &str, product_id: &str, instance: Option<&str>, children: Vec<PartNode>) -> PartNode {
    PartNode {
        name: name.into(),
        product_id: product_id.into(),
        instance: instance.map(Into::into),
        children,
    }
}

fn read(text: &str) -> Result<PartTree, StepError> {
    part_tree(text.as_bytes(), &IDLE)
}

/// A minimal file: `body` between `DATA;` and `ENDSEC;`.
fn file(body: &str) -> String {
    format!("ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n{body}\nENDSEC;\nEND-ISO-10303-21;\n")
}

/// One part: PRODUCT #n, formation #n+1, definition #n+2.
fn part(n: u64, id: &str, name: &str) -> String {
    format!(
        "#{n}=PRODUCT('{id}','{name}','',(#2));\n#{}=PRODUCT_DEFINITION_FORMATION('','',#{n});\n#{}=PRODUCT_DEFINITION('design','',#{},#3);\n",
        n + 1,
        n + 2,
        n + 1
    )
}

fn usage(n: u64, name: &str, parent: u64, child: u64) -> String {
    format!("#{n}=NEXT_ASSEMBLY_USAGE_OCCURRENCE('U{n}','{name}','',#{parent},#{child},$);\n")
}

fn depth(node: &PartNode) -> usize {
    1 + node.children.iter().map(depth).max().unwrap_or(0)
}

#[test]
fn rebuilds_the_whole_nested_tree_of_a_six_level_assembly() {
    let tree = part_tree(NESTED, &IDLE).unwrap();
    let screw = |instance| node("SCR-M3", "SCR-M3", Some(instance), vec![]);
    let expected = PartTree {
        roots: vec![node(
            "Printer",
            "PRN-1",
            None,
            vec![
                node(
                    "Frame",
                    "FRM-1",
                    Some("Frame:1"),
                    vec![
                        node(
                            "Gantry",
                            "GNT-1",
                            Some("Gantry:1"),
                            vec![node(
                                "Carriage",
                                "CAR-1",
                                Some("Carriage:1"),
                                vec![
                                    node(
                                        "Hotend",
                                        "HOT-1",
                                        Some("Hotend:1"),
                                        vec![node(
                                            "D\u{fc}se 0.4",
                                            "NOZ-1",
                                            Some("Nozzle:1"),
                                            vec![],
                                        )],
                                    ),
                                    screw("Screw:1"),
                                    screw("Screw:2"),
                                ],
                            )],
                        ),
                        // An occurrence with no name is labelled by its id.
                        screw("NAUO8"),
                    ],
                ),
                node("Bed; 'heated'", "BED-1", Some("Bed:1"), vec![]),
            ],
        )],
    };
    assert_eq!(tree, expected);
    assert_eq!(depth(&tree.roots[0]), 6);
}

#[test]
fn the_tree_serializes_as_nested_json() {
    let tree = part_tree(NESTED, &IDLE).unwrap();
    let json = serde_json::to_value(&tree).unwrap();
    let nozzle = &json["roots"][0]["children"][0]["children"][0]["children"][0]["children"][0]["children"]
        [0];
    assert_eq!(nozzle["name"], "D\u{fc}se 0.4");
    assert_eq!(nozzle["instance"], "Nozzle:1");
    assert_eq!(serde_json::from_value::<PartTree>(json).unwrap(), tree);
}

#[test]
fn a_single_part_file_is_one_root() {
    let tree = read(&file(&part(10, "P", "Bracket"))).unwrap();
    assert_eq!(tree.roots, vec![node("Bracket", "P", None, vec![])]);
}

#[test]
fn separate_top_assemblies_are_separate_roots_in_entity_order() {
    let body = [part(20, "B", "Second"), part(10, "A", "First")].concat();
    let tree = read(&file(&body)).unwrap();
    let names: Vec<_> = tree.roots.iter().map(|root| root.name.as_str()).collect();
    assert_eq!(names, ["First", "Second"]);
}

#[test]
fn refuses_a_file_that_is_not_step() {
    assert!(matches!(
        read("solid cube\nendsolid\n"),
        Err(StepError::NotStep)
    ));
    assert!(matches!(read(""), Err(StepError::NotStep)));
    assert!(matches!(
        read("ISO-10303-21 HEADER;"),
        Err(StepError::NotStep)
    ));
}

#[test]
fn refuses_a_file_without_products() {
    let body = "#1=CARTESIAN_POINT('',(0.,0.,0.));";
    assert!(matches!(read(&file(body)), Err(StepError::NoProducts)));
    // A definition whose formation is missing names no product.
    let body = "#12=PRODUCT_DEFINITION('design','',#11,#3);";
    assert!(matches!(read(&file(body)), Err(StepError::NoProducts)));
}

#[test]
fn refuses_an_assembly_that_contains_itself() {
    // A -> B -> A, under a root R.
    let body = [
        part(10, "R", "Root"),
        part(20, "A", "A"),
        part(30, "B", "B"),
        usage(100, "", 12, 22),
        usage(101, "", 22, 32),
        usage(102, "", 32, 22),
    ]
    .concat();
    assert!(matches!(read(&file(&body)), Err(StepError::Cycle)));
    // With no root at all.
    let body = [
        part(20, "A", "A"),
        part(30, "B", "B"),
        usage(101, "", 22, 32),
        usage(102, "", 32, 22),
    ]
    .concat();
    assert!(matches!(read(&file(&body)), Err(StepError::Cycle)));
}

#[test]
fn refuses_a_tree_deeper_than_the_cap() {
    let mut body = String::new();
    for level in 0..=(MAX_DEPTH as u64 + 1) {
        body.push_str(&part(10 * level + 10, &format!("L{level}"), "Level"));
        if level > 0 {
            body.push_str(&usage(100_000 + level, "", 10 * level + 2, 10 * level + 12));
        }
    }
    assert!(matches!(read(&file(&body)), Err(StepError::TooLarge)));
}

#[test]
fn refuses_a_tree_with_too_many_uses() {
    // Each level uses the next one 4 times: 4^9 leaves is over the cap.
    let mut body = String::new();
    for level in 0..10u64 {
        body.push_str(&part(10 * level + 10, &format!("L{level}"), "Level"));
        if level > 0 {
            for copy in 0..4 {
                let parent = 10 * level + 2;
                body.push_str(&usage(100_000 + level * 10 + copy, "", parent, parent + 10));
            }
        }
    }
    assert!(matches!(read(&file(&body)), Err(StepError::TooLarge)));
}

#[test]
fn stops_when_cancelled() {
    let cancelled = AtomicBool::new(true);
    let body = "#1=CARTESIAN_POINT('',(0.,0.,0.));\n".repeat(CANCEL_EVERY as usize);
    let text = file(&format!("{}{body}", part(10, "P", "Part")));
    assert!(matches!(
        part_tree(text.as_bytes(), &cancelled),
        Err(StepError::Cancelled)
    ));
}

#[test]
fn reads_across_buffer_boundaries_and_skips_long_geometry() {
    // A geometry record far longer than the scanner's buffer and the record cap.
    let long = format!("#1=POLYLINE('{}',(#2));\n", "x;".repeat(200_000));
    let body = [long, part(10, "P", "After")].concat();
    let tree = read(&file(&body)).unwrap();
    assert_eq!(tree.roots, vec![node("After", "P", None, vec![])]);
}

#[test]
fn decodes_string_directives() {
    assert_eq!(decode("plain"), "plain");
    assert_eq!(decode("D\\X2\\00FC\\X0\\se"), "D\u{fc}se");
    assert_eq!(decode("\\X4\\0001F600\\X0\\"), "\u{1f600}");
    assert_eq!(decode("caf\\X\\E9"), "caf\u{e9}");
    assert_eq!(decode("\\S\\i"), "\u{e9}");
    assert_eq!(decode("a\\\\b"), "a\\b");
    assert_eq!(decode("\\PA\\x"), "x");
    // Malformed directives stay as written.
    assert_eq!(decode("\\X2\\00F\\X0\\"), "\\X2\\00F\\X0\\");
    assert_eq!(decode("\\X2\\00FC"), "\\X2\\00FC");
    assert_eq!(decode("end\\"), "end\\");
}
