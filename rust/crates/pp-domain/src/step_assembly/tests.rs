use super::*;

static IDLE: AtomicBool = AtomicBool::new(false);

const NESTED: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../fixtures/step/nested-assembly.step"
));

const HEADER: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_NAME('t','',(''),(''),'','','');\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\n";

fn within(text: &[u8], limits: Limits) -> Result<ProductStructure, StepError> {
    product_structure(text, limits, &IDLE)
}

fn read(text: &str) -> Result<ProductStructure, StepError> {
    within(text.as_bytes(), Limits::default())
}

fn nested() -> &'static str {
    std::str::from_utf8(NESTED).unwrap()
}

/// A minimal file: `body` between `DATA;` and `ENDSEC;`.
fn file(body: &str) -> String {
    format!("{HEADER}DATA;\n{body}\nENDSEC;\nEND-ISO-10303-21;\n")
}

/// One part: PRODUCT #n, formation #n+1, definition #n+2.
fn part(n: u64, id: &str, name: &str) -> String {
    let (formation, definition) = (n + 1, n + 2);
    format!(
        "#{n}=PRODUCT('{id}','{name}','',(#2));\n#{formation}=PRODUCT_DEFINITION_FORMATION('','',#{n});\n#{definition}=PRODUCT_DEFINITION('design','',#{formation},#3);\n"
    )
}

/// `record` and then one part.
fn with_part(record: &str) -> String {
    file(&format!("{record}\n{}", part(10, "P", "Part")))
}

/// The result as a short label, for tables of cases.
fn outcome(result: Result<ProductStructure, StepError>) -> String {
    match result {
        Ok(_) => "Ok".into(),
        Err(StepError::Malformed { .. }) => "Malformed".into(),
        Err(StepError::Unsupported { .. }) => "Unsupported".into(),
        Err(StepError::InvalidStructure { .. }) => "InvalidStructure".into(),
        Err(error) => format!("{error:?}"),
    }
}

/// The fixture with `from` replaced by `to`, once.
fn mutated(from: &str, to: &str) -> String {
    assert_eq!(nested().matches(from).count(), 1, "{from}");
    nested().replacen(from, to, 1)
}

#[rustfmt::skip]
fn usage(instance: Option<&str>, parent: u64, child: u64) -> Usage {
    Usage { instance: instance.map(Into::into), parent, child }
}

#[test]
fn reads_the_product_structure_of_a_six_level_assembly() {
    let structure = product_structure(NESTED, Limits::default(), &IDLE).unwrap();
    let name = |id: u64| structure.products[&id].1.as_str();
    assert_eq!(structure.products.len(), 8);
    let names = [name(10), name(60), name(70), name(80)];
    assert_eq!(names, ["Printer", "D\u{fc}se 0.4", "", "Bed; 'heated'"]);
    assert_eq!(structure.formations[&21], 20);
    let definitions: Vec<u64> = structure.definitions.keys().copied().collect();
    assert_eq!(definitions, [12, 22, 32, 42, 52, 62, 72, 82]);
    assert_eq!(structure.usages.len(), 9);
    assert_eq!(structure.usages[&100], usage(Some("Frame:1"), 12, 22));
    // An occurrence with no name is labelled by its id.
    assert_eq!(structure.usages[&107], usage(Some("NAUO8"), 22, 72));
    let geometry = read(&file("#1=CARTESIAN_POINT('',(0.,0.,0.));")).unwrap();
    assert_eq!(geometry, ProductStructure::default());
}

/// Every construct the contract names is read and dropped without changing
/// what is kept from the one part beside it.
#[test]
#[rustfmt::skip]
fn each_supported_construct_leaves_the_result_unchanged() {
    let one = part(10, "P", "Part");
    let expected = read(&file(&one)).unwrap();
    assert_eq!(expected.products[&10], ("P".into(), "Part".into()));
    let header = |extra: &str| HEADER.replace("ENDSEC;", &format!("{extra}ENDSEC;"));
    let (product, rest) = one.split_once('\n').unwrap();
    let cases = [
        ("integers and reals", with_part("#1=X(0,-12,+3,1.,-2.5E-3,6.02E+23,7.E2);")),
        ("enumerations", with_part("#1=X(.T.,.F.,.U.,.NOT_KNOWN.,._A1.);")),
        ("binaries", with_part("#1=X(\"0\",\"3FA0\");")),
        ("omitted and derived", with_part("#1=X($,*,(*,$));")),
        ("an empty parameter list", with_part("#1=X();")),
        ("nested and empty lists", with_part("#1=X((),((1,2),(3,(4))));")),
        ("typed parameters", with_part("#1=X(LENGTH_MEASURE(1.),(RATIO(2.)),A(B((1))));")),
        ("references to any record", with_part("#1=X(#2,(#3,#99999));")),
        ("strings in a dropped record", with_part("#1=X('a''b;c \\Q\\ \u{e9}');")),
        ("a complex instance", with_part("#1=(A()B(1,'x')C((#2)));")),
        ("a user-defined keyword", with_part("#1=!MY_POINT(1.);")),
        ("comments between tokens", with_part("#1/**/=/* ; */X/**/(/**/1/**/,'a'/**/)/**/;")),
        ("tab, CR and LF", with_part("#1\t=\r\nX(\t1\r\n);")),
        ("spaces between tokens", with_part("#1 = X ( 1 , 2 ) ;")),
        ("a record longer than the buffer and the record cap", with_part(&format!("#1=X('{}');", "x;".repeat(200_000)))),
        ("blanks before the first ;", file(&one).replacen("ISO-10303-21;", "ISO-10303-21 /**/ ;", 1)),
        ("extra and user-defined header entities", format!("{}DATA;\n{one}ENDSEC;\nEND-ISO-10303-21;", header("FILE_POPULATION('S',*,());\n!NOTE('x');\n"))),
        ("two DATA sections with parameter lists", format!("{HEADER}DATA('a',('S'));\n{product}\nENDSEC;\nDATA('b',('S'));\n{rest}ENDSEC;\nEND-ISO-10303-21;")),
    ];
    for (name, text) in cases {
        assert_eq!(read(&text).ok().as_ref(), Some(&expected), "{name}");
    }
}

/// The kept records accept each form the contract names for them.
#[test]
fn kept_records_take_each_form_of_their_parameters() {
    let body = [
        // A comment counts as blank, and `$` is an empty name.
        "#10=PRODUCT('R',$,/* note */'',(#2));",
        "#11=PRODUCT_DEFINITION_FORMATION_WITH_SPECIFIED_SOURCE('','',#10,.MADE.);",
        "#12=PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS('','',#11,#3,(#4));",
        // A name folded across lines, with raw UTF-8 and a quote.
        "#20=PRODUCT('S','Sch\r\nr\u{e4}ube ''M3''','',(#2));",
        "#21=PRODUCT_DEFINITION_FORMATION('','',#20);",
        "#22=PRODUCT_DEFINITION('','',#21,#3);",
        "#30=NEXT_ASSEMBLY_USAGE_OCCURRENCE($,$,$,#12,#22,$);",
        "#31=NEXT_ASSEMBLY_USAGE_OCCURRENCE('U31',$,'',#12,#22,'d');",
    ]
    .join("\n");
    let structure = read(&file(&body)).unwrap();
    assert_eq!(structure.products[&10], ("R".into(), String::new()));
    assert_eq!(structure.products[&20].1, "Schr\u{e4}ube 'M3'");
    assert_eq!(structure.formations[&11], 10);
    assert_eq!(structure.definitions[&12], 11);
    assert_eq!(structure.usages[&30], usage(None, 12, 22));
    assert_eq!(structure.usages[&31], usage(Some("U31"), 12, 22));
}

#[test]
#[rustfmt::skip]
fn decodes_string_directives() {
    assert_eq!(decode("plain"), Ok("plain".into()));
    assert_eq!(decode("D\\X2\\00FC\\X0\\se"), Ok("D\u{fc}se".into()));
    assert_eq!(decode("\\X2\\D83DDE00\\X0\\"), Ok("\u{1f600}".into()));
    assert_eq!(decode("\\X4\\0001F600\\X0\\"), Ok("\u{1f600}".into()));
    assert_eq!(decode("caf\\X\\E9"), Ok("caf\u{e9}".into()));
    assert_eq!(decode("\\S\\i"), Ok("\u{e9}".into()));
    assert_eq!(decode("a\\\\b"), Ok("a\\b".into()));
    assert_eq!(decode("\\PA\\x"), Ok("x".into()));
    // Another code page would change what `\S\` means.
    assert_eq!(decode("\\PB\\x"), Err(true));
    let bad = ["\\X2\\00F\\X0\\", "\\X2\\00FC", "\\X2\\00fc\\X0\\", "\\X2\\D83D\\X0\\", "\\X2\\\\X0\\", "\\X4\\00110000\\X0\\", "\\X\\E", "\\S\\", "\\Q\\", "end\\"];
    for bad in bad {
        assert_eq!(decode(bad), Err(false), "{bad}");
    }
    // Sol on cdcc37b1: a multi-byte character inside `\X2\` panicked. Put one
    // at every position of each escape, inserted and in place of two bytes.
    for (prefix, body) in [("\\X\\", "E9"), ("\\X2\\", "00FC00FC\\X0\\"), ("\\X4\\", "0001F6000001F600\\X0\\"), ("\\S\\", "i")] {
        for at in 0..body.len() {
            let inserted = format!("{prefix}{}\u{e9}{}", &body[..at], &body[at..]);
            let swapped = format!("{prefix}{}\u{e9}{}", &body[..at], body.get(at + 2..).unwrap_or(""));
            for text in [inserted, swapped] {
                assert_eq!(decode(&text), Err(false), "{text}");
            }
        }
    }
}

/// Scorer finding on 4c2445ee: the first 42 lines of the fixture returned a
/// two-level tree as a success. Every proper prefix is refused.
#[test]
fn every_truncation_of_the_fixture_is_refused() {
    let whole = NESTED.trim_ascii_end();
    assert!(within(whole, Limits::default()).is_ok());
    for length in 0..whole.len() {
        match within(&whole[..length], Limits::default()) {
            Err(StepError::Incomplete | StepError::NotStep) => {}
            other => panic!("prefix of {length} bytes: {other:?}"),
        }
    }
}

/// Everything outside the contract is refused with its typed error before
/// anything is returned, including every case Sol found on d15ed842, ca6b79fb
/// and cdcc37b1.
#[test]
#[rustfmt::skip]
fn every_form_outside_the_contract_is_refused_with_its_error() {
    let text = nested();
    let at = |needle: &str| text.find(needle).unwrap();
    let end = at("END-ISO-10303-21;");
    let header = &text[at("HEADER;")..at("DATA;")];
    let data = &text[at("DATA;")..text.rfind("END-ISO").unwrap()];
    let reordered = HEADER.replace("FILE_NAME", "TMP").replace("FILE_SCHEMA", "FILE_NAME").replace("TMP", "FILE_SCHEMA");
    let one = part(10, "P", "Part");
    let section = |name: &str| format!("{HEADER}{name};\nENDSEC;\nDATA;\n{one}ENDSEC;\nEND-ISO-10303-21;");
    let cases = [
        ("NotStep", "not STEP", "solid cube\n".to_string()),
        ("NotStep", "empty", "".to_string()),
        ("NotStep", "a blank first", " ISO-10303-21;".to_string()),
        ("NotStep", "no ; after the magic", "ISO-10303-21 HEADER;".to_string()),
        ("Incomplete", "mid-record", text[..at("#104=") + 20].to_string()),
        ("Incomplete", "mid-string", text[..at("'Bed; ''heated''") + 6].to_string()),
        ("Incomplete", "mid-keyword", text[..at("#105=") + 10].to_string()),
        ("Incomplete", "mid-number", text[..at("(0.,0.,0.)") + 2].to_string()),
        ("Incomplete", "between records", text[..at("#105=")].to_string()),
        ("Incomplete", "before ENDSEC", text[..text.rfind("ENDSEC;").unwrap()].to_string()),
        ("Incomplete", "without the END line", text[..end].to_string()),
        ("Incomplete", "mid END line", text[..end + 8].to_string()),
        ("Incomplete", "in a comment", format!("{}/* never closed", &text[..end])),
        ("Incomplete", "after a /", format!("{}/", &text[..end])),
        ("Malformed", "no HEADER section", text.replacen(header, "", 1)),
        ("Malformed", "no HEADER keyword", mutated("HEADER;\n", "")),
        ("Malformed", "HEADER with a parameter list (Sol)", mutated("HEADER;", "HEADER();")),
        ("Malformed", "ENDSEC with a parameter list (Sol)", mutated("ENDSEC;\nDATA;", "ENDSEC();\nDATA;")),
        ("Malformed", "END with a parameter list (Sol)", mutated("END-ISO-10303-21;", "END-ISO-10303-21();")),
        ("Malformed", "no FILE_DESCRIPTION (Sol)", mutated("FILE_DESCRIPTION(", "X_DESCRIPTION(")),
        ("Malformed", "no FILE_NAME (Sol)", HEADER.replace("FILE_NAME", "FILE_POPULATION") + "DATA;\nENDSEC;\nEND-ISO-10303-21;"),
        ("Malformed", "header entities out of order", reordered + "DATA;\nENDSEC;\nEND-ISO-10303-21;"),
        ("Malformed", "a header entity without a parameter list", mutated("ENDSEC;\nDATA;", "NOTE;\nENDSEC;\nDATA;")),
        ("Malformed", "no DATA section", format!("{HEADER}END-ISO-10303-21;")),
        ("Malformed", "a partial record after the end", format!("{text}#109=PROD")),
        ("Malformed", "a missing ; between records", mutated("#72,$);\n#108=", "#72,$)\n#108=")),
        ("Malformed", "DATA before HEADER", format!("ISO-10303-21;\n{data}{header}END-ISO-10303-21;\n")),
        ("Malformed", "text between ENDSEC and DATA", mutated("ENDSEC;\nDATA;", "ENDSEC;\nNOTE('x');\nDATA;")),
        ("Malformed", "an entity in the header", mutated("ENDSEC;\nDATA;", "#1=X('');\nENDSEC;\nDATA;")),
        ("Malformed", "END inside DATA", mutated("ENDSEC;\nEND-ISO", "END-ISO")),
        ("Malformed", "a kept id used twice", mutated("#20=PRODUCT(", "#10=PRODUCT(")),
        ("Malformed", "a bare word in DATA", mutated("#100=", "STRAY;\n#100=")),
        ("Malformed", "text after a parameter list", mutated("#12,#22,$);\n#101", "#12,#22,$) x;\n#101")),
        ("Malformed", "an unclosed parameter list", mutated("#12,#82,$);", "#12,#82,$;")),
        ("Malformed", "#N=; (Sol)", with_part("#109=;")),
        ("Malformed", "a record without a parameter list (Sol)", with_part("#109=CARTESIAN_POINT;")),
        ("Malformed", "PRODUCT without a parameter list (Sol)", mutated("#20=PRODUCT('FRM-1','Frame','',(#2));", "#20=PRODUCT;")),
        ("Malformed", "no =", with_part("#1 X();")),
        ("Malformed", "no number", with_part("#=X();")),
        ("Malformed", "a lowercase keyword", with_part("#1=x();")),
        ("Malformed", "two values without a comma", with_part("#1=X(1 2);")),
        ("Malformed", "a leading comma", with_part("#1=X(,1);")),
        ("Malformed", "a trailing comma", with_part("#1=X(1,);")),
        ("Malformed", "two values in a typed parameter", with_part("#1=X(LENGTH(1.,2.));")),
        ("Malformed", "an empty typed parameter", with_part("#1=X(LENGTH());")),
        ("Malformed", "an empty complex instance", with_part("#1=();")),
        ("Malformed", "a complex part without a list", with_part("#1=(A B());")),
        ("Malformed", "a real with no exponent digits", with_part("#1=X(1.E);")),
        ("Malformed", "a lowercase exponent", with_part("#1=X(1.e5);")),
        ("Malformed", "a sign alone", with_part("#1=X(-);")),
        ("Malformed", "a lowercase enumeration", with_part("#1=X(.t.);")),
        ("Malformed", "an empty enumeration", with_part("#1=X(..);")),
        ("Malformed", "a binary with a bad first digit", with_part("#1=X(\"4F\");")),
        ("Malformed", "a binary with a non-hex digit", with_part("#1=X(\"0G\");")),
        ("Malformed", "a control byte in a string", with_part("#1=X('a\u{1}b');")),
        ("Malformed", "a stray /", with_part("#1=X(1)/;")),
        ("Malformed", "an unknown character", with_part("#1=X(%);")),
        ("Malformed", "! without a keyword", with_part("#1=!();")),
        ("Malformed", "a bad directive in a kept string", mutated("'Gantry','',", "'Gan\\Q\\try','',")),
        ("Malformed", "a multi-byte character in \\X2\\ (Sol)", with_part("#30=PRODUCT('p','\\X2\\000\u{e9}000\\X0\\',$,());")),
        ("Malformed", "lowercase hex in a kept string", mutated("\\X2\\00FC\\X0\\", "\\X2\\00fc\\X0\\")),
        ("Unsupported", "an ANCHOR section", section("ANCHOR")),
        ("Unsupported", "a REFERENCE section", section("REFERENCE")),
        ("Unsupported", "a SIGNATURE section", file(&one) + "SIGNATURE;\nENDSEC;\n"),
        ("Unsupported", "a value instance name", with_part("#1=X(@2);")),
        ("Unsupported", "a constant instance name", with_part("#1=X(#PI);")),
        ("Unsupported", "a URI reference", with_part("#1=X(<a.stp#2>);")),
        ("Unsupported", "a kept kind in a complex instance", with_part("#1=(A()PRODUCT('Q','Q','',(#2)));")),
        ("Unsupported", "another code page in a kept string", mutated("'Gantry','',", "'\\PB\\Gantry','',")),
        ("InvalidStructure", "PRODUCT with 3 parameters", mutated("'Gantry','',(#2))", "'Gantry',(#2))")),
        ("InvalidStructure", "PRODUCT with $ for its id", mutated("('GNT-1',", "($,")),
        ("InvalidStructure", "PRODUCT with a number for its name", mutated("'Gantry'", "1")),
        ("InvalidStructure", "a formation of a geometry record", mutated("'',#30);", "'',#4);")),
        ("InvalidStructure", "a formation with $ for its product", mutated("'',#30);", "'',$);")),
        ("InvalidStructure", "a definition of a missing formation", mutated("'',#31,#3)", "'',#39,#3)")),
        ("InvalidStructure", "an occurrence of a product, not a definition", mutated("#12,#22,$", "#12,#20,$")),
        ("InvalidStructure", "an occurrence of a missing definition", mutated("#12,#22,$", "#12,#999,$")),
        ("InvalidStructure", "an occurrence with 5 parameters", mutated("#12,#22,$)", "#12,#22)")),
        ("InvalidStructure", "a definition with documents and 4 parameters", mutated("PRODUCT_DEFINITION('design','',#31", "PRODUCT_DEFINITION_WITH_ASSOCIATED_DOCUMENTS('design','',#31")),
        ("InvalidStructure", "a plain formation with 4 parameters", mutated("FORMATION('','',#30)", "FORMATION('','',#30,.NOT_KNOWN.)")),
    ];
    for (expected, name, text) in cases {
        assert_eq!(outcome(read(&text)), expected, "{name}");
    }
    let mut invalid_utf8 = NESTED.to_vec();
    invalid_utf8[at("Gantry")] = 0xff;
    assert_eq!(outcome(within(&invalid_utf8, Limits::default())), "Malformed");
    // Whitespace, CRLF and closed comments after the end keep the result.
    let trailing = format!("{text}\r\n  /* closed */\r\n");
    assert_eq!(read(&trailing).unwrap(), read(text).unwrap());
}

/// Each cap passes an input at the cap and refuses one a byte or record over
/// it.
#[test]
#[rustfmt::skip]
fn each_limit_accepts_its_cap_and_refuses_one_more() {
    type Set = fn(&mut Limits, usize);
    // The PRODUCT record is the longest kept record, and holds a comment.
    let one = part(10, "P", "A longer part name").replacen("'',(#2)", "/* c */'',(#2)", 1);
    let base = file(&one);
    let longest_record = one.lines().next().unwrap().len();
    assert_eq!(longest_record, one.lines().map(str::len).max().unwrap());
    // 98 bytes and a doubled quote: 100 bytes as written.
    let string = with_part(&format!("#1=X('{}''');", "x".repeat(98)));
    let comment = with_part(&format!("/*{}*/", "x".repeat(96)));
    let nesting = with_part("#1=X(((A((1)))));");
    let cases: [(Limit, &str, usize, Set); 8] = [
        (Limit::FileBytes, &base, base.len(), |l, n| l.max_file_bytes = n as u64),
        (Limit::Entities, &base, 3, |l, n| l.max_entities = n as u64),
        (Limit::Nesting, &nesting, 5, |l, n| l.max_nesting = n),
        (Limit::StringBytes, &string, 100, |l, n| l.max_string_bytes = n),
        (Limit::CommentBytes, &comment, 100, |l, n| l.max_comment_bytes = n),
        (Limit::RecordBytes, &base, longest_record, |l, n| l.max_record_bytes = n),
        (Limit::Records, &base, 3, |l, n| l.max_records = n),
        // "P", "A longer part name" and "design".
        (Limit::KeptText, &base, 25, |l, n| l.max_kept_text = n),
    ];
    for (limit, text, cap, set) in cases {
        let mut limits = Limits::default();
        set(&mut limits, cap);
        assert_eq!(outcome(within(text.as_bytes(), limits)), "Ok", "{limit:?} at {cap}");
        set(&mut limits, cap - 1);
        assert_eq!(outcome(within(text.as_bytes(), limits)), format!("TooLarge({limit:?})"), "{limit:?} at {}", cap - 1);
    }
}

#[test]
#[rustfmt::skip]
fn keywords_and_instance_numbers_have_fixed_caps() {
    let record = |record: String| outcome(read(&with_part(&record)));
    let keyword = |length| record(format!("#1={}();", "K".repeat(length)));
    assert_eq!(keyword(MAX_KEYWORD), "Ok");
    assert_eq!(keyword(MAX_KEYWORD + 1), "TooLarge(KeywordBytes)");
    assert_eq!(record(format!("#{}=X();", u64::MAX)), "Ok");
    assert_eq!(record("#18446744073709551616=X();".into()), "TooLarge(InstanceNumber)");
    // The record's own parameter list is nesting level 1.
    assert_eq!(outcome(within(with_part("").as_bytes(), Limits { max_nesting: 0, ..Limits::default() })), "TooLarge(Nesting)");
}

/// A reader that sets `flag` once `after` bytes have been read.
struct Tripwire<'a> {
    bytes: io::Cursor<&'a [u8]>,
    after: u64,
    flag: &'a AtomicBool,
}

impl Read for Tripwire<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let size = out.len().min(4096);
        let count = self.bytes.read(&mut out[..size])?;
        if self.bytes.position() >= self.after {
            self.flag.store(true, Ordering::Relaxed);
        }
        Ok(count)
    }
}

/// Reads `text` through a [`Tripwire`]; answers the outcome and the bytes read.
fn tripwire(text: &str, after: u64, limits: Limits) -> (String, u64) {
    let flag = AtomicBool::new(after == 0);
    let mut reader = Tripwire {
        bytes: io::Cursor::new(text.as_bytes()),
        after,
        flag: &flag,
    };
    let result = product_structure(&mut reader, limits, &flag);
    (outcome(result), reader.bytes.position())
}

/// Sol on ca6b79fb and cdcc37b1: each refusal stops within one read of the
/// byte where the contract says the cap applies, measured from `needle`.
#[test]
#[rustfmt::skip]
fn each_refusal_stops_near_its_cap() {
    let comment = format!("/*{}*/", "x".repeat(8 << 20));
    let gaps = format!("/*{}*/", "x".repeat(65_532)).repeat(50);
    // One part (3 records, 11 bytes of kept strings), then a PRODUCT with a 60,000-byte name.
    let second = file(&format!("{}#20=PRODUCT('q','{}',$,());", part(10, "P", "Part"), "q".repeat(60_000)));
    let zeros = format!("#18446744073709551616{}=X();", "0".repeat(8 << 20));
    let default = Limits::default();
    let cases = [
        ("RecordBytes", default, with_part(&format!("#30=PRODUCT('P',{comment}'Part','',(#2));")), "#30=", 64 << 10),
        ("CommentBytes", default, with_part(&format!("#1=X({comment});")), "#1=", 64 << 10),
        ("RecordBytes", default, with_part(&format!("#30={gaps}PRODUCT('p','Part',$,());")), "#30=", 64 << 10),
        ("Records", Limits { max_records: 3, ..default }, second.clone(), "#20=PRODUCT", 11),
        ("KeptText", Limits { max_kept_text: 12, ..default }, second, "'q','", 5),
        ("InstanceNumber", default, with_part(&zeros), "#1844", 21),
    ];
    for (limit, limits, text, needle, allowed) in cases {
        let (result, read) = tripwire(&text, u64::MAX, limits);
        assert_eq!(result, format!("TooLarge({limit})"), "{limit} at {needle}");
        let most = (text.find(needle).unwrap() + allowed + READ_CHUNK) as u64;
        assert!(read <= most, "{limit} at {needle}: read {read} bytes, at most {most}");
    }
}

/// Fixed-seed mutations of the fixture: each returns a structure or a typed
/// error, and none panics.
#[test]
#[rustfmt::skip]
fn mutated_inputs_never_panic() {
    let pieces: [&[u8]; 12] = [b"'", b"(", b")", b";", b"#", b"\\", b"\\X2\\", "\u{e9}".as_bytes(), b"/*", b"$", b"9", b"."];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    for _ in 0..3_000 {
        let mut text = NESTED.to_vec();
        for _ in 0..1 + next(4) {
            let (at, piece) = (next(text.len()), pieces[next(pieces.len())].iter().copied());
            match next(3) {
                0 => drop(text.splice(at..at + 1, piece)),
                1 => drop(text.splice(at..at, piece)),
                _ => drop(text.remove(at)),
            }
        }
        let _ = within(&text, Limits::default());
    }
}

#[test]
fn cancellation_stops_the_read_within_one_chunk() {
    assert_eq!(
        tripwire(&with_part(""), 0, Limits::default()).0,
        "Cancelled"
    );
    let text = file(&format!("#1=POLYLINE('{}',(#2));", "x".repeat(8 << 20)));
    let limits = Limits {
        max_string_bytes: 16 << 20,
        ..Limits::default()
    };
    let (result, read) = tripwire(&text, 1 << 20, limits);
    assert_eq!(result, "Cancelled");
    assert!(read <= (1 << 20) + READ_CHUNK as u64, "read {read} bytes");
}
