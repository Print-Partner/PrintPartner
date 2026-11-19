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

/// A chain of `levels` parts, each the only child of the one before.
fn chain(levels: u64) -> String {
    let mut body = String::new();
    for level in 0..levels {
        body.push_str(&part(10 * level + 10, &format!("L{level}"), "Level"));
        if level > 0 {
            body.push_str(&usage(100_000 + level, "", 10 * level + 2, 10 * level + 12));
        }
    }
    file(&body)
}

#[test]
fn accepts_a_tree_at_the_level_cap_and_refuses_one_level_more() {
    let tree = read(&chain(MAX_LEVELS as u64)).unwrap();
    assert_eq!(depth(&tree.roots[0]), MAX_LEVELS);
    assert!(matches!(
        read(&chain(MAX_LEVELS as u64 + 1)),
        Err(StepError::TooLarge)
    ));
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
    let text = file(&part(10, "P", "Part"));
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

fn within(text: &[u8], limits: Limits) -> Result<PartTree, StepError> {
    part_tree_within(text, limits, &IDLE)
}

/// Scorer finding on 4c2445ee: the first 42 lines of the fixture returned a
/// two-level tree as a success. Every proper prefix is now refused.
#[test]
fn every_truncation_of_the_fixture_is_refused() {
    let whole = NESTED.trim_ascii_end();
    assert!(part_tree(whole, &IDLE).is_ok());
    for length in 0..whole.len() {
        match part_tree(&whole[..length], &IDLE) {
            Err(StepError::Incomplete | StepError::NotStep) => {}
            other => panic!("prefix of {length} bytes: {other:?}"),
        }
    }
}

#[test]
fn a_cut_at_each_kind_of_point_is_incomplete() {
    let text = std::str::from_utf8(NESTED).unwrap();
    let at = |needle: &str| text.find(needle).unwrap();
    let cuts = [
        ("mid-record", at("#104=") + 20),
        ("mid-string", at("'Bed; ''heated''") + 6),
        ("between records", at("#105=")),
        ("before ENDSEC", text.rfind("ENDSEC;").unwrap()),
        ("without the END line", at("END-ISO-10303-21;")),
        ("mid END line", at("END-ISO-10303-21;") + 8),
    ];
    for (name, cut) in cuts {
        let result = part_tree(&NESTED[..cut], &IDLE);
        assert!(
            matches!(result, Err(StepError::Incomplete)),
            "{name}: {result:?}"
        );
    }
    let comment = format!("{}/* never closed", &text[..at("END-ISO-10303-21;")]);
    let result = part_tree(comment.as_bytes(), &IDLE);
    assert!(
        matches!(result, Err(StepError::Incomplete)),
        "open comment: {result:?}"
    );
}

#[test]
fn a_product_record_over_the_record_cap_is_refused_and_geometry_is_not() {
    let limits = Limits {
        max_record_bytes: 200,
        ..Limits::default()
    };
    let long_name = "N".repeat(300);
    let refused = within(file(&part(10, "P", &long_name)).as_bytes(), limits);
    assert!(matches!(refused, Err(StepError::TooLarge)), "{refused:?}");
    let geometry = format!(
        "#1=POLYLINE('{}',(#2));\n{}",
        "x".repeat(300),
        part(10, "P", "Part")
    );
    assert!(within(file(&geometry).as_bytes(), limits).is_ok());
}

#[test]
fn records_and_kept_text_are_capped_as_they_are_read() {
    let two_parts = file(&[part(10, "A", "First"), part(20, "B", "Second")].concat());
    let records = Limits {
        max_records: 5,
        ..Limits::default()
    };
    assert!(matches!(
        within(two_parts.as_bytes(), records),
        Err(StepError::TooLarge)
    ));
    let one_part = file(&part(10, "A", "First"));
    assert!(within(one_part.as_bytes(), records).is_ok());

    let text = Limits {
        max_kept_text: 10,
        ..Limits::default()
    };
    let long = file(&part(10, "A", "Eleven char"));
    assert!(matches!(
        within(long.as_bytes(), text),
        Err(StepError::TooLarge)
    ));
}

#[test]
fn tree_text_is_capped_across_every_use_of_a_shared_part() {
    // A 40-byte part used 10 times: 10 parts read, 11 nodes built.
    let name = "S".repeat(40);
    let mut body = [part(10, "R", "Root"), part(20, "S", &name)].concat();
    for copy in 0..10 {
        body.push_str(&usage(100 + copy, "", 12, 22));
    }
    let limits = Limits {
        max_tree_text: 300,
        ..Limits::default()
    };
    let refused = within(file(&body).as_bytes(), limits);
    assert!(matches!(refused, Err(StepError::TooLarge)), "{refused:?}");
    assert_eq!(
        within(file(&body).as_bytes(), Limits::default())
            .unwrap()
            .roots[0]
            .children
            .len(),
        10
    );
}

/// A reader that sets `flag` once `after` bytes have been read, and counts
/// what it hands out.
struct Tripwire<'a> {
    bytes: &'a [u8],
    at: usize,
    after: usize,
    flag: &'a AtomicBool,
}

impl Read for Tripwire<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = out.len().min(self.bytes.len() - self.at).min(4096);
        out[..count].copy_from_slice(&self.bytes[self.at..self.at + count]);
        self.at += count;
        if self.at >= self.after {
            self.flag.store(true, Ordering::Relaxed);
        }
        Ok(count)
    }
}

#[test]
fn cancellation_stops_a_long_record_within_one_chunk() {
    let long = format!(
        "#1=POLYLINE('{}',(#2));\n{}",
        "x".repeat(8 << 20),
        part(10, "P", "Part")
    );
    let text = file(&long);
    let flag = AtomicBool::new(false);
    let mut reader = Tripwire {
        bytes: text.as_bytes(),
        at: 0,
        after: 1 << 20,
        flag: &flag,
    };
    let result = part_tree(&mut reader, &flag);
    assert!(matches!(result, Err(StepError::Cancelled)), "{result:?}");
    assert!(
        reader.at <= (1 << 20) + READ_CHUNK,
        "read {} bytes",
        reader.at
    );
}

#[test]
fn cancellation_stops_the_tree_build() {
    let flag = AtomicBool::new(false);
    let mut reader = Tripwire {
        bytes: NESTED,
        at: 0,
        after: NESTED.len(),
        flag: &flag,
    };
    let result = part_tree(&mut reader, &flag);
    assert!(matches!(result, Err(StepError::Cancelled)), "{result:?}");
}

/// The fixture with `from` replaced by `to`, once.
fn mutated(from: &str, to: &str) -> String {
    let text = std::str::from_utf8(NESTED).unwrap();
    assert_eq!(text.matches(from).count(), 1, "{from}");
    text.replacen(from, to, 1)
}

/// Sol on d15ed842 found six altered exchanges the reader accepted, and
/// asked for four more. Each is refused, and none returns a tree.
#[test]
fn an_exchange_that_is_not_well_formed_is_refused() {
    let text = std::str::from_utf8(NESTED).unwrap();
    let header = &text[text.find("HEADER;").unwrap()..text.find("DATA;").unwrap()];
    let data = &text[text.find("DATA;").unwrap()..text.rfind("END-ISO").unwrap()];
    let cases = [
        ("no HEADER section", text.replacen(header, "", 1)),
        ("no HEADER keyword", mutated("HEADER;\n", "")),
        ("a partial record after the end", format!("{text}#109=PROD")),
        (
            "a missing ; between records",
            mutated("#72,$);\n#108=", "#72,$)\n#108="),
        ),
        (
            "a second DATA section",
            mutated("ENDSEC;\nEND-ISO", "ENDSEC;\nDATA;\nENDSEC;\nEND-ISO"),
        ),
        (
            "DATA before HEADER",
            format!("ISO-10303-21;\n{data}{header}END-ISO-10303-21;\n"),
        ),
        (
            "text between ENDSEC and DATA",
            mutated("ENDSEC;\nDATA;", "ENDSEC;\nNOTE('x');\nDATA;"),
        ),
        (
            "an entity in the header",
            mutated("ENDSEC;\nDATA;", "#1=X('');\nENDSEC;\nDATA;"),
        ),
        ("END inside DATA", mutated("ENDSEC;\nEND-ISO", "END-ISO")),
        (
            "a duplicated product id",
            mutated("#20=PRODUCT(", "#10=PRODUCT("),
        ),
        ("a bare word in DATA", mutated("#100=", "STRAY;\n#100=")),
        (
            "text after a parameter list",
            mutated("#12,#22,$);\n#101", "#12,#22,$) x;\n#101"),
        ),
        (
            "an unclosed parameter list",
            mutated("#12,#82,$);", "#12,#82,$;"),
        ),
    ];
    for (name, text) in cases {
        let result = part_tree(text.as_bytes(), &IDLE);
        assert!(
            matches!(result, Err(StepError::Malformed)),
            "{name}: {result:?}"
        );
    }
    let open_comment = format!("{text}/* open");
    let result = part_tree(open_comment.as_bytes(), &IDLE);
    assert!(matches!(result, Err(StepError::Incomplete)), "{result:?}");
    // Whitespace, CRLF and closed comments after the end keep the tree.
    let trailing = format!("{text}\r\n  /* closed */\r\n");
    assert_eq!(
        part_tree(trailing.as_bytes(), &IDLE).unwrap(),
        part_tree(NESTED, &IDLE).unwrap()
    );
}

/// Sol on d15ed842: the record cap counted only the buffered body. It now
/// counts the whole record, `#` through `;`, comments included.
#[test]
fn the_record_cap_counts_the_whole_record() {
    let cap = Limits::default().max_record_bytes;
    // A PRODUCT record of exactly `length` bytes, with a comment inside.
    let product = |length: usize| {
        let shell = "#10=PRODUCT('P','',/**/'',(#2));";
        let record = format!(
            "#10=PRODUCT('P','{}',/**/'',(#2));",
            "N".repeat(length - shell.len())
        );
        assert_eq!(record.len(), length);
        let rest = "\n#11=PRODUCT_DEFINITION_FORMATION('','',#10);\n#12=PRODUCT_DEFINITION('design','',#11,#3);";
        file(&format!("{record}{rest}"))
    };
    assert!(part_tree(product(cap).as_bytes(), &IDLE).is_ok());
    let result = part_tree(product(cap + 1).as_bytes(), &IDLE);
    assert!(matches!(result, Err(StepError::TooLarge)), "{result:?}");
}
